extern crate sans_core;

use sans_core::{Camera, Orientation, CameraConfig, CameraTrait};

fn main() {

    for n in 0..=11 {
        let cfg = CameraConfig {
            path: format!("/dev/video{}", n),
            format: "MJPG".into(),
            interval: (1, 14),
            resolution: (4224, 3156)
        };

        let cam = match Camera::new(Orientation::Left, cfg) {
            Ok(c) => c,
            Err(_) => continue,
        };
        println!("Init /dev/video{}", n);
    }
        
}
