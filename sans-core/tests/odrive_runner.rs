//! Link runner behaviour through the packet-transport seam: real threads,
//! fake Fibre device. No hardware is touched.

mod support;

use std::fs;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use sans_core::{
    prepare_machine_profile, spawn_link, CommandResult, LinkCommand, LinkConfig, LinkEvent,
    LinkHandle, LinkPhase, LinkRunner, Priority, Ticket,
};
use support::fake_fibre::*;
use support::GATE_WAIT;

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
position_tolerance_mm = 0.5
stationary_velocity_mm_s = 0.2
move_timeout_ms = 2000
home_timeout_ms = 3000
cancel_timeout_ms = 1000
touchdown_grace_ms = 100
touchdown_dwell_ms = 100
touchdown_no_progress_mm = 1.0
touchdown_saturation_percent = 90.0
touchdown_hold_depth_mm = 2.0
"#;

/// Widest timing the profile bounds allow, so emulated CI is not timing-bound.
fn config() -> LinkConfig {
    config_with_open_ms(60_000)
}

fn config_with_open_ms(device_open_ms: u64) -> LinkConfig {
    let device_open = format!("device_open_ms = {device_open_ms}");
    let profile = [
        ("poll_interval_ms = 20", "poll_interval_ms = 100"),
        ("telemetry_max_age_ms = 100", "telemetry_max_age_ms = 250"),
        ("request_deadline_ms = 100", "request_deadline_ms = 250"),
        ("urgent_deadline_ms = 50", "urgent_deadline_ms = 100"),
        ("device_open_ms = 5000", device_open.as_str()),
    ]
    .iter()
    .fold(format!("{VALID_PROFILE}{LIFT_AXIS}"), |text, (from, to)| {
        let replaced = text.replacen(from, to, 1);
        assert_ne!(replaced, text, "fixture no longer contains {from:?}");
        replaced
    });
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sans.toml");
    fs::write(&path, profile).unwrap();
    LinkConfig::from_profile(prepare_machine_profile(Some(&path)).unwrap().profile()).unwrap()
}

struct Link {
    handle: LinkHandle,
    _runner: LinkRunner,
    events: Receiver<LinkEvent>,
    seen: Vec<LinkEvent>,
}

impl Link {
    fn start(opener: &FakeOpener) -> Self {
        Self::start_with(opener, config())
    }

    fn start_with(opener: &FakeOpener, config: LinkConfig) -> Self {
        let (sender, events) = mpsc::channel();
        let (handle, runner) = spawn_link(config, Box::new(opener.clone()), sender).unwrap();
        Self {
            handle,
            _runner: runner,
            events,
            seen: Vec::new(),
        }
    }

    fn wait_for(&mut self, mut predicate: impl FnMut(&LinkEvent) -> bool) -> LinkEvent {
        let deadline = Instant::now() + GATE_WAIT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = self
                .events
                .recv_timeout(remaining)
                .unwrap_or_else(|error| panic!("{error}; seen {:#?}", self.seen.last()));
            self.seen.push(event.clone());
            if predicate(&event) {
                return event;
            }
        }
    }

    fn submit(&self, command: LinkCommand, priority: Priority) -> Ticket {
        self.handle
            .submit(command, priority)
            .expect("the link is open")
    }

    /// Arm, then hold an urgent ClearErrors in the writer so later packets
    /// queue behind it. Returns the ClearErrors ticket.
    fn stall_writer(&mut self, opener: &FakeOpener) -> Ticket {
        self.wait_for_phase(1, LinkPhase::Verified);
        let arm = self.submit(LinkCommand::Arm, Priority::Ordinary);
        assert_eq!(self.result(arm), CommandResult::Completed);
        // Act right after a sample so the ordinary slot is free.
        self.wait_for(|event| matches!(event, LinkEvent::Telemetry(_)));
        opener.block_writes();
        let held = opener.writes_held.load(Ordering::SeqCst);
        let clear = self.submit(LinkCommand::ClearErrors, Priority::Urgent);
        let deadline = Instant::now() + Duration::from_secs(5);
        while opener.writes_held.load(Ordering::SeqCst) == held {
            assert!(
                Instant::now() < deadline,
                "the writer never picked up the urgent packet"
            );
            std::thread::yield_now();
        }
        clear
    }

    fn wait_for_phase(&mut self, epoch: u64, phase: LinkPhase) {
        self.wait_for(|event| {
            matches!(event, LinkEvent::Status(status) if status.epoch == epoch && status.phase == phase)
        });
    }

    fn result(&mut self, ticket: Ticket) -> CommandResult {
        let previous = self.seen.iter().find_map(|event| match event {
            LinkEvent::CommandResult {
                ticket: t, result, ..
            } if *t == ticket => Some(result.clone()),
            _ => None,
        });
        if let Some(result) = previous {
            return result;
        }
        match self.wait_for(
            |event| matches!(event, LinkEvent::CommandResult { ticket: t, .. } if *t == ticket),
        ) {
            LinkEvent::CommandResult { result, .. } => result,
            _ => unreachable!(),
        }
    }
}

