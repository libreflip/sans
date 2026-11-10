//! Link engine seam: commands, time and packets in; packets and events out.
//!
//! The other side is the fake Fibre device. Time is a parameter, so there are
//! no sleeps and no clock fake. These results are fake-backed host behaviour
//! against upstream semantics, not hardware acceptance.

mod support;

use std::fs;
use std::time::{Duration, Instant};

use sans_core::{
    prepare_machine_profile, CommandResult, LinkCommand, LinkConfig, LinkEngine, LinkEvent,
    LinkPhase, Priority, Ticket,
};
use support::fake_fibre::*;

const VALID_PROFILE: &str = include_str!("fixtures/valid-sans.toml");
const LIFT_AXIS: &str = r#"
[odrive.lift_axis]
direction = "normal"
mm_per_turn = 8.0
travel_minimum_mm = 0.0
travel_maximum_mm = 300.0
current_limit_a = 4.0
velocity_limit_mm_s = 6.0
acceleration_limit_mm_s2 = 4.0
deceleration_limit_mm_s2 = 2.0
"#;

fn config_from(profile: &str) -> LinkConfig {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sans.toml");
    fs::write(&path, profile).unwrap();
    let prepared = prepare_machine_profile(Some(&path)).unwrap();
    LinkConfig::from_profile(prepared.profile()).unwrap()
}

fn uncommissioned() -> LinkConfig {
    config_from(VALID_PROFILE)
}

fn commissioned() -> LinkConfig {
    config_from(&format!("{VALID_PROFILE}{LIFT_AXIS}"))
}

struct Harness {
    engine: LinkEngine,
    board: FakeFibre,
    now: Instant,
    /// Every packet the engine transmitted, in order, with its urgent flag.
    wire: Vec<(bool, Vec<u8>)>,
    events: Vec<LinkEvent>,
    /// Sequence numbers in order: `true` when sent, `false` when its reply was delivered.
    trace: Vec<(bool, u16)>,
}

impl Harness {
    fn new(config: LinkConfig, board: FakeFibre) -> Self {
        Self {
            engine: LinkEngine::new(config, 0x1234),
            board,
            now: Instant::now(),
            wire: Vec::new(),
            events: Vec::new(),
            trace: Vec::new(),
        }
    }

    /// Start an attempt and report the fake device as opened.
    fn connect(&mut self) {
        self.engine.begin_attempt(self.now);
        self.engine.transport_opened(self.now, "316536713335");
        self.exchange();
    }

    /// Deliver transmitted packets to the board and replies back until quiet.
    fn exchange(&mut self) {
        loop {
            let mut replies = Vec::new();
            while let Some(transmit) = self.engine.take_transmit() {
                self.wire.push((transmit.urgent, transmit.packet.clone()));
                self.trace
                    .push((true, decode_request(&transmit.packet).sequence));
                if let Some(reply) = self.board.handle(&transmit.packet) {
                    replies.push(reply);
                }
            }
            self.drain_events();
            if replies.is_empty() {
                return;
            }
            for reply in replies {
                self.trace
                    .push((false, u16::from_le_bytes([reply[0], reply[1]]) & 0x7fff));
                self.engine.receive(self.now, &reply);
            }
        }
    }

    /// Transmit without delivering, returning the decoded requests.
    fn transmit_only(&mut self) -> Vec<(bool, Received, Vec<u8>)> {
        let mut sent = Vec::new();
        while let Some(transmit) = self.engine.take_transmit() {
            self.wire.push((transmit.urgent, transmit.packet.clone()));
            sent.push((
                transmit.urgent,
                decode_request(&transmit.packet),
                transmit.packet,
            ));
        }
        self.drain_events();
        sent
    }

    /// Advance time in 1 ms steps, exchanging at each step.
    fn run_for(&mut self, ms: u64) {
        for _ in 0..ms {
            self.now += Duration::from_millis(1);
            self.engine.poll(self.now);
            self.exchange();
        }
    }

    fn drain_events(&mut self) {
        while let Some(event) = self.engine.take_event() {
            self.events.push(event);
        }
    }

    fn submit(&mut self, ticket: u64, command: LinkCommand, priority: Priority) {
        self.engine
            .submit(self.now, Ticket(ticket), command, priority);
        self.exchange();
    }

    fn result(&self, ticket: u64) -> Option<CommandResult> {
        self.events.iter().find_map(|event| match event {
            LinkEvent::CommandResult {
                ticket: Ticket(t),
                result,
                ..
            } if *t == ticket => Some(result.clone()),
            _ => None,
        })
    }

    fn requests_to(&self, endpoint: u16) -> Vec<Received> {
        self.wire
            .iter()
            .map(|(_, packet)| decode_request(packet))
            .filter(|request| request.endpoint == endpoint)
            .collect()
    }
}

