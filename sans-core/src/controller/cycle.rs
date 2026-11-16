//! The automatic Capture cycle of a Scan session, as a sans-I/O core.
//!
//! [`ScanSession::on`] takes one input and the current time and returns the
//! effects the controller runs: board commands, lift requests, captures,
//! saves and Stop. It reads no clock and touches no device. The controller
//! feeds each board reply straight back before anything else, so a step that
//! awaits a reply always ends its effects with that one command.
//!
//! One successful Capture cycle:
//!
//! 1. A stationary Capture pair becomes canonical and starts attempt 1. The
//!    first cycle then arms the lift axis.
//! 2. `VACUUM OFF`, then at least one second from its `OK`, then `PRESS?` for
//!    the Pressure baseline, then Touchdown with nothing in between. After a
//!    turn the Turn blower stays on until the Touchdown's descent cutoff.
//! 3. Vacuum and Flutter fan on, then a lift to the classification anchor,
//!    where the classifier starts.
//! 4. Once the page is acquired: up to the wiggle start, the vacuum-on wiggle
//!    down, Flutter fan off, up to the blower-on anchor, Turn blower on, up to
//!    the vacuum-off anchor, vacuum off (classification ends), and up to the
//!    final lift. The suction box stays there with the Turn blower on.
//! 5. Both Page records and the attempt record are saved. Only then does the
//!    next Sequence pair begin, with its own Capture pair at the final lift.
//!
//! #27 makes no retries. A pickup failure cancels any running lift, switches
//! the relays `ALL OFF`, logs the attempt and blocks the session. A failed
//! board command or refused lift request latches Stop, since a Touchdown hold
//! may still be pressing on the book.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, SecondsFormat, Utc};

use super::persist::{AttemptRecord, AttemptResult, SaveRequest};
use super::workflow::ScanSession;
use super::{CaptureStatus, Relay};
use crate::camera::CapturePair;
use crate::config::PickupProfile;
use crate::lift::{LiftFault, LiftOutcome, LiftRequest, PageTurnLeg, VacuumEvidence};

/// The least wait from the `VACUUM OFF` acknowledgement to the Pressure baseline.
pub(super) const BASELINE_WAIT: Duration = Duration::from_secs(1);

/// How long the classifier may take at the classification anchor.
pub(super) const CLASSIFY_TIMEOUT: Duration = Duration::from_secs(2);

/// A Monospace command the cycle waits on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BoardCommand {
    Relay(Relay, bool),
    AllOff,
    /// `PRESS?`, answered with millibar.
    ReadPressure,
}

/// Something that happened to the Scan session.
#[derive(Debug)]
pub(super) enum Input {
    Captured(Result<Arc<CapturePair>, String>),
    /// The reply to the last board command; `Some` carries a `PRESS?` reading.
    Board(BoardCommand, Result<Option<f32>, String>),
    LiftRefused(String),
    /// The cycle's own lift operation finished.
    LiftFinished(LiftOutcome),
    /// The cycle's Touchdown passed the Turn-blower cutoff.
    TurnBlowerCutoff,
    /// A streamed pressure reading in millibar.
    Pressure(f32),
    Saved(Result<(), String>),
    /// A deadline from [`ScanSession::next_wakeup`] may have passed.
    Tick,
}

/// What the controller must do next.
#[derive(Debug)]
pub(super) enum Effect {
    Board(BoardCommand),
    Lift(LiftRequest, VacuumEvidence),
    /// Routinely cancel the cycle's running lift operation.
    CancelLift,
    Capture,
    Save(Box<SaveRequest>),
    Stop(String),
}

/// Where the Capture cycle is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Step {
    Capturing,
    Arming,
    /// Waiting for `VACUUM OFF` before the Pressure baseline.
    VacuumOff,
    BaselineWait {
        until: Instant,
    },
    /// Waiting for `PRESS?`.
    Baseline,
    Touchdown,
    /// Touchdown is done; switching the Turn blower off, vacuum and Flutter fan on.
    Pickup,
    /// A page-turn leg is running.
    Leg(PageTurnLeg),
    /// A leg finished and its relay command is out.
    AfterLeg(PageTurnLeg),
    /// Stationary at the classification anchor, waiting for a verdict.
    Classifying {
        until: Instant,
    },
    Saving,
    /// Waiting for a routine cancel and `ALL OFF` after a failure.
    CleaningUp {
        lift: bool,
        relays: bool,
    },
    Blocked,
}

