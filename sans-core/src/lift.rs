//! Host lift-axis operations: the motion policy the retired Ligature firmware
//! used to own, on top of the stock ODrive link.
//!
//! [`LiftOperations`] has no threads, no USB and no clock. It takes operation
//! requests, every lift-axis [`LinkEvent`] and the current time, submits typed
//! link commands through a port the caller supplies, and hands back typed
//! [`LiftEvent`]s. It runs one operation at a time, with no queue, and reports
//! exactly one [`LiftEvent::Finished`] for each accepted operation. It derives
//! Position trust from a Home it watched end to end, detects Touchdown contact
//! from polled position and current, and separates routine cancel, which
//! leaves the axis armed, from Stop, which latches a host fault.
//!
//! Lift-axis millimetres are 0 at the Home reference and increase downward.
//!
//! # Integration contract
//!
//! - `LiftOperations` is the only writer of lift-axis commands on its link. No
//!   other code submits a link command to the same link. Requesting a
//!   reconnect is allowed and arrives as an epoch change.
//! - The port submits one command and returns its ticket, or `None` if the
//!   link is closed. A closed link while the axis is armed or an operation is
//!   active latches Stop.
//! - Cancel and Stop use urgent priority. An urgent command retires ordinary
//!   commands that have not left the host; those retirements arrive for
//!   tickets the module no longer waits on and are ignored as stale.
//! - The caller feeds every link event, in order, with a monotonic time, and
//!   calls [`LiftOperations::poll`] no later than
//!   [`LiftOperations::next_wakeup`]. After each call it drains the link's
//!   output and [`LiftOperations::take_event`].
//! - The module cannot detect a dropped event. Its deadlines then end the
//!   operation in Stop.
//! - Vacuum evidence is supplied fresh on each `start` and is never cached.
//!   Confirmed off means an exact `OK` to a vacuum-off or all-off command on
//!   the current Monospace connection, with no vacuum-on command since.
//!   Unknown state, a failed relay command or a Monospace reconnect is not
//!   confirmed off. The module cannot check this provenance; the caller is
//!   responsible for it. The module checks vacuum evidence at `start` only.
//! - Stop evidence is reported, never judged: #25 owns Stop deadlines and
//!   Confirmed or Unconfirmed Stop.
//!
//! # Motion contract and evidence classes
//!
//! Every clause is a *reference contract*: it is taken from upstream ODrive
//! `fw-v0.5.1` source at revision `7831d795235e5ef8535e4b46621a0721b458ec8f`
//! unless it says otherwise. None of it is verified on the installed vendor
//! build; #60 owns that. Automated tests are fake-backed host behaviour
//! against these semantics, not hardware evidence.
//!
//! - Home request (reference): write the homing state. The adapter confirms
//!   entry by observing the homing state.
//! - Home completion (reference): the first fresh sample after that
//!   confirmation that reads idle. Success needs all four error words zero,
//!   `is_homed` true, encoder ready, trajectory done, stationary velocity and position within
//!   the position tolerance of 0. An idle sample without that evidence is a
//!   clean `HomeFailed`; any other state or the Home timeout is Stop. Upstream
//!   clears `is_homed` on entry and sets it true on every exit, including
//!   after a fault or an interrupting state request, and an interrupted run
//!   rewrites the encoder count. So `is_homed` alone is never evidence, and an
//!   interrupted Home grants no trust.
//! - Home leaves the axis idle (reference). Motion needs a separate Arm.
//!   Home is accepted from idle because the adapter's Arm rejects any error
//!   word, and the #57 capture showed the axis idle on a pressed endstop with
//!   the endstop error set (captured). That native Home works on this machine
//!   is an operator observation with no log.
//! - Cancel during Home (reference): the only way to interrupt native homing
//!   is a state request, so it is an urgent Disarm.
//! - Move completion (reference): a fresh sample taken strictly after the
//!   position readback showing closed-loop control, zero error words,
//!   trajectory done, the target within tolerance and stationary velocity.
//!   Upstream clears trajectory done in the control loop after the position
//!   write, so only samples after the readback count.
//! - Arming (reference) sets the input position to the current estimate, so
//!   arming commands no motion.
//! - Routine cancel (reference): an urgent absolute move to the sampled
//!   position. A position write in trapezoidal mode replans from the current
//!   setpoint with the configured deceleration, so the axis may overshoot by
//!   up to the braking reach and return. #60 must measure overshoot, return
//!   and stop time before this is relied on.
//! - Current limit (reference): upstream reads the configured limit every
//!   control cycle, so a runtime write takes effect at once. Units are amps.
//! - Touchdown contact (host policy): saturated current in the descent
//!   direction (measured current signed by the commissioned direction, so
//!   opposite torque never counts), stationary velocity
//!   and no progress for a whole dwell window, after a startup grace, with
//!   the travel bound checked first. Whether a stalled hold stays saturated
//!   at the press current is unverified (#60).
//! - Trajectory done (reference) is planned-time upstream: it reads true
//!   when the planned trajectory time has elapsed, not when the axis
//!   arrives, so it is never taken alone as physical arrival. A contact so
//!   late that the planned time runs out before the dwell completes is
//!   `TravelExhausted`; that fails closed, and #60 checks how close to the
//!   descent limit a real contact can be.
//! - Live hold (host policy): after a completed Touchdown the setpoint still
//!   presses into the book, limited only by the press current. Until a
//!   `Completed` or `Cancelled` operation ends stationary above the hold's
//!   settled position by more than the position tolerance, or Stop or a new
//!   epoch ends it, no current limit is written and Home and every descent
//!   are rejected. The first upward move therefore runs at the press limit,
//!   and the next motion restores the commissioned limit first. A routine
//!   cancel keeps the press limit and the hold. The hold is independent of
//!   Position trust; without trust the only way out is Stop. Unverified:
//!   #60 must check that the press current can lift the loaded suction box
//!   off the book. If it cannot, the lift-away stalls, the move times out
//!   and becomes Stop, which fails closed.
//! - Stop (reference): a Disarm, which floats the motor phases upstream. What
//!   the vertical axis does unpowered is a physical question for #60 and #25.
//! - Stale telemetry: only fresh current-epoch samples count as evidence.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::config::{LiftAxisProfile, LiftDirection, MotionProfile};
use crate::odrive::{
    AxisState, CommandResult, LinkCommand, LinkEvent, LinkPhase, LinkStatus, Priority,
    TelemetrySample, TelemetryValues, Ticket,
};
use crate::MachineProfile;

