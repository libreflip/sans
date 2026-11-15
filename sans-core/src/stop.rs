//! Cross-board Stop: one latch, both boards, one verdict.
//!
//! [`StopControl`] sits beside [`LiftOperations`]. It has no threads, no I/O
//! and no clock. The Stop latch is the lift module's own: whoever sets it,
//! an operator Stop, the physical button, a lift fault, an ODrive error
//! (other than the minimum-endstop flag alone while the endstop is pressed)
//! or a lost link, `StopControl` sees the latch and sends Monospace `ALL OFF`
//! once for it.
//! It then judges Confirmed or Unconfirmed Stop within the Machine profile's
//! `timeouts.stop_terminal_ms`, counted from controller receipt.
//!
//! # Stop mapping and evidence
//!
//! - Lift axis: an urgent Disarm, submitted without waiting. Evidence is
//!   [`StopEvidence::Stationary`]: a confirmed Disarm followed by a fresh
//!   idle, stationary sample. The lift module withdraws it on contradiction,
//!   link loss or a new epoch.
//! - Monospace: an ordinary `ALL OFF` whose exact `OK` arrives within the
//!   remaining budget on the current connection. The urgent writer is not
//!   used, because it can never observe the reply.
//! - Confirmed Stop needs both by the deadline. A relay failure, unconfirmed
//!   axis evidence or a missed deadline is Unconfirmed Stop, which stays
//!   until the next explicit Stop starts a new attempt. Later evidence never
//!   upgrades it; a Confirmed Stop drops to Unconfirmed if the axis evidence
//!   is withdrawn.
//! - Recovery: Clear Stop is accepted only from Confirmed Stop and runs the
//!   lift module's `clear_fault`, which never arms. Position trust stays lost
//!   until a Home.
//!
//! # Integration contract
//!
//! Call [`StopControl::observe`] after every [`LiftOperations`] call and link
//! event, and call it no later than [`StopControl::next_wakeup`]. The
//! `relays` port sends `ALL OFF` and waits at most the given duration for
//! its `OK`; it is called at most once per observed latch or explicit Stop.

use std::time::{Duration, Instant};

use crate::lift::{
    LiftEvent, LiftFault, LiftOperations, LiftOutcome, LiftRejection, LinkPort, OperationId,
    StopCause, StopEvidence,
};
use crate::MachineProfile;

/// Sends Monospace `ALL OFF` and waits at most the given time for its `OK`.
pub type Relays<'a> = dyn FnMut(Duration) -> Result<(), String> + 'a;

/// How a latched Stop stands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StopVerdict {
    /// Waiting for both boards' evidence before the deadline.
    Confirming,
    /// Motion stopped and `ALL OFF` processed within the budget.
    Confirmed,
    /// Evidence is missing, late or contradicted; switch off machine power.
    Unconfirmed {
        /// What was missing.
        reason: String,
    },
}

/// The latched Stop, for presentation.
#[derive(Clone, Debug, PartialEq)]
pub struct StopStatus {
    /// Why Stop latched first.
    pub cause: StopCause,
    /// Confirmed, Unconfirmed or still confirming.
    pub verdict: StopVerdict,
    /// Why the last Clear Stop did not clear the latch.
    pub notice: Option<String>,
}

/// Why Clear Stop was refused.
#[derive(Clone, Debug, PartialEq)]
pub enum ClearRefused {
    /// Only a Confirmed Stop can be cleared in software.
    NotConfirmed,
    /// The lift module refused `clear_fault`.
    Lift(LiftRejection),
}

/// Judges cross-board Stop. See the module documentation.
pub struct StopControl {
    budget: Duration,
    record: Option<Record>,
}

struct Record {
    cause: StopCause,
    /// The lift module's Stop count this attempt follows.
    stops: u64,
    deadline: Instant,
    relays: Result<(), String>,
    verdict: StopVerdict,
    notice: Option<String>,
}

impl StopControl {
    /// Use the profile's `timeouts.stop_terminal_ms` as the budget.
    pub fn new(profile: &MachineProfile) -> Self {
        Self {
            budget: Duration::from_millis(profile.timeouts.stop_terminal_ms),
            record: None,
        }
    }

