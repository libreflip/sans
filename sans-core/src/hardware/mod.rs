//! Hardware abstraction module

mod protocol;

use serialport::{self, prelude::*};
use std::io::{self, Write};
use std::sync::mpsc::{channel, Sender, Receiver};
use std::time::Duration;

pub use protocol::{Response, Command, Status, Direction};

pub struct Hardware {
    rxtx: Box<SerialPort>,
    port: String,
    settings: SerialPortSettings,

    /// Send replies via this channel
    sender: Sender<Response>,

    /// Get commands via this channel
    receiver: Receiver<Command>,
}

impl Hardware {
    /// Try to create a serial communication handler
    ///
    /// This function takes a `channel` to rescieve commands
    /// and returns a resceiver to send replies to.
    ///
    /// All actions are done on a dedicated thread.
    ///
    pub fn new(
        port: &str,
        baud: u32,
        receiver: Receiver<Command>,
    ) -> Option<(Hardware, Receiver<Response>)> {
        let mut settings: SerialPortSettings = Default::default();
        settings.timeout = Duration::from_millis(10);
        settings.baud_rate = baud.into();
        let (sender, replier) = channel();

        return Some((Hardware {
            rxtx: serialport::open_with_settings(&port, &settings).ok()?,
            port: String::from(port),
            settings,
            sender,
            receiver,
        }, replier));
    }

    /// Try to read a byte - nonblocking
    ///
    /// This function returns `None` if there's no byte to be had
    fn read_byte(&mut self) -> Option<u8> {
        let mut buf = [0; 1];
        if self.rxtx.bytes_to_read().ok()? > 0 && self.rxtx.read_exact(&mut buf).is_ok() {
            println!("Read a byte: `{:08b}`", buf[0]);
            Some(buf[0])
        } else {
            None
        }
    }

    fn send(&mut self, cmd: Vec<u8>) -> Option<()> {
        self.rxtx.write(cmd.as_slice()).ok().map(|_| Some(()))?
    }

    pub fn run(&mut self) {
        // Check for sends and internal commands
        loop {
            let cmd = self.receiver.recv_timeout(Duration::from_micros(50));
            match cmd {
                Ok(Command::__Internal) => break,
                Ok(c) => self.send(Command::encode(c)).expect("Failed to send Command!"),
                _ => {},
            };

            if let Some(resp) = Response::build(|| self.read_byte()) {
                self.sender.send(resp).expect("Failed to send Response!");
            }
        }
    }
}
