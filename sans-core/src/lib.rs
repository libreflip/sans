//! sans-core – Libreflip backend daemon core library

extern crate sans_types;

// extern crate magicrust;
extern crate rscam;

#[macro_use]
extern crate serde_derive;
extern crate serde;
extern crate toml;

mod camera;
mod config;
mod hardware;

pub use config::SansConfig;

/// Core `sans` state, initiliasers & run loop
pub struct Sans {}

impl Sans {
    /// A factory function to create a new Sans container
    pub fn new() -> Self {
        return Sans {};
    }
}

// fn main() {

//     let camera = rscam::Camera::new("/dev/video0").unwrap();
//     let mut vec = Vec::new();

//     camera.formats().all(|f| {
//         match f {
//             Ok(f) => vec.push(f),
//             _ => return false,
//         }
//         return true;
//     });

//     println!("{:?}", vec);

// camera
//     .start(&Config {
//         interval: (1, 60), // 30 fps.
//         resolution: (1920, 1080),
//         format: b"MJPG",
//         ..Default::default()
//     })
//     .unwrap();

// for i in 0..120 {
//     let frame = camera.capture().unwrap();
//     let mut file = fs::File::create(&format!("frame-{}.jpg", i)).unwrap();
//     file.write_all(&frame[..]).unwrap();
// }
// }
