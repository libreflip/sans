//! Hardware specific implementation for V4L2 cameras
//!
//!

use super::{Camera, CameraConfig, CameraError, Orientation};
use rscam::Config;
pub use rscam::{Camera as CameraBackend, ResolutionInfo};
use std::{fs, io::Write};

pub struct VLCamera {
    backend: CameraBackend,
    meta: Orientation,
}

impl VLCamera {
    
    /// Bind a new camera with a path on the FS
    pub fn new(meta: Orientation, cfg: CameraConfig) -> Result<VLCamera, CameraError> {
        let CameraConfig {
            path,
            format,
            interval,
            resolution,
        } = cfg;

        let mut backend = match CameraBackend::new(path.as_str()) {
            Ok(c) => c,
            Err(e) => {
                return Err(CameraError::ReceiverNotFound(format!(
                    "Failed to allocate '{}': {:?}",
                    path, e
                )))
            }
        };
        
        match backend.start(&Config {
            interval: (1, 14),
            resolution: (4224, 3156),
            format: b"MJPG",
            ..Default::default()
        }) {
            Ok(res) => Ok(res),
            Err(_) => Err(CameraError::FailedInitialisation("...".into())),
        }?;

        Ok(VLCamera { backend, meta })
    }
}

impl Camera for VLCamera {
    fn auto_config(&mut self, _cfg: Option<CameraConfig>) -> Result<(), CameraError> {
        unimplemented!()
    }

    fn capture_image(&self) -> Result<(), CameraError> {
        let frame = self.backend.capture().unwrap();
        fs::remove_file(&format!("static/img/frame-{}.jpg", self.meta));
        let mut file = fs::File::create(&format!("static/img/frame-{}.jpg", self.meta)).unwrap();
        file.write_all(&frame[..]).unwrap();
        Ok(())
    }

    fn capture_video(&self, _fps: u32, _time: u32) -> Result<(), CameraError> {
        unimplemented!()
    }

    fn start_liveview(&self, _fps: u32) -> Result<(), CameraError> {
        unimplemented!()
    }

    fn stop_liveview(&self) -> Result<(), CameraError> {
        unimplemented!()
    }

    fn get_liveview_chunk(&self) {
        unimplemented!()
    }
}
