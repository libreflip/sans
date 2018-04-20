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

extern crate sans_core;
extern crate sans_types;
extern crate rocket;

#[get("/")]
fn hello() -> &'static str {
    "Hello, world!"
}

fn main() {
    println!("Hello, world!");
    rocket::ignite().mount("/", routes![hello]).launch();
}
