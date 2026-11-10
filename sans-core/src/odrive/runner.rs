//! The link runner: a thin thread shell around the engine.
//!
//! One engine thread owns the [`LinkEngine`]. Each connection gets its own
//! reader thread and writer thread, so a blocked read cannot hold up a write,
//! and the writer always sends urgent packets before anything still queued.
//! Device open runs on a short-lived thread so the engine thread never blocks
//! on device I/O. Events leave on a channel as typed values; no callback runs
//! on an I/O thread.

use std::collections::VecDeque;
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use super::engine::{
    LinkCommand, LinkConfig, LinkEngine, LinkEvent, LinkPhase, Priority, TelemetrySample, Ticket,
    Transmit,
};
use super::transport::{FibreConnection, FibreOpener, OpenRequest, PacketReader, PacketWriter};

/// How long the reader waits per call before checking whether to stop.
const READ_SLICE: Duration = Duration::from_millis(20);
/// Stale packets discarded per open, each waited for at most `DRAIN_SLICE`.
const DRAIN_LIMIT: usize = 64;
const DRAIN_SLICE: Duration = Duration::from_millis(5);
/// Longest idle wait of the engine thread when nothing is scheduled.
const IDLE_WAKE: Duration = Duration::from_millis(100);

enum Input {
    Submit(Ticket, LinkCommand, Priority),
    Reconnect,
    Shutdown,
    Opened(u64, Result<FibreConnection, String>),
    Packet(u64, Vec<u8>),
    TransportFailed(u64, String),
}

#[derive(Default)]
struct Shared {
    epoch: u64,
    ended: bool,
    latest: Option<TelemetrySample>,
}

/// A cloneable handle whose calls never block on device I/O.
#[derive(Clone)]
pub struct LinkHandle {
    inputs: Sender<Input>,
    tickets: Arc<AtomicU64>,
    shared: Arc<Mutex<Shared>>,
}

impl LinkHandle {
    /// Queue a typed command and return its ticket at once.
    pub fn submit(&self, command: LinkCommand, priority: Priority) -> Ticket {
        let ticket = Ticket(self.tickets.fetch_add(1, Ordering::Relaxed));
        let _ = self.inputs.send(Input::Submit(ticket, command, priority));
        ticket
    }

    /// End the current epoch and start one new connection attempt.
    pub fn reconnect(&self) {
        let _ = self.inputs.send(Input::Reconnect);
    }

    /// The latest current-epoch sample, or `None` if it is older than the
    /// profile allows.
    pub fn fresh_sample(&self, now: Instant) -> Option<TelemetrySample> {
        let shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        if shared.ended {
            return None;
        }
        shared
            .latest
            .as_ref()
            .filter(|sample| sample.epoch() == shared.epoch && sample.fresh_values(now).is_some())
            .cloned()
    }
}

/// Owns the link threads; dropping it stops them.
pub struct LinkRunner {
    inputs: Sender<Input>,
}

impl Drop for LinkRunner {
    fn drop(&mut self) {
        let _ = self.inputs.send(Input::Shutdown);
    }
}

/// Start the engine thread. It makes one connection attempt at once and no
/// further attempt unless [`LinkHandle::reconnect`] is called.
pub fn spawn_link<E>(
    config: LinkConfig,
    opener: Box<dyn FibreOpener>,
    events: Sender<E>,
) -> io::Result<(LinkHandle, LinkRunner)>
where
    E: From<LinkEvent> + Send + 'static,
{
    let (inputs, inbox) = mpsc::channel();
    let shared = Arc::new(Mutex::new(Shared::default()));
    let handle = LinkHandle {
        inputs: inputs.clone(),
        tickets: Arc::new(AtomicU64::new(1)),
        shared: Arc::clone(&shared),
    };
    let loopback = inputs.clone();
    thread::Builder::new()
        .name("sans-odrive-link".into())
        .spawn(move || {
            EngineThread {
                engine: LinkEngine::new(config.clone(), sequence_seed()),
                config,
                opener: Arc::new(Mutex::new(opener)),
                loopback,
                events,
                shared,
                active: None,
                releasing: None,
            }
            .run(inbox);
        })?;
    Ok((handle, LinkRunner { inputs }))
}

/// A random starting point for the process-wide sequence counter.
fn sequence_seed() -> u16 {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos()),
    );
    hasher.finish() as u16
}

struct ActiveTransport {
    epoch: u64,
    stop: Arc<AtomicBool>,
    writer: Arc<WriterQueue>,
    /// Disconnects once the reader and writer threads have both exited and
    /// dropped their halves of the device.
    released: Receiver<()>,
}

impl ActiveTransport {
    /// Ask both I/O threads to stop and return the signal of their exit.
    fn close(self) -> Receiver<()> {
        self.stop.store(true, Ordering::SeqCst);
        self.writer.close();
        self.released
    }
}

