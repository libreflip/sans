//! Controller-owned Machine state exposed as typed intents and snapshots.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use thiserror::Error;

use crate::{prepare_machine_profile, PreparedMachineProfile, StartupError};

/// The complete set of UI-to-controller actions in the bootstrap slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerIntent {
    /// Terminate the controller without starting machine work.
    Exit,
}

/// One operator-facing reason live Machine setup cannot proceed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupBlocker {
    /// A concise diagnostic suitable for the Setup screen.
    pub summary: String,
}

impl SetupBlocker {
    /// Create an operator-facing Setup blocker.
    pub fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
        }
    }
}

/// Live readiness shown by the non-actuating Setup screen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SetupState {
    /// Every adapter needed by this slice passed its live readiness gate.
    Ready,
    /// Live devices or commissioning are unavailable, so actuation remains disabled.
    Blocked {
        /// Diagnostics explaining what must be corrected.
        reasons: Vec<SetupBlocker>,
    },
}

/// Authoritative workflow projection rendered by the native UI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MachineScreen {
    /// The non-actuating live-readiness screen.
    Setup(SetupState),
    /// The controller accepted `Exit` and has stopped processing intents.
    Exited,
}

/// A revisioned projection of controller-owned state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControllerSnapshot {
    /// Monotonically increasing controller-state revision.
    pub revision: u64,
    /// The authoritative workflow projection for presentation.
    pub screen: MachineScreen,
}

/// A deferred constructor for controller-thread-owned Machine resources.
pub trait MachineFactory: Send + 'static {
    /// Live Machine resources retained exclusively by the controller thread.
    type Machine: Send + 'static;

    /// Open live resources after structural startup checks and report nonfatal Setup blockers.
    fn open(self, profile: &PreparedMachineProfile) -> Result<Self::Machine, Vec<SetupBlocker>>;
}

/// An intent channel that also exposes the latest controller projections.
pub struct ControllerHandle {
    intents: Sender<ControllerIntent>,
    snapshots: Receiver<ControllerSnapshot>,
    controller_thread: Option<JoinHandle<()>>,
}

impl ControllerHandle {
    /// Queue an intent without waiting for device or storage work.
    pub fn send(&self, intent: ControllerIntent) -> Result<(), ControllerClosed> {
        self.intents.send(intent).map_err(|_| ControllerClosed)
    }

    /// Poll a snapshot without blocking a render loop.
    pub fn try_snapshot(&self) -> Result<ControllerSnapshot, TryRecvError> {
        self.snapshots.try_recv()
    }

    /// Wait for a projection with a bounded deadline.
    ///
    /// This is intended for deterministic tests and non-rendering callers.
    pub fn recv_snapshot_timeout(
        &self,
        timeout: Duration,
    ) -> Result<ControllerSnapshot, RecvTimeoutError> {
        self.snapshots.recv_timeout(timeout)
    }
}

impl Drop for ControllerHandle {
    fn drop(&mut self) {
        let _ = self.intents.send(ControllerIntent::Exit);
        if let Some(thread) = self.controller_thread.take() {
            let _ = thread.join();
        }
    }
}

/// The controller is no longer accepting intents.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("the Machine controller is closed")]
pub struct ControllerClosed;

/// Validate startup completely, then transfer the machine adapter to one controller thread.
pub fn bootstrap(
    explicit_profile_path: Option<&Path>,
    factory: impl MachineFactory,
) -> Result<ControllerHandle, StartupError> {
    let profile = prepare_machine_profile(explicit_profile_path)?;
    spawn_controller(profile, factory)
}

fn spawn_controller(
    profile: PreparedMachineProfile,
    factory: impl MachineFactory,
) -> Result<ControllerHandle, StartupError> {
    let (intent_sender, intent_receiver) = mpsc::channel();
    let (snapshot_sender, snapshot_receiver) = mpsc::channel();
    let controller_thread = thread::Builder::new()
        .name("sans-controller".into())
        .spawn(move || {
            let (setup, _machine) = match factory.open(&profile) {
                Ok(machine) => (SetupState::Ready, Some(machine)),
                Err(reasons) => (SetupState::Blocked { reasons }, None),
            };
            if snapshot_sender
                .send(ControllerSnapshot {
                    revision: 0,
                    screen: MachineScreen::Setup(setup),
                })
                .is_err()
            {
                return;
            }

            if let Ok(ControllerIntent::Exit) = intent_receiver.recv() {
                let _ = snapshot_sender.send(ControllerSnapshot {
                    revision: 1,
                    screen: MachineScreen::Exited,
                });
            }
        })
        .map_err(StartupError::ControllerThreadStart)?;

    Ok(ControllerHandle {
        intents: intent_sender,
        snapshots: snapshot_receiver,
        controller_thread: Some(controller_thread),
    })
}
