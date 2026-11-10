//! The ODrive link engine: a deterministic protocol state machine.
//!
//! It has no threads, no USB and no clock of its own. Callers feed it received
//! packets, typed commands, open or loss notifications and the current
//! monotonic time. It hands back packets to transmit, events, and the next time
//! it needs to be woken. It owns descriptor download, the capability check, the
//! connection gate, the poll schedule, request correlation, deadlines, epochs,
//! readiness and unit conversion.
//!
//! There is no public raw read or write: every write Sans can make is a named
//! typed command, and Sans never writes control mode, input mode or any stored
//! configuration other than the gate's limits, and never calls save, erase,
//! reboot or DFU functions.

use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use super::descriptor::{check_capabilities, value_type, Descriptor, Endpoint, ResolvedEndpoints};
use super::protocol::{
    decode_reply, encode_descriptor_read, encode_request, WireValue,
    AXIS_ERROR_MIN_ENDSTOP_PRESSED, AXIS_STATE_CLOSED_LOOP_CONTROL, AXIS_STATE_HOMING,
    AXIS_STATE_IDLE, CONTROL_MODE_POSITION_CONTROL, DESCRIPTOR_CHUNK_REQUEST_LEN,
    INPUT_MODE_TRAP_TRAJ, SEQUENCE_FORCED_BIT, SEQUENCE_MASK,
};
use crate::config::{odrive_profile_problems, LiftAxisProfile, LiftDirection, MotorLimits};
use crate::MachineProfile;

/// Largest descriptor the engine accepts before blocking the link.
const MAX_DESCRIPTOR_BYTES: usize = 256 * 1024;
/// Ordinary commands that may wait for the link; a fifth is rejected.
const ORDINARY_QUEUE_CAPACITY: usize = 4;
/// Pause between state-confirmation reads that did not match yet.
const CONFIRM_REREAD_INTERVAL: Duration = Duration::from_millis(1);

/// Identity and status reads made once per connection, in order.
const IDENTITY_READS: [Endpoint; 10] = [
    Endpoint::SerialNumber,
    Endpoint::HwVersionMajor,
    Endpoint::HwVersionMinor,
    Endpoint::HwVersionVariant,
    Endpoint::FwVersionMajor,
    Endpoint::FwVersionMinor,
    Endpoint::FwVersionRevision,
    Endpoint::FwVersionUnreleased,
    Endpoint::VbusVoltage,
    Endpoint::MotorArmedState,
];

/// One telemetry cycle reads every telemetry row of the capability table.
const TELEMETRY_READS: [Endpoint; 16] = [
    Endpoint::CurrentState,
    Endpoint::AxisError,
    Endpoint::MotorError,
    Endpoint::EncoderError,
    Endpoint::ControllerError,
    Endpoint::PosEstimate,
    Endpoint::VelEstimate,
    Endpoint::IqMeasured,
    Endpoint::IqSetpoint,
    Endpoint::TrajectoryDone,
    Endpoint::IsHomed,
    Endpoint::MinEndstopState,
    Endpoint::MotorIsCalibrated,
    Endpoint::EncoderIsReady,
    Endpoint::ControlMode,
    Endpoint::InputMode,
];

/// A caller-chosen command identity; results carry it back.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Ticket(pub u64);

/// Which path a command takes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Priority {
    /// Waits for the one ordinary request slot; goes before the next telemetry read.
    Ordinary,
    /// Transmitted at once, without waiting for an outstanding ordinary reply,
    /// and retires every ordinary command not yet transmitted.
    Urgent,
}

/// Typed lift-axis commands. Units are millimetres; the engine converts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LinkCommand {
    /// Write `requested_state` idle; confirmed when `current_state` reads idle,
    /// otherwise the link faults. Accepted whenever endpoints are resolved.
    Disarm,
    /// Write `requested_state` closed-loop control; confirmed when
    /// `current_state` reads closed-loop control.
    Arm,
    /// Write `requested_state` homing; confirmed when `current_state` reads
    /// homing. Home progress and evidence belong to #59.
    Home,
    /// Write `controller.input_pos`; confirmed when it reads back the written
    /// value. Motion completion belongs to #59.
    MoveAbsolute {
        /// Target in millimetres, inside the commissioned travel bounds.
        position_mm: f32,
    },
    /// Call `clear_errors`; confirmed by the next telemetry sample.
    ClearErrors,
}

/// The outcome of one command. `Completed` means the named readback matched;
/// it never means motion finished.
#[derive(Clone, Debug, PartialEq)]
pub enum CommandResult {
    /// The confirming read matched.
    Completed,
    /// Refused before anything was sent.
    Rejected {
        /// Why the command was not accepted.
        reason: String,
    },
    /// The command was sent but its confirming read showed something else.
    NotConfirmed {
        /// What the confirming read observed.
        observed: String,
    },
    /// A request timed out or got a malformed reply; it may or may not have executed.
    OutcomeUnknown {
        /// What went wrong.
        reason: String,
    },
    /// The command ended with its connection or was displaced. When
    /// `transmitted` is true its outcome is unknown.
    Retired {
        /// Whether any of its packets left the host.
        transmitted: bool,
    },
}

/// Coarse link phase for diagnostics and the touchscreen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkPhase {
    /// No connection attempt, or the transport ended.
    Disconnected,
    /// The connection gate is running.
    Connecting,
    /// The gate stopped short of verification; reasons are listed.
    Blocked,
    /// The gate completed and no link fault is latched. Necessary for motion,
    /// not sufficient: it says nothing about calibration, Home, Position trust or Stop.
    Verified,
    /// A link fault latched after verification; a reconnect is required.
    Faulted,
}

/// What the connection discovered about the board.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DiscoveryRecord {
    /// USB serial-number string the device reported.
    pub usb_serial: Option<String>,
    /// Raw descriptor length in bytes.
    pub descriptor_length: Option<usize>,
    /// Descriptor CRC16, the request trailer.
    pub descriptor_crc: Option<u16>,
    /// Flattened endpoint count, including endpoint 0.
    pub endpoint_count: Option<usize>,
    /// Discovered API fingerprint; copy into the profile to re-commission.
    pub api_fingerprint: Option<String>,
    /// The board's `serial_number` endpoint.
    pub board_serial: Option<u64>,
    /// Hardware major, minor, variant. Recorded, never compared.
    pub hardware_version: Option<[u8; 3]>,
    /// Firmware major, minor, revision, unreleased. Recorded, never compared.
    pub firmware_version: Option<[u8; 4]>,
    /// Bus voltage at connection.
    pub vbus_voltage: Option<f32>,
    /// `motor.armed_state`, raw and uninterpreted.
    pub armed_state: Option<i32>,
}

/// A snapshot of link state.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkStatus {
    /// Current connection epoch; 0 before the first attempt.
    pub epoch: u64,
    /// Coarse phase.
    pub phase: LinkPhase,
    /// Every reason the lift axis is blocked, faulted or disconnected.
    pub blockers: Vec<String>,
    /// What the current connection discovered.
    pub discovery: Option<DiscoveryRecord>,
    /// Replies that matched no pending request, over the process lifetime.
    pub unmatched_replies: u64,
}

/// The axis state as Sans interprets it. Numbers other than the three Sans
/// uses are unknown and count as neither idle nor armed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AxisState {
    /// `current_state` 1.
    Idle,
    /// `current_state` 8.
    ClosedLoopControl,
    /// `current_state` 11.
    Homing,
    /// Any other number.
    Unknown(i32),
}

impl AxisState {
    fn from_raw(raw: i32) -> Self {
        match raw {
            AXIS_STATE_IDLE => Self::Idle,
            AXIS_STATE_CLOSED_LOOP_CONTROL => Self::ClosedLoopControl,
            AXIS_STATE_HOMING => Self::Homing,
            other => Self::Unknown(other),
        }
    }

    /// True only for the idle state.
    pub fn is_idle(self) -> bool {
        self == Self::Idle
    }