#[test]
fn compatible_device_downloads_the_descriptor_in_chunks_then_uses_its_crc_trailer() {
    let mut link = Harness::new(uncommissioned(), FakeFibre::compatible());

    link.connect();

    let chunks = link.requests_to(0);
    // 38157 bytes at 30 bytes per reply, then one empty reply.
    assert_eq!(chunks.len(), 1273);
    let mut expected_offset = 0_u32;
    for chunk in &chunks {
        assert_eq!(chunk.expected, 512);
        assert_eq!(
            u32::from_le_bytes(chunk.payload[..].try_into().unwrap()),
            expected_offset
        );
        expected_offset = (expected_offset + 30).min(38157);
    }
    let after_descriptor: Vec<_> = link
        .wire
        .iter()
        .filter(|(_, packet)| decode_request(packet).endpoint != 0)
        .collect();
    assert!(!after_descriptor.is_empty());
    for (_, packet) in after_descriptor {
        assert_eq!(&packet[packet.len() - 2..], &[0x40, 0x9B]);
    }
    assert_eq!(link.board.dropped_bad_trailer, 0);
    let status = link.engine.status();
    let discovery = status.discovery.unwrap();
    assert_eq!(discovery.descriptor_crc, Some(39744));
    assert_eq!(discovery.endpoint_count, Some(535));
    assert_eq!(
        discovery.api_fingerprint.as_deref(),
        Some("a36d84437bd2bf5925c0119a1df53d88de8ebe1aa8df0a338d2f6dd56fd5e52b")
    );
    assert_ne!(status.phase, LinkPhase::Disconnected);
}

fn connect_with_usb_serial(link: &mut Harness, usb_serial: &str) {
    link.engine.begin_attempt(link.now);
    link.engine.transport_opened(link.now, usb_serial);
    link.exchange();
}

#[test]
fn identity_mismatch_between_profile_usb_string_and_board_endpoint_blocks() {
    let mut usb_differs = Harness::new(uncommissioned(), FakeFibre::compatible());
    connect_with_usb_serial(&mut usb_differs, "316536713336");
    let status = usb_differs.engine.status();
    assert_eq!(status.phase, LinkPhase::Blocked);
    assert!(
        status.blockers[0].contains("USB serial 316536713336 does not match"),
        "{status:?}"
    );
    assert!(
        usb_differs.wire.is_empty(),
        "nothing is sent to the wrong device"
    );

    let mut board = FakeFibre::compatible();
    board.set_u64(SERIAL_NUMBER, 0x3165_3671_3336);
    let mut endpoint_differs = Harness::new(uncommissioned(), board);
    endpoint_differs.connect();
    let status = endpoint_differs.engine.status();
    assert_eq!(status.phase, LinkPhase::Blocked);
    assert!(
        status.blockers[0].contains("board serial_number 316536713336 does not match"),
        "{status:?}"
    );
    assert!(endpoint_differs.board.writes.is_empty());
}

#[test]
fn fingerprint_mismatch_blocks_with_the_discovered_value_in_the_reason() {
    let profile = VALID_PROFILE.replace(
        "a36d84437bd2bf5925c0119a1df53d88de8ebe1aa8df0a338d2f6dd56fd5e52b",
        "0000000000000000000000000000000000000000000000000000000000000000",
    );
    let mut link = Harness::new(config_from(&profile), FakeFibre::compatible());

    link.connect();

    let status = link.engine.status();
    assert_eq!(status.phase, LinkPhase::Blocked);
    assert_eq!(status.blockers.len(), 1, "{status:?}");
    assert!(status.blockers[0].contains(
        "API fingerprint a36d84437bd2bf5925c0119a1df53d88de8ebe1aa8df0a338d2f6dd56fd5e52b"
    ));
    assert!(link.board.writes.is_empty());
}

#[test]
fn incompatible_descriptor_lists_every_problem_and_writes_nothing() {
    let descriptor = String::from_utf8(DESCRIPTOR.to_vec())
        .unwrap()
        .replacen(
            r#"{"name":"current_lim","id":180,"type":"float","access":"rw"}"#,
            r#"{"name":"current_lim","id":180,"type":"int32","access":"rw"}"#,
            1,
        )
        .replacen(
            r#"{"name":"requested_state","id":74,"type":"int32","access":"rw"}"#,
            r#"{"name":"requested_state","id":74,"type":"int32","access":"r"}"#,
            1,
        );
    assert_ne!(descriptor.as_bytes(), DESCRIPTOR);
    let board = FakeFibre::compatible().with_descriptor(descriptor.into_bytes());
    let mut link = Harness::new(commissioned(), board);

    link.connect();

    let status = link.engine.status();
    assert_eq!(status.phase, LinkPhase::Blocked);
    assert_eq!(
        status.blockers[..2],
        [
            "endpoint axis0.requested_state has access r, Sans needs write".to_string(),
            "endpoint axis0.motor.config.current_lim has type int32, Sans needs float".to_string(),
        ]
    );
    assert!(status.blockers[2].starts_with("API fingerprint "));
    assert!(link.board.writes.is_empty());
}

