//! MKS ODrive Mini lift-axis link over legacy native Fibre USB.
//!
//! The factory firmware is never modified: Sans never calls save, erase, reboot
//! or DFU functions. Typed Touchdown commands temporarily select torque control
//! with the firmware velocity limiter; motion and recovery restore position control.

mod descriptor;
mod engine;
mod protocol;
mod runner;
mod transport;
mod usb;

pub use self::engine::{
    AxisState, CommandResult, DiscoveryRecord, LinkCommand, LinkConfig, LinkEngine, LinkEvent,
    LinkPhase, LinkStatus, Priority, TelemetrySample, TelemetryValues, Ticket, TorqueSettings,
    Transmit,
};
pub(crate) use self::protocol::{
    AXIS_ERROR_MIN_ENDSTOP_PRESSED, CONTROL_MODE_POSITION_CONTROL, CONTROL_MODE_TORQUE_CONTROL,
    INPUT_MODE_PASSTHROUGH, INPUT_MODE_TRAP_TRAJ,
};
pub use self::runner::{spawn_link, LinkClosed, LinkHandle, LinkRunner};
pub use self::transport::{FibreConnection, FibreOpener, OpenRequest, PacketReader, PacketWriter};
pub use self::usb::UsbFibreOpener;