    /// True for closed-loop control and homing; never for an unknown state.
    pub fn is_armed(self) -> bool {
        matches!(self, Self::ClosedLoopControl | Self::Homing)
    }
}

/// One complete telemetry cycle in Sans units.
#[derive(Clone, Debug, PartialEq)]
pub struct TelemetryValues {
    /// Interpreted `current_state`.
    pub axis_state: AxisState,
    /// Raw axis error bits.
    pub axis_error: u32,
    /// Raw motor error bits.
    pub motor_error: u32,
    /// Raw encoder error bits.
    pub encoder_error: u32,
    /// Raw controller error bits.
    pub controller_error: u32,
    /// Position in millimetres; `None` while uncommissioned.
    pub position_mm: Option<f32>,
    /// Velocity in millimetres per second; `None` while uncommissioned.
    pub velocity_mm_s: Option<f32>,
    /// Measured quadrature current in amps.
    pub iq_measured_a: f32,
    /// Quadrature current setpoint in amps.
    pub iq_setpoint_a: f32,
    /// `controller.trajectory_done`.
    pub trajectory_done: bool,
    /// `is_homed`.
    pub is_homed: bool,
    /// `min_endstop.endstop_state`.
    pub min_endstop_pressed: bool,
    /// `motor.is_calibrated`.
    pub motor_calibrated: bool,
    /// `encoder.is_ready`.
    pub encoder_ready: bool,
    /// Raw `controller.config.control_mode`.
    pub control_mode: i32,
    /// Raw `controller.config.input_mode`.
    pub input_mode: i32,
}

impl TelemetryValues {
    fn error_words_zero(&self) -> bool {
        self.axis_error == 0
            && self.motor_error == 0
            && self.encoder_error == 0
            && self.controller_error == 0
    }
}

/// A telemetry sample, readable only while fresh.
///
/// The reads are separate requests, so a sample is not an atomic snapshot. Its
/// timestamp is the send time of its cycle's first read, so its age is never
/// understated.
#[derive(Clone, Debug, PartialEq)]
pub struct TelemetrySample {
    epoch: u64,
    taken_at: Instant,
    max_age: Duration,
    cycle_duration: Duration,
    values: TelemetryValues,
}

impl TelemetrySample {
    /// The connection epoch that produced this sample.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Send time of the cycle's first read.
    pub fn taken_at(&self) -> Instant {
        self.taken_at
    }

    /// Measured time from first read sent to last reply received.
    pub fn cycle_duration(&self) -> Duration {
        self.cycle_duration
    }

    /// Age of the sample at `now`.
    pub fn age(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.taken_at)
    }

    /// The values regardless of age, for presentation beside an explicit
    /// freshness flag. Not public, so consumers cannot use stale values as evidence.
    pub(crate) fn display_values(&self) -> &TelemetryValues {
        &self.values
    }

    /// The values, or `None` once older than the profile's maximum age.
    pub fn fresh_values(&self, now: Instant) -> Option<&TelemetryValues> {
        (self.age(now) <= self.max_age).then_some(&self.values)
    }
}

/// Engine output: either a status change, a telemetry sample, or a command result.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkEvent {
    /// Phase, blockers, discovery or diagnostics changed.
    Status(LinkStatus),
    /// One complete telemetry cycle.
    Telemetry(TelemetrySample),
    /// A command finished.
    CommandResult {
        /// The caller's ticket.
        ticket: Ticket,
        /// The epoch the command belonged to.
        epoch: u64,
        /// The command.
        command: LinkCommand,
        /// What happened.
        result: CommandResult,
    },
}

/// One packet to hand to the transport writer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transmit {
    /// The epoch the packet belongs to.
    pub epoch: u64,
    /// Urgent packets go before anything still queued.
    pub urgent: bool,
    /// The whole request packet.
    pub packet: Vec<u8>,
}

/// Validated link configuration drawn from the Machine profile.
#[derive(Clone, Debug)]
pub struct LinkConfig {
    usb_serial_text: String,
    usb_serial: u64,
    axis: u8,
    api_fingerprint: String,
    poll_interval: Duration,
    max_age: Duration,
    request_deadline: Duration,
    urgent_deadline: Duration,
    gate_deadline: Duration,
    lift_axis: Option<LiftAxisProfile>,
}

impl LinkConfig {
    /// Build from a profile, revalidating the ODrive section.
    pub fn from_profile(profile: &MachineProfile) -> Result<Self, Vec<String>> {
        let odrive = &profile.odrive;
        let mut problems = odrive_profile_problems(odrive);
        if profile.timeouts.device_open_ms == 0 {
            problems.push("timeouts.device_open_ms must be positive".into());
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        let usb_serial = u64::from_str_radix(&odrive.usb_serial, 16)
            .map_err(|error| vec![format!("odrive.usb_serial is not hex: {error}")])?;
        Ok(Self {
            usb_serial_text: odrive.usb_serial.clone(),
            usb_serial,
            axis: odrive.axis,
            api_fingerprint: odrive.api_fingerprint_sha256.clone(),
            poll_interval: Duration::from_millis(odrive.poll_interval_ms),
            max_age: Duration::from_millis(odrive.telemetry_max_age_ms),
            request_deadline: Duration::from_millis(odrive.request_deadline_ms),
            urgent_deadline: Duration::from_millis(odrive.urgent_deadline_ms),
            gate_deadline: Duration::from_millis(profile.timeouts.device_open_ms),
            lift_axis: odrive.lift_axis.clone(),
        })
    }

    /// The profile's USB serial as hex text, for the opener.
    pub fn usb_serial(&self) -> &str {
        &self.usb_serial_text
    }

    /// Bound on the whole connection gate, including device open.
    pub fn gate_deadline(&self) -> Duration {
        self.gate_deadline
    }

    /// Oldest usable telemetry sample.
    pub fn max_telemetry_age(&self) -> Duration {
        self.max_age
    }

    /// Ordinary request deadline.
    pub fn request_deadline(&self) -> Duration {
        self.request_deadline
    }

    /// Urgent request deadline.
    pub fn urgent_deadline(&self) -> Duration {
        self.urgent_deadline
    }

    fn direction_sign(&self) -> Option<(f32, f32)> {
        self.lift_axis.as_ref().map(|lift_axis| {
            let sign = match lift_axis.direction {
                LiftDirection::Normal => 1.0,
                LiftDirection::Reversed => -1.0,
            };
            (sign, lift_axis.mm_per_turn)
        })
    }
}

/// Process-lifetime sequence allocation: 15 bits with bit 7 forced on.
struct Sequences {
    counter: u16,
    pending: HashSet<u16>,
    quarantined: HashSet<u16>,
}

impl Sequences {
    fn allocate(&mut self) -> Option<u16> {
        for _ in 0..=SEQUENCE_MASK {
            self.counter = self.counter.wrapping_add(1) & SEQUENCE_MASK;
            let sequence = self.counter | SEQUENCE_FORCED_BIT;
            if !self.pending.contains(&sequence) && !self.quarantined.contains(&sequence) {
                self.pending.insert(sequence);
                return Some(sequence);
            }
        }
        None
    }

    fn complete(&mut self, sequence: u16) {
        self.pending.remove(&sequence);
    }