#[test]
fn urgent_disarm_withdraws_a_move_still_waiting_in_the_writer() {
    let opener = FakeOpener::new(FakeFibre::compatible());
    let mut link = Link::start(&opener);
    let clear = link.stall_writer(&opener);
    // The move's write now waits in the writer queue behind the held packet.
    let movement = link.submit(
        LinkCommand::MoveAbsolute { position_mm: 40.0 },
        Priority::Ordinary,
    );
    let disarm = link.submit(LinkCommand::Disarm, Priority::Urgent);
    let move_result = link.result(movement);
    opener.release_writes();

    assert_eq!(move_result, CommandResult::Retired { transmitted: false });
    assert_eq!(link.result(disarm), CommandResult::Completed);
    assert_eq!(link.result(clear), CommandResult::Completed);
    let board = opener.board.lock().unwrap();
    assert!(
        board.writes_to(INPUT_POS).is_empty(),
        "a withdrawn move reached the board"
    );
    assert_eq!(board.requested_states(), vec![1, 8, 1]);
}

#[test]
fn reconnect_waits_for_the_previous_connection_to_release_the_device() {
    let opener = FakeOpener::new(FakeFibre::compatible());
    let mut link = Link::start(&opener);
    link.wait_for_phase(1, LinkPhase::Verified);

    link.handle.reconnect();
    link.wait_for_phase(2, LinkPhase::Verified);

    assert_eq!(opener.busy_refusals.load(Ordering::SeqCst), 0);
    assert_eq!(*opener.opens.lock().unwrap(), 2);
    assert_eq!(opener.board.lock().unwrap().requested_states(), vec![1, 1]);
}

#[test]
fn ended_connection_leaves_no_fresh_sample_behind() {
    let opener = FakeOpener::new(FakeFibre::compatible());
    let mut link = Link::start(&opener);
    link.wait_for_phase(1, LinkPhase::Verified);
    link.wait_for(|event| matches!(event, LinkEvent::Telemetry(_)));
    assert!(link.handle.fresh_sample(Instant::now()).is_some());

    *opener.cut.lock().unwrap() = true;
    link.wait_for_phase(1, LinkPhase::Disconnected);

    assert!(link.handle.fresh_sample(Instant::now()).is_none());
}

#[test]
fn move_stuck_in_the_writer_past_its_deadline_never_reaches_the_board() {
    let opener = FakeOpener::new(FakeFibre::compatible());
    let mut link = Link::start(&opener);
    let clear = link.stall_writer(&opener);
    let movement = link.submit(
        LinkCommand::MoveAbsolute { position_mm: 40.0 },
        Priority::Ordinary,
    );
    // Keep the writer stuck until the link has faulted and the move is retired.
    let move_result = link.result(movement);
    link.wait_for(|event| {
        matches!(event, LinkEvent::Status(status)
            if status.blockers.iter().any(|blocker| blocker.starts_with("link fault")))
    });
    opener.release_writes();

    assert_eq!(move_result, CommandResult::Retired { transmitted: false });
    assert!(matches!(
        link.result(clear),
        CommandResult::OutcomeUnknown { .. }
    ));
    // Disarm stays possible on the faulted link.
    let disarm = link.submit(LinkCommand::Disarm, Priority::Urgent);
    assert_eq!(link.result(disarm), CommandResult::Completed);
    let board = opener.board.lock().unwrap();
    assert!(
        board.writes_to(INPUT_POS).is_empty(),
        "a stale move reached the board"
    );
    assert_eq!(board.requested_states(), vec![1, 8, 1]);
}

#[test]
fn reopen_waits_until_the_old_device_halves_are_destroyed() {
    let opener = FakeOpener::new(FakeFibre::compatible());
    // Each half takes 100 ms to give up the claim in its destructor.
    *opener.claim_release_delay.lock().unwrap() = Duration::from_millis(100);
    let mut link = Link::start(&opener);
    link.wait_for_phase(1, LinkPhase::Verified);

    link.handle.reconnect();
    link.wait_for_phase(2, LinkPhase::Verified);

    assert_eq!(opener.busy_refusals.load(Ordering::SeqCst), 0);
}

#[test]
fn superseded_open_is_destroyed_before_the_next_open() {
    let opener = FakeOpener::new(FakeFibre::compatible());
    *opener.reap_delay.lock().unwrap() = Duration::from_millis(500);
    *opener.claim_release_delay.lock().unwrap() = Duration::from_millis(100);
    *opener.hold_open.0.lock().unwrap() = true;
    let mut link = Link::start(&opener);
    // Epoch 1's open has claimed the device and is still draining.
    let deadline = Instant::now() + Duration::from_secs(5);
    while opener.claimed.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "epoch 1 never claimed the device"
        );
        std::thread::yield_now();
    }

    link.handle.reconnect();
    link.wait_for(|event| matches!(event, LinkEvent::Status(status) if status.epoch == 2));
    // A third attempt supersedes another thread queued on the opener mutex.
    link.handle.reconnect();
    link.wait_for(|event| matches!(event, LinkEvent::Status(status) if status.epoch == 3));
    opener.release_open();
    link.wait_for_phase(3, LinkPhase::Verified);

    assert_eq!(opener.busy_refusals.load(Ordering::SeqCst), 0);
    assert_eq!(*opener.opens.lock().unwrap(), 2);
}

