//! Hardware abstraction module

use mio::unix::UnixReady;
use mio::{Events, Poll, PollOpt, Ready, Token};
use mio_serial;

use std::io::{Read, Write};
use std::{env, str};

/// A delimiter token for serial comms
const SERIAL_TOKEN: Token = Token(0);
const SERIAL_BUF_SIZE: usize = 1024;

struct Hardware {
    port: String,
}

/// Utility function to check if port is ready & willing
fn readiness() -> Ready {
    Ready::readable() | UnixReady::hup() | UnixReady::error()
}

/// Utility function to check if port is closed
fn is_closed(state: Ready) -> bool {
    state.contains(UnixReady::hup() | UnixReady::error())
}

enum Command {
    TurnPage = 0x010001001,
}

impl Hardware {
    pub fn new(port: &str) {
        let poll = Poll::new().unwrap();
        let mut events = Events::with_capacity(SERIAL_BUF_SIZE);

        // Create the listener
        let settings = mio_serial::SerialPortSettings::default();

        println!("Opening {} at 9600,8N1", port);
        let mut rx = mio_serial::Serial::from_path(&port, &settings).unwrap();

        poll.register(&rx, SERIAL_TOKEN, self::readiness(), PollOpt::edge())
            .unwrap();

        let mut rx_buf = [0u8; 1024];
        'outer: loop {
            poll.poll(&mut events, None).unwrap();

            if events.is_empty() {
                println!("Read timed out!");
                continue;
            }

            for event in events.iter() {
                match event.token() {
                    SERIAL_TOKEN => {
                        let ready = event.readiness();
                        if self::is_closed(ready) {
                            println!("Quitting due to event: {:?}", ready);
                            break 'outer;
                        }
                        if ready.is_readable() {
                            match rx.read(&mut rx_buf) {
                                Ok(b) => match b {
                                    b if b > 0 => {
                                        println!("{:?}", String::from_utf8_lossy(&rx_buf[..b]))
                                    }
                                    _ => println!("Read would have blocked."),
                                },
                                Err(e) => println!("Error:  {}", e),
                            }
                        }
                    }
                    t => unreachable!("Unexpected token: {:?}", t),
                }
            }

            /* foo */
        }
    }
}

/// Utility function which parses a single command-code
fn parse_message() {}

/// Utility function which sends a single command-code
fn send_message() {}