    /// A transmitted request ended without its reply: never use its number again.
    fn quarantine(&mut self, sequence: u16) {
        self.pending.remove(&sequence);
        self.quarantined.insert(sequence);
    }
}

#[derive(Clone, Copy, Debug)]
enum RequestSpec {
    Chunk(u32),
    Read(Endpoint),
    Write(Endpoint, WireValue),
    Call(Endpoint),
}

impl RequestSpec {
    fn label(self) -> String {
        match self {
            Self::Chunk(offset) => format!("descriptor chunk at offset {offset}"),
            Self::Read(endpoint) => format!("read of {endpoint:?}"),
            Self::Write(endpoint, _) => format!("write of {endpoint:?}"),
            Self::Call(endpoint) => format!("call of {endpoint:?}"),
        }
    }
}

#[derive(Clone, Debug)]
struct InFlight {
    sequence: u16,
    spec: RequestSpec,
    deadline: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrdinaryOwner {
    Gate,
    Telemetry,
    Command,
}

#[derive(Debug)]
enum Reply {
    Value(WireValue),
    Ack,
    Chunk(Vec<u8>),
}

#[derive(Clone, Debug)]
enum GateStep {
    Opening,
    Descriptor { buffer: Vec<u8> },
    Identity { values: Vec<WireValue> },
    IdleWrite,
    IdleRead { not_before: Instant },
    Limit { index: usize, readback: bool },
    FirstCycle,
}

#[derive(Clone, Copy, Debug)]
enum Confirm {
    /// Read `current_state` until it equals the value or the window closes.
    State { expected: i32, fault_on_miss: bool },
    /// Read the written endpoint back once and compare 32-bit patterns.
    Readback(Endpoint, WireValue),
    /// Wait for the next complete telemetry sample.
    NextSample,
}

#[derive(Clone, Copy, Debug)]
enum OpStep {
    SendWrite,
    AwaitWrite,
    SendConfirm,
    AwaitConfirm,
}

#[derive(Clone, Debug)]
struct Operation {
    ticket: Ticket,
    command: LinkCommand,
    write: RequestSpec,
    confirm: Confirm,
    step: OpStep,
    transmitted: bool,
    window: Duration,
    confirm_until: Option<Instant>,
    not_before: Option<Instant>,
}

struct UrgentLane {
    op: Operation,
    in_flight: Option<InFlight>,
}

struct AwaitingSample {
    op: Operation,
    after: Instant,
    until: Instant,
}

struct Cycle {
    started_at: Instant,
    values: Vec<WireValue>,
}

struct Connection {
    epoch: u64,
    opened: bool,
    ended: bool,
    gate: Option<GateStep>,
    gate_until: Instant,
    blockers: Vec<String>,
    fault: Option<String>,
    trailer: Option<u16>,
    endpoints: Option<ResolvedEndpoints>,
    discovery: DiscoveryRecord,
    verified: bool,
    polling: bool,
    freshness_reference: Option<Instant>,
    ordinary: Option<(InFlight, OrdinaryOwner)>,
    active: Option<Operation>,
    queue: VecDeque<(Ticket, LinkCommand)>,
    urgent: Vec<UrgentLane>,
    awaiting_sample: Vec<AwaitingSample>,
    cycle: Option<Cycle>,
    next_cycle_at: Instant,
    latest: Option<TelemetrySample>,
}

impl Connection {
    fn usable(&self) -> bool {
        self.opened && !self.ended
    }
}

/// The deterministic Fibre link state machine for one lift axis.
pub struct LinkEngine {
    config: LinkConfig,
    sequences: Sequences,
    epoch: u64,
    connection: Option<Connection>,
    transmit_urgent: VecDeque<Transmit>,
    transmit_ordinary: VecDeque<Transmit>,
    events: VecDeque<LinkEvent>,
    unmatched_replies: u64,
    last_status: Option<LinkStatus>,
}

impl LinkEngine {
    /// Create an engine for the life of the process. `sequence_seed` sets the
    /// random starting point of the sequence counter, which continues across epochs.
    pub fn new(config: LinkConfig, sequence_seed: u16) -> Self {
        Self {
            config,
            sequences: Sequences {
                counter: sequence_seed & SEQUENCE_MASK,
                pending: HashSet::new(),
                quarantined: HashSet::new(),
            },
            epoch: 0,
            connection: None,
            transmit_urgent: VecDeque::new(),
            transmit_ordinary: VecDeque::new(),
            events: VecDeque::new(),
            unmatched_replies: 0,
            last_status: None,
        }
    }

    /// Start a connection attempt with a fresh epoch, ending any current one.
    pub fn begin_attempt(&mut self, now: Instant) -> u64 {
        self.end_epoch(now, "connection ended for a new attempt");
        self.epoch += 1;
        self.connection = Some(Connection {
            epoch: self.epoch,
            opened: false,
            ended: false,
            gate: Some(GateStep::Opening),
            gate_until: now + self.config.gate_deadline,
            blockers: Vec::new(),
            fault: None,
            trailer: None,
            endpoints: None,
            discovery: DiscoveryRecord::default(),
            verified: false,
            polling: false,
            freshness_reference: None,
            ordinary: None,
            active: None,
            queue: VecDeque::new(),
            urgent: Vec::new(),
            awaiting_sample: Vec::new(),
            cycle: None,
            next_cycle_at: now,
            latest: None,
        });
        self.publish_status();
        self.epoch
    }

    /// The device could not be opened; the attempt is blocked with `reason`.
    pub fn open_failed(&mut self, now: Instant, reason: impl Into<String>) {
        let _ = now;
        if let Some(connection) = self.connection.as_mut() {
            if !connection.opened && connection.gate.is_some() {
                connection.gate = None;
                connection.blockers.push(reason.into());
            }
        }
        self.publish_status();
    }

    /// The device opened and stale IN packets were discarded; start the gate.
    pub fn transport_opened(&mut self, now: Instant, usb_serial: &str) {
        let Some(connection) = self.connection.as_mut() else {
            return;
        };
        if connection.opened || !matches!(connection.gate, Some(GateStep::Opening)) {
            return;
        }
        connection.opened = true;
        connection.discovery.usb_serial = Some(usb_serial.to_owned());
        let reported = u64::from_str_radix(usb_serial, 16).ok();
        if reported != Some(self.config.usb_serial) {
            let reason = format!(
                "USB serial {usb_serial} does not match profile odrive.usb_serial {}",
                self.config.usb_serial_text
            );
            self.fail_gate(Some(reason));
        } else {
            connection.gate = Some(GateStep::Descriptor { buffer: Vec::new() });
        }
        self.pump(now);
        self.publish_status();
    }

    /// The transport failed or the device went away; end the epoch.
    pub fn transport_lost(&mut self, now: Instant, reason: impl Into<String>) {
        self.end_epoch(now, &reason.into());
        self.publish_status();
    }

    /// Explicitly end the current epoch.
    pub fn disconnect(&mut self, now: Instant) {
        self.end_epoch(now, "disconnected");
        self.publish_status();
    }

    /// Handle one received packet.
    pub fn receive(&mut self, now: Instant, packet: &[u8]) {
        self.handle_packet(now, packet);
        self.pump(now);
        self.publish_status();
    }

    /// Accept a typed command. The result arrives later as an event.
    pub fn submit(
        &mut self,
        now: Instant,
        ticket: Ticket,
        command: LinkCommand,
        priority: Priority,
    ) {
        let usable = self.connection.as_ref().is_some_and(Connection::usable);
        if !usable {
            self.finish(
                ticket,
                command,
                CommandResult::Rejected {
                    reason: "no ODrive connection".into(),
                },
            );
            self.publish_status();
            return;
        }
        match priority {
            Priority::Urgent => match self.plan(now, command) {
                Err(reason) => self.finish(ticket, command, CommandResult::Rejected { reason }),
                Ok((write, confirm)) => {
                    let op = Operation {
                        ticket,
                        command,
                        write,
                        confirm,
                        step: OpStep::SendWrite,
                        transmitted: false,
                        window: self.config.urgent_deadline,
                        confirm_until: None,
                        not_before: None,
                    };
                    let connection = self.connection.as_mut().expect("usable connection");
                    connection.urgent.push(UrgentLane {
                        op,
                        in_flight: None,
                    });
                    let lane = connection.urgent.len() - 1;
                    self.advance_urgent(now, lane);
                    if self.connection.as_ref().is_some_and(|c| {
                        c.urgent
                            .iter()
                            .any(|lane| lane.op.ticket == ticket && lane.op.transmitted)
                    }) {
                        self.retire_queue();
                    }
                }
            },
            Priority::Ordinary => {
                let connection = self.connection.as_mut().expect("usable connection");
                if connection.queue.len() >= ORDINARY_QUEUE_CAPACITY {
                    self.finish(ticket, command, CommandResult::Rejected {
                        reason: format!(
                            "ordinary command queue already holds {ORDINARY_QUEUE_CAPACITY} commands"
                        ),
                    });
                } else {
                    connection.queue.push_back((ticket, command));
                }
            }
        }
        self.pump(now);
        self.publish_status();
    }

