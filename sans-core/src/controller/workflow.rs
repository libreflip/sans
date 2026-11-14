//! The operator workflow: Home consent, Idle, page width, Ready and the
//! Scan session started from it.
//!
//! The controller owns one [`Workflow`] value. Screens, the status LED and
//! the physical button are projections of it, never parallel state.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use chrono::{DateTime, Utc};

use super::{CaptureStatus, ControllerIntent, Led};
use crate::camera::CapturePair;
use crate::lift::{LiftFault, LiftOutcome, OperationId};

/// Where the operator is in the journey from boot to a running Scan session.
pub(super) enum Workflow {
    /// Home needs explicit consent: after boot, after a clean Home failure
    /// and after every loss of Position trust.
    Setup {
        /// Why the last Home ended without Position trust, if it did.
        home_failure: Option<String>,
    },
    /// Native Home is running with the operator's consent.
    Homing(OperationId),
    /// Position trust is held and no book is being prepared.
    Idle,
    /// The operator is entering the book's page width.
    PageWidth {
        /// The last value outside the Machine profile's bounds.
        rejected_mm: Option<u16>,
    },
    /// The page width is accepted; Start is the next step.
    Ready {
        /// The accepted page width.
        page_width_mm: u16,
        /// Why the last Start did not create a Scan session.
        notice: Option<String>,
    },
    /// A Scan session is running.
    Scanning(ScanSession),
}

impl Workflow {
    pub(super) fn setup() -> Self {
        Self::Setup { home_failure: None }
    }

    /// The intent the physical button stands for in this state: New Book
    /// from Idle, Start from Ready and Stop while the machine is active.
    /// Elsewhere the button is inert.
    pub(super) fn button_intent(&self) -> Option<ControllerIntent> {
        match self {
            Self::Idle => Some(ControllerIntent::NewBook),
            Self::Ready { .. } => Some(ControllerIntent::Start),
            Self::Homing(_) | Self::Scanning(_) => Some(ControllerIntent::Stop),
            Self::Setup { .. } | Self::PageWidth { .. } => None,
        }
    }

    /// The status LED colour, with a latched Stop overriding everything.
    pub(super) fn led(&self, stop_latched: bool) -> Led {
        if stop_latched {
            return Led::Red;
        }
        match self {
            Self::Homing(_) | Self::Scanning(_) => Led::Amber,
            Self::Ready { .. } => Led::Green,
            Self::Setup { .. } | Self::Idle | Self::PageWidth { .. } => Led::Blue,
        }
    }

    /// Fold in the finish of a lift operation; true when it was this
    /// workflow's Home. A Stop is shown by the Stop status, so it leaves no
    /// Home failure of its own.
    pub(super) fn home_finished(&mut self, id: OperationId, outcome: &LiftOutcome) -> bool {
        if !matches!(self, Self::Homing(homing) if *homing == id) {
            return false;
        }
        let home_failure = match outcome {
            LiftOutcome::Completed(_) => {
                *self = Self::Idle;
                return true;
            }
            LiftOutcome::Fault(LiftFault::Stopped(_)) => None,
            LiftOutcome::Fault(LiftFault::HomeFailed(reason)) => {
                Some(format!("Home failed: {reason}"))
            }
            LiftOutcome::Fault(fault) => Some(format!("Home failed: {fault:?}")),
            LiftOutcome::Cancelled { .. } => Some("Home was cancelled".into()),
        };
        *self = Self::Setup { home_failure };
        true
    }
}

/// The in-memory Scan session and its first Capture cycle.
///
/// Sequence numbers are reserved, not advanced: #27 advances them once the
/// Capture cycle's Page records are durable.
pub(super) struct ScanSession {
    pub(super) directory: PathBuf,
    pub(super) page_width_mm: u16,
    /// The Left Page record's Sequence number; Right is the next odd one.
    pub(super) left_sequence: u32,
    /// The one-based Capture attempt within this Capture cycle.
    pub(super) attempt: u32,
    /// Attempt 1's first valid complete Capture pair.
    pub(super) canonical: Option<Arc<CapturePair>>,
    pub(super) capture: CaptureStatus,
}