    /// Latch Stop, urgently disarm the axis, then send `ALL OFF`.
    ///
    /// A Stop while one is still confirming disarms again but keeps its
    /// deadline and its processed `ALL OFF`. Any other Stop starts a new
    /// attempt that needs fresh evidence from both boards.
    pub fn stop(
        &mut self,
        now: Instant,
        cause: StopCause,
        ops: &mut LiftOperations,
        link: &mut LinkPort<'_>,
        relays: &mut Relays<'_>,
    ) {
        ops.stop(cause.clone(), link);
        match self.record.as_mut() {
            Some(record) if record.verdict == StopVerdict::Confirming => {
                record.stops = ops.status().stops;
            }
            _ => self.dispatch(now, cause, ops, relays),
        }
        self.observe(now, ops, relays);
    }

    /// Follow the lift latch and judge the verdict at `now`.
    pub fn observe(&mut self, now: Instant, ops: &LiftOperations, relays: &mut Relays<'_>) {
        let status = ops.status();
        let Some(cause) = status.latch else {
            self.record = None;
            return;
        };
        if self
            .record
            .as_ref()
            .is_none_or(|record| record.stops != status.stops)
        {
            // The lift module latched or stopped again on its own: a fault,
            // a lost link or a failed clear. That is a new attempt.
            self.dispatch(now, cause, ops, relays);
        }
        let record = self.record.as_mut().expect("dispatched");
        record.verdict = judge(record, now, status.stop_evidence);
    }

    /// Note a lift event; a failed clear is shown with the Stop.
    pub fn note(&mut self, event: &LiftEvent) {
        if let (
            Some(record),
            LiftEvent::Finished {
                outcome: LiftOutcome::Fault(fault @ LiftFault::ClearFailed(_)),
                ..
            },
        ) = (self.record.as_mut(), event)
        {
            record.notice = Some(format!("{fault:?}"));
        }
    }

    /// Clear the board's errors and, on success, the latch. Never arms.
    pub fn clear(
        &mut self,
        now: Instant,
        ops: &mut LiftOperations,
        link: &mut LinkPort<'_>,
    ) -> Result<OperationId, ClearRefused> {
        let record = self.record.as_mut().ok_or(ClearRefused::NotConfirmed)?;
        let result = if record.verdict == StopVerdict::Confirmed {
            ops.clear_fault(now, link).map_err(ClearRefused::Lift)
        } else {
            Err(ClearRefused::NotConfirmed)
        };
        record.notice = result.as_ref().err().map(|refused| format!("{refused:?}"));
        result
    }

    /// The latched Stop, if any.
    pub fn status(&self) -> Option<StopStatus> {
        self.record.as_ref().map(|record| StopStatus {
            cause: record.cause.clone(),
            verdict: record.verdict.clone(),
            notice: record.notice.clone(),
        })
    }

    /// The deadline, while a Stop is still confirming.
    pub fn next_wakeup(&self) -> Option<Instant> {
        self.record
            .as_ref()
            .filter(|record| record.verdict == StopVerdict::Confirming)
            .map(|record| record.deadline)
    }

    fn dispatch(
        &mut self,
        now: Instant,
        cause: StopCause,
        ops: &LiftOperations,
        relays: &mut Relays<'_>,
    ) {
        let relays = relays(self.budget);
        // The first cause stays, as it does in the lift latch.
        let cause = self.record.take().map_or(cause, |record| record.cause);
        self.record = Some(Record {
            cause,
            stops: ops.status().stops,
            deadline: now + self.budget,
            relays,
            verdict: StopVerdict::Confirming,
            notice: None,
        });
    }
}

fn judge(record: &Record, now: Instant, evidence: Option<StopEvidence>) -> StopVerdict {
    let unconfirmed = |reason: &str| StopVerdict::Unconfirmed {
        reason: reason.into(),
    };
    if matches!(record.verdict, StopVerdict::Unconfirmed { .. }) {
        return record.verdict.clone();
    }
    if let Err(error) = &record.relays {
        return unconfirmed(&format!("ALL OFF was not confirmed: {error}"));
    }
    match evidence {
        Some(StopEvidence::Stationary) if record.verdict == StopVerdict::Confirmed => {
            StopVerdict::Confirmed
        }
        Some(StopEvidence::Stationary) if now <= record.deadline => StopVerdict::Confirmed,
        Some(StopEvidence::Pending) if now < record.deadline => StopVerdict::Confirming,
        Some(StopEvidence::Pending | StopEvidence::Stationary) => {
            unconfirmed("no stopped-motion evidence before the Stop deadline")
        }
        Some(StopEvidence::Unconfirmed) | None => {
            unconfirmed("the lift axis did not confirm stopped motion")
        }
    }
}
