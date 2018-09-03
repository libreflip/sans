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

use std::io::{stdin, Cursor};

#[get("/")]
fn hello() -> Result<Response<'static>, Status> {
    let body = "<!DOCTYPE html>
<html>
<body>
    <a href=\"/pictures\">Camera calibrator</a>
</body>
</html>";

    Response::build()
        .header(ContentType::HTML)
        .sized_body(Cursor::new(body))
        .ok()
}

use rocket::http::{ContentType, Status};
use rocket::response::Response;
use sans_core::{Camera, CameraTrait, CameraType};
use std::thread;

#[get("/pictures")]
fn pictures() -> Result<Response<'static>, Status> {
    let body = "<!DOCTYPE html>
<html>
<head>
    <title>Libreflip Camera Calibrator 3000</title>
</head>
<body>
    <h1>Libreflip Camera Calibrator 3000</h1>

    <img src=\"frame-left.jpg\"  width=450 />
    <img src=\"frame-right.jpg\" width=450 />
</body>
</html>";

    Response::build()
        .header(ContentType::HTML)
        .sized_body(Cursor::new(body))
        .ok()
}

fn main() {
    println!("=== sans server ===");
    thread::spawn(|| {
        rocket::ignite()
            .mount("/", routes![hello, pictures])
            .launch();
    });

    let left = Camera::new("/dev/video0".into(), CameraType::Left).unwrap();
    // let right = Camera::new("/dev/video1".into(), CameraType::Right).unwrap();

    println!("Press <enter> to take two pictures – reload your browser after");
    loop {
        let mut _s = String::new();
        stdin()
            .read_line(&mut _s)
            .expect("Failed to read user input!");

        left.capture_image().unwrap();
        // right.capture_image().unwrap();
        println!("*click*");
    }
}
