//! A fake legacy-Fibre ODrive built from the #57 capture.
//!
//! It serves the reconstructed descriptor, keeps a raw value per endpoint ID,
//! drops requests whose trailer is wrong as the pinned dispatcher does, and
//! records every write in order. It carries its own packet decoding so a bug in
//! the production codec cannot cancel itself out.
#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::Duration;

use sans_core::{FibreConnection, FibreOpener, OpenRequest, PacketReader, PacketWriter};

pub const DESCRIPTOR: &[u8] =
    include_bytes!("../fixtures/odrive/descriptor-RECONSTRUCTED-2026-11-10.json");
const ENDPOINTS: &str = include_str!("../../test-results/odrive-mini-endpoints-2026-11-10.json");
/// The trailer the board reported in the #57 capture.
pub const CAPTURED_CRC: u16 = 39744;

// Captured axis-0 endpoint IDs, for assertions only.
pub const SERIAL_NUMBER: u16 = 4;
pub const VBUS_VOLTAGE: u16 = 1;
pub const AXIS_ERROR: u16 = 71;
pub const CURRENT_STATE: u16 = 73;
pub const REQUESTED_STATE: u16 = 74;
pub const MOTOR_ERROR: u16 = 132;
pub const MOTOR_IS_CALIBRATED: u16 = 134;
pub const IQ_SETPOINT: u16 = 149;
pub const IQ_MEASURED: u16 = 150;
pub const CURRENT_LIM: u16 = 180;
pub const CONTROLLER_ERROR: u16 = 193;
pub const INPUT_POS: u16 = 194;
pub const CONTROL_MODE: u16 = 208;
pub const INPUT_MODE: u16 = 209;
pub const CONTROLLER_VEL_LIMIT: u16 = 213;
pub const ENCODER_ERROR: u16 = 236;
pub const ENCODER_IS_READY: u16 = 237;
pub const POS_ESTIMATE: u16 = 243;
pub const VEL_ESTIMATE: u16 = 249;
pub const TRAP_VEL_LIMIT: u16 = 281;
pub const TRAP_ACCEL_LIMIT: u16 = 282;
pub const TRAP_DECEL_LIMIT: u16 = 283;
pub const MIN_ENDSTOP_STATE: u16 = 284;
pub const CLEAR_ERRORS: u16 = 299;

/// One request as the fake board decoded it.
#[derive(Clone, Debug, PartialEq)]
pub struct Received {
    pub sequence: u16,
    pub endpoint: u16,
    pub expected: u16,
    pub payload: Vec<u8>,
}

/// What to do with the reply to the next request on one endpoint.
#[derive(Clone, Copy, Debug)]
pub enum ReplyFault {
    /// Send nothing.
    Drop,
    /// Keep the reply; the test delivers it later with `take_held`.
    Hold,
    /// Remove the last payload byte.
    Truncate,
    /// Append one byte.
    Pad,
}

pub struct FakeFibre {
    descriptor: Vec<u8>,
    trailer: u16,
    sizes: HashMap<u16, usize>,
    functions: Vec<u16>,
    values: HashMap<u16, Vec<u8>>,
    /// Every accepted request in arrival order.
    pub requests: Vec<Received>,
    /// Every value write and function call in arrival order: (endpoint, payload).
    pub writes: Vec<(u16, Vec<u8>)>,
    /// Requests dropped because their trailer was wrong.
    pub dropped_bad_trailer: usize,
    faults: HashMap<u16, VecDeque<ReplyFault>>,
    held: Vec<Vec<u8>>,
    /// Every reply sent, for replay tests.
    pub sent_replies: Vec<Vec<u8>>,
    /// Requested-state writes the fake state machine ignores.
    pub ignore_state_requests: Vec<i32>,
    /// Values the fake returns on readback instead of what was written.
    pub readback_override: HashMap<u16, Vec<u8>>,
    /// Requested states that are visible for exactly one `current_state`
    /// read and then settle to the mapped state, like a homing run that ends.
    pub settle_after_one_read: HashMap<i32, i32>,
    settling: Option<i32>,
}

