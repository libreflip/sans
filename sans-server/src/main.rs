//! sans-server – Libreflip sans server daemon
//!
//! The design principle for this software is taken from Douglas Adams:
//!
//! > Don't Panic!
//!
//! Also something about towels.
//!
//! The daemon should never just stop. Every possible error needs to be wrapped
//! into a Result<_> and communicate them to the user interface. Any actual panic
//! is a condition in the operation that prevents communication with the UI. These
//! should however still be logged.

use sans_core::{Hardware, Command, Direction};
use std::sync::mpsc::channel;
use std::{io::{self, Write}, thread, time::Duration};

fn main() {
    let (send, recv) = channel();
    let (mut hw, recv) = Hardware::new("/dev/ttyACM0", 9600, recv).expect("Failed to initialise hardware!");

    // Spawn the hardware handle
    thread::spawn(move || hw.run());

    (|| -> Result<(), Box<std::error::Error>> {
        loop {
            print!("> ");
            io::stdout().flush().ok().expect("Could not flush stdout");
            let mut line = String::new();
            io::stdin().read_line(&mut line)?;

            match line.as_str().trim() {
                "quit" => break,
                "light 1" => send.send(Command::Lighting(true))?,
                "light 0" => send.send(Command::Lighting(false))?,
                "move 1" => send.send(Command::MoveBox(Direction::Up))?,
                "move 0" => send.send(Command::MoveBox(Direction::Down))?,
                "flip" => send.send(Command::FlipPage(50))?,
                _ => continue,
            }

            let resp = match recv.recv_timeout(Duration::from_secs(5)) {
                Ok(i) => i,
                Err(_) => continue,
            };
            println!("{:?}", resp);
        }

        Ok(())
    })().expect("Crashed due to some error!");
}