/// A process-lifetime operation identity; never reused.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OperationId(pub u64);

/// One lift-axis operation request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LiftRequest {
    /// Enter closed-loop control; completes when the adapter confirms it.
    Arm,
    /// Run native homing from idle. Grants Position trust on clean evidence.
    Home,
    /// Absolute move, also used to park.
    MoveTo {
        /// Target in millimetres, 0 at Home and increasing downward.
        position_mm: f32,
    },
    /// Lower until contact and hold the press. With a post-turn page width,
    /// emits [`LiftEvent::TurnBlowerCutoff`] on the way down.
    Touchdown {
        /// Page width of the turn just made, if this Touchdown follows one.
        post_turn_page_width_mm: Option<f32>,
    },
    /// One page-turn leg, targeted from the recorded Touchdown position.
    PageTurnLeg {
        /// Which leg.
        leg: PageTurnLeg,
        /// The scan session's page width.
        page_width_mm: f32,
    },
}

/// Page-turn legs, each at a Machine-profile Lift percentage of the page width.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PageTurnLeg {
    /// Up to the wiggle-start percentage.
    ToWiggleStart,
    /// Down by the wiggle-retreat percentage; the only descent allowed with vacuum on.
    Wiggle,
    /// Up to the blower-on percentage.
    ToBlowerOn,
    /// Up to the vacuum-off percentage.
    ToVacuumOff,
    /// Up to the final-lift percentage.
    ToFinalLift,
}

/// Vacuum evidence the caller supplies on each `start`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VacuumEvidence {
    /// Confirmed off on the current Monospace connection.
    ConfirmedOff,
    /// Anything else.
    NotConfirmedOff,
}

/// Why a request was refused; nothing was submitted.
#[derive(Clone, Debug, PartialEq)]
pub enum LiftRejection {
    /// `[odrive.lift_axis]` is absent.
    Uncommissioned,
    /// A Stop latch is set; clear it first.
    Latched,
    /// Another operation is active.
    Busy,
    /// The link is not verified in the current epoch.
    LinkNotVerified,
    /// No fresh current-epoch sample.
    NoFreshSample,
    /// Position trust is not held.
    PositionUntrusted,
    /// No Touchdown position is recorded.
    NoTouchdownPosition,
    /// The page width is not finite or outside the profile bounds.
    InvalidPageWidth(f32),
    /// The target is not finite or outside the commissioned travel.
    InvalidTarget(f32),
    /// A descent needs confirmed vacuum-off evidence.
    VacuumNotConfirmedOff,
    /// The wiggle starts only at the wiggle-start anchor.
    NotAtWiggleStart,
    /// The Touchdown descent limit is outside travel or not below the axis.
    NoDescentRoom,
    /// A Touchdown hold is live: Home and descents wait for a lift-away or Stop.
    InHold,
    /// Clearing the fault needs a fresh sample reading idle.
    NotIdle,
}

/// Why Stop latched.
#[derive(Clone, Debug, PartialEq)]
pub enum StopCause {
    /// The caller asked for Stop.
    Requested(String),
    /// The link left the verified phase or changed epoch while armed or active.
    LinkLost(String),
    /// A command or the axis gave no certain outcome.
    Uncertain(String),
}

/// A typed operation fault.
#[derive(Clone, Debug, PartialEq)]
pub enum LiftFault {
    /// The adapter refused or retired a command before it left the host.
    CommandRejected(String),
    /// Home ended idle without clean evidence; the axis is idle and unlatched.
    HomeFailed(String),
    /// Touchdown reached the descent limit or a hold target outside travel.
    TravelExhausted,
    /// Touchdown stalled where it started.
    NoProgress,
    /// The hold reached its target without resistance.
    ContactNotHeld,
    /// Touchdown did not settle within the move timeout.
    TimedOut,
    /// Stop latched.
    Stopped(StopCause),
}

/// What a completed Touchdown recorded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TouchdownHold {
    /// The recorded Touchdown position.
    pub position_mm: f32,
    /// The press current limit in force.
    pub press_current_a: f32,
    /// The hold target, the hold depth below contact.
    pub hold_target_mm: f32,
}

/// How an operation ended.
#[derive(Clone, Debug, PartialEq)]
pub enum LiftOutcome {
    /// Done; a Touchdown carries what it recorded.
    Completed(Option<TouchdownHold>),
    /// Routinely cancelled; the axis stopped here.
    Cancelled {
        /// Where the axis stopped.
        position_mm: f32,
    },
    /// Ended with a fault.
    Fault(LiftFault),
}

