//! Ligature session wiring, including the production serial adapter.

use std::io::{self, BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serialport::SerialPort;
use thiserror::Error;

use super::{
    parse_ligature_line, ConnectionEpoch, LigatureCommand, LigatureEvent, LigatureLine,
    LigatureRequest, LigatureSession, LigatureSessionError, RequestPriority, LIGATURE_BAUD,
};

/// Transport failure that invalidates the current Ligature connection.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LigatureTransportError {
    /// The configured serial device could not be opened or cloned.
    #[error("cannot open Ligature device: {0}")]
    Device(String),
    /// An established serial device failed while reading or writing.
    #[error("Ligature serial I/O failed: {0}")]
    Io(String),
    /// No complete state query arrived within the configured timeout.
    #[error("Ligature did not return a parseable state before the deadline")]
    QueryTimeout,
    /// The query returned a frame that cannot establish current state.
    #[error("Ligature readiness query returned an invalid frame: {0}")]
    QueryInvalid(String),
    /// A serial worker or its channel stopped.
    #[error("Ligature serial worker closed")]
    Closed,
    /// The response violated the current operation lifecycle.
    #[error(transparent)]
    Session(#[from] LigatureSessionError),
}

/// Minimal wire boundary used by the controller and deterministic fakes.
pub trait LigatureWire: 'static {
    /// Tag subsequent responses with the connection epoch that opened this wire.
    fn set_epoch(&mut self, epoch: ConnectionEpoch);
    /// Write one already-correlated request through its selected priority path.
    fn send(&mut self, request: &LigatureRequest) -> Result<(), LigatureTransportError>;
    /// Request a fresh public-state frame after an asynchronous lifecycle change.
    fn query_current_state(&mut self) -> Result<(), LigatureTransportError>;
    /// Poll one epoch-tagged response without blocking.
    fn try_line(&mut self) -> Result<Option<(ConnectionEpoch, String)>, LigatureTransportError>;
    /// Stop all access to the physical connection before a replacement is opened.
    fn shutdown(self) -> Result<(), LigatureTransportError>
    where
        Self: Sized,
    {
        Ok(())
    }
}

/// Correlated Ligature lifecycle over an injected wire.
pub struct LigatureClient<W> {
    session: LigatureSession,
    wire: Option<W>,
}

impl<W: LigatureWire> LigatureClient<W> {
    /// Create a client from a wire whose open query already returned `query`.
    pub fn from_query(mut wire: W, query: &str) -> Result<Self, LigatureTransportError> {
        let mut session = LigatureSession::from_query(query)?;
        wire.set_epoch(ConnectionEpoch(1));
        stop_orphaned_firmware_work(&mut session, &mut wire)?;
        Ok(Self {
            session,
            wire: Some(wire),
        })
    }

    /// Inspect the authoritative lifecycle and latest public status.
    pub fn session(&self) -> &LigatureSession {
        &self.session
    }

    /// Correlate and write one command immediately. No request is queued in the client.
    pub fn begin(
        &mut self,
        command: LigatureCommand,
    ) -> Result<LigatureRequest, LigatureTransportError> {
        let request = self.session.begin(command)?;
        self.wire
            .as_mut()
            .ok_or(LigatureTransportError::Closed)?
            .send(&request)?;
        Ok(request)
    }

    /// Close the old wire, then open, query, and install a fresh connection epoch.
    pub fn reconnect_with(
        &mut self,
        open: impl FnOnce() -> Result<(W, String), LigatureTransportError>,
    ) -> Result<super::LigatureReconnect, LigatureTransportError> {
        if let Some(wire) = self.wire.take() {
            wire.shutdown()?;
        }
        let (mut wire, query) = open()?;
        let reconnect = self.session.reconnect(&query)?;
        wire.set_epoch(reconnect.epoch);
        stop_orphaned_firmware_work(&mut self.session, &mut wire)?;
        self.wire = Some(wire);
        Ok(reconnect)
    }

    /// Route at most one currently available wire line.
    pub fn try_event(&mut self) -> Result<Option<LigatureEvent>, LigatureTransportError> {
        let Some((epoch, line)) = self
            .wire
            .as_mut()
            .ok_or(LigatureTransportError::Closed)?
            .try_line()?
        else {
            return Ok(None);
        };
        let event = self.session.receive(epoch, &line)?;
        if matches!(
            event,
            LigatureEvent::Completed { .. }
                | LigatureEvent::Failed { .. }
                | LigatureEvent::Cancelled { .. }
                | LigatureEvent::HardFault { .. }
        ) {
            self.wire
                .as_mut()
                .ok_or(LigatureTransportError::Closed)?
                .query_current_state()?;
        }
        Ok(Some(event))
    }
}

fn stop_orphaned_firmware_work(
    session: &mut LigatureSession,
    wire: &mut impl LigatureWire,
) -> Result<(), LigatureTransportError> {
    if session.has_orphaned_active() {
        let stop = session.begin(LigatureCommand::Stop)?;
        wire.send(&stop)?;
    }
    Ok(())
}

enum WireMessage {
    Line(String),
    Failed(String),
}

/// Production serial wire with a priority-aware writer and independent reader.
pub struct SerialLigatureWire {
    ordinary: Sender<String>,
    urgent: Sender<String>,
    incoming: Receiver<WireMessage>,
    epoch: ConnectionEpoch,
    shutdown: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}