    /// Process deadlines, freshness and the poll schedule at `now`.
    pub fn poll(&mut self, now: Instant) {
        self.expire(now);
        let lanes = self.connection.as_ref().map_or(0, |c| c.urgent.len());
        for lane in 0..lanes {
            self.advance_urgent(now, lane);
        }
        self.pump(now);
        self.publish_status();
    }

    /// The next packet to transmit; urgent packets first.
    pub fn take_transmit(&mut self) -> Option<Transmit> {
        self.transmit_urgent
            .pop_front()
            .or_else(|| self.transmit_ordinary.pop_front())
    }

    /// The next event.
    pub fn take_event(&mut self) -> Option<LinkEvent> {
        self.events.pop_front()
    }

    /// The earliest time `poll` has work to do.
    pub fn next_wakeup(&self) -> Option<Instant> {
        let connection = self.connection.as_ref()?;
        if connection.ended {
            return None;
        }
        let mut times = Vec::new();
        if connection.gate.is_some() {
            times.push(connection.gate_until);
        }
        if let Some((in_flight, _)) = &connection.ordinary {
            times.push(in_flight.deadline);
        }
        times.extend(
            connection
                .urgent
                .iter()
                .filter_map(|lane| lane.in_flight.as_ref().map(|in_flight| in_flight.deadline)),
        );
        times.extend(
            connection
                .awaiting_sample
                .iter()
                .map(|waiting| waiting.until),
        );
        times.extend(
            connection
                .urgent
                .iter()
                .filter_map(|lane| lane.op.not_before),
        );
        times.extend(connection.active.as_ref().and_then(|op| op.not_before));
        if let Some(GateStep::IdleRead { not_before }) = connection.gate {
            times.push(not_before);
        }
        if connection.polling && connection.ordinary.is_none() && connection.cycle.is_none() {
            times.push(connection.next_cycle_at);
        }
        if let Some(reference) = self.freshness_reference(connection) {
            times.push(reference + self.config.max_age + Duration::from_millis(1));
        }
        times.into_iter().min()
    }

    /// Current link status.
    pub fn status(&self) -> LinkStatus {
        let Some(connection) = self.connection.as_ref() else {
            return LinkStatus {
                epoch: 0,
                phase: LinkPhase::Disconnected,
                blockers: vec!["no connection attempt yet".into()],
                discovery: None,
                unmatched_replies: self.unmatched_replies,
            };
        };
        let phase = if connection.ended {
            LinkPhase::Disconnected
        } else if connection.gate.is_some() {
            LinkPhase::Connecting
        } else if connection.verified && connection.fault.is_none() {
            LinkPhase::Verified
        } else if connection.verified {
            LinkPhase::Faulted
        } else {
            LinkPhase::Blocked
        };
        let mut blockers = connection.blockers.clone();
        if let Some(fault) = &connection.fault {
            blockers.push(format!("link fault: {fault}"));
        }
        LinkStatus {
            epoch: connection.epoch,
            phase,
            blockers,
            discovery: connection.opened.then(|| connection.discovery.clone()),
            unmatched_replies: self.unmatched_replies,
        }
    }

    /// The latest current-epoch sample, or `None` if there is none or it is
    /// older than the profile allows. A stale sample latches a link fault.
    pub fn fresh_sample(&mut self, now: Instant) -> Option<TelemetrySample> {
        let sample = self.fresh_sample_inner(now);
        self.publish_status();
        sample
    }

    fn fresh_sample_inner(&mut self, now: Instant) -> Option<TelemetrySample> {
        let connection = self.connection.as_ref()?;
        if connection.ended {
            return None;
        }
        let sample = connection.latest.clone()?;
        if sample.epoch != connection.epoch {
            return None;
        }
        if sample.fresh_values(now).is_none() {
            self.latch_fault(format!(
                "telemetry is older than {} ms",
                self.config.max_age.as_millis()
            ));
            return None;
        }
        Some(sample)
    }

    // ----- epochs -------------------------------------------------------------

    fn end_epoch(&mut self, now: Instant, reason: &str) {
        let _ = now;
        let Some(connection) = self.connection.as_mut() else {
            return;
        };
        if connection.ended {
            return;
        }
        connection.ended = true;
        connection.gate = None;
        connection.blockers = vec![reason.to_owned()];
        let epoch = connection.epoch;
        let mut results = Vec::new();
        let mut quarantine = Vec::new();
        if let Some((in_flight, _)) = connection.ordinary.take() {
            quarantine.push(in_flight.sequence);
        }
        if let Some(op) = connection.active.take() {
            results.push((op.ticket, op.command, op.transmitted));
        }
        for (ticket, command) in connection.queue.drain(..) {
            results.push((ticket, command, false));
        }
        for lane in connection.urgent.drain(..) {
            if let Some(in_flight) = lane.in_flight {
                quarantine.push(in_flight.sequence);
            }
            results.push((lane.op.ticket, lane.op.command, lane.op.transmitted));
        }
        for waiting in connection.awaiting_sample.drain(..) {
            results.push((waiting.op.ticket, waiting.op.command, true));
        }
        connection.cycle = None;
        connection.polling = false;
        for sequence in quarantine {
            self.sequences.quarantine(sequence);
        }
        self.transmit_urgent
            .retain(|transmit| transmit.epoch != epoch);
        self.transmit_ordinary
            .retain(|transmit| transmit.epoch != epoch);
        for (ticket, command, transmitted) in results {
            self.events.push_back(LinkEvent::CommandResult {
                ticket,
                epoch,
                command,
                result: CommandResult::Retired { transmitted },
            });
        }
    }

    // ----- faults and retirement ---------------------------------------------

    fn latch_fault(&mut self, reason: String) {
        let Some(connection) = self.connection.as_mut() else {
            return;
        };
        if connection.fault.is_none() {
            connection.fault = Some(reason);
        }
        self.retire_queue();
    }

    fn retire_queue(&mut self) {
        let Some(connection) = self.connection.as_mut() else {
            return;
        };
        let epoch = connection.epoch;
        let retired: Vec<_> = connection.queue.drain(..).collect();
        for (ticket, command) in retired {
            self.events.push_back(LinkEvent::CommandResult {
                ticket,
                epoch,
                command,
                result: CommandResult::Retired { transmitted: false },
            });
        }
    }

    fn finish(&mut self, ticket: Ticket, command: LinkCommand, result: CommandResult) {
        self.events.push_back(LinkEvent::CommandResult {
            ticket,
            epoch: self.epoch,
            command,
            result,
        });
    }

    fn fail_gate(&mut self, reason: Option<String>) {
        if let Some(connection) = self.connection.as_mut() {
            if connection.gate.take().is_some() {
                if let Some(reason) = reason {
                    connection.blockers.push(reason);
                }
                if connection.polling {
                    connection.freshness_reference = Some(connection.next_cycle_at);
                }
            }
        }
    }

    // ----- sending ------------------------------------------------------------

    fn send(&mut self, now: Instant, spec: RequestSpec, urgent: bool) -> Option<InFlight> {
        let connection = self.connection.as_ref()?;
        let epoch = connection.epoch;
        let Some(sequence) = self.sequences.allocate() else {
            self.latch_fault("all 16384 sequence numbers are pending or quarantined".into());
            return None;
        };
        let packet = if let RequestSpec::Chunk(offset) = spec {
            encode_descriptor_read(sequence, offset)
        } else {
            let connection = self.connection.as_ref()?;
            let (Some(endpoints), Some(trailer)) =
                (connection.endpoints.as_ref(), connection.trailer)
            else {
                self.sequences.complete(sequence);
                return None;
            };
            match spec {
                RequestSpec::Read(endpoint) => encode_request(
                    sequence,
                    endpoints.id(endpoint),
                    value_type(endpoint).size() as u16,
                    &[],
                    trailer,
                ),
                RequestSpec::Write(endpoint, value) => encode_request(
                    sequence,
                    endpoints.id(endpoint),
                    0,
                    &value.encode(),
                    trailer,
                ),
                RequestSpec::Call(endpoint) => {
                    encode_request(sequence, endpoints.id(endpoint), 0, &[], trailer)
                }
                RequestSpec::Chunk(_) => unreachable!(),
            }
        };
        let deadline = now
            + if urgent {
                self.config.urgent_deadline
            } else {
                self.config.request_deadline
            };
        let transmit = Transmit {
            epoch,
            urgent,
            packet,
        };
        if urgent {
            self.transmit_urgent.push_back(transmit);
        } else {
            self.transmit_ordinary.push_back(transmit);
        }
        Some(InFlight {
            sequence,
            spec,
            deadline,
        })
    }

