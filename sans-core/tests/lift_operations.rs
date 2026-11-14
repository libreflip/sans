//! Lift-operations seam: requests, link events and time in; link commands and
//! typed events out.
//!
//! `LiftOperations` runs against the real link engine and the fake Fibre
//! board. Time is a parameter, so there are no threads, sleeps or clock fake.
//! Tests script the board's position, velocity, current, state and errors.
//! These are fake-backed host results against upstream semantics, not
//! hardware evidence.

mod support;

use std::fs;
use std::time::{Duration, Instant};

use sans_core::{
    prepare_machine_profile, LiftDirection, LiftEvent, LiftFault, LiftOperations, LiftOutcome,
    LiftRejection, LiftRequest, LinkCommand, LinkConfig, LinkEngine, LinkEvent, OperationId,
    PageTurnLeg, Priority, StopCause, StopControl, StopEvidence, StopVerdict, Ticket,
    TouchdownHold, VacuumEvidence,
};
use support::fake_fibre::*;

use LiftRequest::{Arm, Home, MoveTo, PageTurnLeg as Leg, Touchdown};
use VacuumEvidence::{ConfirmedOff as Off, NotConfirmedOff as Unknown};

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
/// 50% of the 4 A commissioned limit.
const PRESS_A: f32 = 2.0;

struct Rig {
    engine: LinkEngine,
    board: FakeFibre,
    ops: LiftOperations,
    control: StopControl,
    /// The next `ALL OFF` replies; `Ok` once the script runs out.
    relay_replies: Vec<Result<(), String>>,
    /// The wait each `ALL OFF` was given.
    relay_calls: Vec<Duration>,
    now: Instant,
    tickets: u64,
    sign: f32,
    /// Every packet transmitted, with its urgent flag.
    wire: Vec<(bool, Received)>,
    link_events: Vec<(Instant, LinkEvent)>,
    events: Vec<LiftEvent>,
}

/// The submission port: allocates tickets and submits to the engine.
fn port<'a>(
    engine: &'a mut LinkEngine,
    tickets: &'a mut u64,
    now: Instant,
) -> impl FnMut(LinkCommand, Priority) -> Option<Ticket> + 'a {
    move |command, priority| {
        *tickets += 1;
        engine.submit(now, Ticket(*tickets), command, priority);
        Some(Ticket(*tickets))
    }
}

impl Rig {
    fn new(lift_axis: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sans.toml");
        fs::write(&path, format!("{VALID_PROFILE}{lift_axis}")).unwrap();
        let prepared = prepare_machine_profile(Some(&path)).unwrap();
        let profile = prepared.profile();
        let sign = match profile.odrive.lift_axis.as_ref().unwrap().direction {
            LiftDirection::Normal => 1.0,
            LiftDirection::Reversed => -1.0,
        };
        let mut rig = Self {
            engine: LinkEngine::new(LinkConfig::from_profile(profile).unwrap(), 0x1234),
            board: FakeFibre::compatible(),
            ops: LiftOperations::new(profile),
            control: StopControl::new(profile),
            relay_replies: Vec::new(),
            relay_calls: Vec::new(),
            now: Instant::now(),
            tickets: 0,
            sign,
            wire: Vec::new(),
            link_events: Vec::new(),
            events: Vec::new(),
        };
        rig.connect();
        rig
    }

    /// Start a connection attempt, as a reconnect does, and run its gate.
    fn connect(&mut self) {
        self.engine.begin_attempt(self.now);
        self.settle();
        self.engine.transport_opened(self.now, "316536713335");
        self.settle();
        self.run_for(5);
    }

    /// Exchange packets with the board and feed link events until quiet.
    fn settle(&mut self) {
        for _ in 0..50_000 {
            let mut replies = Vec::new();
            while let Some(transmit) = self.engine.take_transmit() {
                if !transmit.claim_for_send(self.now) {
                    continue;
                }
                self.wire
                    .push((transmit.urgent, decode_request(&transmit.packet)));
                replies.extend(self.board.handle(&transmit.packet));
            }
            let mut fed = false;
            while let Some(event) = self.engine.take_event() {
                fed = true;
                let mut link = port(&mut self.engine, &mut self.tickets, self.now);
                self.ops.handle_link_event(self.now, &event, &mut link);
                self.link_events.push((self.now, event));
            }
            while let Some(event) = self.ops.take_event() {
                self.control.note(&event);
                self.events.push(event);
            }
            self.observe();
            if replies.is_empty() && !fed {
                return;
            }
            for reply in replies {
                self.engine.receive(self.now, &reply);
            }
        }
        panic!("the rig kept exchanging without time passing");
    }

    fn observe(&mut self) {
        let (calls, replies) = (&mut self.relay_calls, &mut self.relay_replies);
        let mut relays = relays(calls, replies);
        self.control.observe(self.now, &self.ops, &mut relays);
    }

    /// A cross-board Stop through `StopControl`.
    fn machine_stop(&mut self) {
        let (calls, replies) = (&mut self.relay_calls, &mut self.relay_replies);
        let mut relays = relays(calls, replies);
        let mut link = port(&mut self.engine, &mut self.tickets, self.now);
        let cause = StopCause::Requested("touchscreen".into());
        self.control
            .stop(self.now, cause, &mut self.ops, &mut link, &mut relays);
        drop((link, relays));
        self.settle();
    }

    fn clear_stop(&mut self) -> Result<OperationId, sans_core::ClearRefused> {
        let mut link = port(&mut self.engine, &mut self.tickets, self.now);
        let result = self.control.clear(self.now, &mut self.ops, &mut link);
        drop(link);
        self.settle();
        result
    }

    fn verdict(&self) -> Option<StopVerdict> {
        self.control.status().map(|status| status.verdict)
    }

    fn run_for(&mut self, ms: u64) {
        for _ in 0..ms {
            self.now += Duration::from_millis(1);
            self.engine.poll(self.now);
            self.settle();
            self.ops.poll(
                self.now,
                &mut port(&mut self.engine, &mut self.tickets, self.now),
            );
            self.settle();
        }
    }

    /// Step 1 ms at a time until `done`, at most `ms`.
    fn run_until(&mut self, ms: u64, mut done: impl FnMut(&Self) -> bool) {
        for _ in 0..ms {
            if done(self) {
                return;
            }
            self.run_for(1);
        }
        assert!(done(self), "condition not met within {ms} ms");
    }

    fn start(
        &mut self,
        request: LiftRequest,
        vacuum: VacuumEvidence,
    ) -> Result<OperationId, LiftRejection> {
        let mut link = port(&mut self.engine, &mut self.tickets, self.now);
        let result = self.ops.start(self.now, request, vacuum, &mut link);
        drop(link);
        self.settle();
        result
    }

    fn cancel(&mut self, id: OperationId) {
        let mut link = port(&mut self.engine, &mut self.tickets, self.now);
        self.ops.cancel(self.now, id, &mut link);
        drop(link);
        self.settle();
    }