impl FakeFibre {
    /// A compatible board matching the #57 capture: idle, calibrated,
    /// encoder ready, in position control with trapezoidal input, no errors.
    pub fn compatible() -> Self {
        let flat: serde_json::Value = serde_json::from_str(ENDPOINTS).unwrap();
        let mut sizes = HashMap::new();
        let mut functions = Vec::new();
        for entry in flat.as_object().unwrap().values() {
            let id = entry["id"].as_u64().unwrap() as u16;
            let size = match entry["type"].as_str().unwrap() {
                "bool" | "uint8" => 1,
                "uint16" => 2,
                "int32" | "uint32" | "float" => 4,
                "uint64" => 8,
                "function" => {
                    functions.push(id);
                    0
                }
                _ => 0,
            };
            sizes.insert(id, size);
        }
        let mut fake = Self {
            descriptor: DESCRIPTOR.to_vec(),
            trailer: CAPTURED_CRC,
            sizes,
            functions,
            values: HashMap::new(),
            requests: Vec::new(),
            writes: Vec::new(),
            dropped_bad_trailer: 0,
            faults: HashMap::new(),
            held: Vec::new(),
            sent_replies: Vec::new(),
            ignore_state_requests: Vec::new(),
            readback_override: HashMap::new(),
            settle_after_one_read: HashMap::new(),
            settling: None,
        };
        fake.set_u64(SERIAL_NUMBER, 54_310_774_846_261);
        fake.set_raw(5, vec![3]);
        fake.set_raw(6, vec![6]);
        fake.set_raw(7, vec![56]);
        fake.set_raw(8, vec![0]);
        fake.set_raw(9, vec![0]);
        fake.set_raw(10, vec![0]);
        fake.set_raw(11, vec![1]);
        fake.set_f32(VBUS_VOLTAGE, 24.0);
        fake.set_i32(CURRENT_STATE, 1);
        fake.set_bool(MOTOR_IS_CALIBRATED, true);
        fake.set_bool(ENCODER_IS_READY, true);
        fake.set_i32(CONTROL_MODE, 3);
        fake.set_i32(INPUT_MODE, 5);
        fake.set_f32(CURRENT_LIM, 4.8);
        fake
    }

    /// Serve a different descriptor. Its CRC is computed here, independently.
    pub fn with_descriptor(mut self, descriptor: Vec<u8>) -> Self {
        self.trailer = independent_crc(&descriptor);
        self.descriptor = descriptor;
        self
    }

    pub fn set_raw(&mut self, endpoint: u16, bytes: Vec<u8>) {
        self.values.insert(endpoint, bytes);
    }
    pub fn set_i32(&mut self, endpoint: u16, value: i32) {
        self.set_raw(endpoint, value.to_le_bytes().to_vec());
    }
    pub fn set_u64(&mut self, endpoint: u16, value: u64) {
        self.set_raw(endpoint, value.to_le_bytes().to_vec());
    }
    pub fn set_f32(&mut self, endpoint: u16, value: f32) {
        self.set_raw(endpoint, value.to_le_bytes().to_vec());
    }
    pub fn set_bool(&mut self, endpoint: u16, value: bool) {
        self.set_raw(endpoint, vec![u8::from(value)]);
    }
    pub fn f32_value(&self, endpoint: u16) -> f32 {
        f32::from_le_bytes(self.raw(endpoint).try_into().unwrap())
    }

    fn raw(&self, endpoint: u16) -> Vec<u8> {
        self.values
            .get(&endpoint)
            .cloned()
            .unwrap_or_else(|| vec![0; *self.sizes.get(&endpoint).unwrap_or(&0)])
    }

    /// Apply a fault to the reply of the next request on `endpoint`.
    pub fn fault_next(&mut self, endpoint: u16, fault: ReplyFault) {
        self.faults.entry(endpoint).or_default().push_back(fault);
    }