    fn send_ordinary(&mut self, now: Instant, spec: RequestSpec, owner: OrdinaryOwner) -> bool {
        match self.send(now, spec, false) {
            Some(in_flight) => {
                if let Some(connection) = self.connection.as_mut() {
                    connection.ordinary = Some((in_flight, owner));
                }
                true
            }
            None => false,
        }
    }

    /// Fill the ordinary slot: gate, then a waiting command, then telemetry.
    fn pump(&mut self, now: Instant) {
        loop {
            let Some(connection) = self.connection.as_ref() else {
                return;
            };
            if !connection.usable() || connection.ordinary.is_some() {
                return;
            }
            if let Some(step) = connection.gate.clone() {
                if !matches!(step, GateStep::FirstCycle) {
                    if matches!(step, GateStep::IdleRead { not_before } if now < not_before) {
                        return;
                    }
                    if let Some(spec) = self.gate_request(&step) {
                        self.send_ordinary(now, spec, OrdinaryOwner::Gate);
                    }
                    return;
                }
            }
            let connection = self.connection.as_ref().expect("checked above");
            if let Some(op) = connection.active.clone() {
                if op.not_before.is_some_and(|not_before| now < not_before) {
                    return;
                }
                if let Some(spec) = self.op_request(&op, false) {
                    if self.send_ordinary(now, spec, OrdinaryOwner::Command) {
                        let connection = self.connection.as_mut().expect("connection");
                        let active = connection.active.as_mut().expect("active");
                        active.transmitted = true;
                        advance_after_send(active, now);
                    }
                }
                return;
            }
            if connection.gate.is_none() || matches!(connection.gate, Some(GateStep::FirstCycle)) {
                if let Some((ticket, command)) = self
                    .connection
                    .as_mut()
                    .and_then(|connection| connection.queue.pop_front())
                {
                    match self.plan(now, command) {
                        Err(reason) => {
                            self.finish(ticket, command, CommandResult::Rejected { reason });
                        }
                        Ok((write, confirm)) => {
                            let window = self.config.request_deadline;
                            if let Some(connection) = self.connection.as_mut() {
                                connection.active = Some(Operation {
                                    ticket,
                                    command,
                                    write,
                                    confirm,
                                    step: OpStep::SendWrite,
                                    transmitted: false,
                                    window,
                                    confirm_until: None,
                                    not_before: None,
                                });
                            }
                        }
                    }
                    continue;
                }
            }
            let connection = self.connection.as_ref().expect("connection");
            if !connection.polling {
                return;
            }
            let index = match &connection.cycle {
                Some(cycle) => cycle.values.len(),
                None if now >= connection.next_cycle_at => {
                    if let Some(connection) = self.connection.as_mut() {
                        connection.cycle = Some(Cycle {
                            started_at: now,
                            values: Vec::new(),
                        });
                    }
                    0
                }
                None => return,
            };
            self.send_ordinary(
                now,
                RequestSpec::Read(TELEMETRY_READS[index]),
                OrdinaryOwner::Telemetry,
            );
            return;
        }
    }

    fn gate_request(&self, step: &GateStep) -> Option<RequestSpec> {
        Some(match step {
            GateStep::Opening | GateStep::FirstCycle => return None,
            GateStep::Descriptor { buffer } => RequestSpec::Chunk(buffer.len() as u32),
            GateStep::Identity { values } => RequestSpec::Read(IDENTITY_READS[values.len()]),
            GateStep::IdleWrite => {
                RequestSpec::Write(Endpoint::RequestedState, WireValue::I32(AXIS_STATE_IDLE))
            }
            GateStep::IdleRead { .. } => RequestSpec::Read(Endpoint::CurrentState),
            GateStep::Limit { index, readback } => {
                let (endpoint, value) = self.limit_writes()?[*index];
                if *readback {
                    RequestSpec::Read(endpoint)
                } else {
                    RequestSpec::Write(endpoint, WireValue::F32(value))
                }
            }
        })
    }

    fn limit_writes(&self) -> Option<[(Endpoint, f32); 5]> {
        let MotorLimits {
            current_a,
            velocity_turns_per_s,
            acceleration_turns_per_s2,
            deceleration_turns_per_s2,
        } = self.config.lift_axis.as_ref()?.motor_limits();
        Some([
            (Endpoint::CurrentLimit, current_a),
            (Endpoint::ControllerVelLimit, velocity_turns_per_s),
            (Endpoint::TrapVelLimit, velocity_turns_per_s),
            (Endpoint::TrapAccelLimit, acceleration_turns_per_s2),
            (Endpoint::TrapDecelLimit, deceleration_turns_per_s2),
        ])
    }

    fn op_request(&self, op: &Operation, _urgent: bool) -> Option<RequestSpec> {
        match op.step {
            OpStep::SendWrite => Some(op.write),
            OpStep::SendConfirm => Some(match op.confirm {
                Confirm::State { .. } => RequestSpec::Read(Endpoint::CurrentState),
                Confirm::Readback(endpoint, _) => RequestSpec::Read(endpoint),
                Confirm::NextSample => return None,
            }),
            OpStep::AwaitWrite | OpStep::AwaitConfirm => None,
        }
    }

    fn advance_urgent(&mut self, now: Instant, lane: usize) {
        let Some(connection) = self.connection.as_ref() else {
            return;
        };
        let Some(entry) = connection.urgent.get(lane) else {
            return;
        };
        if entry.in_flight.is_some() {
            return;
        }
        let op = entry.op.clone();
        if op.not_before.is_some_and(|not_before| now < not_before) {
            return;
        }
        let Some(spec) = self.op_request(&op, true) else {
            return;
        };
        if let Some(in_flight) = self.send(now, spec, true) {
            if let Some(entry) = self
                .connection
                .as_mut()
                .and_then(|connection| connection.urgent.get_mut(lane))
            {
                entry.in_flight = Some(in_flight);
                entry.op.transmitted = true;
                advance_after_send(&mut entry.op, now);
            }
        }
    }

    // ----- command acceptance --------------------------------------------------