    fn stop(&mut self) {
        let mut link = port(&mut self.engine, &mut self.tickets, self.now);
        self.ops
            .stop(StopCause::Requested("test".into()), &mut link);
        drop(link);
        self.settle();
    }

    fn clear_fault(&mut self) -> Result<OperationId, LiftRejection> {
        let mut link = port(&mut self.engine, &mut self.tickets, self.now);
        let result = self.ops.clear_fault(self.now, &mut link);
        drop(link);
        self.settle();
        result
    }

    fn feed(&mut self, event: LinkEvent) {
        let mut link = port(&mut self.engine, &mut self.tickets, self.now);
        self.ops.handle_link_event(self.now, &event, &mut link);
        drop(link);
        self.settle();
    }

    /// Script the axis in Sans units.
    fn axis(&mut self, position_mm: f32, velocity_mm_s: f32, iq_a: f32) {
        self.board
            .set_f32(POS_ESTIMATE, self.sign * position_mm / 8.0);
        self.board
            .set_f32(VEL_ESTIMATE, self.sign * velocity_mm_s / 8.0);
        // Native current is signed with motor turns, like position.
        self.board.set_f32(IQ_MEASURED, self.sign * iq_a);
    }

    fn trajectory_done(&mut self) {
        self.board.set_bool(TRAJECTORY_DONE, true);
    }

    fn finishes(&self, id: OperationId) -> Vec<LiftOutcome> {
        self.events
            .iter()
            .filter_map(|event| match event {
                LiftEvent::Finished {
                    id: finished,
                    outcome,
                    ..
                } if *finished == id => Some(outcome.clone()),
                _ => None,
            })
            .collect()
    }

    fn outcome(&self, id: OperationId) -> Option<LiftOutcome> {
        let finishes = self.finishes(id);
        assert!(finishes.len() <= 1, "{finishes:?}");
        finishes.into_iter().next()
    }

    /// Every `input_pos` write, in millimetres.
    fn moves(&self) -> Vec<f32> {
        self.f32_writes(INPUT_POS)
            .into_iter()
            .map(|turns| self.sign * turns * 8.0)
            .collect()
    }

    fn f32_writes(&self, endpoint: u16) -> Vec<f32> {
        self.board
            .writes_to(endpoint)
            .into_iter()
            .map(|payload| f32::from_le_bytes(payload.try_into().unwrap()))
            .collect()
    }

    fn write_order_since(&self, mark: usize) -> Vec<u16> {
        self.board.writes[mark..]
            .iter()
            .map(|(id, _)| *id)
            .collect()
    }

    /// The latest write on the wire, with its urgent flag.
    fn last_write(&self) -> &(bool, Received) {
        self.wire
            .iter()
            .rev()
            .find(|(_, request)| !request.payload.is_empty())
            .unwrap()
    }
}

/// The scripted Monospace `ALL OFF` port.
fn relays<'a>(
    calls: &'a mut Vec<Duration>,
    replies: &'a mut Vec<Result<(), String>>,
) -> impl FnMut(Duration) -> Result<(), String> + 'a {
    move |within| {
        calls.push(within);
        if replies.is_empty() {
            Ok(())
        } else {
            replies.remove(0)
        }
    }
}

fn close(left: f32, right: f32) -> bool {
    (left - right).abs() < 1e-3
}

/// Home from idle: the run ends idle at 0 with clean evidence.
fn finish_home(rig: &mut Rig) {
    rig.board.set_i32(CURRENT_STATE, 1);
    rig.board.set_bool(IS_HOMED, true);
    rig.axis(0.0, 0.0, 0.0);
    rig.trajectory_done();
}

fn homed() -> Rig {
    homed_with(LIFT_AXIS)
}

fn homed_with(lift_axis: &str) -> Rig {
    let mut rig = Rig::new(lift_axis);
    home(&mut rig);
    rig
}

/// Run a Home from idle to clean completion.
fn home(rig: &mut Rig) {
    let id = rig.start(Home, Unknown).unwrap();
    rig.run_for(5);
    finish_home(rig);
    rig.run_for(25);
    assert_eq!(rig.outcome(id), Some(LiftOutcome::Completed(None)));
}

fn arm(rig: &mut Rig) {
    let id = rig.start(Arm, Unknown).unwrap();
    rig.run_for(25);
    assert_eq!(rig.outcome(id), Some(LiftOutcome::Completed(None)));
}

fn armed() -> Rig {
    armed_with(LIFT_AXIS)
}

fn armed_with(lift_axis: &str) -> Rig {
    let mut rig = homed_with(lift_axis);
    arm(&mut rig);
    rig
}

/// Start a Touchdown, descend past the grace and stall at `contact_mm`
/// until the hold move is written.
fn descend_to_contact(rig: &mut Rig, request: LiftRequest, contact_mm: f32) -> OperationId {
    let id = rig.start(request, Off).unwrap();
    let moves = rig.moves().len();
    rig.axis(contact_mm - 50.0, 5.0, PRESS_A);
    rig.run_for(150);
    rig.axis(contact_mm, 0.0, PRESS_A);
    rig.run_until(300, |rig| rig.moves().len() == moves + 1);
    id
}

fn touched_down() -> Rig {
    let mut rig = armed();
    let id = descend_to_contact(
        &mut rig,
        Touchdown {
            post_turn_page_width_mm: None,
        },
        200.0,
    );
    rig.axis(200.4, 0.0, 1.9);
    rig.run_for(150);
    assert!(
        matches!(rig.outcome(id), Some(LiftOutcome::Completed(Some(_)))),
        "{:?}",
        rig.outcome(id)
    );
    rig
}

/// A completed Touchdown followed by an upward move that ends the hold,
/// leaving the press limit applied.
fn lifted_away() -> Rig {
    let mut rig = touched_down();
    let id = rig.start(MoveTo { position_mm: 100.0 }, Unknown).unwrap();
    arrive(&mut rig);
    assert_eq!(rig.outcome(id), Some(LiftOutcome::Completed(None)));
    assert_eq!(rig.ops.status().live_hold_mm, None);
    rig
}

/// Complete the move just written by arriving at its target.
fn arrive(rig: &mut Rig) {
    let target = *rig.moves().last().unwrap();
    rig.axis(target, 0.0, 0.0);
    rig.trajectory_done();
    rig.run_for(25);
}

// ----- 1. Lifecycle -------------------------------------------------------------

#[test]
fn an_operation_finishes_once_and_a_second_request_is_busy() {
    let mut rig = homed();

    rig.board.fault_next(REQUESTED_STATE, ReplyFault::Hold);
    let arm = rig.start(Arm, Unknown).unwrap();
    assert_eq!(rig.start(Home, Unknown), Err(LiftRejection::Busy));
    for reply in rig.board.take_held() {
        rig.engine.receive(rig.now, &reply);
    }
    rig.run_for(50);

    assert!(arm > OperationId(1));
    assert_eq!(rig.finishes(arm), vec![LiftOutcome::Completed(None)]);
    assert_eq!(rig.board.requested_states(), vec![1, 11, 8]);
}

