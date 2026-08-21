//! Hardware abstraction module — typed client for the `monospace` text
//! protocol (see `monospace.md` §4-§6 for the wire format this implements).

mod protocol;

use protocol::{classify_line, LineKind};
use serialport::SerialPort;
use std::io::{self, BufRead, BufReader, Write};
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::Duration;

pub use protocol::LineKind as HwLine;

#[derive(Debug)]
pub enum HwError {
    Io(io::Error),
    Device(String),
    NoReply,
    UnexpectedReply(String),
}

impl From<io::Error> for HwError {
    fn from(e: io::Error) -> Self {
        HwError::Io(e)
    }
}

const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// Typed client for a `monospace`-protocol board.
///
/// Opening the port resets the Arduino (its DTR auto-reset circuit), so
/// only one `HwClient` should exist per physical connection at a time.
/// Reply lines are matched to whichever command triggered them; the two
/// kinds of unsolicited line are routed to callbacks instead so they never
/// get mistaken for a command reply: `PRESS <mbar>` telemetry (while
/// streaming is active, `monospace.md` §6) goes to `on_telemetry`, and
/// `EVENT ...` lines (e.g. `EVENT BUTTON PRESSED`, §10) go to `on_event`.
pub struct HwClient {
    write_half: Box<dyn SerialPort>,
    responses: Receiver<String>,
}

impl HwClient {
    /// Open a connection, waiting out the post-reset boot delay before
    /// returning. `boot_delay` should be measured empirically on real
    /// hardware (see `monospace.md` §9.1) rather than assumed.
    pub fn open(
        path: &str,
        baud: u32,
        boot_delay: Duration,
        mut on_telemetry: impl FnMut(f32) + Send + 'static,
        mut on_event: impl FnMut(&str) + Send + 'static,
    ) -> Result<Self, HwError> {
        let port = serialport::new(path, baud)
            .timeout(Duration::from_millis(100))
            .open()
            .map_err(|e| HwError::Device(e.to_string()))?;

        thread::sleep(boot_delay);

        let read_half = port
            .try_clone()
            .map_err(|e| HwError::Device(e.to_string()))?;
        let write_half = port;

        let (sender, responses) = channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            loop {
                match reader.read_line(&mut line) {
                    Ok(0) => break, // EOF: port closed
                    Ok(_) => {
                        let trimmed = line.trim_end_matches(['\r', '\n']).to_string();
                        line.clear();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match classify_line(&trimmed) {
                            LineKind::Telemetry(mbar) => on_telemetry(mbar),
                            LineKind::Event(text) => on_event(&text),
                            _ => {
                                if sender.send(trimmed).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    // A per-read timeout can fire mid-line (e.g. the
                    // firmware sends "OK " immediately but the trailing
                    // value only after a slower averaged sensor read
                    // completes) — do NOT clear `line` here, or the
                    // already-buffered partial content is lost and the
                    // rest of the line gets misread as its own line on
                    // the next iteration.
                    Err(ref e) if e.kind() == io::ErrorKind::TimedOut => continue,
                    Err(_) => break,
                }
            }
        });

        Ok(HwClient {
            write_half,
            responses,
        })
    }

    /// Send a raw protocol line, return the board's raw reply line —
    /// escape hatch for exercising unknown/malformed commands directly.
    pub fn send_raw(&mut self, line: &str) -> Result<String, HwError> {
        self.write_half.write_all(line.as_bytes())?;
        self.write_half.write_all(b"\n")?;
        self.write_half.flush()?;
        self.responses
            .recv_timeout(REPLY_TIMEOUT)
            .map_err(|_| HwError::NoReply)
    }

    fn expect_ok(&mut self, cmd: &str) -> Result<(), HwError> {
        match self.send_raw(cmd)?.as_str() {
            "OK" => Ok(()),
            other => Err(HwError::UnexpectedReply(other.to_string())),
        }
    }

    pub fn set_vacuum(&mut self, on: bool) -> Result<(), HwError> {
        self.expect_ok(if on { "VACUUM ON" } else { "VACUUM OFF" })
    }

    pub fn set_fan(&mut self, on: bool) -> Result<(), HwError> {
        self.expect_ok(if on { "FAN ON" } else { "FAN OFF" })
    }

    pub fn set_blower(&mut self, on: bool) -> Result<(), HwError> {
        self.expect_ok(if on { "BLOWER ON" } else { "BLOWER OFF" })
    }

    pub fn set_light(&mut self, on: bool) -> Result<(), HwError> {
        self.expect_ok(if on { "LIGHT ON" } else { "LIGHT OFF" })
    }

    /// Atomically de-energizes vacuum, fan, and blower (light untouched).
    pub fn all_off(&mut self) -> Result<(), HwError> {
        self.expect_ok("ALL OFF")
    }

    /// Single-shot averaged pressure read (mbar) — accuracy-favoring.
    pub fn press_once(&mut self) -> Result<f32, HwError> {
        let reply = self.send_raw("PRESS?")?;
        match classify_line(&reply) {
            LineKind::OkPress(mbar) => Ok(mbar),
            _ => Err(HwError::UnexpectedReply(reply)),
        }
    }

    /// Begin continuous pressure streaming; readings arrive via the
    /// `on_telemetry` callback passed to `open()`.
    pub fn start_press_stream(&mut self) -> Result<(), HwError> {
        self.expect_ok("PRESS START")
    }

    pub fn stop_press_stream(&mut self) -> Result<(), HwError> {
        self.expect_ok("PRESS STOP")
    }

    /// Set the RGB status LED to raw 0..255 per-channel values (`LED SET`,
    /// `monospace.md` §5/§10.3). The firmware handles the common-anode
    /// inversion; these are plain host-facing values (0 = off, 255 = on).
    /// Blinking is not a firmware mode — a caller wanting it sends repeated
    /// `set_led` calls at whatever cadence it likes (§10.3).
    pub fn set_led(&mut self, r: u8, g: u8, b: u8) -> Result<(), HwError> {
        self.expect_ok(&format!("LED SET {r} {g} {b}"))
    }
}