/// Wait, on the open thread, until the previous connection's I/O threads have
/// released the device, so a reopen never races the old claim.
fn await_release(released: Option<Receiver<()>>, bound: Duration) -> Result<(), String> {
    let Some(released) = released else {
        return Ok(());
    };
    match released.recv_timeout(bound) {
        Err(RecvTimeoutError::Disconnected) | Ok(()) => Ok(()),
        Err(RecvTimeoutError::Timeout) => Err(format!(
            "the previous ODrive connection did not release the device within {} ms",
            bound.as_millis()
        )),
    }
}

struct EngineThread<E> {
    engine: LinkEngine,
    config: LinkConfig,
    opener: Arc<Mutex<Box<dyn FibreOpener>>>,
    loopback: Sender<Input>,
    events: Sender<E>,
    shared: Arc<Mutex<Shared>>,
    active: Option<ActiveTransport>,
    /// Exit signal of the most recently closed connection's I/O threads.
    releasing: Option<Receiver<()>>,
}

impl<E: From<LinkEvent> + Send + 'static> EngineThread<E> {
    fn run(mut self, inbox: Receiver<Input>) {
        self.start_attempt();
        loop {
            let now = Instant::now();
            let wait = self
                .engine
                .next_wakeup()
                .map_or(IDLE_WAKE, |wake| wake.saturating_duration_since(now))
                .min(IDLE_WAKE);
            let input = match inbox.recv_timeout(wait) {
                Ok(input) => Some(input),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let now = Instant::now();
            match input {
                None => {}
                Some(Input::Shutdown) => break,
                Some(Input::Submit(ticket, command, priority)) => {
                    self.engine.submit(now, ticket, command, priority);
                }
                Some(Input::Reconnect) => self.start_attempt(),
                Some(Input::Opened(epoch, result)) => self.opened(now, epoch, result),
                Some(Input::Packet(epoch, packet)) if self.is_current(epoch) => {
                    self.engine.receive(now, &packet);
                }
                Some(Input::TransportFailed(epoch, reason)) if self.is_current(epoch) => {
                    self.close_transport();
                    self.engine.transport_lost(now, reason);
                }
                // Packets from an ended epoch are counted and dropped.
                Some(Input::Packet(..)) => self.engine.stale_packet_dropped(),
                Some(Input::TransportFailed(..)) => {}
            }
            self.engine.poll(Instant::now());
            if !self.flush() {
                break;
            }
        }
        self.close_transport();
    }

    fn is_current(&self, epoch: u64) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.epoch == epoch)
    }

    fn start_attempt(&mut self) {
        self.close_transport();
        let epoch = self.engine.begin_attempt(Instant::now());
        let opener = Arc::clone(&self.opener);
        let loopback = self.loopback.clone();
        let request = OpenRequest {
            usb_serial: self.config.usb_serial().to_owned(),
        };
        let released = self.releasing.take();
        let release_bound = self.config.gate_deadline();
        let spawned = thread::Builder::new()
            .name("sans-odrive-open".into())
            .spawn(move || {
                let result = await_release(released, release_bound).and_then(|()| {
                    opener
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .open(&request)
                        .map(|mut connection| {
                            drain(connection.reader.as_mut());
                            connection
                        })
                });
                let _ = loopback.send(Input::Opened(epoch, result));
            });
        if let Err(error) = spawned {
            self.engine.open_failed(
                Instant::now(),
                format!("cannot start the ODrive open thread: {error}"),
            );
        }
        let _ = self.flush();
    }

    fn opened(&mut self, now: Instant, epoch: u64, result: Result<FibreConnection, String>) {
        let current = self.engine.status();
        let still_opening = current.epoch == epoch
            && current.phase == LinkPhase::Connecting
            && current.discovery.is_none();
        if !still_opening {
            return; // A late open after the gate deadline or a newer attempt is dropped.
        }
        let connection = match result {
            Ok(connection) => connection,
            Err(reason) => {
                self.engine.open_failed(now, reason);
                return;
            }
        };
        let FibreConnection {
            usb_serial,
            reader,
            writer,
        } = connection;
        let stop = Arc::new(AtomicBool::new(false));
        let queue = Arc::new(WriterQueue::default());
        let (release_guard, released) = mpsc::channel::<()>();
        let started = spawn_reader(
            epoch,
            reader,
            Arc::clone(&stop),
            self.loopback.clone(),
            release_guard.clone(),
        )
        .and_then(|()| {
            spawn_writer(
                epoch,
                writer,
                Arc::clone(&queue),
                self.loopback.clone(),
                self.config.request_deadline(),
                self.config.urgent_deadline(),
                release_guard,
            )
        });
        self.active = Some(ActiveTransport {
            epoch,
            stop,
            writer: queue,
            released,
        });
        match started {
            Ok(()) => self.engine.transport_opened(now, &usb_serial),
            Err(error) => {
                self.close_transport();
                self.engine
                    .open_failed(now, format!("cannot start ODrive I/O threads: {error}"));
            }
        }
    }

    fn close_transport(&mut self) {
        if let Some(active) = self.active.take() {
            self.releasing = Some(active.close());
        }
    }

    /// Hand transmits to the writer and events to the consumer. Returns false
    /// when the consumer is gone.
    fn flush(&mut self) -> bool {
        // Withdrawals can produce further transmits and events, so repeat.
        loop {
            if !self.flush_once() {
                return false;
            }
            if !self.engine.has_output() {
                return true;
            }
        }
    }

    fn flush_once(&mut self) -> bool {
        while let Some(transmit) = self.engine.take_transmit() {
            let Some(active) = self
                .active
                .as_ref()
                .filter(|active| active.epoch == transmit.epoch)
            else {
                continue;
            };
            let epoch = active.epoch;
            let withdrawn = active.writer.push(transmit);
            for sequence in withdrawn {
                self.engine
                    .transmit_withdrawn(Instant::now(), epoch, sequence);
            }
        }
        while let Some(event) = self.engine.take_event() {
            {
                let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
                match &event {
                    LinkEvent::Status(status) => {
                        if status.epoch != shared.epoch {
                            shared.latest = None;
                        }
                        shared.epoch = status.epoch;
                        shared.ended = status.phase == LinkPhase::Disconnected;
                        if shared.ended {
                            shared.latest = None;
                        }
                    }
                    LinkEvent::Telemetry(sample) => shared.latest = Some(sample.clone()),
                    LinkEvent::CommandResult { .. } => {}
                }
            }
            if self.events.send(E::from(event)).is_err() {
                return false;
            }
        }
        true
    }
}

