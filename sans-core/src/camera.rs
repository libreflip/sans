//! A hardware agnostic wrapper for different camera backends
//!
//! By defalt the camera capture functionality is being implemented by
//! the rscam crate, using the V4L2 backend. Via this API it is however possible
//! to load other camera backends without changing other code functionality.
//!
//! In the future this should also be possible via a plugin system that doesn't
//! require other developers to have to modify `sans` sources.

mod vl_cam;
pub use self::vl_cam::VLCamera;

/// The camera type, usually location
pub enum CameraType {
    Left,
    Right,
}

use std::fmt;
impl fmt::Display for CameraType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use CameraType::*;
        write!(
            f,
            "{}",
            match self {
                Left => "left",
                Right => "right",
            }
        )
    }
}

/// A camera-backend overarching configuration setting
///
/// It describes capabilites such as resolutions, framerates
/// and if a camera can be put into "live view" mode.
pub struct CameraConfig {
    pub id: String,
}

/// A hardware agnostic camera trait
pub trait Camera {
    /// Tries to run the very lengthy and time-intensive process of
    /// auto-configuring this camera device. It can optionally be provided
    /// with a `target` config which acts as a guideline to what
    /// configuration is **desired**.
    ///
    /// As this is a trait it is however not enforced!
    fn auto_config(&mut self, Option<CameraConfig>) -> Result<(), CameraError>;

    /// Capture an image into memory
    fn capture_image(&self) -> Result<(), CameraError>;

    /// Capture a short burst of video to memory
    fn capture_video(&self, fps: u32, time: u32) -> Result<(), CameraError>;

    /// Initialise the liveview stream with a specific framerate
    fn start_liveview(&self, fps: u32) -> Result<(), CameraError>;

    /// Stop liveview stream
    fn stop_liveview(&self) -> Result<(), CameraError>;

    /// Get a data fragment from liveview buffer
    fn get_liveview_chunk(&self);
}

/// Error codes that are shared across the camera module
#[derive(Debug)]
pub enum CameraError {
    /// When the camera target isn't known
    ReceiverNotFound(String),
    /// A general error occured when initialising a camera
    FailedInitialisation(String),
    /// Failed to capture a image
    FailedToCapture,
    /// Failed to initialise a live buffer for video or streaming
    FailedToBurstCapture,
    /// Invalid camera mode was specified
    InvalidCameraMode(String, String),
}