/// Module output.
#[derive(Clone, Debug, PartialEq)]
pub enum LiftEvent {
    /// The one finish of an accepted operation.
    Finished {
        /// The operation.
        id: OperationId,
        /// The link epoch it ran in.
        epoch: u64,
        /// How it ended.
        outcome: LiftOutcome,
    },
    /// A post-turn Touchdown passed the blower-off height; switch the Turn blower off.
    TurnBlowerCutoff {
        /// The Touchdown.
        id: OperationId,
    },
}

/// Axis evidence after Stop. The module applies no deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopEvidence {
    /// Waiting for the Disarm and a later idle stationary sample.
    Pending,
    /// A confirmed Disarm and a later fresh idle stationary sample.
    Stationary,
    /// The Disarm was not confirmed, the link left the verified phase or
    /// changed epoch, or a later sample was not idle and stationary. Final
    /// until the next Stop or a cleared fault.
    Unconfirmed,
}

/// A snapshot of lift-axis policy state.
#[derive(Clone, Debug, PartialEq)]
pub struct LiftStatus {
    /// The link epoch the module follows.
    pub epoch: u64,
    /// Whether the position may be used for absolute motion.
    pub position_trusted: bool,
    /// The Stop latch and its cause.
    pub latch: Option<StopCause>,
    /// The active operation.
    pub active: Option<OperationId>,
    /// The recorded Touchdown position.
    pub touchdown_mm: Option<f32>,
    /// The settled position of a live Touchdown hold.
    pub live_hold_mm: Option<f32>,
    /// Axis evidence since the last Stop.
    pub stop_evidence: Option<StopEvidence>,
}

/// Owns every lift-axis operation on one link. See the module documentation.
pub struct LiftOperations {
    axis: Option<LiftAxisProfile>,
    motion: MotionProfile,
    page_width_mm: (f32, f32),
    max_age: Duration,
    urgent_deadline: Duration,
    epoch: u64,
    verified: bool,
    latest: Option<TelemetrySample>,
    trusted: bool,
    touchdown_mm: Option<f32>,
    /// A live hold's own reference, kept apart from trust and `touchdown_mm`.
    live_hold_mm: Option<f32>,
    applied_limit_a: Option<f32>,
    latch: Option<StopCause>,
    stop_disarm: Option<Ticket>,
    stop_confirmed_at: Option<Instant>,
    stop_evidence: Option<StopEvidence>,
    last_id: u64,
    active: Option<Active>,
    events: VecDeque<LiftEvent>,
}

struct Active {
    id: OperationId,
    deadline: Instant,
    /// The one command result this operation waits on.
    ticket: Option<Ticket>,
    /// When the latest motion command was confirmed; only later samples count.
    confirmed_at: Option<Instant>,
    /// A current limit waiting for its readback before the phase's command.
    limit_a: Option<f32>,
    /// A motion command was confirmed, so the axis may be moving.
    moving: bool,
    phase: Phase,
}

#[derive(Clone, Debug)]
enum Phase {
    Arm,
    Clear,
    Home,
    Move {
        target_mm: f32,
    },
    Descend {
        touchdown: Touchdown,
        window: Option<Window>,
    },
    Hold {
        touchdown: Touchdown,
        target_mm: f32,
        window: Option<Window>,
    },
    Cancel {
        fault: Option<LiftFault>,
    },
    HomeCancel,
}

#[derive(Clone, Copy, Debug)]
struct Touchdown {
    start_mm: f32,
    limit_mm: f32,
    press_a: f32,
    cutoff_mm: Option<f32>,
}

#[derive(Clone, Copy, Debug)]
struct Window {
    opened_at: Instant,
    position_mm: f32,
}

/// What one fresh sample means for the active operation.
enum Step {
    Wait,
    Finish(LiftOutcome),
    Stop(String),
    Cancel(LiftFault),
    Hold { target_mm: f32 },
}

type Port<'a> = dyn FnMut(LinkCommand, Priority) -> Option<Ticket> + 'a;

impl LiftOperations {
    /// Build from the validated Machine profile. An uncommissioned lift axis
    /// rejects every request.
    pub fn new(profile: &MachineProfile) -> Self {
        Self {
            axis: profile.odrive.lift_axis.clone(),
            motion: profile.motion.clone(),
            page_width_mm: (profile.page_width.minimum_mm, profile.page_width.maximum_mm),
            max_age: Duration::from_millis(profile.odrive.telemetry_max_age_ms),
            urgent_deadline: Duration::from_millis(profile.odrive.urgent_deadline_ms),
            epoch: 0,
            verified: false,
            latest: None,
            trusted: false,
            touchdown_mm: None,
            live_hold_mm: None,
            applied_limit_a: None,
            latch: None,
            stop_disarm: None,
            stop_confirmed_at: None,
            stop_evidence: None,
            last_id: 0,
            active: None,
            events: VecDeque::new(),
        }
    }

