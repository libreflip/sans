//! sans-core – Libreflip backend daemon core library

mod camera;
mod config;
mod controller;
mod hardware;

#[cfg(target_os = "linux")]
pub use crate::camera::VLCamera as Camera;
pub use crate::camera::{Camera as CameraTrait, CameraConfig, CameraType};
pub use crate::config::{
    prepare_machine_profile, resolve_machine_profile_path, BoardProfile, CameraProfiles,
    CameraRoleProfile, CropGeometry, MachineProfile, MotionProfile, PageWidthProfile,
    PickupProfile, PreparedMachineProfile, ProfileSaveError, StartupError, TimeoutProfile,
    MACHINE_PROFILE_TEMPLATE,
};
pub use crate::controller::{
    bootstrap, ControllerClosed, ControllerHandle, ControllerIntent, ControllerSnapshot,
    MachineFactory, MachineScreen, SetupBlocker, SetupState,
};
pub use crate::hardware::{HwClient, HwError, HwLine};