    /// Replies withheld by [`ReplyFault::Hold`].
    pub fn take_held(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.held)
    }

    /// Value writes recorded on one endpoint.
    pub fn writes_to(&self, endpoint: u16) -> Vec<Vec<u8>> {
        self.writes
            .iter()
            .filter(|(id, _)| *id == endpoint)
            .map(|(_, payload)| payload.clone())
            .collect()
    }

    /// Every `requested_state` value written, decoded.
    pub fn requested_states(&self) -> Vec<i32> {
        self.writes_to(REQUESTED_STATE)
            .into_iter()
            .map(|payload| i32::from_le_bytes(payload.try_into().unwrap()))
            .collect()
    }

    /// Handle one request packet and return the reply, if any.
    pub fn handle(&mut self, packet: &[u8]) -> Option<Vec<u8>> {
        if packet.len() < 8 {
            return None;
        }
        let le = |at: usize| u16::from(packet[at]) | (u16::from(packet[at + 1]) << 8);
        let sequence = le(0);
        let raw_endpoint = le(2);
        let endpoint = raw_endpoint & 0x7fff;
        let wants_reply = raw_endpoint & 0x8000 != 0;
        let expected = le(4);
        let trailer = le(packet.len() - 2);
        let expected_trailer = if endpoint == 0 { 1 } else { self.trailer };
        if trailer != expected_trailer {
            self.dropped_bad_trailer += 1;
            return None;
        }
        let payload = packet[6..packet.len() - 2].to_vec();
        self.requests.push(Received {
            sequence,
            endpoint,
            expected,
            payload: payload.clone(),
        });
        // The pinned dispatcher caps the reply at its 32-byte TX buffer.
        let limit = usize::from(expected).min(30);
        let output = if endpoint == 0 {
            let offset = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
            let end = (offset + limit).min(self.descriptor.len());
            self.descriptor.get(offset..end).unwrap_or(&[]).to_vec()
        } else if self.functions.contains(&endpoint) {
            self.writes.push((endpoint, payload));
            Vec::new()
        } else {
            // Exchange semantics: a write returns the old value.
            let current = if payload.is_empty() {
                self.readback_override
                    .get(&endpoint)
                    .cloned()
                    .unwrap_or_else(|| self.raw(endpoint))
            } else {
                let old = self.raw(endpoint);
                self.writes.push((endpoint, payload.clone()));
                self.apply_write(endpoint, payload);
                old
            };
            if endpoint == CURRENT_STATE && payload_was_empty(packet) {
                if let Some(settled) = self.settling.take() {
                    self.set_i32(CURRENT_STATE, settled);
                }
            }
            current[..limit.min(current.len())].to_vec()
        };
        if !wants_reply {
            return None;
        }
        let mut reply = (sequence | 0x8000).to_le_bytes().to_vec();
        reply.extend_from_slice(&output);
        match self.faults.get_mut(&endpoint).and_then(VecDeque::pop_front) {
            Some(ReplyFault::Drop) => return None,
            Some(ReplyFault::Hold) => {
                self.held.push(reply);
                return None;
            }
            Some(ReplyFault::Truncate) => {
                reply.pop();
            }
            Some(ReplyFault::Pad) => reply.push(0),
            None => {}
        }
        self.sent_replies.push(reply.clone());
        Some(reply)
    }

    fn apply_write(&mut self, endpoint: u16, payload: Vec<u8>) {
        if endpoint == REQUESTED_STATE {
            let state = i32::from_le_bytes(payload.try_into().unwrap());
            if !self.ignore_state_requests.contains(&state) {
                self.set_i32(CURRENT_STATE, state);
                self.settling = self.settle_after_one_read.get(&state).copied();
            }
            return;
        }
        self.values.insert(endpoint, payload);
    }
}

fn payload_was_empty(packet: &[u8]) -> bool {
    packet.len() == 8
}

/// CRC16, polynomial 0x3d65, initial value 1, MSB first; written out here so the
/// fake does not share the production implementation.
pub fn independent_crc(bytes: &[u8]) -> u16 {
    let mut crc: u32 = 1;
    for &byte in bytes {
        crc ^= u32::from(byte) << 8;
        for _ in 0..8 {
            crc <<= 1;
            if crc & 0x1_0000 != 0 {
                crc ^= 0x3d65;
            }
            crc &= 0xffff;
        }
    }
    crc as u16
}