impl SerialLigatureWire {
    /// Open at Ligature's fixed baud rate and require a parseable `?` response.
    pub fn open(
        path: &str,
        query_timeout: Duration,
    ) -> Result<(Self, String), LigatureTransportError> {
        let port = serialport::new(path, LIGATURE_BAUD)
            .timeout(Duration::from_millis(50))
            .open()
            .map_err(|error| LigatureTransportError::Device(error.to_string()))?;
        let read_port = port
            .try_clone()
            .map_err(|error| LigatureTransportError::Device(error.to_string()))?;

        let (ordinary_sender, ordinary_receiver) = mpsc::channel();
        let (urgent_sender, urgent_receiver) = mpsc::channel();
        let (incoming_sender, incoming_receiver) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let reader = spawn_reader(read_port, incoming_sender.clone(), Arc::clone(&shutdown))?;
        let writer = match spawn_writer(
            port,
            ordinary_receiver,
            urgent_receiver,
            incoming_sender,
            Arc::clone(&shutdown),
        ) {
            Ok(writer) => writer,
            Err(error) => {
                shutdown.store(true, Ordering::Release);
                let _ = reader.join();
                return Err(error);
            }
        };

        let wire = Self {
            ordinary: ordinary_sender,
            urgent: urgent_sender,
            incoming: incoming_receiver,
            epoch: ConnectionEpoch(1),
            shutdown,
            workers: vec![reader, writer],
        };
        wire.ordinary
            .send("?".into())
            .map_err(|_| LigatureTransportError::Closed)?;

        let deadline = Instant::now() + query_timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(LigatureTransportError::QueryTimeout);
            }
            match wire.incoming.recv_timeout(remaining) {
                Ok(WireMessage::Line(line)) => match parse_ligature_line(&line) {
                    Ok(LigatureLine::State(_)) => return Ok((wire, line)),
                    Ok(LigatureLine::Status(_)) => continue,
                    Ok(_) => return Err(LigatureTransportError::QueryInvalid(line)),
                    Err(error) => {
                        return Err(LigatureTransportError::QueryInvalid(error.to_string()))
                    }
                },
                Ok(WireMessage::Failed(error)) => {
                    return Err(LigatureTransportError::Io(error));
                }
                Err(_) => return Err(LigatureTransportError::QueryTimeout),
            }
        }
    }
}

impl LigatureWire for SerialLigatureWire {
    fn set_epoch(&mut self, epoch: ConnectionEpoch) {
        self.epoch = epoch;
    }

    fn send(&mut self, request: &LigatureRequest) -> Result<(), LigatureTransportError> {
        let sender = match request.priority {
            RequestPriority::Ordinary => &self.ordinary,
            RequestPriority::Urgent => &self.urgent,
        };
        sender
            .send(request.line.clone())
            .map_err(|_| LigatureTransportError::Closed)
    }

    fn query_current_state(&mut self) -> Result<(), LigatureTransportError> {
        self.ordinary
            .send("?".into())
            .map_err(|_| LigatureTransportError::Closed)
    }

    fn try_line(&mut self) -> Result<Option<(ConnectionEpoch, String)>, LigatureTransportError> {
        match self.incoming.try_recv() {
            Ok(WireMessage::Line(line)) => Ok(Some((self.epoch, line))),
            Ok(WireMessage::Failed(error)) => Err(LigatureTransportError::Io(error)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(LigatureTransportError::Closed),
        }
    }

    fn shutdown(mut self) -> Result<(), LigatureTransportError> {
        self.stop_workers();
        Ok(())
    }
}

impl SerialLigatureWire {
    fn stop_workers(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

impl Drop for SerialLigatureWire {
    fn drop(&mut self) {
        self.stop_workers();
    }
}

fn spawn_reader(
    mut port: Box<dyn SerialPort>,
    incoming: Sender<WireMessage>,
    shutdown: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, LigatureTransportError> {
    thread::Builder::new()
        .name("ligature-reader".into())
        .spawn(move || {
            let mut reader = BufReader::new(&mut port);
            let mut line = String::new();
            while !shutdown.load(Ordering::Acquire) {
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        let _ = incoming.send(WireMessage::Failed("device disconnected".into()));
                        return;
                    }
                    Ok(_) => {
                        let complete = line.trim_end_matches(['\r', '\n']).to_owned();
                        line.clear();
                        if !complete.is_empty()
                            && incoming.send(WireMessage::Line(complete)).is_err()
                        {
                            return;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::TimedOut => continue,
                    Err(error) => {
                        let _ = incoming.send(WireMessage::Failed(error.to_string()));
                        return;
                    }
                }
            }
        })
        .map_err(|error| LigatureTransportError::Device(error.to_string()))
}

fn spawn_writer(
    mut port: Box<dyn SerialPort>,
    ordinary: Receiver<String>,
    urgent: Receiver<String>,
    incoming: Sender<WireMessage>,
    shutdown: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, LigatureTransportError> {
    thread::Builder::new()
        .name("ligature-writer".into())
        .spawn(move || {
            while !shutdown.load(Ordering::Acquire) {
                let line = match urgent.try_recv() {
                    Ok(line) => line,
                    Err(TryRecvError::Disconnected) | Err(TryRecvError::Empty) => {
                        match ordinary.recv_timeout(Duration::from_millis(5)) {
                            Ok(line) => line,
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(mpsc::RecvTimeoutError::Disconnected) => return,
                        }
                    }
                };
                if let Err(error) = write_line(&mut port, &line) {
                    let _ = incoming.send(WireMessage::Failed(error.to_string()));
                    return;
                }
            }
        })
        .map_err(|error| LigatureTransportError::Device(error.to_string()))
}

fn write_line(port: &mut Box<dyn SerialPort>, line: &str) -> io::Result<()> {
    port.write_all(line.as_bytes())?;
    port.write_all(b"\n")?;
    port.flush()
}