    /// Check acceptance where the write happens and plan the wire action.
    fn plan(
        &mut self,
        now: Instant,
        command: LinkCommand,
    ) -> Result<(RequestSpec, Confirm), String> {
        let resolved = self.connection.as_ref().is_some_and(|connection| {
            connection.endpoints.is_some() && connection.trailer.is_some()
        });
        if !resolved {
            return Err("ODrive endpoints are not resolved on this connection".into());
        }
        let state_write =
            |state| RequestSpec::Write(Endpoint::RequestedState, WireValue::I32(state));
        match command {
            LinkCommand::Disarm => Ok((
                state_write(AXIS_STATE_IDLE),
                Confirm::State {
                    expected: AXIS_STATE_IDLE,
                    fault_on_miss: true,
                },
            )),
            LinkCommand::ClearErrors => Ok((
                RequestSpec::Call(Endpoint::ClearErrors),
                Confirm::NextSample,
            )),
            LinkCommand::Arm => {
                let values = self.ready_to_energize(now)?;
                require_error_words_zero(&values)?;
                Ok((
                    state_write(AXIS_STATE_CLOSED_LOOP_CONTROL),
                    Confirm::State {
                        expected: AXIS_STATE_CLOSED_LOOP_CONTROL,
                        fault_on_miss: false,
                    },
                ))
            }
            LinkCommand::Home => {
                let values = self.ready_to_energize(now)?;
                if values.motor_error != 0
                    || values.encoder_error != 0
                    || values.controller_error != 0
                {
                    return Err(format!(
                        "error words are not zero: motor 0x{:x}, encoder 0x{:x}, controller 0x{:x}",
                        values.motor_error, values.encoder_error, values.controller_error
                    ));
                }
                let only_endstop = values.axis_error == AXIS_ERROR_MIN_ENDSTOP_PRESSED
                    && values.min_endstop_pressed;
                if values.axis_error != 0 && !only_endstop {
                    return Err(format!(
                        "axis error 0x{:x} is not zero or only the minimum-endstop bit with the endstop pressed",
                        values.axis_error
                    ));
                }
                Ok((
                    state_write(AXIS_STATE_HOMING),
                    Confirm::State {
                        expected: AXIS_STATE_HOMING,
                        fault_on_miss: false,
                    },
                ))
            }
            LinkCommand::MoveAbsolute { position_mm } => {
                let Some(lift_axis) = self.config.lift_axis.clone() else {
                    return Err("the lift axis is uncommissioned".into());
                };
                if !position_mm.is_finite() {
                    return Err(format!("move target {position_mm} mm is not finite"));
                }
                if position_mm < lift_axis.travel_minimum_mm
                    || position_mm > lift_axis.travel_maximum_mm
                {
                    return Err(format!(
                        "move target {position_mm} mm is outside commissioned travel {} to {} mm",
                        lift_axis.travel_minimum_mm, lift_axis.travel_maximum_mm
                    ));
                }
                let values = self.ready_to_energize(now)?;
                require_error_words_zero(&values)?;
                if values.axis_state != AxisState::ClosedLoopControl
                    || values.control_mode != CONTROL_MODE_POSITION_CONTROL
                    || values.input_mode != INPUT_MODE_TRAP_TRAJ
                {
                    return Err(format!(
                        "move needs closed-loop control, control mode {CONTROL_MODE_POSITION_CONTROL} and input mode {INPUT_MODE_TRAP_TRAJ}; observed state {:?}, control mode {}, input mode {}",
                        values.axis_state, values.control_mode, values.input_mode
                    ));
                }
                let (sign, mm_per_turn) = self.config.direction_sign().expect("commissioned");
                let turns = WireValue::F32(sign * position_mm / mm_per_turn);
                Ok((
                    RequestSpec::Write(Endpoint::InputPos, turns),
                    Confirm::Readback(Endpoint::InputPos, turns),
                ))
            }
        }
    }

    fn ready_to_energize(&mut self, now: Instant) -> Result<TelemetryValues, String> {
        let connection = self.connection.as_ref().expect("checked by caller");
        if let Some(fault) = &connection.fault {
            return Err(format!("link fault is latched: {fault}"));
        }
        if !connection.verified {
            return Err("the link is not verified on this connection".into());
        }
        let sample = self
            .fresh_sample_inner(now)
            .ok_or_else(|| "no fresh telemetry sample".to_string())?;
        let values = sample.values;
        if !values.motor_calibrated || !values.encoder_ready {
            return Err(format!(
                "motor calibrated {} and encoder ready {} must both be true",
                values.motor_calibrated, values.encoder_ready
            ));
        }
        Ok(values)
    }

    // ----- receiving ----------------------------------------------------------

    fn handle_packet(&mut self, now: Instant, packet: &[u8]) {
        let Some((sequence, payload)) = decode_reply(packet) else {
            self.unmatched_replies += 1;
            return;
        };
        let Some(connection) = self.connection.as_mut() else {
            self.unmatched_replies += 1;
            return;
        };
        if connection.ended {
            self.unmatched_replies += 1;
            return;
        }
        if connection
            .ordinary
            .as_ref()
            .is_some_and(|(in_flight, _)| in_flight.sequence == sequence)
        {
            let (in_flight, owner) = connection.ordinary.take().expect("checked");
            match validate(&in_flight, payload) {
                Ok(reply) => {
                    self.sequences.complete(sequence);
                    self.on_ordinary_reply(now, owner, reply);
                }
                Err(reason) => {
                    self.sequences.quarantine(sequence);
                    self.latch_fault(reason.clone());
                    self.fail_ordinary(now, owner, reason);
                }
            }
            return;
        }
        if let Some(lane) = connection.urgent.iter().position(|lane| {
            lane.in_flight
                .as_ref()
                .is_some_and(|in_flight| in_flight.sequence == sequence)
        }) {
            let in_flight = connection.urgent[lane].in_flight.take().expect("checked");
            match validate(&in_flight, payload) {
                Ok(reply) => {
                    self.sequences.complete(sequence);
                    self.on_urgent_reply(now, lane, reply);
                }
                Err(reason) => {
                    self.sequences.quarantine(sequence);
                    self.latch_fault(reason.clone());
                    let lane = self
                        .connection
                        .as_mut()
                        .expect("connection")
                        .urgent
                        .remove(lane);
                    self.finish(
                        lane.op.ticket,
                        lane.op.command,
                        CommandResult::OutcomeUnknown { reason },
                    );
                }
            }
            return;
        }
        self.unmatched_replies += 1;
    }

    fn on_ordinary_reply(&mut self, now: Instant, owner: OrdinaryOwner, reply: Reply) {
        match owner {
            OrdinaryOwner::Gate => self.on_gate_reply(now, reply),
            OrdinaryOwner::Telemetry => self.on_telemetry_reply(now, reply),
            OrdinaryOwner::Command => {
                let Some(mut op) = self.connection.as_mut().and_then(|c| c.active.take()) else {
                    return;
                };
                match self.on_op_reply(now, &mut op, reply) {
                    OpOutcome::Done(result) => self.finish(op.ticket, op.command, result),
                    OpOutcome::Continue => {
                        if let Some(connection) = self.connection.as_mut() {
                            connection.active = Some(op);
                        }
                    }
                    OpOutcome::AwaitSample => self.await_sample(now, op),
                }
            }
        }
    }

    fn on_urgent_reply(&mut self, now: Instant, lane: usize, reply: Reply) {
        let Some(mut op) = self
            .connection
            .as_ref()
            .and_then(|connection| connection.urgent.get(lane))
            .map(|lane| lane.op.clone())
        else {
            return;
        };
        let outcome = self.on_op_reply(now, &mut op, reply);
        let Some(connection) = self.connection.as_mut() else {
            return;
        };
        match outcome {
            OpOutcome::Continue => {
                connection.urgent[lane].op = op;
                self.advance_urgent(now, lane);
            }
            OpOutcome::Done(result) => {
                connection.urgent.remove(lane);
                self.finish(op.ticket, op.command, result);
            }
            OpOutcome::AwaitSample => {
                connection.urgent.remove(lane);
                self.await_sample(now, op);
            }
        }
    }

    fn await_sample(&mut self, now: Instant, op: Operation) {
        let until = now + self.config.poll_interval + self.config.max_age;
        if let Some(connection) = self.connection.as_mut() {
            connection.awaiting_sample.push(AwaitingSample {
                op,
                after: now,
                until,
            });
        }
    }