/// The Capture-cycle state a [`ScanSession`] carries.
#[derive(Clone, Debug)]
pub(super) struct Cycle {
    pub(super) step: Step,
    pickup: PickupProfile,
    record: AttemptRecord,
    /// The canonical pair started an attempt whose record is not yet saved.
    unlogged: bool,
    armed: bool,
    /// An exact `OK` to `VACUUM OFF` or `ALL OFF`, with no `VACUUM ON` since.
    vacuum_off: bool,
    /// The Turn blower was switched on after the last turn.
    turn_blower_on: bool,
    classifier: Option<Classifier>,
}

impl Cycle {
    pub(super) fn new(pickup: &PickupProfile) -> Self {
        Self {
            step: Step::Capturing,
            pickup: pickup.clone(),
            record: AttemptRecord::default(),
            unlogged: false,
            armed: false,
            vacuum_off: false,
            turn_blower_on: false,
            classifier: None,
        }
    }

    fn vacuum(&self) -> VacuumEvidence {
        if self.vacuum_off {
            VacuumEvidence::ConfirmedOff
        } else {
            VacuumEvidence::NotConfirmedOff
        }
    }
}

impl ScanSession {
    /// The next deadline the controller must report as [`Input::Tick`].
    pub(super) fn next_wakeup(&self) -> Option<Instant> {
        match self.cycle.step {
            Step::BaselineWait { until } | Step::Classifying { until } => Some(until),
            _ => None,
        }
    }

    /// The attempt a Stop ended before its record was saved. Its result is
    /// `stopped` unless a pickup outcome was already decided.
    pub(super) fn take_unlogged(&mut self) -> Option<SaveRequest> {
        if !std::mem::take(&mut self.cycle.unlogged) {
            return None;
        }
        Some(SaveRequest {
            directory: self.directory.clone(),
            record: self.cycle.record.clone(),
            pages: None,
        })
    }