#[test]
fn stale_and_redelivered_results_and_samples_change_nothing() {
    let mut rig = armed();
    let arm_ticket = Ticket(rig.tickets);
    rig.board.fault_next(INPUT_POS, ReplyFault::Hold);
    let id = rig.start(MoveTo { position_mm: 50.0 }, Off).unwrap();
    let move_ticket = Ticket(rig.tickets);
    let epoch = rig.ops.status().epoch;
    let refused = |ticket, epoch| LinkEvent::CommandResult {
        ticket,
        epoch,
        command: LinkCommand::Arm,
        result: sans_core::CommandResult::Rejected {
            reason: "stale".into(),
        },
    };

    rig.feed(refused(move_ticket, epoch - 1));
    rig.feed(refused(arm_ticket, epoch));
    assert_eq!(rig.outcome(id), None);

    let mark = rig.link_events.len();
    for reply in rig.board.take_held() {
        rig.engine.receive(rig.now, &reply);
    }
    rig.settle();
    arrive(&mut rig);
    assert_eq!(rig.outcome(id), Some(LiftOutcome::Completed(None)));

    let status = rig.ops.status();
    let replay: Vec<_> = rig.link_events[mark..]
        .iter()
        .map(|(_, event)| event.clone())
        .collect();
    for event in replay {
        rig.feed(event);
    }
    assert_eq!(rig.finishes(id).len(), 1);
    assert_eq!(rig.ops.status(), status);
}

// ----- 2. Home ------------------------------------------------------------------

#[test]
fn home_success_grants_trust_and_leaves_the_axis_idle() {
    let mut rig = Rig::new(LIFT_AXIS);
    assert!(!rig.ops.status().position_trusted);

    let id = rig.start(Home, Unknown).unwrap();
    rig.run_for(5);
    assert_eq!(rig.write_order_since(6), vec![CURRENT_LIM, REQUESTED_STATE]);
    assert_eq!(rig.outcome(id), None, "entering homing is not completion");
    finish_home(&mut rig);
    rig.run_for(25);

    assert_eq!(rig.outcome(id), Some(LiftOutcome::Completed(None)));
    assert!(rig.ops.status().position_trusted);
    assert_eq!(rig.board.requested_states(), vec![1, 11], "Home never arms");
}

#[test]
fn idle_without_clean_evidence_is_home_failed() {
    let spoils: [fn(&mut Rig); 4] = [
        |rig| rig.board.set_i32(AXIS_ERROR, 0x800),
        |rig| rig.axis(3.0, 0.0, 0.0),
        |rig| rig.board.set_bool(IS_HOMED, false),
        |rig| rig.board.set_bool(ENCODER_IS_READY, false),
    ];
    for spoil in spoils {
        let mut rig = Rig::new(LIFT_AXIS);
        let id = rig.start(Home, Unknown).unwrap();
        rig.run_for(5);
        finish_home(&mut rig);
        spoil(&mut rig);
        rig.run_for(25);

        assert!(
            matches!(
                rig.outcome(id),
                Some(LiftOutcome::Fault(LiftFault::HomeFailed(_)))
            ),
            "{:?}",
            rig.outcome(id)
        );
        let status = rig.ops.status();
        assert_eq!(status.latch, None);
        assert!(!status.position_trusted);
    }
}

#[test]
fn cancelled_home_disarms_waits_for_stationary_idle_and_grants_no_trust() {
    let mut rig = Rig::new(LIFT_AXIS);
    let id = rig.start(Home, Unknown).unwrap();
    rig.run_for(5);
    rig.axis(0.0, 1.0, 0.0);

    rig.cancel(id);
    let (urgent, request) = rig.last_write();
    assert!(*urgent);
    assert_eq!(request.endpoint, REQUESTED_STATE);
    assert_eq!(request.payload, 1_i32.to_le_bytes().to_vec());
    rig.run_for(50);
    assert_eq!(rig.outcome(id), None, "idle but still moving");

    rig.axis(4.0, 0.0, 0.0);
    rig.board.set_bool(IS_HOMED, true);
    rig.run_for(25);
    assert_eq!(
        rig.outcome(id),
        Some(LiftOutcome::Cancelled { position_mm: 4.0 })
    );
    let status = rig.ops.status();
    assert!(!status.position_trusted);
    assert_eq!(status.latch, None);

    let retry = rig.start(Home, Unknown).unwrap();
    assert_ne!(retry, id);
}

#[test]
fn home_timeout_becomes_stop() {
    let mut rig = Rig::new(LIFT_AXIS);
    let id = rig.start(Home, Unknown).unwrap();
    rig.run_for(3100);

    assert!(
        matches!(
            rig.outcome(id),
            Some(LiftOutcome::Fault(LiftFault::Stopped(
                StopCause::Uncertain(_)
            )))
        ),
        "{:?}",
        rig.outcome(id)
    );
    assert!(rig.ops.status().latch.is_some());
    assert_eq!(rig.board.requested_states().last(), Some(&1));
}

// ----- 3. Target rejection ------------------------------------------------------

#[test]
fn invalid_targets_are_rejected_before_anything_is_sent() {
    let leg = |leg, page_width_mm| Leg { leg, page_width_mm };
    let cases = [
        (
            MoveTo {
                position_mm: f32::NAN,
            },
            Off,
            "non-finite",
        ),
        (MoveTo { position_mm: 300.5 }, Off, "past maximum"),
        (MoveTo { position_mm: -0.5 }, Off, "past minimum"),
        (
            MoveTo { position_mm: 250.0 },
            Unknown,
            "descent with vacuum",
        ),
        (leg(PageTurnLeg::ToWiggleStart, f32::NAN), Off, "width"),
        (leg(PageTurnLeg::ToBlowerOn, 99.0), Off, "narrow"),
        (
            leg(PageTurnLeg::ToFinalLift, 350.0),
            Off,
            "leg past minimum",
        ),
        (
            leg(PageTurnLeg::Wiggle, 150.0),
            Unknown,
            "wiggle off anchor",
        ),
        (
            Touchdown {
                post_turn_page_width_mm: None,
            },
            Unknown,
            "touchdown vacuum",
        ),
        (
            Touchdown {
                post_turn_page_width_mm: Some(351.0),
            },
            Off,
            "touchdown width",
        ),
    ];
    let mut rig = touched_down();
    let writes = rig.board.writes.len();
    let mut rejections = Vec::new();
    for (request, vacuum, name) in cases {
        let result = rig.start(request, vacuum);
        assert!(result.is_err(), "{name}: {result:?}");
        rejections.push(result.unwrap_err());
    }
    use LiftRejection::*;
    assert!(matches!(
        rejections[..],
        [
            InvalidTarget(_),
            InvalidTarget(_),
            InvalidTarget(_),
            VacuumNotConfirmedOff,
            InvalidPageWidth(_),
            InvalidPageWidth(_),
            InvalidTarget(_),
            NotAtWiggleStart,
            VacuumNotConfirmedOff,
            InvalidPageWidth(_),
        ]
    ));
    rig.run_for(25);
    assert_eq!(rig.board.writes.len(), writes, "nothing reached the board");

    let mut untrusted = Rig::new(LIFT_AXIS);
    assert_eq!(
        untrusted.start(MoveTo { position_mm: 10.0 }, Off),
        Err(PositionUntrusted)
    );
    assert_eq!(
        untrusted.start(
            Touchdown {
                post_turn_page_width_mm: None
            },
            Off
        ),
        Err(PositionUntrusted)
    );
    assert_eq!(
        untrusted.start(leg(PageTurnLeg::ToVacuumOff, 150.0), Off),
        Err(NoTouchdownPosition)
    );
    assert!(untrusted.moves().is_empty());
}