    fn on_op_reply(&mut self, now: Instant, op: &mut Operation, reply: Reply) -> OpOutcome {
        match (op.step, reply) {
            (OpStep::AwaitWrite, Reply::Ack) => {
                if matches!(op.confirm, Confirm::NextSample) {
                    return OpOutcome::AwaitSample;
                }
                op.step = OpStep::SendConfirm;
                OpOutcome::Continue
            }
            (OpStep::AwaitConfirm, Reply::Value(value)) => match op.confirm {
                Confirm::State {
                    expected,
                    fault_on_miss,
                } => {
                    let observed = as_i32(value);
                    if observed == expected {
                        return OpOutcome::Done(CommandResult::Completed);
                    }
                    if op.confirm_until.is_some_and(|until| now >= until) {
                        let observed = format!(
                            "current_state {observed} after {} ms, expected {expected}",
                            op.window.as_millis()
                        );
                        if fault_on_miss {
                            self.latch_fault(format!("disarm not confirmed: {observed}"));
                        }
                        return OpOutcome::Done(CommandResult::NotConfirmed { observed });
                    }
                    op.step = OpStep::SendConfirm;
                    op.not_before = Some(now + CONFIRM_REREAD_INTERVAL);
                    OpOutcome::Continue
                }
                Confirm::Readback(endpoint, written) => {
                    if same_bits(value, written) {
                        OpOutcome::Done(CommandResult::Completed)
                    } else {
                        OpOutcome::Done(CommandResult::NotConfirmed {
                            observed: format!(
                                "{endpoint:?} read back {value:?}, wrote {written:?}"
                            ),
                        })
                    }
                }
                Confirm::NextSample => OpOutcome::Done(CommandResult::Completed),
            },
            (step, reply) => OpOutcome::Done(CommandResult::OutcomeUnknown {
                reason: format!("unexpected reply {reply:?} in step {step:?}"),
            }),
        }
    }

    fn on_gate_reply(&mut self, now: Instant, reply: Reply) {
        let Some(step) = self.connection.as_ref().and_then(|c| c.gate.clone()) else {
            return;
        };
        match (step, reply) {
            (GateStep::Descriptor { mut buffer }, Reply::Chunk(chunk)) => {
                if chunk.is_empty() {
                    self.finish_descriptor(&buffer);
                    return;
                }
                buffer.extend_from_slice(&chunk);
                if buffer.len() > MAX_DESCRIPTOR_BYTES {
                    self.fail_gate(Some(format!(
                        "descriptor exceeds {MAX_DESCRIPTOR_BYTES} bytes"
                    )));
                    return;
                }
                self.set_gate(GateStep::Descriptor { buffer });
            }
            (GateStep::Identity { mut values }, Reply::Value(value)) => {
                values.push(value);
                if values.len() < IDENTITY_READS.len() {
                    self.set_gate(GateStep::Identity { values });
                    return;
                }
                self.finish_identity(&values);
            }
            (GateStep::IdleWrite, Reply::Ack) => {
                self.set_gate(GateStep::IdleRead { not_before: now })
            }
            (GateStep::IdleRead { .. }, Reply::Value(value)) => {
                if as_i32(value) != AXIS_STATE_IDLE {
                    self.set_gate(GateStep::IdleRead {
                        not_before: now + CONFIRM_REREAD_INTERVAL,
                    });
                    return;
                }
                if let Some(connection) = self.connection.as_mut() {
                    connection.polling = true;
                    connection.next_cycle_at = now;
                }
                if self.config.lift_axis.is_none() {
                    self.fail_gate(Some(
                        "lift axis is uncommissioned: [odrive.lift_axis] is absent, so no limit was written"
                            .into(),
                    ));
                    if let Some(connection) = self.connection.as_mut() {
                        connection.freshness_reference = Some(now);
                    }
                } else {
                    self.set_gate(GateStep::Limit {
                        index: 0,
                        readback: false,
                    });
                }
            }
            (
                GateStep::Limit {
                    index,
                    readback: false,
                },
                Reply::Ack,
            ) => {
                self.set_gate(GateStep::Limit {
                    index,
                    readback: true,
                });
            }
            (
                GateStep::Limit {
                    index,
                    readback: true,
                },
                Reply::Value(value),
            ) => {
                let (endpoint, written) = self.limit_writes().expect("commissioned")[index];
                if !same_bits(value, WireValue::F32(written)) {
                    self.fail_gate(Some(format!(
                        "{endpoint:?} read back {value:?} after Sans wrote {written}"
                    )));
                    return;
                }
                if index + 1 < 5 {
                    self.set_gate(GateStep::Limit {
                        index: index + 1,
                        readback: false,
                    });
                } else {
                    self.set_gate(GateStep::FirstCycle);
                    if let Some(connection) = self.connection.as_mut() {
                        connection.next_cycle_at = now;
                    }
                }
            }
            (step, reply) => self.fail_gate(Some(format!(
                "unexpected reply {reply:?} during gate step {step:?}"
            ))),
        }
    }

    fn set_gate(&mut self, step: GateStep) {
        if let Some(connection) = self.connection.as_mut() {
            connection.gate = Some(step);
        }
    }

    fn finish_descriptor(&mut self, bytes: &[u8]) {
        let descriptor = match Descriptor::parse(bytes) {
            Ok(descriptor) => descriptor,
            Err(reason) => {
                self.fail_gate(Some(reason));
                return;
            }
        };
        let axis = self.config.axis;
        let expected = self.config.api_fingerprint.clone();
        let Some(connection) = self.connection.as_mut() else {
            return;
        };
        connection.discovery.descriptor_length = Some(descriptor.length());
        connection.discovery.descriptor_crc = Some(descriptor.crc16());
        connection.discovery.endpoint_count = Some(descriptor.endpoint_count());
        connection.discovery.api_fingerprint = Some(descriptor.fingerprint().to_owned());
        connection.trailer = Some(descriptor.crc16());
        let mut problems = Vec::new();
        match check_capabilities(descriptor.flattened(), axis) {
            Ok(endpoints) => connection.endpoints = Some(endpoints),
            Err(found) => problems.extend(found),
        }
        if descriptor.fingerprint() != expected {
            problems.push(format!(
                "API fingerprint {} does not match commissioned odrive.api_fingerprint_sha256 {expected}",
                descriptor.fingerprint()
            ));
        }
        if problems.is_empty() {
            connection.gate = Some(GateStep::Identity { values: Vec::new() });
        } else {
            connection.gate = None;
            connection.blockers.extend(problems);
        }
    }

    fn finish_identity(&mut self, values: &[WireValue]) {
        let byte = |index: usize| match values[index] {
            WireValue::U8(value) => value,
            _ => 0,
        };
        let board_serial = match values[0] {
            WireValue::U64(value) => value,
            _ => 0,
        };
        let vbus = match values[8] {
            WireValue::F32(value) => value,
            _ => f32::NAN,
        };
        let expected = self.config.usb_serial;
        let expected_text = self.config.usb_serial_text.clone();
        let Some(connection) = self.connection.as_mut() else {
            return;
        };
        let record = &mut connection.discovery;
        record.board_serial = Some(board_serial);
        record.hardware_version = Some([byte(1), byte(2), byte(3)]);
        record.firmware_version = Some([byte(4), byte(5), byte(6), byte(7)]);
        record.vbus_voltage = Some(vbus);
        record.armed_state = Some(as_i32(values[9]));
        if board_serial != expected {
            self.fail_gate(Some(format!(
                "board serial_number {board_serial:x} does not match profile odrive.usb_serial {expected_text}"
            )));
        } else {
            self.set_gate(GateStep::IdleWrite);
        }
    }

    fn on_telemetry_reply(&mut self, now: Instant, reply: Reply) {
        let Reply::Value(value) = reply else {
            return;
        };
        let Some(connection) = self.connection.as_mut() else {
            return;
        };
        let Some(cycle) = connection.cycle.as_mut() else {
            return;
        };
        cycle.values.push(value);
        if cycle.values.len() < TELEMETRY_READS.len() {
            return;
        }
        let cycle = connection.cycle.take().expect("checked");
        connection.next_cycle_at = (cycle.started_at + self.config.poll_interval).max(now);
        let values = self.convert(&cycle.values);
        let sample = TelemetrySample {
            epoch: self.epoch,
            taken_at: cycle.started_at,
            max_age: self.config.max_age,
            cycle_duration: now.saturating_duration_since(cycle.started_at),
            values,
        };
        let connection = self.connection.as_mut().expect("connection");
        connection.latest = Some(sample.clone());
        if matches!(connection.gate, Some(GateStep::FirstCycle)) {
            connection.gate = None;
            connection.verified = true;
        }
        let (completed, waiting): (Vec<_>, Vec<_>) = connection
            .awaiting_sample
            .drain(..)
            .partition(|waiting| sample.taken_at >= waiting.after);
        connection.awaiting_sample = waiting;
        self.events.push_back(LinkEvent::Telemetry(sample));
        for waiting in completed {
            self.finish(
                waiting.op.ticket,
                waiting.op.command,
                CommandResult::Completed,
            );
        }
    }