#[test]
fn gate_writes_idle_first_and_waits_for_the_idle_readback() {
    let mut board = FakeFibre::compatible();
    board.set_i32(CURRENT_STATE, 8);
    let mut link = Harness::new(uncommissioned(), board);

    link.connect();

    assert_eq!(link.board.requested_states(), vec![1]);
    let first_write = link
        .wire
        .iter()
        .map(|(_, packet)| decode_request(packet))
        .find(|request| !request.payload.is_empty() && request.endpoint != 0)
        .unwrap();
    assert_eq!(first_write.endpoint, REQUESTED_STATE);
    assert!(!link.requests_to(CURRENT_STATE).is_empty());
}

#[test]
fn board_that_never_reports_idle_blocks_at_the_gate_deadline() {
    let mut board = FakeFibre::compatible();
    board.set_i32(CURRENT_STATE, 8);
    board.ignore_state_requests.push(1);
    let mut link = Harness::new(commissioned(), board);

    link.connect();
    link.run_for(4_990);
    assert_eq!(link.engine.status().phase, LinkPhase::Connecting);
    link.run_for(20);

    let status = link.engine.status();
    assert_eq!(status.phase, LinkPhase::Blocked);
    assert!(
        status.blockers[0].contains("did not finish within 5000 ms"),
        "{status:?}"
    );
    assert!(link.board.writes_to(CURRENT_LIM).is_empty());
    assert_eq!(link.board.requested_states(), vec![1]);
}

#[test]
fn uncommissioned_profile_writes_no_limit_and_rejects_energizing_commands() {
    let mut link = Harness::new(uncommissioned(), FakeFibre::compatible());
    link.connect();
    link.run_for(50);

    let status = link.engine.status();
    assert_eq!(status.phase, LinkPhase::Blocked);
    assert!(status.blockers[0].contains("uncommissioned"), "{status:?}");
    assert_eq!(link.board.requested_states(), vec![1]);
    assert_eq!(link.board.writes.len(), 1, "only the idle write");

    link.submit(1, LinkCommand::Arm, Priority::Ordinary);
    link.submit(2, LinkCommand::Home, Priority::Urgent);
    link.submit(
        3,
        LinkCommand::MoveAbsolute { position_mm: 10.0 },
        Priority::Ordinary,
    );
    link.run_for(5);

    for ticket in 1..=3 {
        assert!(
            matches!(link.result(ticket), Some(CommandResult::Rejected { .. })),
            "{:?}",
            link.result(ticket)
        );
    }
    assert_eq!(link.board.writes.len(), 1);
}

#[test]
fn commissioned_limits_are_converted_written_and_read_back_before_energizing() {
    let mut link = Harness::new(commissioned(), FakeFibre::compatible());
    link.connect();
    link.run_for(5);
    assert_eq!(link.engine.status().phase, LinkPhase::Verified);

    link.submit(1, LinkCommand::Arm, Priority::Ordinary);
    link.run_for(5);
    assert_eq!(link.result(1), Some(CommandResult::Completed));

    // 4.0 A unchanged; 6 mm/s and 4, 2 mm/s^2 over 8 mm per turn.
    let f32s = |endpoint| -> Vec<f32> {
        link.board
            .writes_to(endpoint)
            .into_iter()
            .map(|payload| f32::from_le_bytes(payload.try_into().unwrap()))
            .collect()
    };
    assert_eq!(f32s(CURRENT_LIM), vec![4.0]);
    assert_eq!(f32s(CONTROLLER_VEL_LIMIT), vec![0.75]);
    assert_eq!(f32s(TRAP_VEL_LIMIT), vec![0.75]);
    assert_eq!(f32s(TRAP_ACCEL_LIMIT), vec![0.5]);
    assert_eq!(f32s(TRAP_DECEL_LIMIT), vec![0.25]);
    let order: Vec<u16> = link
        .board
        .writes
        .iter()
        .map(|(endpoint, _)| *endpoint)
        .collect();
    assert_eq!(
        order,
        vec![
            REQUESTED_STATE,
            CURRENT_LIM,
            CONTROLLER_VEL_LIMIT,
            TRAP_VEL_LIMIT,
            TRAP_ACCEL_LIMIT,
            TRAP_DECEL_LIMIT,
            REQUESTED_STATE,
        ]
    );
    assert_eq!(link.board.requested_states(), vec![1, 8]);
    // Each limit write is followed by its readback before the next write.
    let limit_traffic: Vec<(u16, bool)> = link
        .board
        .requests
        .iter()
        .filter(|request| [CURRENT_LIM, CONTROLLER_VEL_LIMIT].contains(&request.endpoint))
        .map(|request| (request.endpoint, request.payload.is_empty()))
        .collect();
    assert_eq!(
        limit_traffic,
        vec![
            (CURRENT_LIM, false),
            (CURRENT_LIM, true),
            (CONTROLLER_VEL_LIMIT, false),
            (CONTROLLER_VEL_LIMIT, true),
        ]
    );
}