/// Decode a request the engine transmitted, independently of the codec.
pub fn decode_request(packet: &[u8]) -> Received {
    let le = |at: usize| u16::from(packet[at]) | (u16::from(packet[at + 1]) << 8);
    Received {
        sequence: le(0),
        endpoint: le(2) & 0x7fff,
        expected: le(4),
        payload: packet[6..packet.len() - 2].to_vec(),
    }
}

/// A shared fake board behind the public transport seam.
#[derive(Clone)]
pub struct FakeOpener {
    pub board: Arc<Mutex<FakeFibre>>,
    /// USB serial strings of matching devices; empty means no device.
    pub usb_serials: Arc<Mutex<Vec<String>>>,
    /// Set to cut the current connection.
    pub cut: Arc<Mutex<bool>>,
    pub opens: Arc<Mutex<usize>>,
    /// While `true`, the writer holds the packet it was given and does not
    /// deliver it; `writes_held` counts packets held so far.
    pub write_gate: Arc<(Mutex<bool>, Condvar)>,
    pub writes_held: Arc<AtomicUsize>,
    /// Live halves of the claimed device; like a USB interface claim, a
    /// second open fails until both halves of the previous one are dropped.
    pub claimed: Arc<AtomicUsize>,
    /// Opens refused because the previous claim was still held.
    pub busy_refusals: Arc<AtomicUsize>,
    /// How long each half's destructor takes to give up the claim, like an
    /// endpoint whose release is slow.
    pub claim_release_delay: Arc<Mutex<Duration>>,
    /// When `true`, the next open claims the device and then waits here until
    /// [`FakeOpener::release_open`], like an open that is still draining.
    pub hold_open: Arc<(Mutex<bool>, Condvar)>,
    /// Like `nusb`, the reader always has an IN transfer pending. Cancelling it
    /// takes this long to be reaped. A reader dropped without a successful
    /// `shutdown` keeps the claim in the background for this long.
    pub reap_delay: Arc<Mutex<Duration>>,
}

impl FakeOpener {
    pub fn block_writes(&self) {
        *self.write_gate.0.lock().unwrap() = true;
    }

    pub fn release_writes(&self) {
        *self.write_gate.0.lock().unwrap() = false;
        self.write_gate.1.notify_all();
    }

    pub fn release_open(&self) {
        *self.hold_open.0.lock().unwrap() = false;
        self.hold_open.1.notify_all();
    }
}

/// Keeps the fake claim while either half is alive.
struct Claim {
    claimed: Arc<AtomicUsize>,
    release_delay: Duration,
    /// Time for a pending transfer to be reaped after cancellation; zero when
    /// nothing is pending or the owner's `shutdown` already reaped it.
    unreaped: Duration,
}

