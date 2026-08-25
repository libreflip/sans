//! Controller-owned Machine state exposed as typed intents and snapshots.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use thiserror::Error;

use crate::camera::{CapturePair, CapturePairError};
use crate::{
    prepare_machine_profile, LigatureClient, LigatureCommand, LigatureEvent, LigatureRequest,
    LigatureSessionError, LigatureStatus, LigatureTransportError, LigatureWire,
    PreparedMachineProfile, SerialLigatureWire, StartupError,
};
#[cfg(target_os = "linux")]
use crate::{CapturePairMachine, V4lCameraMachineFactory};

const MAX_DEVICE_EVENTS_PER_TICK: usize = 32;

/// The complete set of UI-to-controller actions in the current Machine slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerIntent {
    /// Acquire one complete stationary Capture pair.
    CapturePair,
    /// Send one typed command through the controller-owned Ligature client.
    Ligature(LigatureCommand),
    /// Terminate the controller without starting more Machine work.
    Exit,
}

/// One operator-facing reason live Machine setup cannot proceed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupBlocker {
    /// A concise diagnostic suitable for the Setup screen.
    pub summary: String,
    kind: SetupBlockerKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SetupBlockerKind {
    General,
    LigatureTransport,
}

impl SetupBlocker {
    /// Create an operator-facing Setup blocker.
    pub fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            kind: SetupBlockerKind::General,
        }
    }

    fn ligature_transport(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            kind: SetupBlockerKind::LigatureTransport,
        }
    }
}

/// Live readiness shown by the non-actuating Setup screen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SetupState {
    /// Every adapter needed by this slice passed its live readiness gate.
    Ready,
    /// Ligature is connected for diagnostics but its production configuration is incomplete.
    Uncommissioned,
    /// Live devices or commissioning are unavailable, so actuation remains disabled.
    Blocked {
        /// Diagnostics explaining what must be corrected.
        reasons: Vec<SetupBlocker>,
    },
    /// Ligature could not establish or retain a trustworthy transport session.
    TransportFault {
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
#[derive(Clone, Debug, PartialEq)]
pub struct ControllerSnapshot {
    /// Monotonically increasing controller-state revision.
    pub revision: u64,
    /// The authoritative workflow projection for presentation.
    pub screen: MachineScreen,
    /// Latest controller-owned Ligature projection, if transport setup succeeded.
    pub ligature: Option<LigatureStatus>,
}

/// Asynchronous controller traffic that is not itself workflow state.
#[derive(Clone, Debug, PartialEq)]
pub enum ControllerEvent {
    /// One correlated reply or unsolicited message from Ligature.
    Ligature(LigatureEvent),
    /// The current readiness or operation lifecycle rejected a typed command.
    LigatureRejected(LigatureSessionError),
    /// The Ligature connection can no longer correlate commands safely.
    LigatureTransportFault(SetupBlocker),
}

/// Controller-thread-owned Machine behavior.
pub trait ControllerMachine: 'static {
    /// Acquire one complete pair or reject it without publishing a partial pair.
    fn capture_pair(&mut self) -> Result<CapturePair, CapturePairError>;
    /// Whether this adapter has the Camera roles needed for pair acquisition.
    fn capture_available(&self) -> bool {
        true
    }
    /// Report the Setup projection derived from controller-owned state.
    fn setup_state(&self) -> SetupState {
        SetupState::Ready
    }
    /// Return the current public Ligature status, if connected.
    fn ligature_status(&self) -> Option<LigatureStatus> {
        None
    }
    /// Send one typed Ligature command without waiting for its terminal.
    fn begin_ligature(
        &mut self,
        _command: LigatureCommand,
    ) -> Result<LigatureRequest, LigatureTransportError> {
        Err(LigatureTransportError::Closed)
    }
    /// Poll one Ligature event without blocking the controller loop.
    fn try_ligature_event(&mut self) -> Result<Option<LigatureEvent>, LigatureTransportError> {
        Ok(None)
    }
}

/// Controller adapter over a correlated Ligature client.
pub struct LigatureMachine<W> {
    client: LigatureClient<W>,
}

impl<W: LigatureWire> LigatureMachine<W> {
    /// Bind a correlated Ligature client to the Machine controller boundary.
    pub fn new(client: LigatureClient<W>) -> Self {
        Self { client }
    }
}

impl<W: LigatureWire> ControllerMachine for LigatureMachine<W> {
    fn capture_pair(&mut self) -> Result<CapturePair, CapturePairError> {
        Err(CapturePairError::Unavailable)
    }