#[test]
fn differing_limit_readback_blocks_the_lift_axis() {
    let mut board = FakeFibre::compatible();
    board
        .readback_override
        .insert(TRAP_ACCEL_LIMIT, 0.6_f32.to_le_bytes().to_vec());
    let mut link = Harness::new(commissioned(), board);

    link.connect();
    link.run_for(5);
    link.submit(1, LinkCommand::Arm, Priority::Urgent);

    let status = link.engine.status();
    assert_eq!(status.phase, LinkPhase::Blocked);
    assert!(
        status.blockers[0].contains("TrapAccelLimit read back"),
        "{status:?}"
    );
    assert!(link.board.writes_to(TRAP_DECEL_LIMIT).is_empty());
    assert!(matches!(
        link.result(1),
        Some(CommandResult::Rejected { .. })
    ));
    assert_eq!(link.board.requested_states(), vec![1]);
}

fn verified(config: LinkConfig, board: FakeFibre) -> Harness {
    let mut link = Harness::new(config, board);
    link.connect();
    link.run_for(5);
    assert_eq!(link.engine.status().phase, LinkPhase::Verified);
    link
}

fn status_has_fault(link: &Harness) -> bool {
    link.engine
        .status()
        .blockers
        .iter()
        .any(|blocker| blocker.starts_with("link fault"))
}

/// Advance until a telemetry read is transmitted and hold it undelivered.
fn hold_next_telemetry_read(link: &mut Harness) -> Vec<u8> {
    for _ in 0..100 {
        link.now += Duration::from_millis(1);
        link.engine.poll(link.now);
        let sent = link.transmit_only();
        if let Some((_, _, packet)) = sent.into_iter().next() {
            return packet;
        }
    }
    panic!("no telemetry read was transmitted");
}

fn reversed() -> LinkConfig {
    config_from(&format!(
        "{VALID_PROFILE}{}",
        LIFT_AXIS.replace("direction = \"normal\"", "direction = \"reversed\"")
    ))
}

#[test]
fn telemetry_cycle_yields_one_sample_in_millimetres_and_amps() {
    let mut board = FakeFibre::compatible();
    board.set_f32(POS_ESTIMATE, 2.5);
    board.set_f32(VEL_ESTIMATE, 0.5);
    board.set_f32(IQ_MEASURED, 1.25);
    board.set_f32(IQ_SETPOINT, 1.5);
    let mut link = verified(commissioned(), board);

    let sample = link.engine.fresh_sample(link.now).unwrap();
    assert_eq!(sample.epoch(), 1);
    assert!(sample.taken_at() <= link.now);
    let values = sample.fresh_values(link.now).unwrap();
    assert_eq!(values.position_mm, Some(20.0));
    assert_eq!(values.velocity_mm_s, Some(4.0));
    assert_eq!(values.iq_measured_a, 1.25);
    assert_eq!(values.iq_setpoint_a, 1.5);
    assert!(values.axis_state.is_idle());
    // One cycle reads all 16 telemetry rows once.
    let telemetry_reads_per_cycle = link.requests_to(MIN_ENDSTOP_STATE).len();
    assert_eq!(
        telemetry_reads_per_cycle,
        link.requests_to(INPUT_MODE).len()
    );

    let mut board = FakeFibre::compatible();
    board.set_f32(POS_ESTIMATE, 2.5);
    board.set_f32(VEL_ESTIMATE, 0.5);
    let mut flipped = verified(reversed(), board);
    let sample = flipped.engine.fresh_sample(flipped.now).unwrap();
    let values = sample.fresh_values(flipped.now).unwrap();
    assert_eq!(values.position_mm, Some(-20.0));
    assert_eq!(values.velocity_mm_s, Some(-4.0));
}

#[test]
fn uncommissioned_telemetry_shows_no_millimetre_values() {
    let mut link = Harness::new(uncommissioned(), FakeFibre::compatible());
    link.connect();
    link.run_for(5);
    let sample = link.engine.fresh_sample(link.now).unwrap();
    let values = sample.fresh_values(link.now).unwrap();
    assert_eq!(values.position_mm, None);
    assert_eq!(values.velocity_mm_s, None);
}