impl Drop for Claim {
    fn drop(&mut self) {
        if self.unreaped.is_zero() {
            std::thread::sleep(self.release_delay);
            self.claimed.fetch_sub(1, Ordering::SeqCst);
            return;
        }
        // Dropped with a transfer still pending: the claim outlives the half.
        let (claimed, delay) = (Arc::clone(&self.claimed), self.unreaped);
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            claimed.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

impl FakeOpener {
    pub fn new(board: FakeFibre) -> Self {
        Self {
            board: Arc::new(Mutex::new(board)),
            usb_serials: Arc::new(Mutex::new(vec!["316536713335".into()])),
            cut: Arc::new(Mutex::new(false)),
            opens: Arc::new(Mutex::new(0)),
            write_gate: Arc::new((Mutex::new(false), Condvar::new())),
            writes_held: Arc::new(AtomicUsize::new(0)),
            claimed: Arc::new(AtomicUsize::new(0)),
            busy_refusals: Arc::new(AtomicUsize::new(0)),
            claim_release_delay: Arc::new(Mutex::new(Duration::ZERO)),
            hold_open: Arc::new((Mutex::new(false), Condvar::new())),
            reap_delay: Arc::new(Mutex::new(Duration::ZERO)),
        }
    }

    pub fn absent() -> Self {
        let opener = Self::new(FakeFibre::compatible());
        opener.usb_serials.lock().unwrap().clear();
        opener
    }
}

impl FibreOpener for FakeOpener {
    fn open(&mut self, request: &OpenRequest) -> Result<FibreConnection, String> {
        let serials = self.usb_serials.lock().unwrap().clone();
        let matching: Vec<_> = serials
            .iter()
            .filter(|serial| serial.eq_ignore_ascii_case(&request.usb_serial))
            .collect();
        match matching.len() {
            0 => {
                return Err(format!(
                    "no ODrive with USB serial {} is connected",
                    request.usb_serial
                ))
            }
            1 => {}
            n => {
                return Err(format!(
                    "{n} ODrives with USB serial {} are connected",
                    request.usb_serial
                ))
            }
        }
        if self.claimed.load(Ordering::SeqCst) != 0 {
            self.busy_refusals.fetch_add(1, Ordering::SeqCst);
            return Err("fake USB interface 2 is still claimed by the previous connection".into());
        }
        self.claimed.store(2, Ordering::SeqCst);
        let release_delay = *self.claim_release_delay.lock().unwrap();
        let reap_delay = *self.reap_delay.lock().unwrap();
        let claim = |unreaped| Claim {
            claimed: Arc::clone(&self.claimed),
            release_delay,
            unreaped,
        };
        {
            let (held, wake) = &*self.hold_open;
            let mut held = held.lock().unwrap();
            while *held {
                held = wake.wait(held).unwrap();
            }
        }
        *self.cut.lock().unwrap() = false;
        *self.opens.lock().unwrap() += 1;
        let (replies, inbox) = mpsc::channel();
        Ok(FibreConnection {
            usb_serial: matching[0].clone(),
            reader: Box::new(FakeReader {
                inbox,
                _keep_open: replies.clone(),
                cut: Arc::clone(&self.cut),
                shutdown_started: None,
                _claim: claim(reap_delay),
            }),
            writer: Box::new(FakeWriter {
                board: Arc::clone(&self.board),
                replies,
                cut: Arc::clone(&self.cut),
                gate: Arc::clone(&self.write_gate),
                held: Arc::clone(&self.writes_held),
                _claim: claim(Duration::ZERO),
            }),
        })
    }
}

struct FakeReader {
    inbox: mpsc::Receiver<Vec<u8>>,
    /// Like a pending USB IN transfer, a read waits out its whole timeout even
    /// after the writer is gone, holding the claim until it returns.
    _keep_open: mpsc::Sender<Vec<u8>>,
    cut: Arc<Mutex<bool>>,
    shutdown_started: Option<std::time::Instant>,
    _claim: Claim,
}

impl PacketReader for FakeReader {
    fn read_packet(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, String> {
        if *self.cut.lock().unwrap() {
            return Err("fake cable cut".into());
        }
        match self.inbox.recv_timeout(timeout) {
            Ok(packet) => Ok(Some(packet)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err("fake writer gone".into()),
        }
    }

    fn shutdown(&mut self, bound: Duration) -> Result<(), String> {
        let started = *self
            .shutdown_started
            .get_or_insert_with(std::time::Instant::now);
        let remaining = self._claim.unreaped.saturating_sub(started.elapsed());
        if remaining <= bound {
            std::thread::sleep(remaining);
            self._claim.unreaped = Duration::ZERO;
            Ok(())
        } else {
            std::thread::sleep(bound);
            Err("fake IN transfer not reaped yet".into())
        }
    }
}

struct FakeWriter {
    board: Arc<Mutex<FakeFibre>>,
    replies: mpsc::Sender<Vec<u8>>,
    cut: Arc<Mutex<bool>>,
    gate: Arc<(Mutex<bool>, Condvar)>,
    held: Arc<AtomicUsize>,
    _claim: Claim,
}

impl PacketWriter for FakeWriter {
    fn write_packet(&mut self, packet: &[u8], _timeout: Duration) -> Result<(), String> {
        {
            let (blocked, wake) = &*self.gate;
            let mut blocked = blocked.lock().unwrap();
            if *blocked {
                self.held.fetch_add(1, Ordering::SeqCst);
                while *blocked {
                    blocked = wake.wait(blocked).unwrap();
                }
            }
        }
        if *self.cut.lock().unwrap() {
            return Err("fake cable cut".into());
        }
        if let Some(reply) = self.board.lock().unwrap().handle(packet) {
            let _ = self.replies.send(reply);
        }
        Ok(())
    }
}
