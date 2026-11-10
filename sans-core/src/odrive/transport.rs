//! Packet transport seam between the link runner and a Fibre device.
//!
//! One USB bulk transfer carries one packet. The port is split into a reader
//! half and a writer half so a blocked read cannot hold up a write. Every call
//! takes a timeout, so no device I/O is unbounded.

use std::time::Duration;

/// What the link asks an opener to find.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenRequest {
    /// USB serial-number string from the Machine profile, as hex text.
    pub usb_serial: String,
}

/// One opened device: its USB serial string and both packet halves.
pub struct FibreConnection {
    /// The USB serial-number string the device reported.
    pub usb_serial: String,
    /// Receives whole reply packets.
    pub reader: Box<dyn PacketReader>,
    /// Sends whole request packets.
    pub writer: Box<dyn PacketWriter>,
}

/// Opens the one device matching the ODrive vendor and product ID and the
/// profile's USB serial.
///
/// Missing and duplicate matches are errors with an operator-facing reason.
pub trait FibreOpener: Send + 'static {
    /// Open and claim the device. Implementations discard nothing; the runner
    /// drains packets already waiting before the engine sends anything.
    fn open(&mut self, request: &OpenRequest) -> Result<FibreConnection, String>;
}

/// The receiving half of a packet transport.
pub trait PacketReader: Send + 'static {
    /// Wait up to `timeout` for one packet. `Ok(None)` means none arrived.
    fn read_packet(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, String>;
}

/// The sending half of a packet transport.
pub trait PacketWriter: Send + 'static {
    /// Send one whole packet, giving up after `timeout`.
    fn write_packet(&mut self, packet: &[u8], timeout: Duration) -> Result<(), String>;
}