fn drain(reader: &mut dyn PacketReader) {
    for _ in 0..DRAIN_LIMIT {
        match reader.read_packet(DRAIN_SLICE) {
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return,
        }
    }
}

fn spawn_reader(
    epoch: u64,
    mut reader: Box<dyn PacketReader>,
    stop: Arc<AtomicBool>,
    loopback: Sender<Input>,
    release_guard: Sender<()>,
) -> io::Result<()> {
    thread::Builder::new()
        .name("sans-odrive-read".into())
        .spawn(move || {
            // Dropped with `reader` when this thread exits.
            let _release_guard = release_guard;
            while !stop.load(Ordering::SeqCst) {
                match reader.read_packet(READ_SLICE) {
                    Ok(Some(packet)) => {
                        if loopback.send(Input::Packet(epoch, packet)).is_err() {
                            return;
                        }
                    }
                    Ok(None) => {}
                    Err(reason) => {
                        if !stop.load(Ordering::SeqCst) {
                            let _ = loopback.send(Input::TransportFailed(epoch, reason));
                        }
                        return;
                    }
                }
            }
        })
        .map(|_| ())
}

#[derive(Default)]
struct WriterState {
    urgent: VecDeque<Vec<u8>>,
    ordinary: VecDeque<Transmit>,
    closed: bool,
}

/// Urgent-first packet queue for one connection's writer thread.
#[derive(Default)]
struct WriterQueue {
    state: Mutex<WriterState>,
    ready: Condvar,
}

impl WriterQueue {
    /// Queue a packet. An urgent packet discards every ordinary command
    /// packet still waiting here, so a queued move cannot follow a stop; the
    /// returned sequence numbers must be reported to the engine as withdrawn.
    fn push(&self, transmit: Transmit) -> Vec<u16> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut withdrawn = Vec::new();
        if transmit.urgent {
            state.ordinary.retain(|waiting| {
                if waiting.retractable {
                    withdrawn.push(waiting.sequence);
                }
                !waiting.retractable
            });
            state.urgent.push_back(transmit.packet);
        } else {
            state.ordinary.push_back(transmit);
        }
        self.ready.notify_one();
        withdrawn
    }

    fn close(&self) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed = true;
        self.ready.notify_all();
    }

    /// The next packet, urgent first, or `None` once closed.
    fn pop(&self) -> Option<(bool, Vec<u8>)> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if state.closed {
                return None;
            }
            if let Some(packet) = state.urgent.pop_front() {
                return Some((true, packet));
            }
            if let Some(transmit) = state.ordinary.pop_front() {
                return Some((false, transmit.packet));
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

fn spawn_writer(
    epoch: u64,
    mut writer: Box<dyn PacketWriter>,
    queue: Arc<WriterQueue>,
    loopback: Sender<Input>,
    request_deadline: Duration,
    urgent_deadline: Duration,
    release_guard: Sender<()>,
) -> io::Result<()> {
    thread::Builder::new()
        .name("sans-odrive-write".into())
        .spawn(move || {
            // Dropped with `writer` when this thread exits.
            let _release_guard = release_guard;
            while let Some((urgent, packet)) = queue.pop() {
                let timeout = if urgent {
                    urgent_deadline
                } else {
                    request_deadline
                };
                if let Err(reason) = writer.write_packet(&packet, timeout) {
                    let _ = loopback.send(Input::TransportFailed(epoch, reason));
                    return;
                }
            }
        })
        .map(|_| ())
}