// ----- 4. Moves and cancel ------------------------------------------------------

#[test]
fn a_move_sets_its_limit_first_and_completes_only_on_later_evidence() {
    let mut rig = lifted_away();
    let mark = rig.board.writes.len();
    // The board already reads at the target with its trajectory done, and a
    // telemetry cycle is in progress when the move is requested.
    rig.axis(10.0, 0.0, 0.0);
    rig.board.readback_override.insert(TRAJECTORY_DONE, vec![1]);
    rig.board.fault_next(POS_ESTIMATE, ReplyFault::Hold);
    rig.run_until(25, |rig| {
        rig.board
            .requests
            .last()
            .is_some_and(|request| request.endpoint == POS_ESTIMATE)
    });

    let id = rig.start(MoveTo { position_mm: 10.0 }, Unknown).unwrap();
    for reply in rig.board.take_held() {
        rig.engine.receive(rig.now, &reply);
    }
    rig.settle();
    assert_eq!(rig.write_order_since(mark), vec![CURRENT_LIM, INPUT_POS]);
    assert_eq!(rig.f32_writes(CURRENT_LIM).last(), Some(&4.0));
    assert_eq!(
        rig.outcome(id),
        None,
        "a sample whose cycle began before the readback is not evidence"
    );

    rig.run_for(25);
    assert_eq!(rig.outcome(id), Some(LiftOutcome::Completed(None)));
}

#[test]
fn routine_cancel_stops_armed_with_no_idle_write_and_accepts_a_retry() {
    let mut rig = armed();
    let id = rig.start(MoveTo { position_mm: 100.0 }, Off).unwrap();
    rig.axis(40.0, 3.0, 0.5);
    rig.run_for(25);

    rig.cancel(id);
    let (urgent, request) = rig.last_write();
    assert!(*urgent);
    assert_eq!(request.endpoint, INPUT_POS);
    assert_eq!(rig.moves(), vec![100.0, 40.0]);
    rig.axis(41.5, 0.0, 0.0);
    rig.trajectory_done();
    rig.run_for(25);

    assert_eq!(
        rig.outcome(id),
        Some(LiftOutcome::Cancelled { position_mm: 41.5 })
    );
    assert_eq!(rig.board.requested_states(), vec![1, 11, 8]);
    let status = rig.ops.status();
    assert_eq!(status.latch, None);
    assert!(status.position_trusted);
    assert!(rig.start(MoveTo { position_mm: 100.0 }, Off).is_ok());
}

#[test]
fn cancel_whose_braking_envelope_crosses_travel_becomes_urgent_stop() {
    let mut rig = armed();
    let id = rig.start(MoveTo { position_mm: 299.5 }, Off).unwrap();
    // 2 mm/s: 0.3 mm over the 150 ms age and urgent deadline plus 1 mm braking.
    rig.axis(299.0, 2.0, 0.5);
    rig.run_for(25);

    rig.cancel(id);

    assert!(matches!(
        rig.outcome(id),
        Some(LiftOutcome::Fault(LiftFault::Stopped(_)))
    ));
    assert_eq!(rig.moves(), vec![299.5], "no cancel move");
    let (urgent, request) = rig.last_write();
    assert!(*urgent);
    assert_eq!(request.endpoint, REQUESTED_STATE);
}

#[test]
fn cancel_without_stationary_evidence_by_its_timeout_becomes_stop() {
    let mut rig = armed();
    let id = rig.start(MoveTo { position_mm: 100.0 }, Off).unwrap();
    rig.axis(40.0, 3.0, 0.5);
    rig.run_for(25);
    rig.cancel(id);

    rig.run_for(990);
    assert_eq!(rig.outcome(id), None);
    rig.run_for(20);
    assert!(matches!(
        rig.outcome(id),
        Some(LiftOutcome::Fault(LiftFault::Stopped(
            StopCause::Uncertain(_)
        )))
    ));
}

// ----- 5. Stop and recovery -----------------------------------------------------

#[test]
fn stop_latches_and_recovery_is_clear_then_home_then_arm() {
    let mut rig = touched_down();

    rig.stop();
    let (urgent, request) = rig.last_write();
    assert!(*urgent);
    assert_eq!(request.endpoint, REQUESTED_STATE);
    let status = rig.ops.status();
    assert!(status.latch.is_some());
    assert!(!status.position_trusted);
    assert_eq!(status.touchdown_mm, None);
    assert_eq!(status.stop_evidence, Some(StopEvidence::Pending));
    rig.run_for(25);
    assert_eq!(
        rig.ops.status().stop_evidence,
        Some(StopEvidence::Stationary)
    );
    assert_eq!(rig.start(Arm, Off), Err(LiftRejection::Latched));
    assert_eq!(rig.start(Home, Off), Err(LiftRejection::Latched));

    let states = rig.board.requested_states().len();
    let clear = rig.clear_fault().unwrap();
    rig.run_for(50);
    assert_eq!(rig.outcome(clear), Some(LiftOutcome::Completed(None)));
    assert_eq!(rig.board.writes_to(CLEAR_ERRORS).len(), 1);
    assert_eq!(
        rig.board.requested_states().len(),
        states,
        "clear never arms"
    );
    let status = rig.ops.status();
    assert_eq!(status.latch, None);
    assert!(!status.position_trusted);
    assert_eq!(
        rig.start(MoveTo { position_mm: 10.0 }, Off),
        Err(LiftRejection::PositionUntrusted)
    );

    home(&mut rig);
    arm(&mut rig);
}

