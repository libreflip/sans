extern crate sans_core;

use sans_core::{Camera, CameraType, CameraTrait};

fn main() {
    let left = Camera::new("/dev/video0".into(), CameraType::Left).unwrap();
    left.capture_image().unwrap();

    // let right = Camera::new("/dev/right".into(), CameraType::Right);

    
}