    fn convert(&self, raw: &[WireValue]) -> TelemetryValues {
        let float = |index: usize| match raw[index] {
            WireValue::F32(value) => value,
            _ => f32::NAN,
        };
        let flag = |index: usize| matches!(raw[index], WireValue::Bool(true));
        let mm = self
            .config
            .direction_sign()
            .map(|(sign, mm_per_turn)| sign * mm_per_turn);
        TelemetryValues {
            axis_state: AxisState::from_raw(as_i32(raw[0])),
            axis_error: as_i32(raw[1]) as u32,
            motor_error: as_i32(raw[2]) as u32,
            encoder_error: as_i32(raw[3]) as u32,
            controller_error: as_i32(raw[4]) as u32,
            position_mm: mm.map(|scale| float(5) * scale),
            velocity_mm_s: mm.map(|scale| float(6) * scale),
            iq_measured_a: float(7),
            iq_setpoint_a: float(8),
            trajectory_done: flag(9),
            is_homed: flag(10),
            min_endstop_pressed: flag(11),
            motor_calibrated: flag(12),
            encoder_ready: flag(13),
            control_mode: as_i32(raw[14]),
            input_mode: as_i32(raw[15]),
        }
    }

    // ----- deadlines ----------------------------------------------------------

    fn fail_ordinary(&mut self, now: Instant, owner: OrdinaryOwner, reason: String) {
        let _ = now;
        match owner {
            OrdinaryOwner::Gate => self.fail_gate(None),
            OrdinaryOwner::Telemetry => {
                if let Some(connection) = self.connection.as_mut() {
                    connection.cycle = None;
                    connection.next_cycle_at = now;
                }
            }
            OrdinaryOwner::Command => {
                if let Some(op) = self.connection.as_mut().and_then(|c| c.active.take()) {
                    self.finish(
                        op.ticket,
                        op.command,
                        CommandResult::OutcomeUnknown { reason },
                    );
                }
            }
        }
    }

    fn expire(&mut self, now: Instant) {
        let Some(connection) = self.connection.as_mut() else {
            return;
        };
        if connection.ended {
            return;
        }
        if let Some(gate) = connection
            .gate
            .as_ref()
            .filter(|_| now >= connection.gate_until)
        {
            let step = format!("{gate:?}");
            let step = step.split([' ', '{']).next().unwrap_or("").to_owned();
            let reason = format!(
                "connection gate did not finish within {} ms (stopped at {step})",
                self.config.gate_deadline.as_millis()
            );
            self.fail_gate(Some(reason));
        }
        let connection = self.connection.as_mut().expect("connection");
        if connection
            .ordinary
            .as_ref()
            .is_some_and(|(in_flight, _)| now >= in_flight.deadline)
        {
            let (in_flight, owner) = connection.ordinary.take().expect("checked");
            self.sequences.quarantine(in_flight.sequence);
            let reason = format!(
                "{} got no reply within {} ms",
                in_flight.spec.label(),
                self.config.request_deadline.as_millis()
            );
            self.latch_fault(reason.clone());
            self.fail_ordinary(now, owner, reason);
        }
        loop {
            let connection = self.connection.as_mut().expect("connection");
            let Some(lane) = connection.urgent.iter().position(|lane| {
                lane.in_flight
                    .as_ref()
                    .is_some_and(|in_flight| now >= in_flight.deadline)
            }) else {
                break;
            };
            let lane = connection.urgent.remove(lane);
            let in_flight = lane.in_flight.expect("checked");
            self.sequences.quarantine(in_flight.sequence);
            let reason = format!(
                "urgent {} got no reply within {} ms",
                in_flight.spec.label(),
                self.config.urgent_deadline.as_millis()
            );
            self.latch_fault(reason.clone());
            self.finish(
                lane.op.ticket,
                lane.op.command,
                CommandResult::OutcomeUnknown { reason },
            );
        }
        let connection = self.connection.as_mut().expect("connection");
        let (expired, waiting): (Vec<_>, Vec<_>) = connection
            .awaiting_sample
            .drain(..)
            .partition(|waiting| now >= waiting.until);
        connection.awaiting_sample = waiting;
        for waiting in expired {
            self.finish(
                waiting.op.ticket,
                waiting.op.command,
                CommandResult::OutcomeUnknown {
                    reason: "no telemetry sample followed the call".into(),
                },
            );
        }
        let connection = self.connection.as_ref().expect("connection");
        if connection.fault.is_none() {
            if let Some(reference) = self.freshness_reference(connection) {
                if now > reference + self.config.max_age {
                    self.latch_fault(format!(
                        "telemetry is older than {} ms",
                        self.config.max_age.as_millis()
                    ));
                }
            }
        }
    }

    fn freshness_reference(&self, connection: &Connection) -> Option<Instant> {
        if connection.ended || connection.gate.is_some() || !connection.polling {
            return None;
        }
        if connection.fault.is_some() {
            return None;
        }
        connection
            .latest
            .as_ref()
            .map(TelemetrySample::taken_at)
            .or(connection.freshness_reference)
    }

    fn publish_status(&mut self) {
        let status = self.status();
        if self.last_status.as_ref() != Some(&status) {
            self.last_status = Some(status.clone());
            self.events.push_back(LinkEvent::Status(status));
        }
    }
}

enum OpOutcome {
    Continue,
    Done(CommandResult),
    AwaitSample,
}

fn advance_after_send(op: &mut Operation, now: Instant) {
    op.step = match op.step {
        OpStep::SendWrite => {
            op.confirm_until = Some(now + op.window);
            OpStep::AwaitWrite
        }
        OpStep::SendConfirm => OpStep::AwaitConfirm,
        other => other,
    };
}

fn validate(in_flight: &InFlight, payload: &[u8]) -> Result<Reply, String> {
    let wrong = |expected: String| {
        Err(format!(
            "reply to {} had {} payload bytes, expected {expected}",
            in_flight.spec.label(),
            payload.len()
        ))
    };
    match in_flight.spec {
        RequestSpec::Chunk(_) => {
            if payload.len() > usize::from(DESCRIPTOR_CHUNK_REQUEST_LEN) {
                wrong(format!("at most {DESCRIPTOR_CHUNK_REQUEST_LEN}"))
            } else {
                Ok(Reply::Chunk(payload.to_vec()))
            }
        }
        RequestSpec::Read(endpoint) => {
            let value_type = value_type(endpoint);
            match WireValue::decode(value_type, payload) {
                Some(value) => Ok(Reply::Value(value)),
                None if payload.len() == value_type.size() => Err(format!(
                    "reply to {} carried an invalid {} encoding",
                    in_flight.spec.label(),
                    value_type.descriptor_name()
                )),
                None => wrong(value_type.size().to_string()),
            }
        }
        RequestSpec::Write(..) | RequestSpec::Call(_) => {
            if payload.is_empty() {
                Ok(Reply::Ack)
            } else {
                wrong("0".into())
            }
        }
    }
}

fn require_error_words_zero(values: &TelemetryValues) -> Result<(), String> {
    if values.error_words_zero() {
        Ok(())
    } else {
        Err(format!(
            "error words are not zero: axis 0x{:x}, motor 0x{:x}, encoder 0x{:x}, controller 0x{:x}",
            values.axis_error, values.motor_error, values.encoder_error, values.controller_error
        ))
    }
}

fn as_i32(value: WireValue) -> i32 {
    match value {
        WireValue::I32(value) => value,
        _ => i32::MIN,
    }
}

fn same_bits(left: WireValue, right: WireValue) -> bool {
    match (left, right) {
        (WireValue::F32(left), WireValue::F32(right)) => left.to_bits() == right.to_bits(),
        (left, right) => left == right,
    }
}