#[test]
fn sample_older_than_the_maximum_age_is_refused_and_latches_the_link_fault() {
    let mut link = verified(commissioned(), FakeFibre::compatible());
    let sample = link.engine.fresh_sample(link.now).unwrap();

    let later = sample.taken_at() + Duration::from_millis(101);
    assert!(sample.fresh_values(later).is_none());
    assert!(link.engine.fresh_sample(later).is_none());

    assert!(status_has_fault(&link));
    assert_eq!(link.engine.status().phase, LinkPhase::Faulted);
}

#[test]
fn stalled_telemetry_latches_the_fault_without_a_reader() {
    let mut link = verified(commissioned(), FakeFibre::compatible());
    let _held = hold_next_telemetry_read(&mut link);
    // The held read's own deadline (100 ms) and the age limit (100 ms) both expire.
    link.now += Duration::from_millis(150);
    link.engine.poll(link.now);
    assert!(status_has_fault(&link));
}

#[test]
fn wrong_length_reply_latches_the_fault_and_retires_queued_commands() {
    let mut link = verified(commissioned(), FakeFibre::compatible());
    let read = hold_next_telemetry_read(&mut link);
    link.submit(1, LinkCommand::Disarm, Priority::Ordinary);
    link.submit(2, LinkCommand::Arm, Priority::Ordinary);

    let mut reply = link.board.handle(&read).unwrap();
    reply.push(0);
    link.engine.receive(link.now, &reply);
    link.exchange();

    assert!(status_has_fault(&link));
    assert_eq!(
        link.result(1),
        Some(CommandResult::Retired { transmitted: false })
    );
    assert_eq!(
        link.result(2),
        Some(CommandResult::Retired { transmitted: false })
    );
    link.submit(3, LinkCommand::Arm, Priority::Ordinary);
    assert!(matches!(
        link.result(3),
        Some(CommandResult::Rejected { .. })
    ));
    assert_eq!(link.board.requested_states(), vec![1]);
}

#[test]
fn missed_deadline_latches_the_fault_and_retires_queued_commands() {
    let mut link = verified(commissioned(), FakeFibre::compatible());
    let _read = hold_next_telemetry_read(&mut link);
    link.submit(1, LinkCommand::Arm, Priority::Ordinary);

    link.now += Duration::from_millis(100);
    link.engine.poll(link.now);
    link.exchange();

    assert!(status_has_fault(&link));
    assert!(link
        .engine
        .status()
        .blockers
        .iter()
        .any(|blocker| blocker.contains("got no reply within 100 ms")));
    assert_eq!(
        link.result(1),
        Some(CommandResult::Retired { transmitted: false })
    );
    // Polling continues on the faulted link so current-epoch telemetry stays readable.
    link.run_for(30);
    assert!(link.engine.fresh_sample(link.now).is_some());
}

#[test]
fn late_reply_is_ignored_and_its_sequence_number_is_never_used_again() {
    let mut link = verified(commissioned(), FakeFibre::compatible());
    let read = hold_next_telemetry_read(&mut link);
    let timed_out = decode_request(&read).sequence;
    link.now += Duration::from_millis(100);
    link.engine.poll(link.now);
    link.exchange();
    let unmatched_before = link.engine.status().unmatched_replies;

    let late = link.board.handle(&read).unwrap();
    link.engine.receive(link.now, &late);
    link.exchange();
    assert_eq!(link.engine.status().unmatched_replies, unmatched_before + 1);

    let sent_before = link.wire.len();
    link.connect();
    assert_eq!(link.engine.status().epoch, 2);
    // Enough telemetry to cycle through all 16384 usable sequence numbers.
    link.run_for(25_000);
    let later: Vec<u16> = link.wire[sent_before..]
        .iter()
        .map(|(_, packet)| decode_request(packet).sequence)
        .collect();
    assert!(later.len() > 16_384, "{}", later.len());
    assert!(!later.contains(&timed_out));
    assert!(later
        .iter()
        .all(|sequence| sequence & 0x0080 != 0 && sequence & 0x8000 == 0));
}

#[test]
fn replayed_reply_with_a_pending_number_but_wrong_length_is_ignored_and_faults() {
    let mut link = verified(commissioned(), FakeFibre::compatible());
    let read = hold_next_telemetry_read(&mut link);
    let request = decode_request(&read);
    assert_eq!(
        request.expected, 4,
        "the first telemetry read is current_state"
    );

    // An old acknowledgement that happens to carry the pending number.
    let replay = (request.sequence | 0x8000).to_le_bytes().to_vec();
    link.engine.receive(link.now, &replay);
    link.exchange();
    assert!(status_has_fault(&link));

    let genuine = link.board.handle(&read).unwrap();
    let unmatched = link.engine.status().unmatched_replies;
    link.engine.receive(link.now, &genuine);
    assert_eq!(link.engine.status().unmatched_replies, unmatched + 1);
}