    /// Fold in one input and return the effects to run, in order.
    pub(super) fn on(&mut self, now: Instant, wall: SystemTime, input: Input) -> Vec<Effect> {
        let mut out = Vec::new();
        let step = self.cycle.step;
        match (step, input) {
            (Step::Capturing, Input::Captured(Ok(pair))) => {
                self.canonical = Some(pair);
                self.capture = CaptureStatus::Ready;
                self.cycle.record = AttemptRecord {
                    left_sequence: self.left_sequence,
                    attempt: self.attempt,
                    vacuum_working_threshold_mbar: self.cycle.pickup.vacuum_working_drop_mbar,
                    pickup_threshold_mbar: self.cycle.pickup.pickup_drop_mbar,
                    ..AttemptRecord::default()
                };
                self.cycle.unlogged = true;
                if self.cycle.armed {
                    self.vacuum_off_for_baseline(&mut out);
                } else {
                    self.cycle.step = Step::Arming;
                    out.push(Effect::Lift(LiftRequest::Arm, self.cycle.vacuum()));
                }
            }
            (Step::Capturing, Input::Captured(Err(error))) => self.fail(
                AttemptResult::Failed,
                format!("Capture pair failed: {error}"),
                &mut out,
            ),
            (_, Input::Board(command, Err(error))) => {
                self.stop(format!("{command:?} failed: {error}"), &mut out);
            }
            (_, Input::LiftRefused(reason)) => self.stop(reason, &mut out),
            (_, Input::LiftFinished(LiftOutcome::Fault(LiftFault::Stopped(_)))) => {
                // Stop already latched; the controller ends the session.
                self.cycle.step = Step::Blocked;
            }
            (Step::Arming, Input::LiftFinished(LiftOutcome::Completed(_))) => {
                self.cycle.armed = true;
                self.vacuum_off_for_baseline(&mut out);
            }
            (Step::VacuumOff, Input::Board(BoardCommand::Relay(Relay::Vacuum, false), Ok(_))) => {
                self.cycle.vacuum_off = true;
                self.cycle.record.vacuum_off_at = Some(stamp(wall));
                self.cycle.step = Step::BaselineWait {
                    until: now + BASELINE_WAIT,
                };
            }
            (Step::BaselineWait { until }, Input::Tick) if now >= until => {
                self.cycle.step = Step::Baseline;
                out.push(Effect::Board(BoardCommand::ReadPressure));
            }
            (Step::Baseline, Input::Board(BoardCommand::ReadPressure, Ok(Some(mbar)))) => {
                self.cycle.record.baseline_mbar = Some(mbar);
                self.cycle.record.baseline_at = Some(stamp(wall));
                self.cycle.step = Step::Touchdown;
                let request = LiftRequest::Touchdown {
                    press_current_a: None,
                    velocity_limit_mm_s: None,
                    post_turn_page_width_mm: self
                        .cycle
                        .turn_blower_on
                        .then_some(f32::from(self.page_width_mm)),
                };
                out.push(Effect::Lift(request, self.cycle.vacuum()));
            }
            (Step::Touchdown, Input::TurnBlowerCutoff) if self.cycle.turn_blower_on => {
                out.push(turn_blower(false));
            }
            (
                Step::Touchdown | Step::Pickup,
                Input::Board(BoardCommand::Relay(Relay::TurnBlower, false), Ok(_)),
            ) => {
                self.cycle.turn_blower_on = false;
                if step == Step::Pickup {
                    self.vacuum_on(&mut out);
                }
            }
            (Step::Touchdown, Input::LiftFinished(LiftOutcome::Completed(Some(hold)))) => {
                self.cycle.record.touchdown_mm = Some(hold.position_mm);
                self.cycle.record.press_current_a = Some(hold.press_current_a);
                self.cycle.step = Step::Pickup;
                if self.cycle.turn_blower_on {
                    out.push(turn_blower(false));
                } else {
                    self.vacuum_on(&mut out);
                }
            }
            (Step::Pickup, Input::Board(BoardCommand::Relay(Relay::Vacuum, true), Ok(_))) => {
                out.push(Effect::Board(BoardCommand::Relay(Relay::FlutterFan, true)));
            }
            (Step::Pickup, Input::Board(BoardCommand::Relay(Relay::FlutterFan, true), Ok(_))) => {
                self.leg(PageTurnLeg::ToClassificationStart, &mut out);
            }
            (Step::Leg(leg), Input::LiftFinished(LiftOutcome::Completed(_))) => match leg {
                PageTurnLeg::ToClassificationStart => {
                    self.cycle.record.classification_started_at = Some(stamp(wall));
                    self.cycle.classifier = Some(Classifier::new(&self.cycle.pickup));
                    self.cycle.step = Step::Classifying {
                        until: now + CLASSIFY_TIMEOUT,
                    };
                }
                PageTurnLeg::ToFinalLift => {
                    self.cycle.record.result = AttemptResult::Turned;
                    self.cycle.unlogged = false;
                    self.cycle.step = Step::Saving;
                    out.push(save(SaveRequest {
                        directory: self.directory.clone(),
                        record: self.cycle.record.clone(),
                        pages: self.canonical.clone(),
                    }));
                }
                _ => match relay_after(leg) {
                    Some((relay, on)) => {
                        self.cycle.step = Step::AfterLeg(leg);
                        out.push(Effect::Board(BoardCommand::Relay(relay, on)));
                    }
                    None => self.leg(next_leg(leg), &mut out),
                },
            },
            (Step::AfterLeg(leg), Input::Board(BoardCommand::Relay(relay, on), Ok(_))) => {
                match (relay, on) {
                    (Relay::Vacuum, false) => {
                        // Classification ends once vacuum off is acknowledged.
                        self.cycle.vacuum_off = true;
                        self.cycle.classifier = None;
                    }
                    (Relay::TurnBlower, true) => self.cycle.turn_blower_on = true,
                    _ => {}
                }
                self.leg(next_leg(leg), &mut out);
            }
            (_, Input::Pressure(mbar)) => self.classify(wall, mbar, &mut out),
            (Step::Classifying { until }, Input::Tick) if now >= until => self.fail(
                AttemptResult::NoVerdict,
                "no pressure verdict at the classification anchor".into(),
                &mut out,
            ),
            (Step::Saving, Input::Saved(Ok(()))) => {
                self.left_sequence += 2;
                self.attempt = 1;
                self.previous = self.canonical.take();
                self.capture = CaptureStatus::Capturing;
                self.cycle.step = Step::Capturing;
                out.push(Effect::Capture);
            }
            (Step::Saving, Input::Saved(Err(error))) => self.fail(
                AttemptResult::Failed,
                format!("cannot save the Page records: {error}"),
                &mut out,
            ),
            (Step::CleaningUp { relays, .. }, Input::LiftFinished(outcome)) => {
                if let LiftOutcome::Cancelled { position_mm } = outcome {
                    self.cycle.record.stop_position_mm = Some(position_mm);
                }
                self.cleaned_up(false, relays, &mut out);
            }
            (Step::CleaningUp { lift, .. }, Input::Board(BoardCommand::AllOff, Ok(_))) => {
                self.cycle.vacuum_off = true;
                self.cycle.turn_blower_on = false;
                self.cleaned_up(lift, true, &mut out);
            }
            (Step::Touchdown | Step::Leg(_) | Step::Arming, Input::LiftFinished(outcome)) => {
                if let LiftOutcome::Cancelled { position_mm } = outcome {
                    self.cycle.record.stop_position_mm = Some(position_mm);
                }
                self.fail(
                    AttemptResult::Failed,
                    format!("{step:?} ended: {outcome:?}"),
                    &mut out,
                );
            }
            (_, input) => log::debug!("Capture cycle in {step:?} ignored {input:?}"),
        }
        out
    }

