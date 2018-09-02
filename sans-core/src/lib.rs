//! sans-core – Libreflip backend daemon core library

#![feature(non_modrs_mods)]
#![feature(extern_prelude)]

extern crate sans_types;

extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate toml;

extern crate rscam;
extern crate serialport;

mod camera;
mod config;
mod hardware;

pub use config::SansConfig;
pub use camera::{CameraType, VLCamera as Camera};
pub use hardware::Hardware;

/// Core `sans` state, initiliasers & run loop
pub struct Sans {
    cameras: Vec<Camera>,
    hardware: Hardware,
}

pub enum SansErrors {
    ShootingInProgress(String),
}

impl Sans {
    /// A factory function to create a new Sans container
    pub fn new(cfg: SansConfig) -> Option<Self> {
        Some(Sans {
            cameras: vec![
                Camera::new(&*cfg.cameras.left, CameraType::Left).ok()?,
                Camera::new(&*cfg.cameras.right, CameraType::Right).ok()?,
            ],
            hardware: Hardware::new(&*cfg.hw_port, 9200)?,
        })
    }

    /// Start a new process, Err if one is already in progress
    pub fn start_process(&mut self) -> Result<(), SansErrors> {
        Ok(())
    }

    /// Cancel an already running process
    pub fn cancel_running(&mut self) -> Result<(), SansErrors> {
        Ok(())
    }
}
