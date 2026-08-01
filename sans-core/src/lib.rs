//! sans-core – Libreflip backend daemon core library

#[macro_use]
extern crate serde_derive;

mod camera;
mod config;
mod hardware;

pub use crate::camera::{Camera as CameraTrait, CameraConfig, CameraType, VLCamera as Camera};
pub use crate::config::SansConfig;
pub use crate::hardware::{HwClient, HwError, HwLine};