    /// Accept one operation, or refuse it with nothing submitted.
    pub fn start(
        &mut self,
        now: Instant,
        request: LiftRequest,
        vacuum: VacuumEvidence,
        link: &mut Port<'_>,
    ) -> Result<OperationId, LiftRejection> {
        let axis = self.axis.clone().ok_or(LiftRejection::Uncommissioned)?;
        if self.latch.is_some() {
            return Err(LiftRejection::Latched);
        }
        if self.active.is_some() {
            return Err(LiftRejection::Busy);
        }
        if !self.verified {
            return Err(LiftRejection::LinkNotVerified);
        }
        let phase = match request {
            LiftRequest::Arm => Phase::Arm,
            LiftRequest::Home => Phase::Home,
            LiftRequest::MoveTo { position_mm } => Phase::Move {
                target_mm: self.check_move(now, position_mm, vacuum, None)?,
            },
            LiftRequest::PageTurnLeg { leg, page_width_mm } => {
                let target = self.leg_target(leg, page_width_mm)?;
                let wiggle_from = match leg {
                    PageTurnLeg::Wiggle => {
                        Some(self.leg_target(PageTurnLeg::ToWiggleStart, page_width_mm)?)
                    }
                    _ => None,
                };
                Phase::Move {
                    target_mm: self.check_move(now, target, vacuum, wiggle_from)?,
                }
            }
            LiftRequest::Touchdown {
                post_turn_page_width_mm,
            } => Phase::Descend {
                touchdown: self.check_touchdown(now, post_turn_page_width_mm, vacuum)?,
                window: None,
            },
        };
        if self.live_hold_mm.is_some() {
            let position = self.fresh(now).and_then(|values| values.position_mm);
            let descent = match phase {
                Phase::Move { target_mm } => {
                    matches!(
                        request,
                        LiftRequest::PageTurnLeg {
                            leg: PageTurnLeg::Wiggle,
                            ..
                        }
                    ) || position.is_none_or(|p| target_mm > p + axis.position_tolerance_mm)
                }
                Phase::Home | Phase::Descend { .. } => true,
                _ => false,
            };
            if descent {
                return Err(LiftRejection::InHold);
            }
        }
        let timeout = match phase {
            Phase::Home => axis.home_timeout_ms,
            _ => axis.move_timeout_ms,
        };
        if matches!(phase, Phase::Home) {
            self.lose_trust();
        }
        let id = self.begin(now, Duration::from_millis(timeout), phase);
        self.submit_phase(link);
        Ok(id)
    }

    /// Routinely cancel the active operation. Any other ID does nothing.
    pub fn cancel(&mut self, now: Instant, id: OperationId, link: &mut Port<'_>) {
        let Some(phase) = self
            .active
            .as_ref()
            .filter(|active| active.id == id)
            .map(|active| &active.phase)
        else {
            return;
        };
        match phase {
            Phase::Arm | Phase::Clear | Phase::Cancel { .. } | Phase::HomeCancel => {}
            Phase::Home => {
                let deadline = now + Duration::from_millis(self.axis().cancel_timeout_ms);
                let active = self.active.as_mut().expect("checked");
                active.phase = Phase::HomeCancel;
                active.limit_a = None;
                active.deadline = deadline;
                self.submit(LinkCommand::Disarm, Priority::Urgent, link);
            }
            _ => self.begin_cancel(now, None, link),
        }
    }

    /// Latch Stop, finish the active operation and submit an urgent Disarm.
    /// Always accepted.
    pub fn stop(&mut self, cause: StopCause, link: &mut Port<'_>) {
        self.lose_trust();
        // The latch and the Arm needed after it reset the pressing setpoint.
        self.live_hold_mm = None;
        self.finish(LiftOutcome::Fault(LiftFault::Stopped(cause.clone())));
        self.latch.get_or_insert(cause);
        self.stop_disarm = link(LinkCommand::Disarm, Priority::Urgent);
        self.stop_confirmed_at = None;
        self.stop_evidence = Some(if self.stop_disarm.is_some() && self.verified {
            StopEvidence::Pending
        } else {
            StopEvidence::Unconfirmed
        });
    }

    /// Clear the board's errors and, once that completes, the Stop latch.
    /// Never arms.
    pub fn clear_fault(
        &mut self,
        now: Instant,
        link: &mut Port<'_>,
    ) -> Result<OperationId, LiftRejection> {
        let axis = self.axis.clone().ok_or(LiftRejection::Uncommissioned)?;
        if self.active.is_some() {
            return Err(LiftRejection::Busy);
        }
        let values = self.fresh(now).ok_or(LiftRejection::NoFreshSample)?;
        if values.axis_state != AxisState::Idle {
            return Err(LiftRejection::NotIdle);
        }
        let id = self.begin(
            now,
            Duration::from_millis(axis.move_timeout_ms),
            Phase::Clear,
        );
        self.submit(LinkCommand::ClearErrors, Priority::Ordinary, link);
        Ok(id)
    }

    /// Feed one link event.
    pub fn handle_link_event(&mut self, now: Instant, event: &LinkEvent, link: &mut Port<'_>) {
        match event {
            LinkEvent::Status(status) => self.on_status(status, link),
            LinkEvent::Telemetry(sample) => self.on_sample(now, sample, link),
            LinkEvent::CommandResult {
                ticket,
                epoch,
                result,
                ..
            } if *epoch == self.epoch => self.on_result(now, *ticket, result, link),
            LinkEvent::CommandResult { .. } => {}
        }
    }

    /// Process deadlines at `now`.
    pub fn poll(&mut self, now: Instant, link: &mut Port<'_>) {
        let Some(phase) = self
            .active
            .as_ref()
            .filter(|active| now >= active.deadline)
            .map(|active| &active.phase)
        else {
            return;
        };
        if matches!(phase, Phase::Descend { .. } | Phase::Hold { .. }) {
            self.begin_cancel(now, Some(LiftFault::TimedOut), link);
        } else {
            let reason = format!("{} timed out", phase_name(phase));
            self.stop(StopCause::Uncertain(reason), link);
        }
    }

    /// The latest time `poll` must be called.
    pub fn next_wakeup(&self) -> Option<Instant> {
        self.active.as_ref().map(|active| active.deadline)
    }

