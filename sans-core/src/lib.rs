//! sans-core – Libreflip backend daemon core library

mod camera;
mod config;
mod controller;
mod hardware;

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
    CameraRoleProfile, CropGeometry, MachineProfile, MotionProfile, PageWidthProfile,
    PickupProfile, PreparedMachineProfile, ProfileSaveError, StartupError, TimeoutProfile,
    MACHINE_PROFILE_TEMPLATE,
};
pub use crate::controller::{
    bootstrap, CapturePreview, CaptureStatus, ControllerClosed, ControllerEvent, ControllerHandle,
    ControllerIntent, ControllerMachine, ControllerSnapshot, LigatureMachine,
    LigatureMachineFactory, MachineFactory, MachineScreen, SetupBlocker, SetupState,
};
#[cfg(target_os = "linux")]
pub use crate::controller::{SansMachine, SansMachineFactory};
pub use crate::hardware::ligature::{
    parse_ligature_line, ConnectionEpoch, LigatureCaptureSample, LigatureClient, LigatureCommand,
    LigatureEvent, LigatureFault, LigatureLine, LigaturePosition, LigatureProtocolError,
    LigatureReconnect, LigatureRequest, LigatureSession, LigatureSessionError, LigatureState,
    LigatureStatus, LigatureTransportError, LigatureWire, OperationId, PositionTrust,
    ProtocolErrorTerminal, ProtocolTerminal, RequestPriority, SerialLigatureWire, LIGATURE_BAUD,
};
pub use crate::hardware::{HwClient, HwError, HwLine};
