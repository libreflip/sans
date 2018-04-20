//! A hardware agnostic wrapper for different camera backends
//! 
//! By defalt the camera capture functionality is being implemented by
//! the rscam crate, using the V4L2 backend. Via this API it is however possible
//! to load other camera backends without changing other code functionality.
//! 

pub mod vl_cam;


pub struct CameraConfig {

}


/// A hardware agnostic camera trait
pub trait Camera {

    /// Tries to run the very lengthy and time-intensive process of
    /// auto-configuring this camera device. It can optionally be provided 
    /// with a `target` config which acts as a guideline to what 
    /// configuration is **desired**. This is however not enforced!
    fn auto_config(&mut self, Option<CameraConfig>) -> Result<(), CameraError>;

    /// Capture an image into memory
    fn capture_image(&self) -> Result<(), CameraError>;

    /// Capture a short burst of video to memory
    fn capture_video(&self, fps: u32, time: u32) -> Result<(), CameraError>;

    /// Initialise the liveview stream with a specific framerate
    fn init_liveview(&self, fps: u32) -> Result<(), CameraError>;

    /// Stop liveview stream
    fn stop_liveview(&self) -> Result<(), CameraError>;

    /// Get a data fragment from liveview buffer
    fn get_liveview_chunk(&self);
}


/// Error codes that are shared across the camera module
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