    fn capture_available(&self) -> bool {
        false
    }

    fn setup_state(&self) -> SetupState {
        if self.client.session().scan_enabled() {
            SetupState::Ready
        } else {
            SetupState::Uncommissioned
        }
    }

    fn ligature_status(&self) -> Option<LigatureStatus> {
        Some(self.client.session().status().clone())
    }

    fn begin_ligature(
        &mut self,
        command: LigatureCommand,
    ) -> Result<LigatureRequest, LigatureTransportError> {
        self.client.begin(command)
    }

    fn try_ligature_event(&mut self) -> Result<Option<LigatureEvent>, LigatureTransportError> {
        self.client.try_event()
    }
}

/// Production factory that opens Ligature after Machine-profile validation.
#[derive(Clone, Copy, Debug, Default)]
pub struct LigatureMachineFactory;

impl MachineFactory for LigatureMachineFactory {
    type Machine = LigatureMachine<SerialLigatureWire>;

    fn open(self, profile: &PreparedMachineProfile) -> Result<Self::Machine, Vec<SetupBlocker>> {
        let query_timeout = Duration::from_millis(profile.profile().timeouts.command_ms);
        let path = &profile.profile().boards.ligature_path;
        let (wire, query) = SerialLigatureWire::open(path, query_timeout).map_err(|error| {
            vec![SetupBlocker::ligature_transport(format!(
                "Ligature connection failed: {error}"
            ))]
        })?;
        let client = LigatureClient::from_query(wire, &query).map_err(|error| {
            vec![SetupBlocker::ligature_transport(format!(
                "Ligature readiness query failed: {error}"
            ))]
        })?;
        Ok(LigatureMachine::new(client))
    }
}

/// Controller-owned production adapters for Camera capture and Ligature.
#[cfg(target_os = "linux")]
pub struct SansMachine {
    cameras: CapturePairMachine,
    ligature: LigatureMachine<SerialLigatureWire>,
}

#[cfg(target_os = "linux")]
impl ControllerMachine for SansMachine {
    fn capture_pair(&mut self) -> Result<CapturePair, CapturePairError> {
        self.cameras.capture_pair()
    }

    fn setup_state(&self) -> SetupState {
        self.ligature.setup_state()
    }

    fn ligature_status(&self) -> Option<LigatureStatus> {
        self.ligature.ligature_status()
    }

    fn begin_ligature(
        &mut self,
        command: LigatureCommand,
    ) -> Result<LigatureRequest, LigatureTransportError> {
        self.ligature.begin_ligature(command)
    }

    fn try_ligature_event(&mut self) -> Result<Option<LigatureEvent>, LigatureTransportError> {
        self.ligature.try_ligature_event()
    }
}

/// Production factory for all controller-owned Machine adapters.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Default)]
pub struct SansMachineFactory;

#[cfg(target_os = "linux")]
impl MachineFactory for SansMachineFactory {
    type Machine = SansMachine;

    fn open(self, profile: &PreparedMachineProfile) -> Result<Self::Machine, Vec<SetupBlocker>> {
        let cameras = V4lCameraMachineFactory.open(profile);
        let ligature = LigatureMachineFactory.open(profile);
        match (cameras, ligature) {
            (Ok(cameras), Ok(ligature)) => Ok(SansMachine { cameras, ligature }),
            (Err(mut camera), Err(mut ligature)) => {
                camera.append(&mut ligature);
                Err(camera)
            }
            (Err(blockers), Ok(_)) | (Ok(_), Err(blockers)) => Err(blockers),
        }
    }
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
    events: Receiver<ControllerEvent>,
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

    /// Poll asynchronous Ligature traffic without blocking a render loop.
    pub fn try_event(&self) -> Result<ControllerEvent, TryRecvError> {
        self.events.try_recv()
    }

