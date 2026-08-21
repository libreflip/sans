extern crate sans_core;

#[cfg(target_os = "linux")]
use sans_core::{Camera, CameraTrait, CameraType};

#[cfg(target_os = "linux")]
fn main() {
    let left = Camera::new("/dev/video0".into(), CameraType::Left).unwrap();
    left.capture_image().unwrap();

    // let right = Camera::new("/dev/right".into(), CameraType::Right);
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("camcal requires Linux V4L2 camera support");
}