#[test]
fn reconnect_mid_operation_gives_one_fault_and_no_writes_beyond_the_gate() {
    let mut rig = armed();
    let id = rig.start(MoveTo { position_mm: 100.0 }, Off).unwrap();
    rig.axis(40.0, 3.0, 0.5);
    rig.run_for(25);
    let mark = rig.board.writes.len();

    rig.connect();
    rig.run_for(100);

    assert_eq!(rig.finishes(id).len(), 1);
    assert!(matches!(
        rig.outcome(id),
        Some(LiftOutcome::Fault(LiftFault::Stopped(_)))
    ));
    assert_eq!(
        rig.write_order_since(mark),
        vec![
            REQUESTED_STATE,
            CURRENT_LIM,
            CONTROLLER_VEL_LIMIT,
            TRAP_VEL_LIMIT,
            TRAP_ACCEL_LIMIT,
            TRAP_DECEL_LIMIT,
        ]
    );
    let status = rig.ops.status();
    assert_eq!(status.epoch, 2);
    assert!(!status.position_trusted);
    assert_eq!(status.stop_evidence, Some(StopEvidence::Unconfirmed));
}

#[test]
fn stop_leaves_urgently_while_a_telemetry_read_is_held() {
    let mut rig = armed();
    let id = rig.start(MoveTo { position_mm: 100.0 }, Off).unwrap();
    rig.board.fault_next(POS_ESTIMATE, ReplyFault::Hold);
    rig.run_for(25);
    let held = rig.board.take_held();
    assert_eq!(held.len(), 1);

    rig.stop();

    let (urgent, request) = rig.last_write();
    assert!(*urgent);
    assert_eq!(request.endpoint, REQUESTED_STATE);
    assert_eq!(request.payload, 1_i32.to_le_bytes().to_vec());
    assert!(matches!(
        rig.outcome(id),
        Some(LiftOutcome::Fault(LiftFault::Stopped(
            StopCause::Requested(_)
        )))
    ));
}

// ----- 6. Touchdown contact and hold --------------------------------------------

#[test]
fn touchdown_detects_contact_after_grace_and_records_only_once_settled() {
    let mut rig = armed();
    let mark = rig.board.writes.len();
    let id = rig
        .start(
            Touchdown {
                post_turn_page_width_mm: None,
            },
            Off,
        )
        .unwrap();
    assert_eq!(rig.write_order_since(mark), vec![CURRENT_LIM, INPUT_POS]);
    assert_eq!(rig.f32_writes(CURRENT_LIM).last(), Some(&PRESS_A));
    assert_eq!(
        rig.moves(),
        vec![298.0],
        "travel maximum less the hold depth"
    );

    // Saturated and stationary during the 100 ms grace is not contact: a
    // window may open only when it ends, so 140 ms is short of its dwell.
    rig.axis(5.0, 0.0, PRESS_A);
    rig.run_for(140);
    // Saturated while descending is not contact.
    rig.axis(60.0, 5.0, PRESS_A);
    rig.run_for(200);
    assert_eq!(rig.moves().len(), 1);

    rig.axis(200.0, 0.0, PRESS_A);
    let set_at = rig.now;
    rig.run_until(300, |rig| rig.moves().len() == 2);
    let first_stalled = rig
        .link_events
        .iter()
        .find_map(|(_, event)| match event {
            LinkEvent::Telemetry(sample) if sample.taken_at() > set_at => Some(sample.taken_at()),
            _ => None,
        })
        .unwrap();
    assert_eq!(rig.now - first_stalled, Duration::from_millis(100));
    assert_eq!(rig.moves()[1], 202.0, "contact plus the hold depth");
    assert_eq!(rig.f32_writes(CURRENT_LIM).last(), Some(&PRESS_A));
    assert_eq!(rig.write_order_since(mark).last(), Some(&INPUT_POS));

    rig.axis(200.4, 0.0, 1.9);
    rig.run_for(90);
    assert_eq!(rig.outcome(id), None);
    assert_eq!(rig.ops.status().touchdown_mm, None);
    rig.run_for(30);
    assert_eq!(
        rig.outcome(id),
        Some(LiftOutcome::Completed(Some(TouchdownHold {
            position_mm: 200.4,
            press_current_a: PRESS_A,
            hold_target_mm: 202.0,
        })))
    );
    assert_eq!(rig.ops.status().touchdown_mm, Some(200.4));
}

#[test]
fn hold_that_reaches_its_target_unresisted_is_contact_not_held() {
    let mut rig = armed();
    let id = descend_to_contact(
        &mut rig,
        Touchdown {
            post_turn_page_width_mm: None,
        },
        200.0,
    );

    rig.axis(202.0, 0.0, 0.3);
    rig.trajectory_done();
    rig.run_for(25);
    // The routine cancel moves to the sampled position and settles.
    assert_eq!(rig.moves(), vec![298.0, 202.0, 202.0]);
    rig.trajectory_done();
    rig.run_for(25);

    assert_eq!(
        rig.outcome(id),
        Some(LiftOutcome::Fault(LiftFault::ContactNotHeld))
    );
    let status = rig.ops.status();
    assert_eq!(status.touchdown_mm, None);
    assert_eq!(status.latch, None);
    assert_eq!(rig.board.requested_states(), vec![1, 11, 8], "still armed");
}

// ----- 7. Touchdown bounds -------------------------------------------------------

/// Start a Touchdown, script the axis after the grace, and run the routine
/// cancel it ends with to a stationary finish.
fn touchdown_ending(
    rig: &mut Rig,
    position_mm: f32,
    velocity_mm_s: f32,
    iq_a: f32,
    ms: u64,
) -> OperationId {
    let request = Touchdown {
        post_turn_page_width_mm: None,
    };
    let id = rig.start(request, Off).unwrap();
    rig.axis(position_mm, velocity_mm_s, iq_a);
    rig.run_until(ms, |rig| rig.moves().len() == 2);
    rig.axis(position_mm, 0.0, iq_a);
    rig.trajectory_done();
    rig.run_for(25);
    id
}

#[test]
fn touchdown_ends_in_typed_faults_at_its_bounds() {
    use LiftFault::*;
    // Arrival at the descent limit, even saturated.
    let mut rig = armed();
    let exhausted = touchdown_ending(&mut rig, 297.6, 0.0, PRESS_A, 100);
    assert_eq!(rig.moves(), vec![298.0, 297.6]);
    // Permanent stiction at the start.
    let mut stuck = armed();
    let no_progress = touchdown_ending(&mut stuck, 0.3, 0.0, PRESS_A, 400);
    // Descending forever.
    let mut slow = armed();
    let timed_out = touchdown_ending(&mut slow, 50.0, 5.0, 0.5, 2100);

    for (rig, id, fault) in [
        (rig, exhausted, TravelExhausted),
        (stuck, no_progress, NoProgress),
        (slow, timed_out, TimedOut),
    ] {
        assert_eq!(rig.outcome(id), Some(LiftOutcome::Fault(fault)));
        assert_eq!(rig.ops.status().latch, None);
        assert_eq!(rig.board.requested_states(), vec![1, 11, 8]);
    }
}

