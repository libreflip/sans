//! Sans – Libreflip backend daemon
//!
//! The design principle for this software is taken from Aouglas Adams:
//!
//! > Don't Panic!
//!
//! Also something about towels.
//!
//! The daemon should never just stop. Every possible error needs to be wrapped
//! into a Result<_> and communicate them to the user interface. Any actual panic
//! is a condition in the operation that prevents communication with the UI. These
//! should however still be logged.
#![allow(dead_code, unused_variables)]


extern crate rscam;
extern crate clap;

mod binding;
mod process;
mod util;


/// Core sans state object that provides a hardware abstraction API
/// 
/// 
struct Sans {

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
