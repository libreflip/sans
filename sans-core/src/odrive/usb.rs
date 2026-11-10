//! Production packet transport: the ODrive's USB vendor interface via `nusb`.
//!
//! This adapter only enumerates, claims and moves whole packets. It has no
//! automated test; its first real exercise is supervised work in #60. It never
//! touches the CDC interfaces, which stay with the kernel.

use std::time::Duration;

use nusb::descriptors::TransferType;
use nusb::transfer::{Buffer, Bulk, In, Out, TransferError};
use nusb::{Endpoint, MaybeFuture};

use super::transport::{FibreConnection, FibreOpener, OpenRequest, PacketReader, PacketWriter};

// Sources at upstream ODrive revision 7831d795235e5ef8535e4b46621a0721b458ec8f
// (fw-v0.5.1), fetched 2026-11-10, and the #57 capture (`1209:0d32`).

/// USB vendor ID. `Firmware/Board/v3/Src/usbd_desc.c` `USBD_VID 0x1209`.
pub const ODRIVE_USB_VENDOR_ID: u16 = 0x1209;
/// USB product ID. `Firmware/Board/v3/Src/usbd_desc.c` `USBD_PID_FS 0x0D32`.
pub const ODRIVE_USB_PRODUCT_ID: u16 = 0x0d32;
/// The native vendor-class interface: `bInterfaceNumber 0x02`, class `0x00`
/// "vendor specific", in the configuration descriptor of
/// `Firmware/Board/v3/Middlewares/ST/STM32_USB_Device_Library/Class/CDC/Src/usbd_cdc.c`.
const VENDOR_INTERFACE: u8 = 2;
/// `.../Class/CDC/Inc/usbd_cdc.h` `ODRIVE_OUT_EP 0x03`, bulk.
const BULK_OUT: u8 = 0x03;
/// `.../Class/CDC/Inc/usbd_cdc.h` `ODRIVE_IN_EP 0x83`, bulk.
const BULK_IN: u8 = 0x83;

/// Opens the ODrive by USB serial over the native vendor interface.
#[derive(Clone, Copy, Debug, Default)]
pub struct UsbFibreOpener;

impl FibreOpener for UsbFibreOpener {
    fn open(&mut self, request: &OpenRequest) -> Result<FibreConnection, String> {
        let devices = nusb::list_devices()
            .wait()
            .map_err(|error| format!("cannot enumerate USB devices: {error}"))?;
        let odrives: Vec<_> = devices
            .filter(|device| {
                device.vendor_id() == ODRIVE_USB_VENDOR_ID
                    && device.product_id() == ODRIVE_USB_PRODUCT_ID
            })
            .collect();
        let matching: Vec<_> = odrives
            .iter()
            .filter(|device| {
                device
                    .serial_number()
                    .is_some_and(|serial| serial.eq_ignore_ascii_case(&request.usb_serial))
            })
            .collect();
        let info = match matching.as_slice() {
            [one] => *one,
            [] => {
                let others: Vec<_> = odrives
                    .iter()
                    .map(|device| device.serial_number().unwrap_or("<no serial>").to_owned())
                    .collect();
                return Err(format!(
                    "no ODrive 1209:0d32 with USB serial {} is connected{}",
                    request.usb_serial,
                    if others.is_empty() {
                        String::new()
                    } else {
                        format!(" (found serials: {})", others.join(", "))
                    }
                ));
            }
            many => {
                return Err(format!(
                    "{} ODrives with USB serial {} are connected; refusing to choose one",
                    many.len(),
                    request.usb_serial
                ));
            }
        };
        let usb_serial = info.serial_number().unwrap_or_default().to_owned();
        let device = info
            .open()
            .wait()
            .map_err(|error| format!("cannot open ODrive {usb_serial}: {error}"))?;
        check_vendor_interface(&device)?;
        let interface = device
            .claim_interface(VENDOR_INTERFACE)
            .wait()
            .map_err(|error| format!("cannot claim ODrive vendor interface 2: {error}"))?;
        let writer = interface
            .endpoint::<Bulk, Out>(BULK_OUT)
            .map_err(|error| format!("cannot open bulk OUT 0x03: {error}"))?;
        let reader = interface
            .endpoint::<Bulk, In>(BULK_IN)
            .map_err(|error| format!("cannot open bulk IN 0x83: {error}"))?;
        Ok(FibreConnection {
            usb_serial,
            reader: Box::new(UsbReader { endpoint: reader }),
            writer: Box::new(UsbWriter { endpoint: writer }),
        })
    }
}

/// Refuse to open unless interface 2 has exactly bulk OUT 0x03 and bulk IN 0x83.
fn check_vendor_interface(device: &nusb::Device) -> Result<(), String> {
    let configuration = device
        .active_configuration()
        .map_err(|error| format!("cannot read the ODrive's active USB configuration: {error}"))?;
    let interface = configuration
        .interface_alt_settings()
        .find(|setting| {
            setting.interface_number() == VENDOR_INTERFACE && setting.alternate_setting() == 0
        })
        .ok_or("the ODrive has no USB interface 2")?;
    let mut endpoints: Vec<(u8, TransferType)> = interface
        .endpoints()
        .map(|endpoint| (endpoint.address(), endpoint.transfer_type()))
        .collect();
    endpoints.sort_by_key(|(address, _)| *address);
    if endpoints
        != [
            (BULK_OUT, TransferType::Bulk),
            (BULK_IN, TransferType::Bulk),
        ]
    {
        return Err(format!(
            "ODrive interface 2 endpoints are {endpoints:02x?}, expected bulk 0x03 and 0x83"
        ));
    }
    Ok(())
}

struct UsbReader {
    endpoint: Endpoint<Bulk, In>,
}

impl PacketReader for UsbReader {
    fn read_packet(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, String> {
        // Keep one IN transfer pending across timeouts so no reply is lost.
        if self.endpoint.pending() == 0 {
            let length = self.endpoint.max_packet_size();
            self.endpoint.submit(Buffer::new(length));
        }
        let Some(completion) = self.endpoint.wait_next_complete(timeout) else {
            return Ok(None);
        };
        completion
            .status
            .map_err(|error| format!("USB read failed: {error}"))?;
        let mut packet = completion.buffer.into_vec();
        packet.truncate(completion.actual_len);
        Ok(Some(packet))
    }
}

struct UsbWriter {
    endpoint: Endpoint<Bulk, Out>,
}

impl PacketWriter for UsbWriter {
    fn write_packet(&mut self, packet: &[u8], timeout: Duration) -> Result<(), String> {
        let mut buffer = Buffer::new(packet.len());
        buffer.extend_from_slice(packet);
        let completion = self.endpoint.transfer_blocking(buffer, timeout);
        match completion.status {
            Ok(()) => Ok(()),
            Err(TransferError::Cancelled) => Err(format!(
                "USB write did not complete within {} ms",
                timeout.as_millis()
            )),
            Err(error) => Err(format!("USB write failed: {error}")),
        }
    }
}
