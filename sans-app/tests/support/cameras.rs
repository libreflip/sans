//! Scripted fake cameras behind the existing Machine factory seam.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use std::time::Duration;

use sans_core::{
    CameraCaptureError, CameraRole, CapturePairMachine, CapturedFrame, FibreConnection,
    FibreOpener, MachineBoard, MachineEvent, MachineFactory, OpenRequest, OpenedBoard,
    PreparedMachineProfile, RoleCamera, SetupBlocker, SetupDiagnostic,
};

use super::core_support::fake_fibre::{FakeFibre, FakeOpener, IS_HOMED, TRAJECTORY_DONE};

/// What one camera does on one capture call. The last step repeats.
#[derive(Clone, Debug)]
pub enum Step {
    /// Return this frame immediately.
    Frame(CapturedFrame),
    /// Wait until the gate opens, then return this frame.
    Gated(CapturedFrame, Gate),
    /// Fail with a retryable acquisition error.
    Fail(String),
}

/// A latch that holds gated captures until the test releases them.
///
/// There is no wall-clock deadline: a slow host, such as the emulated
/// AArch64 job, must not release a capture the test still holds. The
/// [`GateRelease`] opens the gate when the test calls it or drops it, so a
/// failing test never leaves the controller thread waiting.
#[derive(Clone, Debug)]
pub struct Gate(Arc<(Mutex<bool>, Condvar)>);

/// The test's handle on a [`Gate`]. Opens the gate when dropped.
#[derive(Debug)]
pub struct GateRelease(Gate);

impl Gate {
    /// A closed gate and the handle that opens it.
    pub fn closed() -> (Self, GateRelease) {
        let gate = Self(Arc::default());
        (gate.clone(), GateRelease(gate))
    }

    fn open(&self) {
        let (open, changed) = &*self.0;
        *open.lock().unwrap_or_else(PoisonError::into_inner) = true;
        changed.notify_all();
    }

    fn wait(&self) {
        let (open, changed) = &*self.0;
        let guard = open.lock().unwrap_or_else(PoisonError::into_inner);
        drop(
            changed
                .wait_while(guard, |open| !*open)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
}

impl GateRelease {
    /// Let every waiting and future gated capture finish.
    pub fn open(self) {}
}

impl Drop for GateRelease {
    fn drop(&mut self) {
        self.0.open();
    }
}

/// Capture call counts per role.
#[derive(Clone, Debug, Default)]
pub struct CaptureCalls {
    left: Arc<AtomicUsize>,
    right: Arc<AtomicUsize>,
}

impl CaptureCalls {
    /// Calls to the Left camera so far.
    pub fn left(&self) -> usize {
        self.left.load(Ordering::SeqCst)
    }

    /// Calls to the Right camera so far.
    pub fn right(&self) -> usize {
        self.right.load(Ordering::SeqCst)
    }
}

struct ScriptedCamera {
    role: CameraRole,
    steps: VecDeque<Step>,
    calls: Arc<AtomicUsize>,
}

impl RoleCamera for ScriptedCamera {
    fn role(&self) -> CameraRole {
        self.role
    }

    fn capture(&mut self) -> Result<CapturedFrame, CameraCaptureError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let step = if self.steps.len() > 1 {
            self.steps.pop_front().unwrap()
        } else {
            self.steps.front().cloned().expect("camera script is empty")
        };
        match step {
            Step::Frame(frame) => Ok(frame),
            Step::Gated(frame, gate) => {
                gate.wait();
                Ok(frame)
            }
            Step::Fail(detail) => Err(CameraCaptureError::acquisition(self.role, detail)),
        }
    }
}

/// A Machine factory that opens scripted cameras or reports Setup blockers.
pub enum FixtureFactory {
    Cameras {
        left: Vec<Step>,
        right: Vec<Step>,
        calls: CaptureCalls,
    },
    Blocked(Vec<String>),
    /// Scripted cameras, a Monospace that answers every command and a fake
    /// ODrive whose native Home ends cleanly, so the whole scan workflow runs.
    Scanner {
        left: Vec<Step>,
        right: Vec<Step>,
        calls: CaptureCalls,
        opener: FakeOpener,
    },
}

impl FixtureFactory {
    /// Scripted Left and Right cameras, plus their call counters.
    pub fn cameras(left: Vec<Step>, right: Vec<Step>) -> (Self, CaptureCalls) {
        let calls = CaptureCalls::default();
        (
            Self::Cameras {
                left,
                right,
                calls: calls.clone(),
            },
            calls,
        )
    }

    /// Like [`FixtureFactory::cameras`], with a Monospace and a lift axis that homes.
    pub fn scanner(left: Vec<Step>, right: Vec<Step>) -> (Self, CaptureCalls) {
        let mut board = FakeFibre::compatible();
        board.settle_after_one_read.insert(11, 1);
        board.set_bool(IS_HOMED, true);
        board.set_bool(TRAJECTORY_DONE, true);
        let calls = CaptureCalls::default();
        (
            Self::Scanner {
                left,
                right,
                calls: calls.clone(),
                opener: FakeOpener::new(board),
            },
            calls,
        )
    }
}

/// A Monospace that answers every command and never reports an event.
struct QuietBoard;

impl MachineBoard for QuietBoard {
    fn all_off(&mut self, _within: Duration) -> Result<(), String> {
        Ok(())
    }

    fn poll(&mut self) -> Option<MachineEvent> {
        None
    }

    fn set_light(&mut self, _on: bool) -> Result<(), String> {
        Ok(())
    }
}

/// These fixtures have no lift axis: every open reports no ODrive attached.
struct NoOdrive;

impl FibreOpener for NoOdrive {
    fn open(&mut self, _request: &OpenRequest) -> Result<FibreConnection, String> {
        Err("no ODrive in this fixture".into())
    }
}

impl MachineFactory for FixtureFactory {
    type Machine = CapturePairMachine;

    fn odrive_opener(&mut self) -> Box<dyn FibreOpener> {
        match self {
            Self::Scanner { opener, .. } => Box::new(opener.clone()),
            _ => Box::new(NoOdrive),
        }
    }

    fn open_board(&mut self, _profile: &PreparedMachineProfile) -> Option<OpenedBoard> {
        let Self::Scanner { .. } = self else {
            return None;
        };
        let board: Box<dyn MachineBoard> = Box::new(QuietBoard);
        Some(Ok((board, SetupDiagnostic::ready("Monospace", "fixture"))))
    }

    fn open(
        self,
        _profile: &PreparedMachineProfile,
    ) -> Result<(Self::Machine, Vec<SetupDiagnostic>), Vec<SetupBlocker>> {
        match self {
            Self::Cameras { left, right, calls }
            | Self::Scanner {
                left, right, calls, ..
            } => CapturePairMachine::new(
                Box::new(ScriptedCamera {
                    role: CameraRole::Left,
                    steps: left.into(),
                    calls: calls.left,
                }),
                Box::new(ScriptedCamera {
                    role: CameraRole::Right,
                    steps: right.into(),
                    calls: calls.right,
                }),
            )
            .map(|machine| {
                let ready = ["Left Camera", "Right Camera"]
                    .map(|component| SetupDiagnostic::ready(component, "Ready at fixture"));
                (machine, ready.into())
            })
            .map_err(|error| vec![SetupBlocker::new(error.to_string())]),
            Self::Blocked(reasons) => Err(reasons.into_iter().map(SetupBlocker::new).collect()),
        }
    }
}
