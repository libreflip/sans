//! A hardware agnostic wrapper for different camera backends
//! 
//! By defalt the camera capture functionality is being implemented by
//! the rscam crate, using the V4L2 backend. Via this API it is however possible
//! to load other camera backends without changing other code functionality.
//! 


/// A hardware agnostic camera trait
pub trait Camera {

    /// Capture an image into memory
    fn capture_image(&self) -> Result<bool, ()>;

    /// Capture a short burst of video to memory
    fn capture_video(&self, fps: u32, time: u32) -> Result<bool, ()>;

    /// Initialise the liveview stream with a specific framerate
    fn init_liveview(&self, fps: u32) -> Result<bool, ()>;

    /// Stop liveview stream
    fn stop_liveview(&self) -> Result<bool, ()>;

    /// Get a data fragment from liveview buffer
    fn get_liveview_chunk(&self);
}