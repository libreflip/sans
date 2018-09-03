//! sans-server – Libreflip sans server daemon
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
#![feature(plugin, decl_macro)]
#![plugin(rocket_codegen)]

extern crate rocket;
extern crate rocket_contrib;
extern crate sans_core;
extern crate sans_types;

use std::io::stdin;
use sans_core::{Camera, CameraTrait, CameraType};

fn main() {

    for n in 0..=11 {
        let left = Camera::new(&format!("/dev/video{}", n), CameraType::Left).unwrap();
        print!("Camera: {}", n);
        match left.capture_image() {
            Ok(n) => println!("OK!"),
            Err(_) => println!("FAILED"),
        };
    }
}

    // let right = Camera::new("/dev/video1".into(), CameraType::Right).unwrap();

    // println!("Press <enter> to take two pictures – reload your browser after");
    // loop {
    //     let mut _s = String::new();
    //     stdin()
    //         .read_line(&mut _s)
    //         .expect("Failed to read user input!");

    //     right.capture_image().unwrap();
    //     println!("*click*");
//     }
// }