#[test]
fn contact_too_close_to_travel_for_a_hold_target_is_never_clamped() {
    let lift_axis = LIFT_AXIS.replace("travel_minimum_mm = 0.0", "travel_minimum_mm = 20.0");
    let mut rig = armed_with(&lift_axis);

    let request = Touchdown {
        post_turn_page_width_mm: None,
    };
    let id = rig.start(request, Off).unwrap();
    rig.axis(10.0, 0.0, PRESS_A);
    rig.run_for(300);

    // 12 mm is above the 20 mm travel minimum: no hold write, and with the
    // axis outside travel the cancel cannot run either.
    assert_eq!(rig.moves(), vec![298.0]);
    assert!(
        matches!(rig.outcome(id), Some(LiftOutcome::Fault(_))),
        "{:?}",
        rig.outcome(id)
    );
}

#[test]
fn dropped_telemetry_during_touchdown_ends_in_stop_never_contact() {
    let mut rig = armed();
    let request = Touchdown {
        post_turn_page_width_mm: None,
    };
    let id = rig.start(request, Off).unwrap();
    rig.run_for(150);
    rig.board.fault_next(POS_ESTIMATE, ReplyFault::Drop);
    rig.axis(200.0, 0.0, PRESS_A);
    rig.run_for(300);

    assert_eq!(rig.moves(), vec![298.0]);
    assert!(
        matches!(
            rig.outcome(id),
            Some(LiftOutcome::Fault(LiftFault::Stopped(StopCause::LinkLost(
                _
            ))))
        ),
        "{:?}",
        rig.outcome(id)
    );
    assert!(rig.ops.status().latch.is_some());
}

// ----- 8. Page-turn legs --------------------------------------------------------

#[test]
fn page_turn_legs_follow_the_anchors_and_the_blower_cutoff_fires_once() {
    let mut rig = touched_down();
    let leg = |leg| Leg {
        leg,
        page_width_mm: 150.0,
    };
    assert_eq!(
        rig.start(leg(PageTurnLeg::Wiggle), Unknown),
        Err(LiftRejection::NotAtWiggleStart)
    );

    for (next, vacuum) in [
        (PageTurnLeg::ToWiggleStart, Unknown),
        (PageTurnLeg::Wiggle, Unknown),
        (PageTurnLeg::ToBlowerOn, Unknown),
        (PageTurnLeg::ToVacuumOff, Unknown),
        (PageTurnLeg::ToFinalLift, Unknown),
    ] {
        let id = rig.start(leg(next), vacuum).unwrap();
        arrive(&mut rig);
        assert_eq!(
            rig.outcome(id),
            Some(LiftOutcome::Completed(None)),
            "{next:?}"
        );
    }
    // 200.4 mm less 65, 60, 80, 100 and 105% of 150 mm.
    let legs = &rig.moves()[rig.moves().len() - 5..];
    for (written, expected) in legs.iter().zip([102.9, 110.4, 80.4, 50.4, 42.9]) {
        assert!(close(*written, expected), "{legs:?}");
    }

    // The cutoff is 80% of 150 mm above the previous Touchdown: 80.4 mm.
    let id = rig
        .start(
            Touchdown {
                post_turn_page_width_mm: Some(150.0),
            },
            Off,
        )
        .unwrap();
    for position in [60.0, 85.0, 100.0] {
        rig.axis(position, 5.0, 0.5);
        rig.run_for(25);
    }
    let cutoffs: Vec<_> = rig
        .events
        .iter()
        .filter(|event| matches!(event, LiftEvent::TurnBlowerCutoff { .. }))
        .collect();
    assert_eq!(cutoffs, vec![&LiftEvent::TurnBlowerCutoff { id }]);
    assert_eq!(
        rig.ops.status().touchdown_mm,
        Some(200.4),
        "replaced only on completion"
    );
}

#[test]
fn reversed_direction_negates_the_native_target() {
    let mut rig = armed_with(&reversed());

    rig.start(MoveTo { position_mm: 80.0 }, Off).unwrap();

    assert_eq!(rig.f32_writes(INPUT_POS), vec![-10.0]);
}

fn reversed() -> String {
    LIFT_AXIS.replace("direction = \"normal\"", "direction = \"reversed\"")
}

#[test]
fn reversed_direction_touchdown_detects_contact_from_descent_current_only() {
    let mut rig = armed_with(&reversed());
    let id = rig
        .start(
            Touchdown {
                post_turn_page_width_mm: None,
            },
            Off,
        )
        .unwrap();
    rig.run_for(150);
    // Saturated torque opposing the descent is never contact.
    rig.axis(100.0, 0.0, -PRESS_A);
    rig.run_for(200);
    assert_eq!(rig.moves(), vec![298.0]);
    assert_eq!(rig.outcome(id), None);

    rig.axis(200.0, 0.0, PRESS_A);
    rig.run_until(300, |rig| rig.moves().len() == 2);
    assert_eq!(rig.moves()[1], 202.0);
    assert_eq!(rig.f32_writes(INPUT_POS)[1], -202.0 / 8.0);
    rig.axis(200.4, 0.0, 1.9);
    rig.run_for(150);
    assert_eq!(
        rig.outcome(id),
        Some(LiftOutcome::Completed(Some(TouchdownHold {
            position_mm: 200.4,
            press_current_a: PRESS_A,
            hold_target_mm: 202.0,
        })))
    );
}

#[test]
fn stationary_stop_evidence_is_withdrawn_by_contradiction_or_link_loss() {
    let mut contradicted = armed();
    contradicted.stop();
    contradicted.run_for(25);
    assert_eq!(
        contradicted.ops.status().stop_evidence,
        Some(StopEvidence::Stationary)
    );
    contradicted.axis(0.0, 1.0, 0.0);
    contradicted.run_for(25);
    assert_eq!(
        contradicted.ops.status().stop_evidence,
        Some(StopEvidence::Unconfirmed)
    );

    let mut reconnected = armed();
    reconnected.stop();
    reconnected.run_for(25);
    assert_eq!(
        reconnected.ops.status().stop_evidence,
        Some(StopEvidence::Stationary)
    );
    reconnected.connect();
    // Idle stationary samples in the new epoch reuse no earlier confirmation.
    reconnected.run_for(100);
    assert_eq!(
        reconnected.ops.status().stop_evidence,
        Some(StopEvidence::Unconfirmed)
    );
}

