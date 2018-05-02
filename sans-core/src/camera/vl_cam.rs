//! Hardware specific implementation for V4L2 cameras
//!
//!

use super::{Camera, CameraError, CameraConfig};
use rscam::Camera as CameraBackend;

pub struct VLCamera {
    backend: CameraBackend,
}

impl VLCamera {
    
    /// Bind a new camera with a path on the FS
    pub fn new(path: &str) -> Result<VLCamera, CameraError> {
        return match CameraBackend::new(path) {
            Ok(c) => Ok(VLCamera { backend: c }),
            Err(e) => Err(CameraError::ReceiverNotFound(format!(
                "Failed to allocate '{}': {:?}",
                path, e
            ))),
        };
    }
}

impl Camera for VLCamera {

    fn auto_config(&mut self, cfg: Option<CameraConfig>) -> Result<(), CameraError> {
        unimplemented!()
    }

    fn capture_image(&self) -> Result<(), CameraError> {
        unimplemented!()
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
