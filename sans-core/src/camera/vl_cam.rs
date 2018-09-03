//! Hardware specific implementation for V4L2 cameras
//!
//!

use super::{Camera, CameraConfig, CameraError, CameraType};
pub use rscam::Camera as CameraBackend;
use rscam::Config;
use std::{fs, io::Write};

pub struct VLCamera {
    backend: CameraBackend,
    meta: CameraType,
}

impl VLCamera {
    /// Bind a new camera with a path on the FS
    pub fn new(path: &str, meta: CameraType) -> Result<VLCamera, CameraError> {
        let mut backend = match CameraBackend::new(path) {
            Ok(c) => c,
            Err(e) => {
                return Err(CameraError::ReceiverNotFound(format!(
                    "Failed to allocate '{}': {:?}",
                    path, e
                )))
            }
        };

        backend
            .start(&Config {
                interval: (1, 30), // 30 fps.
                resolution: (1280, 720),
                format: b"MJPG",
                ..Default::default()
            }).unwrap();

        Ok(VLCamera { backend, meta })
    }
}

impl Camera for VLCamera {
    fn auto_config(&mut self, cfg: Option<CameraConfig>) -> Result<(), CameraError> {
        unimplemented!()
    }

    fn capture_image(&self) -> Result<(), CameraError> {
        let frame = self.backend.capture().unwrap();
        let mut file = fs::File::create(&format!("static/img/frame-{}.jpg", self.meta)).unwrap();
        file.write_all(&frame[..]).unwrap();
        Ok(())
    }

    fn capture_video(&self, fps: u32, time: u32) -> Result<(), CameraError> {
        unimplemented!()
    }

    fn start_liveview(&self, fps: u32) -> Result<(), CameraError> {
        unimplemented!()
    }

    fn stop_liveview(&self) -> Result<(), CameraError> {
        unimplemented!()
    }

    fn get_liveview_chunk(&self) {
        unimplemented!()
    }
}