#[test]
fn losing_position_trust_stops_pending_and_active_motion_once() {
    // A sample reporting is_homed false arrives while the move's current
    // limit is still being written and read back.
    let mut rig = lifted_away();
    rig.board.set_bool(IS_HOMED, false);
    rig.board.fault_next(INPUT_MODE, ReplyFault::Hold);
    let mark = rig.board.requests.len();
    rig.run_until(25, |rig| {
        rig.board.requests[mark..]
            .iter()
            .any(|request| request.endpoint == INPUT_MODE)
    });
    let moves = rig.moves();
    let id = rig.start(MoveTo { position_mm: 10.0 }, Unknown).unwrap();
    for reply in rig.board.take_held() {
        rig.engine.receive(rig.now, &reply);
    }
    rig.settle();
    rig.run_for(50);
    assert_eq!(rig.moves(), moves, "no deferred motion");
    assert_eq!(rig.f32_writes(CURRENT_LIM).last(), Some(&4.0));
    assert_eq!(rig.finishes(id).len(), 1);
    assert!(matches!(
        rig.outcome(id),
        Some(LiftOutcome::Fault(LiftFault::Stopped(_)))
    ));

    // The same loss during a confirmed move.
    let mut rig = armed();
    let id = rig.start(MoveTo { position_mm: 100.0 }, Off).unwrap();
    rig.axis(40.0, 3.0, 0.5);
    rig.run_for(25);
    rig.board.set_bool(IS_HOMED, false);
    rig.run_for(50);
    assert_eq!(rig.moves(), vec![100.0]);
    assert_eq!(rig.finishes(id).len(), 1);
    assert!(matches!(
        rig.outcome(id),
        Some(LiftOutcome::Fault(LiftFault::Stopped(_)))
    ));
    assert!(!rig.ops.status().position_trusted);
}

// ----- 9. Leaving the hold ------------------------------------------------------

#[test]
fn a_live_hold_rejects_home_and_descents_and_lifts_away_at_the_press_limit() {
    let mut rig = touched_down();
    assert_eq!(rig.ops.status().live_hold_mm, Some(200.4));
    let writes = rig.board.writes.len();
    let wire = rig.wire.len();

    assert_eq!(rig.start(Home, Off), Err(LiftRejection::InHold));
    assert_eq!(
        rig.start(MoveTo { position_mm: 250.0 }, Off),
        Err(LiftRejection::InHold)
    );
    let touchdown = Touchdown {
        post_turn_page_width_mm: None,
    };
    assert_eq!(rig.start(touchdown, Off), Err(LiftRejection::InHold));
    rig.settle();
    assert_eq!(rig.board.writes.len(), writes);
    assert!(
        rig.wire[wire..]
            .iter()
            .all(|(_, request)| request.payload.is_empty()),
        "only telemetry reads"
    );

    let leg = rig
        .start(
            Leg {
                leg: PageTurnLeg::ToWiggleStart,
                page_width_mm: 150.0,
            },
            Unknown,
        )
        .unwrap();
    assert_eq!(rig.write_order_since(writes), vec![INPUT_POS]);
    assert_eq!(rig.f32_writes(CURRENT_LIM).last(), Some(&PRESS_A));
    arrive(&mut rig);
    assert_eq!(rig.outcome(leg), Some(LiftOutcome::Completed(None)));
    assert_eq!(rig.ops.status().live_hold_mm, None);

    let mark = rig.board.writes.len();
    let park = rig.start(MoveTo { position_mm: 50.0 }, Unknown).unwrap();
    assert_eq!(rig.write_order_since(mark), vec![CURRENT_LIM, INPUT_POS]);
    assert_eq!(rig.f32_writes(CURRENT_LIM).last(), Some(&4.0));
    arrive(&mut rig);
    assert_eq!(rig.outcome(park), Some(LiftOutcome::Completed(None)));
}

#[test]
fn clear_fault_keeps_the_latch_while_errors_other_than_a_pressed_endstop_remain() {
    let mut stuck = armed();
    stuck.stop();
    stuck.run_for(25);
    stuck.board.set_i32(MOTOR_ERROR, 0x40);
    let clear = stuck.clear_fault().unwrap();
    stuck.run_for(50);
    assert!(
        matches!(
            stuck.outcome(clear),
            Some(LiftOutcome::Fault(LiftFault::ClearFailed(_)))
        ),
        "{:?}",
        stuck.outcome(clear)
    );
    assert!(stuck.ops.status().latch.is_some());

    // Parked on a pressed endstop, upstream re-sets only the endstop bit.
    let mut parked = armed();
    parked.stop();
    parked.run_for(25);
    parked.board.set_i32(AXIS_ERROR, 1 << 12);
    parked.board.set_bool(MIN_ENDSTOP_STATE, true);
    let clear = parked.clear_fault().unwrap();
    parked.run_for(50);
    assert_eq!(parked.outcome(clear), Some(LiftOutcome::Completed(None)));
    assert_eq!(parked.ops.status().latch, None);
}

#[test]
fn an_odrive_error_latches_stop_and_clear_stop_clears_it() {
    let mut rig = homed();
    rig.board.set_i32(MOTOR_ERROR, 0x40);
    rig.run_for(25);
    let status = rig.ops.status();
    assert!(
        matches!(&status.latch, Some(StopCause::AxisError(errors)) if errors.contains("motor BrakeCurrentOutOfRange (0x40)")),
        "{:?}",
        status.latch
    );
    assert!(!status.position_trusted);
    assert_eq!(rig.verdict(), Some(StopVerdict::Confirmed));

    let clear = rig.clear_stop().unwrap();
    assert_eq!(rig.board.writes_to(CLEAR_ERRORS).len(), 1);
    // The fake keeps its error words until a test resets them, as upstream
    // `clear_errors` does.
    rig.board.set_i32(MOTOR_ERROR, 0);
    rig.run_for(50);
    assert_eq!(rig.outcome(clear), Some(LiftOutcome::Completed(None)));
    assert_eq!(rig.ops.status().latch, None);
}

#[test]
fn resting_on_the_pressed_minimum_endstop_is_not_an_odrive_error() {
    let mut rig = Rig::new(LIFT_AXIS);
    rig.board.set_i32(AXIS_ERROR, 1 << 12);
    rig.board.set_bool(MIN_ENDSTOP_STATE, true);
    rig.run_for(50);
    assert_eq!(rig.ops.status().latch, None);

    // The bit without the pressed endstop is an error.
    rig.board.set_bool(MIN_ENDSTOP_STATE, false);
    rig.run_for(25);
    assert!(matches!(
        rig.ops.status().latch,
        Some(StopCause::AxisError(_))
    ));
}

// ----- 10. Cross-board Stop -----------------------------------------------------

fn unconfirmed(verdict: Option<StopVerdict>) -> bool {
    matches!(verdict, Some(StopVerdict::Unconfirmed { .. }))
}

#[test]
fn stop_fans_out_disarm_and_all_off_and_confirms_on_stationary_evidence() {
    let mut rig = armed();

    rig.machine_stop();

    let (urgent, request) = rig.last_write();
    assert!(*urgent);
    assert_eq!(request.endpoint, REQUESTED_STATE);
    assert_eq!(rig.relay_calls, vec![Duration::from_millis(500)]);
    assert_eq!(rig.verdict(), Some(StopVerdict::Confirming));
    assert_eq!(
        rig.control.next_wakeup(),
        Some(rig.now + Duration::from_millis(500))
    );
    rig.run_for(25);
    assert_eq!(rig.verdict(), Some(StopVerdict::Confirmed));
    assert_eq!(rig.control.next_wakeup(), None);
    assert_eq!(rig.relay_calls.len(), 1);
}