    fn vacuum_off_for_baseline(&mut self, out: &mut Vec<Effect>) {
        self.cycle.step = Step::VacuumOff;
        out.push(Effect::Board(BoardCommand::Relay(Relay::Vacuum, false)));
    }

    fn vacuum_on(&mut self, out: &mut Vec<Effect>) {
        self.cycle.vacuum_off = false;
        out.push(Effect::Board(BoardCommand::Relay(Relay::Vacuum, true)));
    }

    fn leg(&mut self, leg: PageTurnLeg, out: &mut Vec<Effect>) {
        self.cycle.step = Step::Leg(leg);
        let request = LiftRequest::PageTurnLeg {
            leg,
            page_width_mm: f32::from(self.page_width_mm),
        };
        out.push(Effect::Lift(request, self.cycle.vacuum()));
    }

    fn classify(&mut self, wall: SystemTime, mbar: f32, out: &mut Vec<Effect>) {
        let (Some(classifier), Some(baseline)) = (
            self.cycle.classifier.as_mut(),
            self.cycle.record.baseline_mbar,
        ) else {
            return;
        };
        if !mbar.is_finite() {
            return;
        }
        let record = &mut self.cycle.record;
        record.lowest_mbar = Some(record.lowest_mbar.map_or(mbar, |lowest| lowest.min(mbar)));
        let drop = baseline - mbar;
        let Some(verdict) = classifier.sample(drop) else {
            return;
        };
        record.trigger_drop_mbar = Some(drop);
        let (result, detail) = match verdict {
            Verdict::Acquired => {
                record.acquired_at = Some(stamp(wall));
                if matches!(self.cycle.step, Step::Classifying { .. }) {
                    self.leg(PageTurnLeg::ToWiggleStart, out);
                }
                return;
            }
            Verdict::VacuumFault => (AttemptResult::VacuumFault, "the vacuum is not working"),
            Verdict::NeverSealed => (AttemptResult::NeverSealed, "the page did not seal"),
            Verdict::SealLost => (AttemptResult::SealLost, "the page lost its seal"),
        };
        self.fail(result, format!("pickup failed: {detail}"), out);
    }

    /// End the attempt: cancel a running lift, switch the relays off, then
    /// log and block once both are done.
    fn fail(&mut self, result: AttemptResult, detail: String, out: &mut Vec<Effect>) {
        log::error!("Capture cycle failed: {detail}");
        let record = &mut self.cycle.record;
        record.result = result;
        record.detail = Some(detail);
        let lift = match self.cycle.step {
            Step::Leg(leg) => {
                record.failed_leg = Some(format!("{leg:?}"));
                out.push(Effect::CancelLift);
                true
            }
            Step::Touchdown => {
                out.push(Effect::CancelLift);
                true
            }
            _ => false,
        };
        self.cycle.classifier = None;
        self.cycle.step = Step::CleaningUp {
            lift,
            relays: false,
        };
        out.push(Effect::Board(BoardCommand::AllOff));
    }

    fn cleaned_up(&mut self, lift: bool, relays: bool, out: &mut Vec<Effect>) {
        if lift || !relays {
            self.cycle.step = Step::CleaningUp { lift, relays };
            return;
        }
        self.cycle.step = Step::Blocked;
        let reason = self.cycle.record.detail.clone().unwrap_or_default();
        self.capture = CaptureStatus::Blocked { reason };
        if std::mem::take(&mut self.cycle.unlogged) {
            out.push(save(SaveRequest {
                directory: self.directory.clone(),
                record: self.cycle.record.clone(),
                pages: None,
            }));
        }
    }

    fn stop(&mut self, reason: String, out: &mut Vec<Effect>) {
        log::error!("Capture cycle Stop: {reason}");
        // A pickup outcome already recorded stays; the Stop reason follows it.
        let detail = &mut self.cycle.record.detail;
        *detail = Some(match detail.take() {
            Some(earlier) => format!("{earlier}; then {reason}"),
            None => reason.clone(),
        });
        self.cycle.classifier = None;
        self.cycle.step = Step::Blocked;
        out.push(Effect::Stop(reason));
    }
}

