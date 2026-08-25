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
    bootstrap, CapturePreview, CaptureStatus, ControllerClosed, ControllerHandle, ControllerIntent,
    ControllerMachine, ControllerSnapshot, MachineFactory, MachineScreen, SetupBlocker, SetupState,
};
pub use crate::hardware::{HwClient, HwError, HwLine};