#[test]
fn relay_failure_is_unconfirmed_even_with_a_stationary_axis() {
    let mut rig = armed();
    rig.relay_replies = vec![Err("reply timeout".into())];

    rig.machine_stop();
    rig.run_for(25);

    assert_eq!(
        rig.ops.status().stop_evidence,
        Some(StopEvidence::Stationary)
    );
    assert!(unconfirmed(rig.verdict()), "{:?}", rig.verdict());
}

#[test]
fn missing_evidence_is_unconfirmed_at_the_deadline_and_late_evidence_never_upgrades() {
    let mut rig = armed();
    // The axis keeps moving after Disarm, so no stationary sample arrives.
    rig.axis(50.0, 3.0, 0.0);

    rig.machine_stop();
    rig.run_for(499);
    assert_eq!(rig.verdict(), Some(StopVerdict::Confirming));
    rig.run_for(1);
    assert!(unconfirmed(rig.verdict()), "{:?}", rig.verdict());

    rig.axis(50.0, 0.0, 0.0);
    rig.run_for(50);
    assert_eq!(
        rig.ops.status().stop_evidence,
        Some(StopEvidence::Stationary)
    );
    assert!(unconfirmed(rig.verdict()));
}

#[test]
fn a_lift_fault_latch_is_adopted_and_sends_all_off_once() {
    let mut rig = Rig::new(LIFT_AXIS);
    rig.start(Home, Unknown).unwrap();
    // Home never finishes, so its timeout latches Stop inside the lift module.
    rig.run_for(3_000);

    assert!(rig.ops.status().latch.is_some());
    assert_eq!(rig.relay_calls.len(), 1);
    rig.run_for(100);
    assert_eq!(rig.relay_calls.len(), 1);
    assert_eq!(rig.verdict(), Some(StopVerdict::Confirmed));
}

#[test]
fn confirmed_stop_is_withdrawn_by_a_moving_sample() {
    let mut rig = armed();
    rig.machine_stop();
    rig.run_for(25);
    assert_eq!(rig.verdict(), Some(StopVerdict::Confirmed));

    rig.axis(0.0, 1.0, 0.0);
    rig.run_for(25);

    assert!(unconfirmed(rig.verdict()), "{:?}", rig.verdict());
}

#[test]
fn stopping_again_starts_a_new_attempt_that_can_confirm() {
    let mut rig = armed();
    rig.relay_replies = vec![Err("Monospace disconnected".into())];
    rig.machine_stop();
    rig.run_for(25);
    assert!(unconfirmed(rig.verdict()));
    assert!(
        rig.clear_stop().is_err(),
        "Unconfirmed Stop cannot be cleared"
    );

    rig.machine_stop();
    assert_eq!(rig.verdict(), Some(StopVerdict::Confirming));
    rig.run_for(25);

    assert_eq!(rig.verdict(), Some(StopVerdict::Confirmed));
    assert_eq!(rig.relay_calls.len(), 2);
}

#[test]
fn a_repeated_stop_while_confirming_keeps_the_deadline() {
    let mut rig = armed();
    rig.axis(50.0, 3.0, 0.0);
    rig.machine_stop();
    let deadline = rig.control.next_wakeup().unwrap();
    rig.run_for(100);

    rig.machine_stop();

    assert_eq!(rig.control.next_wakeup(), Some(deadline));
    assert_eq!(rig.relay_calls.len(), 1, "ALL OFF was already processed");
    assert_eq!(
        rig.board
            .requested_states()
            .iter()
            // One idle write from the Home fixture plus one disarm write per Stop.
            .filter(|s| **s == 1)
            .count(),
        3
    );
}

#[test]
fn clear_stop_needs_a_confirmed_stop_and_never_arms() {
    let mut rig = armed();
    rig.axis(50.0, 3.0, 0.0);
    rig.machine_stop();
    assert_eq!(rig.clear_stop(), Err(sans_core::ClearRefused::NotConfirmed));
    rig.axis(50.0, 0.0, 0.0);
    rig.run_for(25);
    assert_eq!(rig.verdict(), Some(StopVerdict::Confirmed));

    let states = rig.board.requested_states().len();
    let clear = rig.clear_stop().unwrap();
    rig.run_for(50);

    assert_eq!(rig.outcome(clear), Some(LiftOutcome::Completed(None)));
    assert_eq!(
        rig.board.requested_states().len(),
        states,
        "clear never arms"
    );
    assert_eq!(rig.ops.status().latch, None);
    assert_eq!(rig.control.status(), None);
    assert!(!rig.ops.status().position_trusted);
}

#[test]
fn reconnect_unconfirms_and_a_new_stop_confirms_in_the_new_epoch() {
    let mut rig = armed();
    rig.machine_stop();
    rig.run_for(25);
    assert_eq!(rig.verdict(), Some(StopVerdict::Confirmed));

    rig.connect();
    rig.run_for(25);
    assert!(
        rig.ops.status().latch.is_some(),
        "a reconnect keeps the latch"
    );
    assert!(unconfirmed(rig.verdict()));

    rig.machine_stop();
    rig.run_for(25);
    assert_eq!(rig.verdict(), Some(StopVerdict::Confirmed));
}

#[test]
fn clear_stop_fails_and_keeps_the_latch_while_the_axis_moves() {
    let mut rig = armed();
    rig.machine_stop();
    rig.run_for(25);
    assert_eq!(rig.verdict(), Some(StopVerdict::Confirmed));

    let clear = rig.clear_stop().unwrap();
    // The disarmed axis drifts while it stays idle.
    rig.axis(10.0, 1.0, 0.0);
    rig.run_for(50);

    assert!(matches!(
        rig.outcome(clear),
        Some(LiftOutcome::Fault(LiftFault::ClearFailed(_)))
    ));
    assert!(rig.ops.status().latch.is_some());
    assert!(unconfirmed(rig.verdict()), "{:?}", rig.verdict());
}

#[test]
fn a_stop_the_lift_module_repeats_on_its_own_is_a_new_attempt() {
    let mut rig = armed();
    rig.machine_stop();
    rig.run_for(25);
    assert_eq!(rig.verdict(), Some(StopVerdict::Confirmed));

    // The clear never answers, so its timeout stops the axis again.
    rig.board.fault_next(CLEAR_ERRORS, ReplyFault::Hold);
    rig.clear_stop().unwrap();
    rig.run_for(2_100);

    assert_eq!(rig.relay_calls.len(), 2);
    assert!(rig.ops.status().latch.is_some());
}