impl ScanSession {
    pub(super) fn new(directory: PathBuf, page_width_mm: u16) -> Self {
        Self {
            directory,
            page_width_mm,
            left_sequence: 0,
            attempt: 1,
            canonical: None,
            capture: CaptureStatus::Capturing,
        }
    }
}

/// Create a new Scan session directory under `root`, named for the UTC
/// time `now`, with `-1`, `-2` and so on after it if that name is taken.
/// An existing directory is never reused.
pub(super) fn create_session_directory(root: &Path, now: SystemTime) -> io::Result<PathBuf> {
    let stamp = DateTime::<Utc>::from(now).format("%Y%m%dT%H%M%SZ");
    for suffix in 0u32.. {
        let name = match suffix {
            0 => stamp.to_string(),
            n => format!("{stamp}-{n}"),
        };
        let directory = root.join(name);
        match std::fs::create_dir(&directory) {
            Ok(()) => return Ok(directory),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    unreachable!("ran out of collision suffixes")
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    #[test]
    fn session_directory_is_utc_and_never_reuses_a_taken_name() {
        let root = tempfile::tempdir().unwrap();
        // 2026-11-13 14:25:30 UTC.
        let now = UNIX_EPOCH + Duration::from_secs(1_794_579_930);

        let first = create_session_directory(root.path(), now).unwrap();
        let second = create_session_directory(root.path(), now).unwrap();
        let third = create_session_directory(root.path(), now).unwrap();

        assert_eq!(first, root.path().join("20261113T142530Z"));
        assert_eq!(second, root.path().join("20261113T142530Z-1"));
        assert_eq!(third, root.path().join("20261113T142530Z-2"));
        assert!(third.is_dir());
    }

    #[test]
    fn button_means_new_book_start_or_stop_and_is_otherwise_inert() {
        let session = || ScanSession::new(PathBuf::new(), 200);
        let cases = [
            (Workflow::setup(), None),
            (
                Workflow::Homing(OperationId(1)),
                Some(ControllerIntent::Stop),
            ),
            (Workflow::Idle, Some(ControllerIntent::NewBook)),
            (Workflow::PageWidth { rejected_mm: None }, None),
            (
                Workflow::Ready {
                    page_width_mm: 200,
                    notice: None,
                },
                Some(ControllerIntent::Start),
            ),
            (Workflow::Scanning(session()), Some(ControllerIntent::Stop)),
        ];
        for (workflow, intent) in cases {
            assert_eq!(workflow.button_intent(), intent);
        }
    }

    #[test]
    fn led_is_blue_green_or_amber_and_red_under_stop() {
        let ready = Workflow::Ready {
            page_width_mm: 200,
            notice: None,
        };
        assert_eq!(Workflow::setup().led(false), Led::Blue);
        assert_eq!(Workflow::Idle.led(false), Led::Blue);
        assert_eq!(ready.led(false), Led::Green);
        assert_eq!(Workflow::Homing(OperationId(1)).led(false), Led::Amber);
        assert_eq!(
            Workflow::Scanning(ScanSession::new(PathBuf::new(), 200)).led(false),
            Led::Amber
        );
        assert_eq!(ready.led(true), Led::Red);
    }

    #[test]
    fn only_this_workflows_home_finishes_it() {
        let mut workflow = Workflow::Homing(OperationId(2));
        assert!(!workflow.home_finished(OperationId(1), &LiftOutcome::Completed(None)));
        assert!(matches!(workflow, Workflow::Homing(_)));

        assert!(workflow.home_finished(
            OperationId(2),
            &LiftOutcome::Fault(LiftFault::HomeFailed("not homed".into()))
        ));
        assert!(matches!(
            &workflow,
            Workflow::Setup { home_failure: Some(reason) } if reason == "Home failed: not homed"
        ));

        let mut workflow = Workflow::Homing(OperationId(3));
        workflow.home_finished(OperationId(3), &LiftOutcome::Completed(None));
        assert!(matches!(workflow, Workflow::Idle));
    }
}