    /// The next event.
    pub fn take_event(&mut self) -> Option<LiftEvent> {
        self.events.pop_front()
    }

    /// Current policy state.
    pub fn status(&self) -> LiftStatus {
        LiftStatus {
            epoch: self.epoch,
            position_trusted: self.trusted,
            latch: self.latch.clone(),
            active: self.active.as_ref().map(|active| active.id),
            touchdown_mm: self.touchdown_mm,
            live_hold_mm: self.live_hold_mm,
            stop_evidence: self.stop_evidence,
        }
    }

    // ----- acceptance ----------------------------------------------------------

    fn axis(&self) -> &LiftAxisProfile {
        self.axis.as_ref().expect("checked at acceptance")
    }

    fn fresh(&self, now: Instant) -> Option<&TelemetryValues> {
        self.latest.as_ref()?.fresh_values(now)
    }

    fn fresh_position(&self, now: Instant) -> Result<f32, LiftRejection> {
        let position = self.fresh(now).and_then(|values| values.position_mm);
        let position = position.ok_or(LiftRejection::NoFreshSample)?;
        if !self.trusted {
            return Err(LiftRejection::PositionUntrusted);
        }
        Ok(position)
    }

    fn check_target(&self, target_mm: f32) -> Result<f32, LiftRejection> {
        let axis = self.axis();
        if target_mm.is_finite()
            && target_mm >= axis.travel_minimum_mm
            && target_mm <= axis.travel_maximum_mm
        {
            Ok(target_mm)
        } else {
            Err(LiftRejection::InvalidTarget(target_mm))
        }
    }

    fn check_move(
        &self,
        now: Instant,
        target_mm: f32,
        vacuum: VacuumEvidence,
        wiggle_from: Option<f32>,
    ) -> Result<f32, LiftRejection> {
        let position = self.fresh_position(now)?;
        let target_mm = self.check_target(target_mm)?;
        let tolerance = self.axis().position_tolerance_mm;
        match wiggle_from {
            Some(anchor) if (position - anchor).abs() > tolerance => {
                Err(LiftRejection::NotAtWiggleStart)
            }
            None if target_mm > position + tolerance && vacuum != VacuumEvidence::ConfirmedOff => {
                Err(LiftRejection::VacuumNotConfirmedOff)
            }
            _ => Ok(target_mm),
        }
    }

    fn check_page_width(&self, page_width_mm: f32) -> Result<f32, LiftRejection> {
        let (minimum, maximum) = self.page_width_mm;
        if page_width_mm.is_finite() && page_width_mm >= minimum && page_width_mm <= maximum {
            Ok(page_width_mm)
        } else {
            Err(LiftRejection::InvalidPageWidth(page_width_mm))
        }
    }

    /// The recorded Touchdown position minus the leg's Lift percentage of the page width.
    fn leg_target(&self, leg: PageTurnLeg, page_width_mm: f32) -> Result<f32, LiftRejection> {
        let page_width_mm = self.check_page_width(page_width_mm)?;
        let touchdown = self
            .touchdown_mm
            .ok_or(LiftRejection::NoTouchdownPosition)?;
        let motion = &self.motion;
        let percent = match leg {
            PageTurnLeg::ToWiggleStart => motion.wiggle_start_percent,
            PageTurnLeg::Wiggle => motion.wiggle_start_percent - motion.wiggle_retreat_percent,
            PageTurnLeg::ToBlowerOn => motion.blower_on_percent,
            PageTurnLeg::ToVacuumOff => motion.vacuum_off_percent,
            PageTurnLeg::ToFinalLift => motion.final_lift_percent,
        };
        Ok(touchdown - percent / 100.0 * page_width_mm)
    }

    fn check_touchdown(
        &self,
        now: Instant,
        post_turn_page_width_mm: Option<f32>,
        vacuum: VacuumEvidence,
    ) -> Result<Touchdown, LiftRejection> {
        let start_mm = self.fresh_position(now)?;
        if vacuum != VacuumEvidence::ConfirmedOff {
            return Err(LiftRejection::VacuumNotConfirmedOff);
        }
        let cutoff_mm = match post_turn_page_width_mm {
            None => None,
            Some(width) => {
                let width = self.check_page_width(width)?;
                let previous = self
                    .touchdown_mm
                    .ok_or(LiftRejection::NoTouchdownPosition)?;
                Some(previous - self.motion.blower_off_descent_percent / 100.0 * width)
            }
        };
        let axis = self.axis();
        let limit_mm = axis.travel_maximum_mm - axis.touchdown_hold_depth_mm;
        if !(limit_mm >= axis.travel_minimum_mm
            && limit_mm <= axis.travel_maximum_mm
            && limit_mm > start_mm + axis.touchdown_no_progress_mm)
        {
            return Err(LiftRejection::NoDescentRoom);
        }
        Ok(Touchdown {
            start_mm,
            limit_mm,
            press_a: self.motion.touchdown_press_percent / 100.0 * axis.current_limit_a,
            cutoff_mm,
        })
    }

    // ----- submission ----------------------------------------------------------

    fn begin(&mut self, now: Instant, timeout: Duration, phase: Phase) -> OperationId {
        self.last_id += 1;
        let id = OperationId(self.last_id);
        self.active = Some(Active {
            id,
            deadline: now + timeout,
            ticket: None,
            confirmed_at: None,
            limit_a: None,
            moving: false,
            phase,
        });
        id
    }