fn save(request: SaveRequest) -> Effect {
    Effect::Save(Box::new(request))
}

fn turn_blower(on: bool) -> Effect {
    Effect::Board(BoardCommand::Relay(Relay::TurnBlower, on))
}

/// The relay switched after a leg completes, before the next leg.
fn relay_after(leg: PageTurnLeg) -> Option<(Relay, bool)> {
    match leg {
        PageTurnLeg::Wiggle => Some((Relay::FlutterFan, false)),
        PageTurnLeg::ToBlowerOn => Some((Relay::TurnBlower, true)),
        PageTurnLeg::ToVacuumOff => Some((Relay::Vacuum, false)),
        _ => None,
    }
}

fn next_leg(leg: PageTurnLeg) -> PageTurnLeg {
    match leg {
        PageTurnLeg::ToClassificationStart => PageTurnLeg::ToWiggleStart,
        PageTurnLeg::ToWiggleStart => PageTurnLeg::Wiggle,
        PageTurnLeg::Wiggle => PageTurnLeg::ToBlowerOn,
        PageTurnLeg::ToBlowerOn => PageTurnLeg::ToVacuumOff,
        PageTurnLeg::ToVacuumOff | PageTurnLeg::ToFinalLift => PageTurnLeg::ToFinalLift,
    }
}

