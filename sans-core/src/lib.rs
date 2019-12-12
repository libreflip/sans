//! sans-core – Libreflip backend daemon core library

#[macro_use]
extern crate serde_derive;
extern crate rscam;

mod camera;
// mod config;
// mod hardware;

pub use crate::camera::{Camera as CameraTrait, CameraConfig, Orientation, VLCamera as Camera};
// pub use crate::config::SansConfig;
// pub use crate::hardware::{Hardware, Response, Direction, Command, Status};

// /// Core `sans` state, initiliasers & run loop
// pub struct Sans {
//     cameras: Vec<Camera>,
//     hardware: Hardware,
// }

// pub enum SansErrors {
//     ShootingInProgress(String),
// }

// impl Sans {
//     /// A factory function to create a new Sans container
//     pub fn new(cfg: SansConfig) -> Option<Self> {
//         Some(Sans {
//             cameras: vec![
//                 Camera::new(&*cfg.cameras.left, CameraType::Left).ok()?,
//                 Camera::new(&*cfg.cameras.right, CameraType::Right).ok()?,
//             ],
//             hardware: Hardware::new(&*cfg.hw_port, 9200, unsafe { std::mem::uninitialized() })?.0,
//         })
//     }

//     /// Start a new process, Err if one is already in progress
//     pub fn start_process(&mut self) -> Result<(), SansErrors> {
//         Ok(())
//     }

//     /// Cancel an already running process
//     pub fn cancel_running(&mut self) -> Result<(), SansErrors> {
//         Ok(())
//     }
// }
