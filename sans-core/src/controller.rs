//! Controller-owned Machine state exposed as typed intents and snapshots.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use thiserror::Error;

use crate::camera::{CapturePair, CapturePairError};
use crate::{prepare_machine_profile, PreparedMachineProfile, StartupError};

/// The complete set of UI-to-controller actions in the Capture-pair slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerIntent {
    /// Acquire one complete stationary Capture pair.
    CapturePair,
    /// Terminate the controller without starting more Machine work.
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
    /// Live devices or commissioning are unavailable, so actuation remains disabled.
    Blocked {
        /// Diagnostics explaining what must be corrected.
        reasons: Vec<SetupBlocker>,
    },
}

/// Current state of complete-pair acquisition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CaptureStatus {
    /// Camera roles are ready for a stationary acquisition.
    Ready,
    /// The controller is acquiring or retrying a pair.
    Capturing,
    /// The pair failed and scanning remains blocked until an explicit retry.
    Blocked {
        /// Operator-facing Camera diagnostic.
        reason: String,
    },
}

/// Capture state and the latest complete pair suitable for presentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturePreview {
    /// Current acquisition state.
    pub status: CaptureStatus,
    /// Latest complete pair. It remains unchanged during capture and retry.
    pub latest_complete_pair: Option<Arc<CapturePair>>,
    /// Prominent nonblocking warning for optional 100% preview degradation.
    pub preview_warning: Option<String>,
}

/// Authoritative workflow projection rendered by the native UI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MachineScreen {
    /// The non-actuating live-readiness screen.
    Setup(SetupState),
    /// Complete-pair acquisition and its latest presentation-safe result.
    CapturePreview(CapturePreview),
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

/// Controller-thread-owned Machine behavior.
pub trait ControllerMachine: 'static {
    /// Acquire one complete pair or reject it without publishing a partial pair.
    fn capture_pair(&mut self) -> Result<CapturePair, CapturePairError>;
}

/// A deferred constructor for controller-thread-owned Machine resources.
pub trait MachineFactory: Send + 'static {
    /// Live Machine resources retained exclusively by the controller thread.
    type Machine: ControllerMachine;

    /// Open live resources after structural startup checks and report nonfatal Setup blockers.
    fn open(self, profile: &PreparedMachineProfile) -> Result<Self::Machine, Vec<SetupBlocker>>;
}

/// An intent channel that also exposes the latest controller projections.
pub struct ControllerHandle {
    intents: Sender<ControllerIntent>,
    snapshots: Receiver<ControllerSnapshot>,
    accepting_intents: Arc<Mutex<bool>>,
    controller_thread: Option<JoinHandle<()>>,
}

impl ControllerHandle {
    /// Queue an intent without waiting for device or storage work.
    pub fn send(&self, intent: ControllerIntent) -> Result<(), ControllerClosed> {
        let mut accepting_intents = self
            .accepting_intents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*accepting_intents {
            return Err(ControllerClosed);
        }
        if intent == ControllerIntent::Exit {
            *accepting_intents = false;
        }
        self.intents.send(intent).map_err(|_| {
            *accepting_intents = false;
            ControllerClosed
        })
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
        let _ = self.send(ControllerIntent::Exit);
        if self
            .controller_thread
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            if let Some(thread) = self.controller_thread.take() {
                let _ = thread.join();
            }
        }
    }
}

/// The controller is no longer accepting intents.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("the Machine controller is closed")]
pub struct ControllerClosed;

/// Validate startup completely, then transfer the Machine adapter to one controller thread.
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
    let accepting_intents = Arc::new(Mutex::new(true));
    let worker_accepting_intents = Arc::clone(&accepting_intents);
    let controller_thread = thread::Builder::new()
        .name("sans-controller".into())
        .spawn(move || {
            struct MarkControllerClosed(Arc<Mutex<bool>>);

            impl Drop for MarkControllerClosed {
                fn drop(&mut self) {
                    *self
                        .0
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
                }
            }

            let _closed_on_return = MarkControllerClosed(worker_accepting_intents);
            let mut revision = 0;
            let mut machine = match factory.open(&profile) {
                Ok(machine) => machine,
                Err(reasons) => {
                    let _ = snapshot_sender.send(ControllerSnapshot {
                        revision,
                        screen: MachineScreen::Setup(SetupState::Blocked { reasons }),
                    });
                    wait_for_exit(&intent_receiver, &snapshot_sender, revision);
                    return;
                }
            };

            let mut latest_complete_pair = None;
            if snapshot_sender
                .send(ControllerSnapshot {
                    revision,
                    screen: capture_preview(CaptureStatus::Ready, None),
                })
                .is_err()
            {
                return;
            }

            while let Ok(intent) = intent_receiver.recv() {
                revision += 1;
                match intent {
                    ControllerIntent::CapturePair => {
                        if snapshot_sender
                            .send(ControllerSnapshot {
                                revision,
                                screen: capture_preview(
                                    CaptureStatus::Capturing,
                                    latest_complete_pair.clone(),
                                ),
                            })
                            .is_err()
                        {
                            return;
                        }
                        revision += 1;
                        let status = match machine.capture_pair() {
                            Ok(pair) => {
                                latest_complete_pair = Some(Arc::new(pair));
                                CaptureStatus::Ready
                            }
                            Err(error) => CaptureStatus::Blocked {
                                reason: error.to_string(),
                            },
                        };
                        if snapshot_sender
                            .send(ControllerSnapshot {
                                revision,
                                screen: capture_preview(status, latest_complete_pair.clone()),
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    ControllerIntent::Exit => {
                        let _ = snapshot_sender.send(ControllerSnapshot {
                            revision,
                            screen: MachineScreen::Exited,
                        });
                        return;
                    }
                }
            }
        })
        .map_err(StartupError::ControllerThreadStart)?;

    Ok(ControllerHandle {
        intents: intent_sender,
        snapshots: snapshot_receiver,
        accepting_intents,
        controller_thread: Some(controller_thread),
    })
}

fn wait_for_exit(
    intent_receiver: &Receiver<ControllerIntent>,
    snapshot_sender: &Sender<ControllerSnapshot>,
    revision: u64,
) {
    while let Ok(intent) = intent_receiver.recv() {
        if intent == ControllerIntent::Exit {
            let _ = snapshot_sender.send(ControllerSnapshot {
                revision: revision + 1,
                screen: MachineScreen::Exited,
            });
            return;
        }
    }
}

fn capture_preview(
    status: CaptureStatus,
    latest_complete_pair: Option<Arc<CapturePair>>,
) -> MachineScreen {
    let preview_warning = latest_complete_pair
        .as_deref()
        .and_then(native_detail_preview_warning);
    MachineScreen::CapturePreview(CapturePreview {
        status,
        latest_complete_pair,
        preview_warning,
    })
}

fn native_detail_preview_warning(pair: &CapturePair) -> Option<String> {
    let mut failures = Vec::new();
    if let Err(error) = pair.left.native_detail_preview() {
        failures.push(format!("Left 100% preview is unavailable: {error}"));
    }
    if let Err(error) = pair.right.native_detail_preview() {
        failures.push(format!("Right 100% preview is unavailable: {error}"));
    }
    (!failures.is_empty()).then(|| failures.join("; "))
}
