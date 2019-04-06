//! Hardware abstraction module

mod reader;
mod protocol;

use serialport::{self, prelude::*};
use std::io::{self, Write};
use std::time::Duration;

use protocol::{Response, Command, Status};

pub struct Hardware {
    rxtx: Box<SerialPort>,
    port: String,
    settings: SerialPortSettings,
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

    /// Try to read a byte - nonblocking
    ///
    /// This function returns `None` if there's no byte to be had
    fn read_byte(&mut self) -> Option<u8> {
        let mut buf = [0; 1];
        if self.rxtx.bytes_to_read().ok()? > 0 && self.rxtx.read_exact(&mut buf).is_ok() {
            Some(buf[0])
        } else {
            None
        }
    }

    /// This function get's thrown on a seperate thread to read and write
    ///
    /// 1. Check if a response can be read
    ///   - Read response and send via out channel
    /// 2. Check if any messages to send are queued
    ///   - Write response via serial port
    fn run(&mut self) {

        let resp = Response::build(|| self.read_byte());
    }
}
