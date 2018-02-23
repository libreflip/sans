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


extern crate rscam;
use rscam::{Camera, Config};
use std::fs;
use std::io::prelude::*;


mod binding;
mod process;
mod rest;
mod state;
mod util;

fn main() {

    let mut camera = Camera::new("/dev/video0").unwrap();

    camera
        .start(&Config {
            interval: (1, 60), // 30 fps.
            resolution: (1920, 1080),
            format: b"MJPG",
            ..Default::default()
        })
        .unwrap();

    for i in 0..120 {
        let frame = camera.capture().unwrap();
        let mut file = fs::File::create(&format!("frame-{}.jpg", i)).unwrap();
        file.write_all(&frame[..]).unwrap();
    }

}


//     let counter = Arc::new(Mutex::new(0));
//     let mut handles = vec![];

//     for _ in 0..10 {
//         let counter = Arc::clone(&counter);
//         let handle = thread::spawn(move || {
//             let mut num = counter.lock().unwrap();
//             *num += 1;
//         });
//         handles.push(handle);
//     }

//     for handle in handles {
//         handle.join().unwrap();
//     }

//     println!("Result: {}", *counter.lock().unwrap());
// }