fn armed(config: LinkConfig) -> Harness {
    let mut link = verified(config, FakeFibre::compatible());
    link.submit(100, LinkCommand::Arm, Priority::Ordinary);
    link.run_for(30);
    assert_eq!(link.result(100), Some(CommandResult::Completed));
    link
}

#[test]
fn urgent_disarm_leaves_while_a_read_is_outstanding_and_retires_a_queued_move() {
    let mut link = armed(commissioned());
    let read = hold_next_telemetry_read(&mut link);
    link.submit(
        1,
        LinkCommand::MoveAbsolute { position_mm: 40.0 },
        Priority::Ordinary,
    );
    let wire_before = link.wire.len();

    link.engine
        .submit(link.now, Ticket(2), LinkCommand::Disarm, Priority::Urgent);
    let sent = link.transmit_only();

    assert_eq!(sent.len(), 1);
    let (urgent, request, packet) = &sent[0];
    assert!(*urgent);
    assert_eq!(request.endpoint, REQUESTED_STATE);
    assert_eq!(request.payload, 1_i32.to_le_bytes());
    assert_eq!(link.wire.len(), wire_before + 1);
    assert_eq!(
        link.result(1),
        Some(CommandResult::Retired { transmitted: false })
    );

    // Only now does the outstanding read's reply arrive.
    let ack = link.board.handle(packet).unwrap();
    let reply = link.board.handle(&read).unwrap();
    link.engine.receive(link.now, &reply);
    link.engine.receive(link.now, &ack);
    link.exchange();
    link.run_for(5);
    assert_eq!(link.result(2), Some(CommandResult::Completed));
    assert!(link.board.writes_to(INPUT_POS).is_empty());
}

#[test]
fn at_most_one_ordinary_request_is_in_flight() {
    let mut link = armed(commissioned());
    let _read = hold_next_telemetry_read(&mut link);
    link.submit(
        1,
        LinkCommand::MoveAbsolute { position_mm: 40.0 },
        Priority::Ordinary,
    );
    link.submit(2, LinkCommand::ClearErrors, Priority::Ordinary);
    link.now += Duration::from_millis(10);
    link.engine.poll(link.now);
    assert!(link.transmit_only().is_empty());
}

#[test]
fn fifth_ordinary_command_is_rejected() {
    let mut link = armed(commissioned());
    let _read = hold_next_telemetry_read(&mut link);
    for ticket in 1..=5 {
        link.submit(ticket, LinkCommand::ClearErrors, Priority::Ordinary);
    }
    assert!(link.result(4).is_none());
    assert!(matches!(
        link.result(5),
        Some(CommandResult::Rejected { .. })
    ));
}

