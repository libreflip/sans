//! Sans – Libreflip backend daemon


mod binding;
mod process;
mod rest;
mod state;
mod util;

extern crate rscam;
use rscam::{Camera, Config};
use std::fs;
use std::io::prelude::*;

fn main() {

    let mut camera = Camera::new("/dev/video0").unwrap();

    camera
        .start(&Config {
            interval: (1, 30), // 30 fps.
            resolution: (1280, 720),
            format: b"MJPG",
            ..Default::default()
        })
        .unwrap();

    for i in 0..10 {
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