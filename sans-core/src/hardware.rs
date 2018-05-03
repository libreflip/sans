//! Hardware abstraction module

mod parser;

use serialport::prelude::*;
use std::io::{self, Write};
use std::time::Duration;

struct Hardware {
    rxtx: Box<SerialPort>,
    port: String,
    settings: SerialPortSettings,
}

/// All supported commands for this hardware
enum Command {
    MoveBox = 0b00000010,
    Lighting = 0b00000100,
    FlipPage = 0b00001000,
}

/// Second-byte is a payload described here
enum Payload {
    Flag(bool),
    Value(u8),
}

struct Response {
    state: u8,
    length: u8,
}

impl Hardware {
    /// Attempt to open a serial connection
    pub fn new(port: &str, baud: u32) -> Option<Hardware> {
        let mut settings: SerialPortSettings = Default::default();
        settings.timeout = Duration::from_millis(10);
        settings.baud_rate = baud.into();

        return Some(Hardware {
            rxtx: serialport::open_with_settings(&port, &settings).ok()?,
            port: String::from(port),
            settings,
        });
    }

    /// Start an async I/O runner for this port
    pub fn run(&mut self) {
        loop {
            self.poll();
        }
    }

    /// Do a polling-loop for the serialport
    fn poll(&mut self) -> Option<()> {
        let mut buffer = vec![0; 2];
        self.rxtx.read_exact(&mut buffer).ok()?;

        /* Parse received input */
        parser::get_response(buffer);

        /* Return the all-clear */
        return Some(());
    }
}