#[test]
fn disarm_supersedes_an_urgent_move_still_waiting_in_the_writer() {
    let opener = FakeOpener::new(FakeFibre::compatible());
    let mut link = Link::start(&opener);
    let clear = link.stall_writer(&opener);
    // An urgent move queues behind the stuck packet, then a disarm follows.
    let movement = link.submit(
        LinkCommand::MoveAbsolute { position_mm: 40.0 },
        Priority::Urgent,
    );
    let disarm = link.submit(LinkCommand::Disarm, Priority::Urgent);
    let move_result = link.result(movement);
    opener.release_writes();

    assert_eq!(move_result, CommandResult::Retired { transmitted: false });
    assert_eq!(link.result(disarm), CommandResult::Completed);
    let _ = link.result(clear);
    let board = opener.board.lock().unwrap();
    assert!(
        board.writes_to(INPUT_POS).is_empty(),
        "a superseded move reached the board"
    );
    assert_eq!(board.requested_states(), vec![1, 8, 1]);
}

#[test]
fn release_is_reported_only_after_pending_transfers_are_reaped() {
    let opener = FakeOpener::new(FakeFibre::compatible());
    // Longer than one shutdown slice, so the background reaper is exercised.
    *opener.reap_delay.lock().unwrap() = Duration::from_millis(250);
    let mut link = Link::start(&opener);
    link.wait_for_phase(1, LinkPhase::Verified);

    link.handle.reconnect();
    link.wait_for_phase(2, LinkPhase::Verified);

    assert_eq!(opener.busy_refusals.load(Ordering::SeqCst), 0);
}

#[test]
fn release_barrier_survives_an_attempt_that_gave_up_waiting() {
    // An empty descriptor blocks each gate at once while the transport stays open.
    let opener = FakeOpener::new(FakeFibre::compatible().with_descriptor(b"[]".to_vec()));
    // Releasing takes far longer than one 300 ms gate budget.
    *opener.claim_release_delay.lock().unwrap() = Duration::from_millis(1_000);
    let mut link = Link::start_with(&opener, config_with_open_ms(300));
    link.wait_for_phase(1, LinkPhase::Blocked);

    link.handle.reconnect();
    link.wait_for_phase(2, LinkPhase::Blocked);
    // The very next attempt must wait on the same barrier, not open at once.
    link.handle.reconnect();
    link.wait_for_phase(3, LinkPhase::Blocked);
    assert_eq!(opener.busy_refusals.load(Ordering::SeqCst), 0);
    assert_eq!(*opener.opens.lock().unwrap(), 1);

    let deadline = Instant::now() + Duration::from_secs(5);
    while opener.claimed.load(Ordering::SeqCst) != 0 {
        assert!(
            Instant::now() < deadline,
            "the old connection never released"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    link.handle.reconnect();
    link.wait_for(|event| {
        matches!(event, LinkEvent::Status(status) if status.epoch == 4 && status.discovery.is_some())
    });
    assert_eq!(opener.busy_refusals.load(Ordering::SeqCst), 0);
    assert_eq!(*opener.opens.lock().unwrap(), 2);
}

#[test]
fn read_failure_while_draining_fails_the_open_and_releases_the_device() {
    let opener = FakeOpener::new(FakeFibre::compatible());
    opener.cut_on_open.store(true, Ordering::SeqCst);
    let mut link = Link::start(&opener);
    let LinkEvent::Status(status) = link.wait_for(|event| {
        matches!(event, LinkEvent::Status(status)
            if status.blockers.iter().any(|blocker| blocker.contains("fake cable cut")))
    }) else {
        unreachable!()
    };
    assert_eq!(status.epoch, 1);
    assert!(
        status.discovery.is_none(),
        "the gate must not start on a connection whose drain failed"
    );
    let deadline = Instant::now() + GATE_WAIT;
    while opener.claimed.load(Ordering::SeqCst) != 0 {
        assert!(Instant::now() < deadline, "the failed open kept the device");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn stopped_runner_rejects_commands_and_leaves_no_fresh_sample() {
    let opener = FakeOpener::new(FakeFibre::compatible());
    let mut link = Link::start(&opener);
    link.wait_for_phase(1, LinkPhase::Verified);
    link.wait_for(|event| matches!(event, LinkEvent::Telemetry(_)));
    assert!(link.handle.fresh_sample(Instant::now()).is_some());

    drop(link._runner);

    let deadline = Instant::now() + GATE_WAIT;
    while link
        .handle
        .submit(LinkCommand::Disarm, Priority::Urgent)
        .is_ok()
    {
        assert!(Instant::now() < deadline, "the runner never stopped");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        link.handle.submit(LinkCommand::Disarm, Priority::Urgent),
        Err(sans_core::LinkClosed)
    );
    assert!(link.handle.fresh_sample(Instant::now()).is_none());
}