    /// Wait for one controller event with a bounded deadline.
    pub fn recv_event_timeout(
        &self,
        timeout: Duration,
    ) -> Result<ControllerEvent, RecvTimeoutError> {
        self.events.recv_timeout(timeout)
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
    let (event_sender, event_receiver) = mpsc::channel();
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
                        screen: MachineScreen::Setup(setup_failure(reasons)),
                        ligature: None,
                    });
                    wait_for_exit(&intent_receiver, &snapshot_sender, revision);
                    return;
                }
            };

            let mut latest_complete_pair = None;
            let mut capture_status = CaptureStatus::Ready;
            if snapshot_sender
                .send(ControllerSnapshot {
                    revision,
                    screen: project_screen(&machine, &capture_status, None),
                    ligature: machine.ligature_status(),
                })
                .is_err()
            {
                return;
            }

            loop {
                let mut transport_failure = None;
                for _ in 0..MAX_DEVICE_EVENTS_PER_TICK {
                    match machine.try_ligature_event() {
                        Ok(Some(event)) => {
                            let status_changed = matches!(event, LigatureEvent::Status(_));
                            if event_sender.send(ControllerEvent::Ligature(event)).is_err() {
                                return;
                            }
                            if status_changed {
                                revision += 1;
                                if snapshot_sender
                                    .send(ControllerSnapshot {
                                        revision,
                                        screen: project_screen(
                                            &machine,
                                            &capture_status,
                                            latest_complete_pair.clone(),
                                        ),
                                        ligature: machine.ligature_status(),
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            transport_failure = Some(error);
                            break;
                        }
                    }
                }
                if let Some(error) = transport_failure {
                    let blocker = SetupBlocker::ligature_transport(error.to_string());
                    let _ =
                        event_sender.send(ControllerEvent::LigatureTransportFault(blocker.clone()));
                    revision += 1;
                    let _ = snapshot_sender.send(ControllerSnapshot {
                        revision,
                        screen: MachineScreen::Setup(SetupState::TransportFault {
                            reasons: vec![blocker],
                        }),
                        ligature: None,
                    });
                    wait_for_exit(&intent_receiver, &snapshot_sender, revision);
                    return;
                }

                match intent_receiver.recv_timeout(Duration::from_millis(5)) {
                    Ok(ControllerIntent::CapturePair)
                        if machine.capture_available()
                            && machine.setup_state() == SetupState::Ready =>
                    {
                        revision += 1;
                        capture_status = CaptureStatus::Capturing;
                        if snapshot_sender
                            .send(ControllerSnapshot {
                                revision,
                                screen: capture_preview(
                                    capture_status.clone(),
                                    latest_complete_pair.clone(),
                                ),
                                ligature: machine.ligature_status(),
                            })
                            .is_err()
                        {
                            return;
                        }
                        revision += 1;
                        capture_status = match machine.capture_pair() {
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
                                screen: capture_preview(
                                    capture_status.clone(),
                                    latest_complete_pair.clone(),
                                ),
                                ligature: machine.ligature_status(),
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Ok(ControllerIntent::CapturePair) => {}
                    Ok(ControllerIntent::Ligature(command)) => {
                        if let Err(error) = machine.begin_ligature(command) {
                            match error {
                                LigatureTransportError::Session(
                                    error @ (LigatureSessionError::Busy
                                    | LigatureSessionError::CommissioningOnly),
                                ) => {
                                    let _ =
                                        event_sender.send(ControllerEvent::LigatureRejected(error));
                                }
                                error => {
                                    let blocker =
                                        SetupBlocker::ligature_transport(error.to_string());
                                    let _ = event_sender.send(
                                        ControllerEvent::LigatureTransportFault(blocker.clone()),
                                    );
                                    revision += 1;
                                    let _ = snapshot_sender.send(ControllerSnapshot {
                                        revision,
                                        screen: MachineScreen::Setup(SetupState::TransportFault {
                                            reasons: vec![blocker],
                                        }),
                                        ligature: None,
                                    });
                                    wait_for_exit(&intent_receiver, &snapshot_sender, revision);
                                    return;
                                }
                            }
                        }
                    }
                    Ok(ControllerIntent::Exit) => {
                        revision += 1;
                        let _ = snapshot_sender.send(ControllerSnapshot {
                            revision,
                            screen: MachineScreen::Exited,
                            ligature: None,
                        });
                        return;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .map_err(StartupError::ControllerThreadStart)?;

    Ok(ControllerHandle {
        intents: intent_sender,
        snapshots: snapshot_receiver,
        events: event_receiver,
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
                ligature: None,
            });
            return;
        }
    }
}

fn setup_failure(reasons: Vec<SetupBlocker>) -> SetupState {
    if reasons
        .iter()
        .any(|reason| reason.kind == SetupBlockerKind::LigatureTransport)
    {
        SetupState::TransportFault { reasons }
    } else {
        SetupState::Blocked { reasons }
    }
}

fn project_screen(
    machine: &impl ControllerMachine,
    capture_status: &CaptureStatus,
    latest_complete_pair: Option<Arc<CapturePair>>,
) -> MachineScreen {
    match machine.setup_state() {
        SetupState::Ready if machine.capture_available() => {
            capture_preview(capture_status.clone(), latest_complete_pair)
        }
        setup => MachineScreen::Setup(setup),
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