/// RFC 3339 UTC with milliseconds.
fn stamp(wall: SystemTime) -> String {
    DateTime::<Utc>::from(wall).to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// A pickup classification from consecutive raw Pressure-drop samples.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Verdict {
    VacuumFault,
    NeverSealed,
    Acquired,
    SealLost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Band {
    BelowWorking,
    Working,
    Pickup,
}

/// Counts consecutive samples in one Pressure-drop band, with no smoothing.
/// Before acquisition, `needed` samples in one band decide the verdict; after
/// it, `needed` samples below the Pickup threshold mean the seal was lost.
#[derive(Clone, Debug)]
pub(super) struct Classifier {
    working_mbar: f32,
    pickup_mbar: f32,
    needed: u32,
    acquired: bool,
    band: Option<Band>,
    run: u32,
}

impl Classifier {
    pub(super) fn new(pickup: &PickupProfile) -> Self {
        Self {
            working_mbar: pickup.vacuum_working_drop_mbar,
            pickup_mbar: pickup.pickup_drop_mbar,
            needed: pickup.confirmation_samples,
            acquired: false,
            band: None,
            run: 0,
        }
    }

    /// Count one sample's Pressure drop; a verdict once one is reached.
    pub(super) fn sample(&mut self, drop_mbar: f32) -> Option<Verdict> {
        let band = if drop_mbar >= self.pickup_mbar {
            Band::Pickup
        } else if drop_mbar >= self.working_mbar {
            Band::Working
        } else {
            Band::BelowWorking
        };
        if self.acquired {
            self.run = if band == Band::Pickup {
                0
            } else {
                self.run + 1
            };
            return (self.run >= self.needed).then_some(Verdict::SealLost);
        }
        if self.band == Some(band) {
            self.run += 1;
        } else {
            self.band = Some(band);
            self.run = 1;
        }
        if self.run < self.needed {
            return None;
        }
        Some(match band {
            Band::Pickup => {
                self.acquired = true;
                self.run = 0;
                Verdict::Acquired
            }
            Band::Working => Verdict::NeverSealed,
            Band::BelowWorking => Verdict::VacuumFault,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::lift::TouchdownHold;
    use crate::{CameraRole, CapturedFrame, CropGeometry};

    const BASELINE: f32 = 1000.0;

    fn pickup() -> PickupProfile {
        PickupProfile {
            vacuum_working_drop_mbar: 0.5,
            pickup_drop_mbar: 1.2,
            confirmation_samples: 3,
        }
    }

    fn pair() -> CapturePair {
        let frame = |role| {
            CapturedFrame::from_rgb8(
                role,
                1,
                1,
                vec![9; 3],
                SystemTime::UNIX_EPOCH,
                CropGeometry {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
            )
            .unwrap()
        };
        CapturePair {
            left: frame(CameraRole::Left),
            right: frame(CameraRole::Right),
        }
    }

    fn hold() -> LiftOutcome {
        LiftOutcome::Completed(Some(TouchdownHold {
            position_mm: -250.0,
            press_current_a: 2.0,
            hold_limit_mm: -252.0,
        }))
    }

    /// One readable line per effect, for ordering assertions.
    fn describe(effect: &Effect) -> String {
        match effect {
            Effect::Board(BoardCommand::Relay(relay, on)) => {
                format!("{relay:?} {}", if *on { "on" } else { "off" })
            }
            Effect::Board(BoardCommand::AllOff) => "ALL OFF".into(),
            Effect::Board(BoardCommand::ReadPressure) => "PRESS?".into(),
            Effect::Lift(LiftRequest::Arm, _) => "Arm".into(),
            Effect::Lift(
                LiftRequest::Touchdown {
                    post_turn_page_width_mm,
                    ..
                },
                vacuum,
            ) => format!("Touchdown {post_turn_page_width_mm:?} {vacuum:?}"),
            Effect::Lift(LiftRequest::PageTurnLeg { leg, .. }, vacuum) => {
                format!("{leg:?} {vacuum:?}")
            }
            Effect::Lift(request, _) => format!("{request:?}"),
            Effect::CancelLift => "cancel".into(),
            Effect::Capture => "capture".into(),
            Effect::Save(request) => format!(
                "save {:?} {} pages={}",
                request.record.result,
                request.record.left_sequence,
                request.pages.is_some()
            ),
            Effect::Stop(reason) => format!("Stop {reason}"),
        }
    }

    /// Plays the controller: answers board commands at once, finishes lift
    /// operations on request and records every effect.
    struct Driver {
        session: ScanSession,
        now: Instant,
        wall: SystemTime,
        log: Vec<String>,
        /// The running lift request, if any.
        lift: Option<LiftRequest>,
        /// The `PRESS?` reply.
        baseline: f32,
    }

    impl Driver {
        fn new() -> Self {
            let mut session = ScanSession::new(PathBuf::from("/session"), 200, &pickup());
            session.capture = CaptureStatus::Capturing;
            Self {
                session,
                now: Instant::now(),
                wall: SystemTime::UNIX_EPOCH,
                log: Vec::new(),
                lift: None,
                baseline: BASELINE,
            }
        }

        fn feed(&mut self, input: Input) {
            let mut inputs = vec![input];
            while let Some(input) = inputs.pop() {
                for effect in self.session.on(self.now, self.wall, input) {
                    self.log.push(describe(&effect));
                    match effect {
                        Effect::Board(command) => {
                            let reply = match command {
                                BoardCommand::ReadPressure => Some(self.baseline),
                                _ => None,
                            };
                            inputs.push(Input::Board(command, Ok(reply)));
                        }
                        Effect::Lift(request, _) => self.lift = Some(request),
                        _ => {}
                    }
                }
            }
        }

        fn advance(&mut self, by: Duration) {
            self.now += by;
            self.wall += by;
            self.feed(Input::Tick);
        }

        /// Finish the running lift operation with `outcome`.
        fn finish(&mut self, outcome: LiftOutcome) {
            assert!(self.lift.take().is_some(), "no lift operation is running");
            self.feed(Input::LiftFinished(outcome));
        }

        fn pressure(&mut self, drop: f32, samples: usize) {
            for _ in 0..samples {
                self.feed(Input::Pressure(self.baseline - drop));
            }
        }

        fn take_log(&mut self) -> Vec<String> {
            std::mem::take(&mut self.log)
        }

        /// From a capture to stationary at the classification anchor.
        fn run_to_classification(&mut self) {
            self.feed(Input::Captured(Ok(Arc::new(pair()))));
            if self.session.cycle.step == Step::Arming {
                self.finish(LiftOutcome::Completed(None));
            }
            self.advance(BASELINE_WAIT);
            self.finish(hold());
            self.finish(LiftOutcome::Completed(None));
        }

        /// From acquisition to the save request at the final lift.
        fn turn(&mut self) {
            self.pressure(2.0, 3);
            for _ in 0..5 {
                self.finish(LiftOutcome::Completed(None));
            }
        }
    }

    #[test]
    fn classifier_decides_from_three_consecutive_raw_samples_in_one_band() {
        let verdicts = |drops: &[f32]| {
            let mut classifier = Classifier::new(&pickup());
            drops
                .iter()
                .filter_map(|drop| classifier.sample(*drop))
                .collect::<Vec<_>>()
        };
        assert_eq!(verdicts(&[0.1, 0.2, 0.49]), [Verdict::VacuumFault]);
        assert_eq!(verdicts(&[0.5, 1.0, 1.19]), [Verdict::NeverSealed]);
        assert_eq!(verdicts(&[1.2, 3.0, 1.2]), [Verdict::Acquired]);
        // A band change restarts the run.
        assert_eq!(verdicts(&[1.2, 1.3, 0.6, 1.2, 1.3]), []);
        // After acquisition, three samples below Pickup lose the seal, whatever band.
        assert_eq!(
            verdicts(&[2.0, 2.0, 2.0, 1.0, 0.1, 2.0, 1.0, 0.1, 1.1]),
            [Verdict::Acquired, Verdict::SealLost]
        );
    }

    #[test]
    fn first_cycle_arms_then_waits_a_second_from_vacuum_off_before_the_baseline_and_touchdown() {
        let mut driver = Driver::new();
        driver.feed(Input::Captured(Ok(Arc::new(pair()))));
        assert_eq!(driver.take_log(), ["Arm"]);
        driver.finish(LiftOutcome::Completed(None));
        assert_eq!(driver.take_log(), ["Vacuum off"]);
        assert_eq!(
            driver.session.cycle.record.vacuum_off_at.as_deref(),
            Some("1970-01-01T00:00:00.000Z")
        );

        driver.advance(BASELINE_WAIT - Duration::from_millis(1));
        assert!(driver.take_log().is_empty(), "too early for the baseline");
        driver.advance(Duration::from_millis(1));

        // Touchdown follows the baseline reply with nothing in between.
        assert_eq!(driver.take_log(), ["PRESS?", "Touchdown None ConfirmedOff"]);
        let record = &driver.session.cycle.record;
        assert_eq!(record.baseline_mbar, Some(BASELINE));
        assert_eq!(
            record.baseline_at.as_deref(),
            Some("1970-01-01T00:00:01.000Z")
        );
    }

    #[test]
    fn a_successful_turn_follows_the_anchors_and_saves_before_the_next_sequence_pair() {
        let mut driver = Driver::new();
        driver.run_to_classification();
        assert!(driver.take_log().ends_with(
            &[
                "Vacuum on",
                "FlutterFan on",
                "ToClassificationStart NotConfirmedOff"
            ]
            .map(String::from)
        ));
        // Readings before the anchor were not counted; these acquire the page.
        driver.turn();

        assert_eq!(
            driver.take_log(),
            [
                "ToWiggleStart NotConfirmedOff",
                "Wiggle NotConfirmedOff",
                "FlutterFan off",
                "ToBlowerOn NotConfirmedOff",
                "TurnBlower on",
                "ToVacuumOff NotConfirmedOff",
                "Vacuum off",
                "ToFinalLift ConfirmedOff",
                "save Turned 0 pages=true",
            ]
        );
        assert_eq!(
            driver.session.left_sequence, 0,
            "not advanced before the save"
        );
        // Classification ended at vacuum off: a lost seal no longer counts.
        driver.pressure(0.0, 3);
        assert!(driver.take_log().is_empty());

        driver.feed(Input::Saved(Ok(())));
        assert_eq!(driver.take_log(), ["capture"]);
        assert_eq!(
            (driver.session.left_sequence, driver.session.attempt),
            (2, 1)
        );
        assert!(driver.session.canonical.is_none());
        assert!(driver.session.previous.is_some());
    }

    #[test]
    fn the_next_touchdown_keeps_the_turn_blower_until_its_cutoff() {
        let mut driver = Driver::new();
        driver.run_to_classification();
        driver.turn();
        driver.feed(Input::Saved(Ok(())));
        driver.take_log();

        driver.feed(Input::Captured(Ok(Arc::new(pair()))));
        driver.advance(BASELINE_WAIT);
        assert_eq!(
            driver.take_log(),
            ["Vacuum off", "PRESS?", "Touchdown Some(200.0) ConfirmedOff"],
            "no Arm and no blower command before the cutoff"
        );
        driver.feed(Input::TurnBlowerCutoff);
        driver.feed(Input::TurnBlowerCutoff);
        assert_eq!(driver.take_log(), ["TurnBlower off"]);

        driver.finish(hold());
        assert_eq!(
            driver.take_log(),
            [
                "Vacuum on",
                "FlutterFan on",
                "ToClassificationStart NotConfirmedOff"
            ]
        );
        assert_eq!(driver.session.cycle.record.left_sequence, 2);
    }

    #[test]
    fn a_touchdown_without_a_cutoff_switches_the_turn_blower_off_before_vacuum() {
        let mut driver = Driver::new();
        driver.run_to_classification();
        driver.turn();
        driver.feed(Input::Saved(Ok(())));
        driver.feed(Input::Captured(Ok(Arc::new(pair()))));
        driver.advance(BASELINE_WAIT);
        driver.take_log();

        driver.finish(hold());
        assert_eq!(
            driver.take_log(),
            [
                "TurnBlower off",
                "Vacuum on",
                "FlutterFan on",
                "ToClassificationStart NotConfirmedOff"
            ]
        );
    }

    #[test]
    fn pickup_failures_switch_all_off_log_the_attempt_and_block() {
        for (drop, result) in [
            (0.0, AttemptResult::VacuumFault),
            (0.8, AttemptResult::NeverSealed),
        ] {
            let mut driver = Driver::new();
            driver.run_to_classification();
            driver.take_log();
            driver.pressure(drop, 3);

            assert_eq!(
                driver.take_log(),
                [
                    "ALL OFF".to_string(),
                    format!("save {result:?} 0 pages=false")
                ]
            );
            assert!(matches!(
                driver.session.capture,
                CaptureStatus::Blocked { .. }
            ));
            assert_eq!(driver.session.left_sequence, 0);
        }
    }

    #[test]
    fn a_lost_seal_cancels_the_leg_and_logs_where_it_stopped() {
        let mut driver = Driver::new();
        driver.run_to_classification();
        driver.pressure(2.0, 3);
        driver.take_log();
        driver.pressure(0.3, 3);
        assert_eq!(driver.take_log(), ["cancel", "ALL OFF"]);

        driver.finish(LiftOutcome::Cancelled {
            position_mm: -150.0,
        });
        let log = driver.take_log();
        assert_eq!(log, ["save SealLost 0 pages=false"]);
        let record = &driver.session.cycle.record;
        assert_eq!(record.stop_position_mm, Some(-150.0));
        assert_eq!(record.failed_leg.as_deref(), Some("ToWiggleStart"));
        assert_eq!(record.trigger_drop_mbar, Some(BASELINE - (BASELINE - 0.3)));
    }

    #[test]
    fn no_pressure_verdict_in_time_fails_the_attempt() {
        let mut driver = Driver::new();
        driver.run_to_classification();
        driver.take_log();
        driver.advance(CLASSIFY_TIMEOUT);
        assert_eq!(
            driver.take_log(),
            ["ALL OFF", "save NoVerdict 0 pages=false"]
        );
    }

    #[test]
    fn a_failed_relay_command_stops_and_starts_no_lift() {
        let mut driver = Driver::new();
        driver.run_to_classification();
        driver.take_log();
        // As if VACUUM ON had failed during the live Touchdown hold.
        driver.session.cycle.step = Step::Pickup;
        let effects = driver.session.on(
            driver.now,
            driver.wall,
            Input::Board(
                BoardCommand::Relay(Relay::Vacuum, true),
                Err("timeout".into()),
            ),
        );

        assert_eq!(
            effects.iter().map(describe).collect::<Vec<_>>(),
            ["Stop Relay(Vacuum, true) failed: timeout"]
        );
        let stopped = driver.session.take_unlogged().unwrap();
        assert_eq!(stopped.record.result, AttemptResult::Stopped);
        assert!(driver.session.take_unlogged().is_none(), "logged once");
    }

    #[test]
    fn a_failed_cleanup_stops_but_keeps_the_pickup_outcome() {
        let mut driver = Driver::new();
        driver.run_to_classification();
        let effects = driver
            .session
            .on(driver.now, driver.wall, Input::Pressure(BASELINE - 0.8));
        assert!(effects.is_empty());
        driver
            .session
            .on(driver.now, driver.wall, Input::Pressure(BASELINE - 0.8));
        driver
            .session
            .on(driver.now, driver.wall, Input::Pressure(BASELINE - 0.8));
        let effects = driver.session.on(
            driver.now,
            driver.wall,
            Input::Board(BoardCommand::AllOff, Err("timeout".into())),
        );

        assert!(matches!(effects.as_slice(), [Effect::Stop(_)]));
        let stopped = driver.session.take_unlogged().unwrap();
        assert_eq!(stopped.record.result, AttemptResult::NeverSealed);
        assert_eq!(
            stopped.record.detail.as_deref(),
            Some("pickup failed: the page did not seal; then AllOff failed: timeout")
        );
    }

    #[test]
    fn a_failed_save_blocks_without_advancing() {
        let mut driver = Driver::new();
        driver.run_to_classification();
        driver.turn();
        driver.take_log();
        driver.feed(Input::Saved(Err("disk full".into())));

        assert_eq!(
            driver.take_log(),
            ["ALL OFF"],
            "the turned record was already sent"
        );
        assert_eq!(driver.session.left_sequence, 0);
        assert!(driver.session.canonical.is_some(), "kept for #29");
    }
}
