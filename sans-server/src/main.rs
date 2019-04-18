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

// use rocket::http::RawStr;
// use rocket::request::{Form, FromFormValue};
// use rocket::response::Redirect;

// use rocket::response::NamedFile;

// use sans_core::{Camera, CameraTrait, CameraType};
// use std::io::{self, stdin};
// use std::path::{Path, PathBuf};

// #[derive(FromForm)]
// struct CaptureRequest {}

// #[post("/shoot", data = "<_form>")]
// fn shoot(_form: Form<CaptureRequest>) -> Result<Redirect, String> {
//     println!("Capturing new pictures!");

//     let left = Camera::new("/dev/video0".into(), CameraType::Left).unwrap();
//     left.capture_image().unwrap();

//     let right = Camera::new("/dev/video0".into(), CameraType::Right).unwrap();
//     right.capture_image().unwrap();

//     Ok(Redirect::to("/"))
// }

// #[get("/")]
// pub fn index() -> io::Result<NamedFile> {
//     NamedFile::open("static/index.html")
// }

// fn rocket() -> rocket::Rocket {
//     rocket::ignite().mount("/", routes![index, shoot])
// }

fn main() {
    // rocket().launch();
    // for n in 0..=11 {
    //     print!("Camera: {}", n);
    //     let left = match Camera::new(&format!("/dev/video{}", n), CameraType::Left) {
    //         Ok(c) => c,
    //         _ => continue
    //     };
    //     match left.capture_image() {
    //         Ok(_) => println!("OK!"),
    //         Err(_) => println!("FAILED"),
    //     };
    // }
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
