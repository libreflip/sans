//! Hardware specific implementation for V4L2 cameras
//! 
//! 

use binding::Camera;

pub struct VLCamera {
}


impl VLCamera {
    pub fn new(path: &str) -> Result<VLCamera> {
        let backend = rscam::Camera::new(path);
                

    }
}