#[test]
fn move_requires_position_control_and_trapezoidal_input_and_never_writes_a_mode() {
    let mut link = armed(commissioned());
    link.board.set_i32(CONTROL_MODE, 2);
    link.run_for(25);
    link.submit(
        1,
        LinkCommand::MoveAbsolute { position_mm: 40.0 },
        Priority::Ordinary,
    );
    link.run_for(2);
    match link.result(1) {
        Some(CommandResult::Rejected { reason }) => {
            assert!(reason.contains("control mode 2"), "{reason}");
            assert!(reason.contains("input mode 5"), "{reason}");
        }
        other => panic!("{other:?}"),
    }

    link.board.set_i32(CONTROL_MODE, 3);
    link.board.set_i32(INPUT_MODE, 1);
    link.run_for(25);
    link.submit(
        2,
        LinkCommand::MoveAbsolute { position_mm: 40.0 },
        Priority::Ordinary,
    );
    link.run_for(2);
    match link.result(2) {
        Some(CommandResult::Rejected { reason }) => {
            assert!(reason.contains("input mode 1"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
    assert!(link.board.writes_to(INPUT_POS).is_empty());

    link.board.set_i32(INPUT_MODE, 5);
    link.run_for(25);
    link.submit(
        3,
        LinkCommand::MoveAbsolute { position_mm: 40.0 },
        Priority::Ordinary,
    );
    link.run_for(5);
    assert_eq!(link.result(3), Some(CommandResult::Completed));
    // 40 mm at 8 mm per turn is 5 turns.
    assert_eq!(
        link.board.writes_to(INPUT_POS),
        vec![5.0_f32.to_le_bytes().to_vec()]
    );
    let input_pos = link.requests_to(INPUT_POS);
    assert_eq!(input_pos.len(), 2);
    assert!(
        input_pos[1].payload.is_empty(),
        "the write is confirmed by a readback"
    );

    for ticket in 4..=6 {
        let position_mm = match ticket {
            4 => 300.5,
            5 => -0.5,
            _ => f32::NAN,
        };
        link.submit(
            ticket,
            LinkCommand::MoveAbsolute { position_mm },
            Priority::Urgent,
        );
        assert!(matches!(
            link.result(ticket),
            Some(CommandResult::Rejected { .. })
        ));
    }
    assert_eq!(link.board.writes_to(INPUT_POS).len(), 1);
    assert!(link.board.writes_to(CONTROL_MODE).is_empty());
    assert!(link.board.writes_to(INPUT_MODE).is_empty());
}

#[test]
fn reversed_move_negates_the_target_in_turns() {
    let mut link = armed(reversed());
    link.submit(
        1,
        LinkCommand::MoveAbsolute { position_mm: 40.0 },
        Priority::Ordinary,
    );
    link.run_for(5);
    assert_eq!(link.result(1), Some(CommandResult::Completed));
    assert_eq!(
        link.board.writes_to(INPUT_POS),
        vec![(-5.0_f32).to_le_bytes().to_vec()]
    );
}

#[test]
fn move_readback_that_differs_is_not_confirmed() {
    let mut link = armed(commissioned());
    link.board
        .readback_override
        .insert(INPUT_POS, 4.0_f32.to_le_bytes().to_vec());
    link.submit(
        1,
        LinkCommand::MoveAbsolute { position_mm: 40.0 },
        Priority::Ordinary,
    );
    link.run_for(5);
    assert!(matches!(
        link.result(1),
        Some(CommandResult::NotConfirmed { .. })
    ));
}

#[test]
fn arm_is_rejected_on_any_error_word_or_missing_calibration() {
    for (endpoint, value) in [
        (AXIS_ERROR, 1),
        (MOTOR_ERROR, 1),
        (ENCODER_ERROR, 1),
        (CONTROLLER_ERROR, 1),
    ] {
        let mut board = FakeFibre::compatible();
        board.set_i32(endpoint, value);
        let mut link = verified(commissioned(), board);
        link.submit(1, LinkCommand::Arm, Priority::Ordinary);
        link.run_for(2);
        assert!(
            matches!(link.result(1), Some(CommandResult::Rejected { ref reason }) if reason.contains("error words")),
            "{endpoint}: {:?}",
            link.result(1)
        );
        assert_eq!(link.board.requested_states(), vec![1]);
    }
    for endpoint in [MOTOR_IS_CALIBRATED, ENCODER_IS_READY] {
        let mut board = FakeFibre::compatible();
        board.set_bool(endpoint, false);
        let mut link = verified(commissioned(), board);
        link.submit(1, LinkCommand::Arm, Priority::Ordinary);
        link.run_for(2);
        assert!(matches!(
            link.result(1),
            Some(CommandResult::Rejected { .. })
        ));
        assert_eq!(link.board.requested_states(), vec![1]);
    }
}

#[test]
fn home_accepts_only_the_minimum_endstop_bit_while_the_endstop_is_pressed() {
    let mut board = FakeFibre::compatible();
    board.set_i32(AXIS_ERROR, 0x1000);
    board.set_bool(MIN_ENDSTOP_STATE, true);
    let mut link = verified(commissioned(), board);
    link.submit(1, LinkCommand::Arm, Priority::Ordinary);
    link.submit(2, LinkCommand::Home, Priority::Ordinary);
    link.run_for(5);
    assert!(matches!(
        link.result(1),
        Some(CommandResult::Rejected { .. })
    ));
    assert_eq!(link.result(2), Some(CommandResult::Completed));
    assert_eq!(link.board.requested_states(), vec![1, 11]);

    for (error, pressed) in [(0x1001, true), (0x1000, false), (0x2000, true)] {
        let mut board = FakeFibre::compatible();
        board.set_i32(AXIS_ERROR, error);
        board.set_bool(MIN_ENDSTOP_STATE, pressed);
        let mut link = verified(commissioned(), board);
        link.submit(1, LinkCommand::Home, Priority::Ordinary);
        link.run_for(2);
        assert!(
            matches!(link.result(1), Some(CommandResult::Rejected { .. })),
            "0x{error:x} pressed {pressed}"
        );
        assert_eq!(link.board.requested_states(), vec![1]);
    }
}

#[test]
fn unknown_state_number_never_counts_as_idle_or_armed() {
    let mut board = FakeFibre::compatible();
    board.set_i32(CURRENT_STATE, 99);
    board.ignore_state_requests.push(1);
    let mut gate = Harness::new(commissioned(), board);
    gate.connect();
    gate.run_for(50);
    assert_eq!(gate.engine.status().phase, LinkPhase::Connecting);

    let mut link = armed(commissioned());
    link.board.ignore_state_requests.push(1);
    link.board.set_i32(CURRENT_STATE, 99);
    link.run_for(25);
    let sample = link.engine.fresh_sample(link.now).unwrap();
    let state = sample.fresh_values(link.now).unwrap().axis_state;
    assert!(!state.is_idle() && !state.is_armed(), "{state:?}");
    link.submit(
        1,
        LinkCommand::MoveAbsolute { position_mm: 40.0 },
        Priority::Ordinary,
    );
    link.submit(2, LinkCommand::Disarm, Priority::Ordinary);
    link.run_for(150);
    assert!(matches!(
        link.result(1),
        Some(CommandResult::Rejected { .. })
    ));
    assert!(matches!(
        link.result(2),
        Some(CommandResult::NotConfirmed { .. })
    ));
    assert!(
        status_has_fault(&link),
        "an unconfirmed disarm faults the link"
    );
}

#[test]
fn clear_errors_is_confirmed_by_the_next_sample() {
    let mut link = verified(commissioned(), FakeFibre::compatible());
    link.submit(1, LinkCommand::ClearErrors, Priority::Ordinary);
    link.run_for(1);
    assert!(link.result(1).is_none());
    link.run_for(30);
    assert_eq!(link.result(1), Some(CommandResult::Completed));
    assert_eq!(link.board.writes_to(CLEAR_ERRORS), vec![Vec::<u8>::new()]);
}

#[test]
fn transport_loss_retires_pending_work_with_transmitted_flags_and_reconnect_only_runs_the_gate() {
    let mut link = armed(commissioned());
    let _read = hold_next_telemetry_read(&mut link);
    link.engine.submit(
        link.now,
        Ticket(1),
        LinkCommand::MoveAbsolute { position_mm: 8.0 },
        Priority::Ordinary,
    );
    link.engine
        .submit(link.now, Ticket(2), LinkCommand::Disarm, Priority::Urgent);
    let _urgent = link.transmit_only();
    link.engine.submit(
        link.now,
        Ticket(3),
        LinkCommand::ClearErrors,
        Priority::Ordinary,
    );
    link.drain_events();
    let writes_before = link.board.writes.len();

    link.engine.transport_lost(link.now, "fake cable cut");
    link.drain_events();

    assert_eq!(link.engine.status().phase, LinkPhase::Disconnected);
    assert_eq!(
        link.result(1),
        Some(CommandResult::Retired { transmitted: false })
    );
    assert_eq!(
        link.result(2),
        Some(CommandResult::Retired { transmitted: true })
    );
    assert_eq!(
        link.result(3),
        Some(CommandResult::Retired { transmitted: false })
    );
    link.submit(4, LinkCommand::Disarm, Priority::Urgent);
    assert!(matches!(
        link.result(4),
        Some(CommandResult::Rejected { .. })
    ));

    let wire_before = link.wire.len();
    link.connect();
    link.run_for(50);
    assert_eq!(link.engine.status().epoch, 2);
    assert_eq!(link.engine.status().phase, LinkPhase::Verified);
    let writes: Vec<u16> = link.board.writes[writes_before..]
        .iter()
        .map(|(endpoint, _)| *endpoint)
        .collect();
    assert_eq!(
        writes,
        vec![
            REQUESTED_STATE,
            CURRENT_LIM,
            CONTROLLER_VEL_LIMIT,
            TRAP_VEL_LIMIT,
            TRAP_ACCEL_LIMIT,
            TRAP_DECEL_LIMIT,
        ]
    );
    assert_eq!(link.board.requested_states().last(), Some(&1));
    assert_eq!(link.requests_to(0).len(), 2 * 1273);
    assert!(link.wire[wire_before..].iter().all(|(urgent, _)| !urgent));
}

#[test]
fn no_request_is_ever_transmitted_twice() {
    let mut link = armed(commissioned());
    link.board.fault_next(POS_ESTIMATE, ReplyFault::Drop);
    link.run_for(200);
    link.board.fault_next(REQUESTED_STATE, ReplyFault::Drop);
    link.submit(1, LinkCommand::Disarm, Priority::Urgent);
    link.run_for(200);
    link.connect();
    link.run_for(200);

    assert!(matches!(
        link.result(1),
        Some(CommandResult::OutcomeUnknown { .. })
    ));
    // A number is sent again only after the previous request with it was
    // answered; an unanswered request is never resent.
    let mut outstanding = std::collections::HashSet::new();
    for (sent, sequence) in &link.trace {
        if *sent {
            assert!(
                outstanding.insert(*sequence),
                "sequence {sequence:#06x} sent again while unanswered"
            );
        } else {
            outstanding.remove(sequence);
        }
    }
    // The two dropped requests stay unanswered and were never sent again.
    assert!(outstanding.len() >= 2, "{outstanding:?}");
}