    /// Submit the active phase's command, first setting its current limit if needed.
    fn submit_phase(&mut self, link: &mut Port<'_>) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        if needs_trust(&active.phase) && !self.trusted {
            let cause = StopCause::Uncertain("Position trust lost before motion".into());
            return self.stop(cause, link);
        }
        let commissioned_a = self.axis.as_ref().expect("commissioned").current_limit_a;
        let (wanted_a, command) = match active.phase {
            Phase::Arm => (None, LinkCommand::Arm),
            Phase::Home => (Some(commissioned_a), LinkCommand::Home),
            Phase::Move { target_mm } => (
                Some(commissioned_a),
                LinkCommand::MoveAbsolute {
                    position_mm: target_mm,
                },
            ),
            Phase::Descend { touchdown, .. } => (
                Some(touchdown.press_a),
                LinkCommand::MoveAbsolute {
                    position_mm: touchdown.limit_mm,
                },
            ),
            _ => return,
        };
        match wanted_a {
            // A live hold's setpoint still presses: never raise the limit under it.
            Some(amps) if self.applied_limit_a != Some(amps) && self.live_hold_mm.is_none() => {
                active.limit_a = Some(amps);
                self.applied_limit_a = None;
                self.submit(
                    LinkCommand::SetCurrentLimit { amps },
                    Priority::Ordinary,
                    link,
                );
            }
            _ => self.submit(command, Priority::Ordinary, link),
        }
    }

    fn submit(&mut self, command: LinkCommand, priority: Priority, link: &mut Port<'_>) {
        let ticket = link(command, priority);
        if let Some(active) = self.active.as_mut() {
            active.ticket = ticket;
            active.confirmed_at = None;
        }
        if ticket.is_none() {
            let cause = StopCause::LinkLost(format!("the link was closed for {command:?}"));
            self.stop(cause, link);
        }
    }

    /// Start the routine cancel sequence: an urgent move to the sampled
    /// position, or an urgent Stop when the braking envelope leaves travel.
    fn begin_cancel(&mut self, now: Instant, fault: Option<LiftFault>, link: &mut Port<'_>) {
        let axis = self.axis().clone();
        let sampled = self
            .fresh(now)
            .and_then(|values| Some((values.position_mm?, values.velocity_mm_s?)));
        let Some((position, velocity)) = sampled else {
            let cause = StopCause::Uncertain("cancel without a fresh sample".into());
            return self.stop(cause, link);
        };
        let speed = velocity.abs();
        let reach = if speed <= axis.stationary_velocity_mm_s {
            0.0
        } else {
            speed * (self.max_age + self.urgent_deadline).as_secs_f32()
                + speed * speed / (2.0 * axis.deceleration_limit_mm_s2)
        };
        let end = position + velocity.signum() * reach;
        let inside = |mm: f32| mm >= axis.travel_minimum_mm && mm <= axis.travel_maximum_mm;
        if !(inside(position) && inside(end)) {
            let cause = StopCause::Uncertain(format!(
                "cancel braking envelope {position} to {end} mm leaves travel"
            ));
            return self.stop(cause, link);
        }
        if let Some(active) = self.active.as_mut() {
            active.phase = Phase::Cancel { fault };
            active.limit_a = None;
            active.deadline = now + Duration::from_millis(axis.cancel_timeout_ms);
        }
        self.submit(
            LinkCommand::MoveAbsolute {
                position_mm: position,
            },
            Priority::Urgent,
            link,
        );
    }

    // ----- link events ---------------------------------------------------------

    fn on_status(&mut self, status: &LinkStatus, link: &mut Port<'_>) {
        let epoch_changed = status.epoch != self.epoch;
        let verified = status.phase == LinkPhase::Verified;
        if epoch_changed || !verified {
            let armed = self
                .latest
                .as_ref()
                .is_some_and(|sample| sample.display_values().axis_state.is_armed());
            if self.active.is_some() || (armed && self.latch.is_none()) {
                let reason = format!("link epoch {} is {:?}", status.epoch, status.phase);
                self.stop(StopCause::LinkLost(reason), link);
            }
            if self.stop_evidence.is_some() {
                self.stop_evidence = Some(StopEvidence::Unconfirmed);
            }
            self.lose_trust();
        }
        if epoch_changed {
            // A new epoch never reuses the previous one's Disarm confirmation.
            self.stop_disarm = None;
            self.stop_confirmed_at = None;
            // The new gate idles the axis, and Arm resets the setpoint.
            self.live_hold_mm = None;
            self.latest = None;
            self.applied_limit_a = None;
        }
        self.epoch = status.epoch;
        self.verified = verified;
    }

    fn on_result(
        &mut self,
        now: Instant,
        ticket: Ticket,
        result: &CommandResult,
        link: &mut Port<'_>,
    ) {
        if self.stop_disarm == Some(ticket) {
            self.stop_disarm = None;
            if *result == CommandResult::Completed {
                self.stop_confirmed_at = Some(now);
            } else {
                self.stop_evidence = Some(StopEvidence::Unconfirmed);
            }
            return;
        }
        let Some(active) = self.active.as_mut().filter(|a| a.ticket == Some(ticket)) else {
            return;
        };
        active.ticket = None;
        let cancelling = matches!(active.phase, Phase::Cancel { .. } | Phase::HomeCancel);
        let unsent = match result {
            CommandResult::Completed => None,
            CommandResult::Rejected { reason } => Some(reason.clone()),
            CommandResult::Retired { transmitted: false } => {
                Some("retired before it left the host".into())
            }
            other => {
                let cause = StopCause::Uncertain(format!("command outcome {other:?}"));
                return self.stop(cause, link);
            }
        };
        if let Some(reason) = unsent {
            if cancelling || active.moving {
                let cause = StopCause::Uncertain(format!("command refused mid-motion: {reason}"));
                return self.stop(cause, link);
            }
            return self.finish(LiftOutcome::Fault(LiftFault::CommandRejected(reason)));
        }
        if let Some(amps) = active.limit_a.take() {
            self.applied_limit_a = Some(amps);
            return self.submit_phase(link);
        }
        if matches!(active.phase, Phase::Arm | Phase::Clear) {
            return self.finish(LiftOutcome::Completed(None));
        }
        active.confirmed_at = Some(now);
        active.moving = true;
    }

    fn on_sample(&mut self, now: Instant, sample: &TelemetrySample, link: &mut Port<'_>) {
        if sample.epoch() != self.epoch
            || self
                .latest
                .as_ref()
                .is_some_and(|latest| sample.taken_at() <= latest.taken_at())
        {
            return;
        }
        self.latest = Some(sample.clone());
        let Some(values) = sample.fresh_values(now).cloned() else {
            return;
        };
        if values.encoder_error != 0 || !values.encoder_ready || !values.is_homed {
            self.lose_trust();
        }
        if !self.trusted && self.active.as_ref().is_some_and(|a| needs_trust(&a.phase)) {
            let cause = StopCause::Uncertain("Position trust lost during motion".into());
            return self.stop(cause, link);
        }
        let (Some(position), Some(velocity)) = (values.position_mm, values.velocity_mm_s) else {
            return;
        };
        let Some(axis) = self.axis.clone() else {
            return;
        };
        let stationary = velocity.abs() <= axis.stationary_velocity_mm_s;
        let taken_at = sample.taken_at();
        if self.stop_evidence == Some(StopEvidence::Pending)
            && self.stop_confirmed_at.is_some_and(|at| taken_at > at)
            && values.axis_state == AxisState::Idle
            && stationary
        {
            self.stop_evidence = Some(StopEvidence::Stationary);
        } else if self.stop_evidence == Some(StopEvidence::Stationary)
            && !(values.axis_state == AxisState::Idle && stationary)
        {
            self.stop_evidence = Some(StopEvidence::Unconfirmed);
        }
        let Some(active) = self.active.as_mut() else {
            return;
        };
        if !active.confirmed_at.is_some_and(|at| taken_at > at) {
            return;
        }
        let step = sample_step(&axis, active, &values, taken_at, position, stationary);
        let id = active.id;
        if let Phase::Descend { touchdown, .. } = &mut active.phase {
            if touchdown.cutoff_mm.is_some_and(|cutoff| position >= cutoff) {
                touchdown.cutoff_mm = None;
                self.events.push_back(LiftEvent::TurnBlowerCutoff { id });
            }
        }
        match step {
            Step::Wait => {}
            Step::Finish(outcome) => self.finish(outcome),
            Step::Stop(reason) => self.stop(StopCause::Uncertain(reason), link),
            Step::Cancel(fault) => self.begin_cancel(now, Some(fault), link),
            Step::Hold { target_mm } => {
                let active = self.active.as_mut().expect("active");
                if let Phase::Descend { touchdown, .. } = active.phase {
                    active.phase = Phase::Hold {
                        touchdown,
                        target_mm,
                        window: None,
                    };
                }
                self.submit(
                    LinkCommand::MoveAbsolute {
                        position_mm: target_mm,
                    },
                    Priority::Ordinary,
                    link,
                );
            }
        }
    }

    // ----- outcomes ------------------------------------------------------------

    /// End a live hold on a stationary sample above its reference by more than the tolerance.
    fn end_hold_on_lift_away(&mut self) {
        let (Some(reference), Some(axis), Some(sample)) =
            (self.live_hold_mm, self.axis.as_ref(), self.latest.as_ref())
        else {
            return;
        };
        let values = sample.display_values();
        if let (Some(position), Some(velocity)) = (values.position_mm, values.velocity_mm_s) {
            if velocity.abs() <= axis.stationary_velocity_mm_s
                && position < reference - axis.position_tolerance_mm
            {
                self.live_hold_mm = None;
            }
        }
    }

    fn lose_trust(&mut self) {
        self.trusted = false;
        self.touchdown_mm = None;
    }

    fn finish(&mut self, outcome: LiftOutcome) {
        let Some(active) = self.active.take() else {
            return;
        };
        if let LiftOutcome::Completed(hold) = &outcome {
            match active.phase {
                Phase::Home => self.trusted = true,
                Phase::Clear => {
                    self.latch = None;
                    self.stop_evidence = None;
                    self.stop_disarm = None;
                }
                _ => {}
            }
            if let Some(hold) = hold {
                self.touchdown_mm = Some(hold.position_mm);
                self.live_hold_mm = Some(hold.position_mm);
            }
        }
        if matches!(
            outcome,
            LiftOutcome::Completed(None) | LiftOutcome::Cancelled { .. }
        ) {
            self.end_hold_on_lift_away();
        }
        self.events.push_back(LiftEvent::Finished {
            id: active.id,
            epoch: self.epoch,
            outcome,
        });
    }
}

