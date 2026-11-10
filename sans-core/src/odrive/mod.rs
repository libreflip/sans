//! MKS ODrive Mini lift-axis link over legacy native Fibre USB.
//!
//! The factory firmware is never modified: Sans never calls save, erase, reboot
//! or DFU functions and never writes control or input mode.

mod descriptor;
mod engine;
mod protocol;
mod runner;
mod transport;
mod usb;

pub use self::engine::{
    AxisState, CommandResult, DiscoveryRecord, LinkCommand, LinkConfig, LinkEngine, LinkEvent,
    LinkPhase, LinkStatus, Priority, TelemetrySample, TelemetryValues, Ticket, Transmit,
};
pub use self::runner::{spawn_link, LinkHandle, LinkRunner};
pub use self::transport::{FibreConnection, FibreOpener, OpenRequest, PacketReader, PacketWriter};
pub use self::usb::{UsbFibreOpener, ODRIVE_USB_PRODUCT_ID, ODRIVE_USB_VENDOR_ID};
