//! Scripted fake cameras behind the existing Machine factory seam.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use sans_core::{
    CameraCaptureError, CameraRole, CapturePairMachine, CapturedFrame, MachineFactory,
    PreparedMachineProfile, RoleCamera, SetupBlocker, SetupDiagnostic,
};

/// What one camera does on one capture call. The last step repeats.
#[derive(Clone, Debug)]
pub enum Step {
    /// Return this frame immediately.
    Frame(CapturedFrame),
    /// Wait for [`Gate::open`], then return this frame.
    Gated(CapturedFrame, Gate),
    /// Fail with a retryable acquisition error.
    Fail(String),
}

/// A latch a test opens to let a gated capture finish.
#[derive(Clone, Debug, Default)]
pub struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    /// Let every waiting and future gated capture finish.
    pub fn open(&self) {
        let (open, changed) = &*self.0;
        *open.lock().unwrap() = true;
        changed.notify_all();
    }

    fn wait(&self) {
        let (open, changed) = &*self.0;
        let guard = open.lock().unwrap();
        let (guard, timeout) = changed
            .wait_timeout_while(guard, Duration::from_secs(10), |open| !*open)
            .unwrap();
        assert!(
            *guard && !timeout.timed_out(),
            "gated capture never released"
        );
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
}

impl MachineFactory for FixtureFactory {
    type Machine = CapturePairMachine;

    fn open(
        self,
        _profile: &PreparedMachineProfile,
    ) -> Result<(Self::Machine, Vec<SetupDiagnostic>), Vec<SetupBlocker>> {
        match self {
            Self::Cameras { left, right, calls } => CapturePairMachine::new(
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