/// Phases whose motion targets depend on Position trust.
fn needs_trust(phase: &Phase) -> bool {
    matches!(
        phase,
        Phase::Move { .. } | Phase::Descend { .. } | Phase::Hold { .. } | Phase::Cancel { .. }
    )
}

fn phase_name(phase: &Phase) -> &'static str {
    match phase {
        Phase::Arm => "Arm",
        Phase::Clear => "clear fault",
        Phase::Home => "Home",
        Phase::Move { .. } => "move",
        Phase::Descend { .. } | Phase::Hold { .. } => "Touchdown",
        Phase::Cancel { .. } => "routine cancel",
        Phase::HomeCancel => "Home cancel",
    }
}

/// Judge one fresh sample taken after the active command's confirmation.
fn sample_step(
    axis: &LiftAxisProfile,
    active: &mut Active,
    values: &TelemetryValues,
    taken_at: Instant,
    position: f32,
    stationary: bool,
) -> Step {
    let tolerance = axis.position_tolerance_mm;
    let errors_zero = values.axis_error == 0
        && values.motor_error == 0
        && values.encoder_error == 0
        && values.controller_error == 0;
    let closed_loop_ok = values.axis_state == AxisState::ClosedLoopControl && errors_zero;
    let armed_fault = || {
        Step::Stop(format!(
            "axis state {:?} with errors axis 0x{:x}, motor 0x{:x}, encoder 0x{:x}, controller 0x{:x}",
            values.axis_state,
            values.axis_error,
            values.motor_error,
            values.encoder_error,
            values.controller_error
        ))
    };
    let dwell = Duration::from_millis(axis.touchdown_dwell_ms);
    // Measured current in the descent direction; opposite torque never counts.
    let descent_iq_a = match axis.direction {
        LiftDirection::Normal => values.iq_measured_a,
        LiftDirection::Reversed => -values.iq_measured_a,
    };
    let stalled = |press_a: f32| {
        descent_iq_a >= axis.touchdown_saturation_percent / 100.0 * press_a && stationary
    };
    // Advance a stall window; returns it once it has lasted the dwell.
    let advance = |window: &mut Option<Window>, stalled: bool| -> Option<Window> {
        if !stalled {
            *window = None;
            return None;
        }
        match window {
            Some(open) if (position - open.position_mm).abs() <= axis.touchdown_no_progress_mm => {}
            _ => {
                *window = Some(Window {
                    opened_at: taken_at,
                    position_mm: position,
                })
            }
        }
        let open = (*window)?;
        (taken_at.duration_since(open.opened_at) >= dwell).then_some(open)
    };
    match &mut active.phase {
        Phase::Arm | Phase::Clear => Step::Wait,
        Phase::Home => match values.axis_state {
            AxisState::Homing => Step::Wait,
            AxisState::Idle => {
                if errors_zero
                    && values.is_homed
                    && values.encoder_ready
                    && values.trajectory_done
                    && stationary
                    && position.abs() <= tolerance
                {
                    Step::Finish(LiftOutcome::Completed(None))
                } else {
                    Step::Finish(LiftOutcome::Fault(LiftFault::HomeFailed(format!(
                        "idle at {position} mm, homed {}, trajectory done {}, errors zero {errors_zero}, stationary {stationary}",
                        values.is_homed, values.trajectory_done
                    ))))
                }
            }
            other => Step::Stop(format!("Home ended in axis state {other:?}")),
        },
        Phase::HomeCancel => match values.axis_state {
            AxisState::Idle if stationary => Step::Finish(LiftOutcome::Cancelled {
                position_mm: position,
            }),
            AxisState::Idle => Step::Wait,
            other => Step::Stop(format!("cancelled Home reads axis state {other:?}")),
        },
        _ if !closed_loop_ok => armed_fault(),
        Phase::Move { target_mm } => {
            if values.trajectory_done && (position - *target_mm).abs() <= tolerance && stationary {
                Step::Finish(LiftOutcome::Completed(None))
            } else {
                Step::Wait
            }
        }
        Phase::Cancel { fault } => {
            if values.trajectory_done && stationary {
                Step::Finish(match fault.take() {
                    Some(fault) => LiftOutcome::Fault(fault),
                    None => LiftOutcome::Cancelled {
                        position_mm: position,
                    },
                })
            } else {
                Step::Wait
            }
        }
        Phase::Descend { touchdown, window } => {
            if position >= touchdown.limit_mm - tolerance || values.trajectory_done {
                return Step::Cancel(LiftFault::TravelExhausted);
            }
            let grace = Duration::from_millis(axis.touchdown_grace_ms);
            let in_grace = active.confirmed_at.is_some_and(|at| taken_at < at + grace);
            let Some(contact) = advance(window, !in_grace && stalled(touchdown.press_a)) else {
                return Step::Wait;
            };
            if (contact.position_mm - touchdown.start_mm).abs() <= axis.touchdown_no_progress_mm {
                return Step::Cancel(LiftFault::NoProgress);
            }
            let target_mm = contact.position_mm + axis.touchdown_hold_depth_mm;
            if !(target_mm.is_finite()
                && target_mm >= axis.travel_minimum_mm
                && target_mm <= axis.travel_maximum_mm)
            {
                return Step::Cancel(LiftFault::TravelExhausted);
            }
            Step::Hold { target_mm }
        }
        Phase::Hold {
            touchdown,
            target_mm,
            window,
        } => {
            let stalled = stalled(touchdown.press_a);
            if let Some(settled) = advance(window, stalled) {
                return Step::Finish(LiftOutcome::Completed(Some(TouchdownHold {
                    position_mm: settled.position_mm,
                    press_current_a: touchdown.press_a,
                    hold_target_mm: *target_mm,
                })));
            }
            if !stalled && values.trajectory_done && (position - *target_mm).abs() <= tolerance {
                return Step::Cancel(LiftFault::ContactNotHeld);
            }
            Step::Wait
        }
    }
}
