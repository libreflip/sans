//! sans-core – Libreflip backend daemon core library

mod camera;
mod config;
mod controller;
mod hardware;
mod odrive;

#[cfg(target_os = "linux")]
pub use crate::camera::{
    capture_configured_camera, CameraDiagnosticError, V4lCameraMachineFactory,
};
pub use crate::camera::{
    CameraCaptureError, CameraPairSetupError, CameraRole, CapturePair, CapturePairError,
    CapturePairMachine, CapturedFrame, FrameBuildError, FrameCrop, NativeDetailPreviewError,
    PreviewImage, RoleCamera,
};
pub use crate::config::{
    prepare_machine_profile, resolve_machine_profile_path, BoardProfile, CameraProfiles,
    CameraRoleProfile, CropGeometry, LiftAxisProfile, LiftDirection, MachineProfile, MotionProfile,
    MotorLimits, OdriveProfile, PageWidthProfile, PickupProfile, PreparedMachineProfile,
    ProfileSaveError, StartupError, TimeoutProfile, MACHINE_PROFILE_SCHEMA_VERSION,
    MACHINE_PROFILE_TEMPLATE, ODRIVE_ACCELERATION_CAP_TURNS_PER_S2, ODRIVE_CURRENT_LIMIT_CAP_A,
    ODRIVE_VELOCITY_CAP_TURNS_PER_S,
};
pub use crate::controller::{
    bootstrap, CapturePreview, CaptureStatus, ControllerClosed, ControllerHandle, ControllerIntent,
    ControllerMachine, ControllerSnapshot, LiftAxisStatus, MachineFactory, MachineScreen,
    ScanReadiness, SetupBlocker, SetupDiagnostic, SetupState, TelemetrySummary,
};
pub use crate::hardware::{
    ConnectionEpoch, HwError, MonospaceClient, MonospaceConnection, MonospaceEvent,
    MonospaceEventKind, MonospaceEvents, MonospaceFault, MonospaceUrgentWriter,
    MONOSPACE_BAUD_RATE,
};
pub use crate::odrive::{
    spawn_link, AxisState, CommandResult, DiscoveryRecord, FibreConnection, FibreOpener,
    LinkCommand, LinkConfig, LinkEngine, LinkEvent, LinkHandle, LinkPhase, LinkRunner, LinkStatus,
    OpenRequest, PacketReader, PacketWriter, Priority, TelemetrySample, TelemetryValues, Ticket,
    Transmit, UsbFibreOpener, ODRIVE_USB_PRODUCT_ID, ODRIVE_USB_VENDOR_ID,
};
