//! Monospace board connection and typed command client.

mod protocol;

use protocol::{classify_line, validate_command_line, EventKind, InvalidCommandLine, LineKind};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;

/// The fixed baud rate implemented by the Monospace text protocol.
pub const MONOSPACE_BAUD_RATE: u32 = 115_200;

const ALL_OFF_COMMAND: &str = "ALL OFF";
static NEXT_CONNECTION_EPOCH: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Eq, PartialEq)]
#[repr(u8)]
enum ConnectionStatus {
    Active,
    Poisoned,
    Retired,
}

struct ConnectionState(AtomicU8);

impl ConnectionState {
    fn active() -> Self {
        Self(AtomicU8::new(ConnectionStatus::Active as u8))
    }

    fn status(&self) -> ConnectionStatus {
        match self.0.load(Ordering::Acquire) {
            value if value == ConnectionStatus::Active as u8 => ConnectionStatus::Active,
            value if value == ConnectionStatus::Poisoned as u8 => ConnectionStatus::Poisoned,
            value if value == ConnectionStatus::Retired as u8 => ConnectionStatus::Retired,
            _ => unreachable!("invalid Monospace connection state"),
        }
    }

    fn is_active(&self) -> bool {
        self.status() == ConnectionStatus::Active
    }

    fn poison(&self) -> bool {
        self.0
            .compare_exchange(
                ConnectionStatus::Active as u8,
                ConnectionStatus::Poisoned as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn mark_poisoned(&self) {
        self.0
            .store(ConnectionStatus::Poisoned as u8, Ordering::Release);
    }

    fn retire(&self) {
        let _ = self.0.compare_exchange(
            ConnectionStatus::Active as u8,
            ConnectionStatus::Retired as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn poison_and_get_previous(&self) -> ConnectionStatus {
        loop {
            let previous = self.status();
            if previous != ConnectionStatus::Active {
                return previous;
            }
            if self
                .0
                .compare_exchange(
                    ConnectionStatus::Active as u8,
                    ConnectionStatus::Poisoned as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return ConnectionStatus::Active;
            }
        }
    }
}

/// Identity assigned to one Monospace serial connection.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ConnectionEpoch(u64);

impl ConnectionEpoch {
    /// Numeric epoch value for diagnostics and logs.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Typed event emitted by the Monospace reader thread.
#[derive(Clone, Debug, PartialEq)]
pub struct MonospaceEvent {
    /// Connection that produced the event.
    pub epoch: ConnectionEpoch,
    /// Classified event payload.
    pub kind: MonospaceEventKind,
}

/// Monospace events forwarded to the Machine controller.
#[derive(Clone, Debug, PartialEq)]
pub enum MonospaceEventKind {
    /// Latest streamed pressure sample in millibar. Samples may be coalesced.
    Pressure(f32),
    /// The debounced physical button was pressed.
    ButtonPressed,
    /// A future firmware event that is not a button intent.
    UnknownEvent(String),
    /// The serial connection ended.
    Disconnected,
    /// A protocol or correlation failure retired the connection.
    Fault(MonospaceFault),
}

/// Fault details suitable for controller diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MonospaceFault {
    /// The reader received a line outside the public protocol grammar.
    MalformedFrame(String),
    /// An ordinary command did not receive a response before its deadline.
    ReplyTimeout,
    /// A response was valid protocol data but wrong for the pending command.
    UnexpectedReply(String),
    /// A separate urgent write made the untagged reply order unknowable.
    AmbiguousUrgentWrite,
    /// Reading from or writing to the serial device failed.
    SerialIo(String),
}

/// A ready command client and its typed event stream.
pub struct MonospaceConnection {
    /// Command client that passed the production readiness gate.
    pub client: MonospaceClient,
    /// Typed events produced by this connection's reader thread.
    pub events: MonospaceEvents,
}

/// Reliable Monospace events plus one coalesced pressure sample.
pub struct MonospaceEvents {
    reliable: Receiver<MonospaceEvent>,
    latest_pressure: Arc<Mutex<Option<MonospaceEvent>>>,
}

impl MonospaceEvents {
    /// Receive an event without blocking the controller thread.
    pub fn try_recv(&self) -> Result<MonospaceEvent, TryRecvError> {
        match self.reliable.try_recv() {
            Ok(event) => Ok(event),
            Err(TryRecvError::Empty) => self.take_pressure().ok_or(TryRecvError::Empty),
            Err(TryRecvError::Disconnected) => {
                self.take_pressure().ok_or(TryRecvError::Disconnected)
            }
        }
    }

    /// Wait up to `timeout` for a reliable event or coalesced pressure sample.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<MonospaceEvent, RecvTimeoutError> {
        if let Ok(event) = self.try_recv() {
            return Ok(event);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return self.take_pressure().ok_or(RecvTimeoutError::Timeout);
            }
            let wait = deadline
                .saturating_duration_since(now)
                .min(Duration::from_millis(10));
            match self.reliable.recv_timeout(wait) {
                Ok(event) => return Ok(event),
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(event) = self.take_pressure() {
                        return Ok(event);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return self.take_pressure().ok_or(RecvTimeoutError::Disconnected);
                }
            }
        }
    }

    fn take_pressure(&self) -> Option<MonospaceEvent> {
        self.latest_pressure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

impl Iterator for MonospaceEvents {
    type Item = MonospaceEvent;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => return Some(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum HwError {
    /// Host-side serial I/O failed.
    #[error(transparent)]
    Io(io::Error),
    /// The serial device or its reader thread could not be opened.
    #[error("Monospace device error: {0}")]
    Device(String),
    /// Outbound data was not one uppercase command line.
    #[error("invalid Monospace command: {0}")]
    InvalidCommand(String),
    /// Firmware rejected a well-correlated command.
    #[error("Monospace firmware error: {0}")]
    Firmware(String),
    /// No response arrived before the configured deadline.
    #[error("Monospace did not reply before the deadline")]
    ReplyTimeout,
    /// An inbound line did not match the public protocol grammar.
    #[error("malformed Monospace frame: {0}")]
    MalformedFrame(String),
    /// A valid response did not match the pending typed command.
    #[error("unexpected Monospace reply: {0}")]
    UnexpectedReply(String),
    /// The reader observed EOF or lost its response channel.
    #[error("Monospace disconnected")]
    Disconnected,
    /// The caller tried to reuse a retired response FIFO.
    #[error("Monospace connection epoch {0} is poisoned")]
    Poisoned(u64),
    /// A separate urgent write made the untagged reply order unknowable.
    #[error("urgent Monospace writing made response correlation ambiguous")]
    AmbiguousUrgentWrite,
}

impl From<io::Error> for HwError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

enum ReaderReply {
    Line(String),
    Malformed(String),
    Unexpected(String),
    Disconnected,
}

/// Command-capable client for one ready Monospace connection.
pub struct MonospaceClient {
    write_half: Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    responses: Receiver<ReaderReply>,
    event_sender: Sender<MonospaceEvent>,
    state: Arc<ConnectionState>,
    pending_response: Arc<AtomicBool>,
    reply_timeout: Duration,
    epoch: ConnectionEpoch,
}

/// Emergency writer for dispatching `ALL OFF` without waiting behind a command.
///
/// Monospace replies are untagged. Using this separate writer therefore retires
/// the connection immediately so its acknowledgement cannot satisfy later work.
#[derive(Clone)]
pub struct MonospaceUrgentWriter {
    write_half: Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    event_sender: Sender<MonospaceEvent>,
    state: Arc<ConnectionState>,
    pending_response: Arc<AtomicBool>,
    epoch: ConnectionEpoch,
}

/// Compatibility name for the original typed Monospace command client.
pub type HwClient = MonospaceClient;

/// Compatibility name for the original urgent Monospace writer.
pub type HwUrgentWriter = MonospaceUrgentWriter;

impl MonospaceClient {
    /// Open Monospace at its fixed baud rate and complete the production gate.
    ///
    /// No command-capable client is returned until the reader thread is active
    /// and exact `OK` replies have arrived for `ALL OFF`, then `PRESS STOP`.
    pub fn connect(
        path: &str,
        boot_delay: Duration,
        reply_timeout: Duration,
    ) -> Result<MonospaceConnection, HwError> {
        let port = serialport::new(path, MONOSPACE_BAUD_RATE)
            .timeout(Duration::from_millis(100))
            .open()
            .map_err(|error| HwError::Device(error.to_string()))?;
        thread::sleep(boot_delay);
        port.clear(serialport::ClearBuffer::Input)
            .map_err(|error| HwError::Device(error.to_string()))?;
        let read_half = port
            .try_clone()
            .map_err(|error| HwError::Device(error.to_string()))?;

        Self::connect_streams(read_half, port, reply_timeout)
    }

    fn connect_streams(
        read_half: impl Read + Send + 'static,
        write_half: impl Write + Send + 'static,
        reply_timeout: Duration,
    ) -> Result<MonospaceConnection, HwError> {
        let epoch = ConnectionEpoch(NEXT_CONNECTION_EPOCH.fetch_add(1, Ordering::Relaxed));
        let state = Arc::new(ConnectionState::active());
        let pending_response = Arc::new(AtomicBool::new(false));
        let (response_sender, responses) = mpsc::channel();
        let (event_sender, reliable_events) = mpsc::channel();
        let latest_pressure = Arc::new(Mutex::new(None));
        let (reader_ready_sender, reader_ready_receiver) = mpsc::sync_channel(0);
        let reader_state = Arc::clone(&state);
        let reader_pending_response = Arc::clone(&pending_response);
        let reader_event_sender = event_sender.clone();
        let reader_pressure = Arc::clone(&latest_pressure);

        thread::Builder::new()
            .name(format!("monospace-reader-{}", epoch.get()))
            .spawn(move || {
                let _ = reader_ready_sender.send(());
                read_lines(
                    read_half,
                    epoch,
                    &reader_state,
                    &reader_pending_response,
                    &response_sender,
                    &reader_event_sender,
                    &reader_pressure,
                );
            })
            .map_err(|error| HwError::Device(error.to_string()))?;
        reader_ready_receiver
            .recv()
            .map_err(|_| HwError::Device("reader thread failed during startup".into()))?;

        let mut client = Self {
            write_half: Arc::new(Mutex::new(Some(Box::new(write_half)))),
            responses,
            event_sender,
            state,
            pending_response,
            reply_timeout,
            epoch,
        };
        client.all_off()?;
        client.stop_press_stream()?;

        Ok(MonospaceConnection {
            client,
            events: MonospaceEvents {
                reliable: reliable_events,
                latest_pressure,
            },
        })
    }

    /// Epoch assigned to this connection.
    pub fn epoch(&self) -> ConnectionEpoch {
        self.epoch
    }

    /// Whether the connection can still accept ordinary commands.
    pub fn is_usable(&self) -> bool {
        self.state.is_active()
    }

    /// Create an independent emergency `ALL OFF` writer.
    pub fn urgent_writer(&self) -> MonospaceUrgentWriter {
        MonospaceUrgentWriter {
            write_half: Arc::clone(&self.write_half),
            event_sender: self.event_sender.clone(),
            state: Arc::clone(&self.state),
            pending_response: Arc::clone(&self.pending_response),
            epoch: self.epoch,
        }
    }

    /// Send one uppercase diagnostic command and return its raw reply.
    pub fn send_raw(&mut self, line: &str) -> Result<String, HwError> {
        validate_command_line(line).map_err(|reason| {
            let reason = match reason {
                InvalidCommandLine::ControlByte => "embedded CR, LF, or NUL",
                InvalidCommandLine::Lowercase => "lowercase wire data",
            };
            HwError::InvalidCommand(reason.into())
        })?;
        self.require_usable()?;
        if self
            .pending_response
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            self.poison(MonospaceFault::AmbiguousUrgentWrite);
            return Err(HwError::AmbiguousUrgentWrite);
        }

        self.write_and_receive(line)
    }

    fn write_and_receive(&mut self, line: &str) -> Result<String, HwError> {
        if let Err(error) = self.write_command(line) {
            self.poison(MonospaceFault::SerialIo(error.to_string()));
            return Err(error);
        }
        match self.responses.recv_timeout(self.reply_timeout) {
            Ok(ReaderReply::Line(reply)) => Ok(reply),
            Ok(ReaderReply::Malformed(frame)) => {
                self.close_writer();
                Err(HwError::MalformedFrame(frame))
            }
            Ok(ReaderReply::Unexpected(reply)) => {
                self.close_writer();
                Err(HwError::UnexpectedReply(reply))
            }
            Ok(ReaderReply::Disconnected) => {
                self.state.mark_poisoned();
                self.close_writer();
                Err(HwError::Disconnected)
            }
            Err(RecvTimeoutError::Timeout) => {
                self.poison(MonospaceFault::ReplyTimeout);
                Err(HwError::ReplyTimeout)
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.state.mark_poisoned();
                self.close_writer();
                Err(HwError::Disconnected)
            }
        }
    }

    fn write_command(&self, line: &str) -> Result<(), HwError> {
        let mut writer = self
            .write_half
            .lock()
            .map_err(|_| HwError::Device("Monospace writer lock is poisoned".into()))?;
        let writer = writer.as_mut().ok_or(HwError::Disconnected)?;
        write_line(writer.as_mut(), line).map_err(HwError::Io)
    }

    fn require_usable(&self) -> Result<(), HwError> {
        if self.is_usable() {
            Ok(())
        } else {
            Err(self.retired_error())
        }
    }

    fn retired_error(&self) -> HwError {
        match self.responses.try_recv() {
            Ok(ReaderReply::Malformed(frame)) => HwError::MalformedFrame(frame),
            Ok(ReaderReply::Unexpected(reply)) => HwError::UnexpectedReply(reply),
            Ok(ReaderReply::Disconnected) => HwError::Disconnected,
            Ok(ReaderReply::Line(reply)) => HwError::UnexpectedReply(reply),
            Err(_) => HwError::Poisoned(self.epoch.get()),
        }
    }

    fn close_writer(&self) {
        let mut writer = self
            .write_half
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        writer.take();
    }

    fn poison(&self, fault: MonospaceFault) {
        if self.state.poison() {
            let _ = self.event_sender.send(MonospaceEvent {
                epoch: self.epoch,
                kind: MonospaceEventKind::Fault(fault),
            });
        }
        self.close_writer();
    }

    fn expect_ok(&mut self, command: &str) -> Result<(), HwError> {
        match self.send_raw(command)?.as_str() {
            "OK" => Ok(()),
            reply if reply.starts_with("ERR ") => Err(HwError::Firmware(reply[4..].to_string())),
            reply => {
                self.poison(MonospaceFault::UnexpectedReply(reply.to_string()));
                Err(HwError::UnexpectedReply(reply.to_string()))
            }
        }
    }

    /// Turn the vacuum pump on or off.
    pub fn set_vacuum(&mut self, on: bool) -> Result<(), HwError> {
        self.expect_ok(if on { "VACUUM ON" } else { "VACUUM OFF" })
    }

    /// Turn the flutter fan on or off.
    pub fn set_fan(&mut self, on: bool) -> Result<(), HwError> {
        self.expect_ok(if on { "FAN ON" } else { "FAN OFF" })
    }

    /// Turn the turn blower on or off.
    pub fn set_blower(&mut self, on: bool) -> Result<(), HwError> {
        self.expect_ok(if on { "BLOWER ON" } else { "BLOWER OFF" })
    }

    /// Turn the capture light on or off.
    pub fn set_light(&mut self, on: bool) -> Result<(), HwError> {
        self.expect_ok(if on { "LIGHT ON" } else { "LIGHT OFF" })
    }

    /// Turn off vacuum, flutter fan, and turn blower. Light is unchanged.
    pub fn all_off(&mut self) -> Result<(), HwError> {
        self.expect_ok(ALL_OFF_COMMAND)
    }

    /// Read one averaged pressure sample in millibar.
    pub fn press_once(&mut self) -> Result<f32, HwError> {
        let reply = self.send_raw("PRESS?")?;
        match classify_line(&reply) {
            LineKind::OkPress(mbar) => Ok(mbar),
            LineKind::Err(reason) => Err(HwError::Firmware(reason)),
            _ => {
                self.poison(MonospaceFault::UnexpectedReply(reply.clone()));
                Err(HwError::UnexpectedReply(reply))
            }
        }
    }

    /// Start streamed pressure telemetry.
    pub fn start_press_stream(&mut self) -> Result<(), HwError> {
        self.expect_ok("PRESS START")
    }

    /// Stop streamed pressure telemetry.
    pub fn stop_press_stream(&mut self) -> Result<(), HwError> {
        self.expect_ok("PRESS STOP")
    }

    /// Set the status LED using host-facing RGB channel values.
    pub fn set_led(&mut self, red: u8, green: u8, blue: u8) -> Result<(), HwError> {
        self.expect_ok(&format!("LED SET {red} {green} {blue}"))
    }
}

impl MonospaceUrgentWriter {
    /// Dispatch `ALL OFF` and poison the untagged response FIFO.
    pub fn all_off(&self) -> Result<(), HwError> {
        if !self.state.is_active() {
            return Err(HwError::Poisoned(self.epoch.get()));
        }
        self.pending_response.store(true, Ordering::Release);
        if self.state.poison() {
            let _ = self.event_sender.send(MonospaceEvent {
                epoch: self.epoch,
                kind: MonospaceEventKind::Fault(MonospaceFault::AmbiguousUrgentWrite),
            });
        }
        let write_result = {
            let mut writer = self
                .write_half
                .lock()
                .map_err(|_| HwError::Device("Monospace writer lock is poisoned".into()))?;
            let result = writer
                .as_mut()
                .ok_or(HwError::Disconnected)
                .and_then(|writer| {
                    write_line(writer.as_mut(), ALL_OFF_COMMAND).map_err(HwError::Io)
                });
            writer.take();
            result
        };
        write_result?;
        Err(HwError::AmbiguousUrgentWrite)
    }
}

impl Drop for MonospaceClient {
    fn drop(&mut self) {
        self.state.retire();
        self.close_writer();
    }
}

fn write_line(writer: &mut dyn Write, command: &str) -> io::Result<()> {
    writer.write_all(command.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn read_lines(
    read_half: impl Read,
    epoch: ConnectionEpoch,
    state: &ConnectionState,
    pending_response: &AtomicBool,
    responses: &mpsc::Sender<ReaderReply>,
    events: &Sender<MonospaceEvent>,
    latest_pressure: &Mutex<Option<MonospaceEvent>>,
) {
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        if !state.is_active() {
            return;
        }
        match reader.read_line(&mut line) {
            Ok(0) => {
                report_disconnect(epoch, state, responses, events);
                return;
            }
            Ok(_) => {
                let frame = line.trim_end_matches(['\r', '\n']).to_string();
                line.clear();
                if frame.is_empty() {
                    continue;
                }
                if !state.is_active() {
                    eprintln!(
                        "ignored stale Monospace frame for retired connection epoch {}: {}",
                        epoch.get(),
                        frame
                    );
                    return;
                }
                match classify_line(&frame) {
                    LineKind::Telemetry(mbar) => {
                        *latest_pressure
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            Some(MonospaceEvent {
                                epoch,
                                kind: MonospaceEventKind::Pressure(mbar),
                            });
                    }
                    LineKind::Event(EventKind::ButtonPressed) => {
                        if events
                            .send(MonospaceEvent {
                                epoch,
                                kind: MonospaceEventKind::ButtonPressed,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    LineKind::Event(EventKind::Unknown(payload)) => {
                        if events
                            .send(MonospaceEvent {
                                epoch,
                                kind: MonospaceEventKind::UnknownEvent(payload),
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    LineKind::Malformed(_) => {
                        if state.poison() {
                            let _ = events.send(MonospaceEvent {
                                epoch,
                                kind: MonospaceEventKind::Fault(MonospaceFault::MalformedFrame(
                                    frame.clone(),
                                )),
                            });
                            let _ = responses.send(ReaderReply::Malformed(frame));
                        } else {
                            eprintln!(
                                "ignored stale malformed Monospace frame for retired connection epoch {}: {}",
                                epoch.get(),
                                frame
                            );
                        }
                        return;
                    }
                    _ => {
                        if pending_response
                            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok()
                        {
                            if responses.send(ReaderReply::Line(frame)).is_err() {
                                return;
                            }
                        } else {
                            if state.poison() {
                                let _ = events.send(MonospaceEvent {
                                    epoch,
                                    kind: MonospaceEventKind::Fault(
                                        MonospaceFault::UnexpectedReply(frame.clone()),
                                    ),
                                });
                                let _ = responses.send(ReaderReply::Unexpected(frame));
                            }
                            return;
                        }
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(error) => {
                if !state.is_active() {
                    eprintln!(
                        "ignored stale Monospace read failure for retired connection epoch {}: {}",
                        epoch.get(),
                        error
                    );
                    return;
                }
                if state.poison() {
                    let _ = events.send(MonospaceEvent {
                        epoch,
                        kind: MonospaceEventKind::Fault(MonospaceFault::SerialIo(
                            error.to_string(),
                        )),
                    });
                    let _ = responses.send(ReaderReply::Disconnected);
                }
                return;
            }
        }
    }
}

fn report_disconnect(
    epoch: ConnectionEpoch,
    state: &ConnectionState,
    responses: &mpsc::Sender<ReaderReply>,
    events: &Sender<MonospaceEvent>,
) {
    if state.poison_and_get_previous() != ConnectionStatus::Retired {
        let _ = events.send(MonospaceEvent {
            epoch,
            kind: MonospaceEventKind::Disconnected,
        });
    }
    let _ = responses.send(ReaderReply::Disconnected);
}

#[cfg(test)]
mod tests;
