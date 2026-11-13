//! sans-core – Libreflip backend daemon core library

mod camera;
mod config;
mod controller;
mod hardware;
mod lift;
mod odrive;
mod stop;

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
    OdriveProfile, PageWidthProfile, PickupProfile, PreparedMachineProfile, ProfileSaveError,
    StartupError, TimeoutProfile, MACHINE_PROFILE_TEMPLATE,
};
pub use crate::controller::{
    bootstrap, CapturePreview, CaptureStatus, ControllerClosed, ControllerHandle, ControllerIntent,
    ControllerMachine, ControllerSnapshot, LiftAxisStatus, MachineEvent, MachineFactory,
    MachineScreen, ScanReadiness, SetupBlocker, SetupDiagnostic, SetupState, TelemetrySummary,
};
pub use crate::hardware::{
    ConnectionEpoch, HwError, MonospaceClient, MonospaceConnection, MonospaceEvent,
    MonospaceEventKind, MonospaceEvents, MonospaceFault, MonospaceUrgentWriter,
    MONOSPACE_BAUD_RATE,
};
pub use crate::lift::{
    LiftEvent, LiftFault, LiftOperations, LiftOutcome, LiftRejection, LiftRequest, LiftStatus,
    LinkPort, OperationId, PageTurnLeg, StopCause, StopEvidence, TouchdownHold, VacuumEvidence,
};
pub use crate::odrive::{
    spawn_link, AxisState, CommandResult, DiscoveryRecord, FibreConnection, FibreOpener,
    LinkClosed, LinkCommand, LinkConfig, LinkEngine, LinkEvent, LinkHandle, LinkPhase, LinkRunner,
    LinkStatus, OpenRequest, PacketReader, PacketWriter, Priority, TelemetrySample,
    TelemetryValues, Ticket, Transmit, UsbFibreOpener,
};
pub use crate::stop::{ClearRefused, Relays, StopControl, StopStatus, StopVerdict};
