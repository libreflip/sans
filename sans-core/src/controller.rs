//! Controller-owned Machine state exposed as typed intents and snapshots.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use thiserror::Error;

use crate::camera::{CapturePair, CapturePairError};
use crate::lift::{
    LiftEvent, LiftFault, LiftOperations, LiftOutcome, LiftRejection, LiftRequest, OperationId,
    StopCause, VacuumEvidence,
};
use crate::odrive::{
    spawn_link, DiscoveryRecord, FibreOpener, LinkCommand, LinkConfig, LinkEvent, LinkHandle,
    LinkPhase, LinkRunner, LinkStatus, Priority, TelemetrySample, TelemetryValues, Ticket,
};
use crate::stop::{Relays, StopControl, StopStatus, StopVerdict};
use crate::{prepare_machine_profile, PreparedMachineProfile, StartupError};

mod machine;
mod workflow;

use machine::{setup_state, spawn_machine, MachineRequest};
use workflow::{create_session_directory, ScanSession, Workflow};

/// Telemetry-only snapshot changes publish at most four times a second.
const TELEMETRY_PUBLISH_INTERVAL: Duration = Duration::from_millis(250);

/// The complete set of UI-to-controller actions.
///
/// `Home` is the only intent that moves the lift axis, and only with the
/// operator's explicit consent. No intent arms or moves it otherwise. Only
/// `ClearStop` clears its errors, and only after a Confirmed Stop. The
/// physical button sends `NewBook`, `Start` or `Stop` by state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerIntent {
    /// Latch Stop: urgently disarm the lift axis and switch Monospace `ALL OFF`.
    Stop,
    /// Clear a Confirmed Stop's lift-axis errors and latch. Never arms.
    ClearStop,
    /// Acquire one complete stationary Capture pair for diagnostics, in Setup only.
    CapturePair,
    /// Consent to native Home. Accepted in Setup while the live gates pass.
    Home,
    /// Prepare a new book: go to page-width entry from Idle or Ready.
    NewBook,
    /// Enter the book's page width in whole millimetres.
    SetPageWidth {
        /// Page width in millimetres, checked against the Machine profile.
        millimetres: u16,
    },
    /// Create the Scan session from Ready and capture its first pair.
    Start,
    /// End the current lift-axis connection and run the connection gate once more.
    ReconnectLiftAxis,
    /// Terminate the controller without starting more Machine work.
    Exit,
}

/// Lift-axis link state projected into every snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct LiftAxisStatus {
    /// Connection epoch; 0 before the first attempt.
    pub epoch: u64,
    /// Disconnected, connecting, blocked, verified or faulted.
    pub phase: LinkPhase,
    /// Every reason the lift axis is not verified.
    pub blockers: Vec<String>,
    /// What the current connection discovered, for commissioning notes.
    pub discovery: Option<DiscoveryRecord>,
    /// Latest current-epoch telemetry and its freshness.
    pub telemetry: Option<TelemetrySummary>,
    /// Replies that matched no pending request.
    pub unmatched_replies: u64,
}

impl LiftAxisStatus {
    fn starting() -> Self {
        Self {
            epoch: 0,
            phase: LinkPhase::Disconnected,
            blockers: vec!["the lift-axis link has not started".into()],
            discovery: None,
            telemetry: None,
            unmatched_replies: 0,
        }
    }

    fn unavailable(reason: String) -> Self {
        Self {
            blockers: vec![reason],
            ..Self::starting()
        }
    }
}

/// The latest telemetry sample as presented, with its age when published.
#[derive(Clone, Debug, PartialEq)]
pub struct TelemetrySummary {
    /// Decoded values. Display only; consumers needing evidence use the link handle.
    pub values: TelemetryValues,
    /// Sample age in milliseconds when this snapshot was published.
    pub age_ms: u64,
    /// Whether the sample was within the profile's maximum age when published.
    pub fresh: bool,
    /// Measured duration of the sample's telemetry cycle in milliseconds.
    pub cycle_ms: u64,
}

/// Whether the live gates allow Home and scanning. Position trust is
/// separate: the workflow screen shows whether Home is still needed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScanReadiness {
    /// A live gate fails, so Home and Start are refused.
    Disabled {
        /// Why scanning is unavailable.
        reason: String,
    },
    /// Cameras and Monospace are live, the lift axis is verified and no Stop
    /// is latched.
    Enabled,
}

/// One operator-facing reason live Machine setup cannot proceed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupBlocker {
    /// A concise diagnostic suitable for the Setup screen.
    pub summary: String,
}

/// One live readiness result shown on the Setup screen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupDiagnostic {
    /// Hardware or adapter whose gate passed.
    pub component: String,
    /// Current-connection evidence suitable for an operator or maintainer.
    pub summary: String,
}

impl SetupDiagnostic {
    /// Record a successful live readiness gate.
    pub fn ready(component: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            component: component.into(),
            summary: summary.into(),
        }
    }
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
    Ready {
        /// Current-connection evidence for each ready adapter.
        diagnostics: Vec<SetupDiagnostic>,
    },
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

/// Page-width entry, with the Machine profile's bounds in whole millimetres.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageWidthEntry {
    /// Smallest accepted page width.
    pub minimum_mm: u16,
    /// Largest accepted page width.
    pub maximum_mm: u16,
    /// The last entry outside the bounds.
    pub rejected_mm: Option<u16>,
}

/// A running Scan session and its current Capture cycle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanView {
    /// The Scan session directory under the storage root.
    pub directory: PathBuf,
    /// The operator-entered page width.
    pub page_width_mm: u16,
    /// The one-based Capture cycle number.
    pub capture_cycle: u32,
    /// The Left Page record's reserved Sequence number (even).
    pub left_sequence: u32,
    /// The Right Page record's reserved Sequence number (odd).
    pub right_sequence: u32,
    /// The one-based Capture attempt within the Capture cycle.
    pub attempt: u32,
    /// Capture state and the canonical Capture pair, once acquired.
    pub capture: CapturePreview,
}

/// The status LED colour, projected from the workflow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Led {
    /// Setup, Home consent, Idle and page-width entry.
    Blue,
    /// Ready.
    Green,
    /// Home or scanning in progress.
    Amber,
    /// Stop latched.
    Red,
}

/// Authoritative workflow projection rendered by the native UI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MachineScreen {
    /// The non-actuating live-readiness screen. Home consent is given here.
    Setup(SetupState),
    /// Diagnostic complete-pair acquisition and its latest presentation-safe result.
    CapturePreview(CapturePreview),
    /// Native Home is running.
    Homing,
    /// Position trust is held; New Book is next.
    Idle,
    /// Page-width entry.
    PageWidth(PageWidthEntry),
    /// The page width is accepted; Start is next.
    Ready {
        /// The accepted page width.
        page_width_mm: u16,
        /// Why the last Start did not create a Scan session.
        notice: Option<String>,
    },
    /// A Scan session is running.
    Scanning(ScanView),
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
    /// Lift-axis link status, independent of the Camera screen.
    pub lift_axis: LiftAxisStatus,
    /// Whether the live gates allow Home and scanning.
    pub scan_readiness: ScanReadiness,
    /// The latched Stop, if any.
    pub stop: Option<StopStatus>,
    /// The latest Monospace pressure reading in millibar, if any arrived.
    pub pressure_mbar: Option<f32>,
}

/// Machine-thread-owned Camera behavior. Blocking work runs here, never on
/// the controller thread, so it cannot delay Stop.
pub trait ControllerMachine: 'static {
    /// Acquire one complete pair or reject it without publishing a partial pair.
    fn capture_pair(&mut self) -> Result<CapturePair, CapturePairError>;

    /// Acquire a Scan session's pair. The controller has already switched
    /// the light on and it stays on, so this must not switch it off.
    fn capture_session_pair(&mut self) -> Result<CapturePair, CapturePairError> {
        self.capture_pair()
    }
}

/// The relay board as the Stop path uses it. It is owned by the controller
/// thread, beside the lift axis, so no Camera work can delay it.
pub trait MachineBoard: Send + 'static {
    /// Send `ALL OFF` and wait at most `within` for its exact `OK`.
    fn all_off(&mut self, within: Duration) -> Result<(), String>;

    /// The next button press, pressure reading or failure, without blocking.
    fn poll(&mut self) -> Option<MachineEvent>;

    /// Send ordinary `LIGHT ON` and wait for its `OK`.
    fn light_on(&mut self) -> Result<(), String> {
        Err("this board cannot switch the light".into())
    }

    /// Show `led` on the status LED. The LED is only a projection, so a
    /// board without one may ignore it.
    fn set_led(&mut self, _led: Led) -> Result<(), String> {
        Ok(())
    }
}

/// The board as the factory opened it, or why it did not open.
pub type OpenedBoard = Result<(Box<dyn MachineBoard>, SetupDiagnostic), SetupBlocker>;

/// Something a live board or Machine reported between intents.
#[derive(Clone, Debug, PartialEq)]
pub enum MachineEvent {
    /// The physical button was pressed. It means New Book, Start or Stop
    /// by workflow state, and nothing elsewhere.
    Button,
    /// A Monospace pressure reading in millibar.
    Pressure(f32),
    /// A live resource failed. A lost board also latches Stop.
    Lost(SetupBlocker),
}

/// A deferred constructor for live Machine resources.
pub trait MachineFactory: Send + 'static {
    /// Live Camera resources retained exclusively by the Machine thread.
    type Machine: ControllerMachine;

    /// Hand over the ODrive device opener. Called once on the controller
    /// thread, only after the profile passed validation; no USB device is
    /// touched before then. Production returns the USB opener.
    fn odrive_opener(&mut self) -> Box<dyn FibreOpener>;

    /// Open the relay board, on the Machine thread before [`Self::open`].
    /// The board then moves to the controller thread. `None` means there is
    /// no board, so every Stop is Unconfirmed.
    fn open_board(&mut self, _profile: &PreparedMachineProfile) -> Option<OpenedBoard> {
        None
    }

    /// Open live resources after structural startup checks and report
    /// nonfatal Setup blockers. Runs on the Machine thread.
    fn open(
        self,
        profile: &PreparedMachineProfile,
    ) -> Result<(Self::Machine, Vec<SetupDiagnostic>), Vec<SetupBlocker>>;

    /// Poll Machine-thread resources for a live failure.
    fn poll_machine(_machine: &mut Self::Machine) -> Option<MachineEvent> {
        None
    }
}

#[derive(Default)]
struct SnapshotState {
    pending: Option<ControllerSnapshot>,
    closed: bool,
}

/// A single pending projection. Publishing replaces and releases its predecessor.
#[derive(Default)]
struct LatestSnapshot {
    state: Mutex<SnapshotState>,
    changed: Condvar,
}

impl LatestSnapshot {
    fn send(&self, snapshot: ControllerSnapshot) -> Result<(), ControllerClosed> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(ControllerClosed);
        }
        let previous = state.pending.replace(snapshot);
        drop(state);
        self.changed.notify_one();
        drop(previous);
        Ok(())
    }

    fn try_recv(&self) -> Result<ControllerSnapshot, TryRecvError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.pending.take().ok_or(if state.closed {
            TryRecvError::Disconnected
        } else {
            TryRecvError::Empty
        })
    }

    fn recv_timeout(&self, timeout: Duration) -> Result<ControllerSnapshot, RecvTimeoutError> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| {
                state.pending.is_none() && !state.closed
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.pending.take().ok_or(if state.closed {
            RecvTimeoutError::Disconnected
        } else {
            RecvTimeoutError::Timeout
        })
    }

    fn close(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closed = true;
        self.changed.notify_all();
    }
}

/// An intent channel that also exposes the latest controller projection.
pub struct ControllerHandle {
    intents: Sender<Inbox>,
    snapshots: Arc<LatestSnapshot>,
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
        self.intents.send(Inbox::Intent(intent)).map_err(|_| {
            *accepting_intents = false;
            ControllerClosed
        })
    }

    /// Take the latest pending snapshot without blocking a render loop.
    ///
    /// Intermediate projections are replaced when the consumer falls behind.
    pub fn try_snapshot(&self) -> Result<ControllerSnapshot, TryRecvError> {
        self.snapshots.try_recv()
    }

    /// Wait for the latest pending projection with a bounded deadline.
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
        self.snapshots.close();
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

/// One controller inbox for intents, lift-axis link events and Machine-thread results.
pub(super) enum Inbox {
    Intent(ControllerIntent),
    Link(LinkEvent),
    Board(OpenedBoard),
    /// A button press or failure the controller polled from its own board.
    BoardEvent(MachineEvent),
    Cameras(Result<Vec<SetupDiagnostic>, Vec<SetupBlocker>>),
    Captured(Result<CapturePair, CapturePairError>),
    Machine(MachineEvent),
    MachinePanicked,
}

impl From<LinkEvent> for Inbox {
    fn from(event: LinkEvent) -> Self {
        Self::Link(event)
    }
}

/// How often the controller polls live Machine resources for Setup failures.
pub(super) const SETUP_POLL_INTERVAL: Duration = Duration::from_millis(16);

fn spawn_controller<Factory: MachineFactory>(
    profile: PreparedMachineProfile,
    mut factory: Factory,
) -> Result<ControllerHandle, StartupError> {
    let (intent_sender, inbox) = mpsc::channel();
    let link_events = intent_sender.clone();
    let machine_events = intent_sender.clone();
    let snapshots = Arc::new(LatestSnapshot::default());
    let snapshot_sender = Arc::clone(&snapshots);
    let accepting_intents = Arc::new(Mutex::new(true));
    let worker_accepting_intents = Arc::clone(&accepting_intents);
    let controller_thread = thread::Builder::new()
        .name("sans-controller".into())
        .spawn(move || {
            struct MarkControllerClosed(Arc<Mutex<bool>>, Arc<LatestSnapshot>);

            impl Drop for MarkControllerClosed {
                fn drop(&mut self) {
                    *self
                        .0
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
                    self.1.close();
                }
            }

            let _closed_on_return =
                MarkControllerClosed(worker_accepting_intents, Arc::clone(&snapshot_sender));
            let profile = Arc::new(profile);
            // The ODrive opener is requested only after the profile validated,
            // and before live Camera and board resources are opened.
            let mut lift = LiftAxis::start(&profile, factory.odrive_opener(), link_events);
            let machine = spawn_machine(Arc::clone(&profile), factory, machine_events);
            let mut board: Option<Box<dyn MachineBoard>> = None;
            let mut board_setup: Option<Result<SetupDiagnostic, SetupBlocker>> = None;
            let mut capturing: Option<CaptureFor> = None;
            let page_width = &profile.profile().page_width;
            let mut state = Projection {
                revision: 0,
                opened: false,
                screen: MachineScreen::Setup(SetupState::Blocked {
                    reasons: Vec::new(),
                }),
                setup: SetupState::Blocked {
                    reasons: Vec::new(),
                },
                latest_complete_pair: None,
                snapshots: snapshot_sender,
                workflow: Workflow::setup(),
                page_width_mm: (
                    page_width.minimum_mm.ceil() as u16,
                    page_width.maximum_mm.floor() as u16,
                ),
                cameras_live: false,
                pressure_mbar: None,
                shown_led: None,
            };
            // Without a Machine thread the lift axis and Stop still run.
            let machine = match machine {
                Ok(machine) => Some(machine),
                Err(error) => {
                    log::error!("cannot start the Machine thread: {error}");
                    state.opened = true;
                    state.set_setup(SetupState::Blocked {
                        reasons: vec![SetupBlocker::new(format!(
                            "cannot start the Machine thread: {error}"
                        ))],
                    });
                    if !state.publish(&mut lift, &mut board) {
                        return;
                    }
                    None
                }
            };

            loop {
                // Lift deadlines run on every pass: a busy inbox never times out.
                if lift.poll(Instant::now(), &mut relays(&mut board))
                    && !state.publish(&mut lift, &mut board)
                {
                    return;
                }
                let mut home_changed = false;
                for (id, outcome) in lift.take_finished() {
                    home_changed |= state.workflow.home_finished(id, &outcome);
                }
                if home_changed && !state.publish(&mut lift, &mut board) {
                    return;
                }
                // Wake for whichever comes first: the next poll of a live
                // board, a lift or Stop deadline, or the next refresh of a
                // shown sample, so its age keeps moving if the link is silent.
                let now = Instant::now();
                let board_poll = board.is_some().then_some(SETUP_POLL_INTERVAL);
                let wait = board_poll
                    .into_iter()
                    .chain(lift.refresh_wait(now))
                    .chain(
                        lift.next_wakeup()
                            .map(|at| at.saturating_duration_since(now)),
                    )
                    .min();
                // A board event is handled before anything else is received.
                let board_event = board.as_mut().and_then(|board| board.poll());
                let message = match (board_event, wait) {
                    (Some(event), _) => Inbox::BoardEvent(event),
                    (None, None) => match inbox.recv() {
                        Ok(message) => message,
                        Err(_) => return,
                    },
                    (None, Some(wait)) => match inbox.recv_timeout(wait) {
                        Ok(message) => message,
                        Err(RecvTimeoutError::Timeout) => {
                            if lift.refresh_due(Instant::now())
                                && !state.publish(&mut lift, &mut board)
                            {
                                return;
                            }
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => return,
                    },
                };
                if let Inbox::Intent(intent) = &message {
                    log::info!("operator requested {intent:?}");
                }
                // The physical button is the touchscreen intent the workflow
                // gives it, or nothing.
                let (message, stopped_by) = match message {
                    Inbox::BoardEvent(MachineEvent::Button)
                    | Inbox::Machine(MachineEvent::Button) => {
                        match state.workflow.button_intent() {
                            Some(intent) => {
                                log::info!("physical button pressed: {intent:?}");
                                (Inbox::Intent(intent), "physical button")
                            }
                            None => {
                                log::info!("physical button pressed: inert in this state");
                                continue;
                            }
                        }
                    }
                    message => (message, "touchscreen Stop"),
                };
                match message {
                    Inbox::Intent(ControllerIntent::Stop) => {
                        let cause = StopCause::Requested(stopped_by.into());
                        lift.stop(Instant::now(), cause, &mut relays(&mut board));
                        if !state.publish(&mut lift, &mut board) {
                            return;
                        }
                    }
                    Inbox::Intent(ControllerIntent::ClearStop) => {
                        lift.clear_stop(Instant::now());
                        if !state.publish(&mut lift, &mut board) {
                            return;
                        }
                    }
                    Inbox::Intent(ControllerIntent::CapturePair) => {
                        if !matches!(state.workflow, Workflow::Setup { .. }) {
                            log::warn!("ignored CapturePair: the capture preview is a Setup step");
                            continue;
                        }
                        if !state.cameras_live {
                            log::warn!("ignored CapturePair: the Cameras are not live");
                            continue;
                        }
                        if capturing.is_some() {
                            log::warn!("ignored CapturePair: a capture is already running");
                            continue;
                        }
                        let sent = machine.as_ref().is_some_and(|machine| {
                            machine.send(MachineRequest::CapturePair).is_ok()
                        });
                        if !sent {
                            log::error!("CapturePair failed: the Machine thread is gone");
                            state.cameras_live = false;
                            continue;
                        }
                        capturing = Some(CaptureFor::Preview);
                        state.screen = capture_preview(
                            CaptureStatus::Capturing,
                            state.latest_complete_pair.clone(),
                        );
                        if !state.publish(&mut lift, &mut board) {
                            return;
                        }
                    }
                    Inbox::Intent(ControllerIntent::Home) => {
                        let setup = matches!(state.workflow, Workflow::Setup { .. });
                        let ready = state.readiness(&lift, board.is_some()).is_enabled();
                        if !setup || !ready || capturing.is_some() {
                            continue;
                        }
                        // Homing leaves the diagnostic preview behind.
                        state.screen = MachineScreen::Setup(state.setup.clone());
                        state.workflow = match lift.home(Instant::now()) {
                            Ok(id) => {
                                log::info!("Home started as lift operation {id:?}");
                                Workflow::Homing(id)
                            }
                            Err(rejection) => {
                                log::error!("Home refused: {rejection:?}");
                                Workflow::Setup {
                                    home_failure: Some(format!("Home was refused: {rejection:?}")),
                                }
                            }
                        };
                        if !state.publish(&mut lift, &mut board) {
                            return;
                        }
                    }
                    Inbox::Intent(ControllerIntent::NewBook) => {
                        if !matches!(
                            state.workflow,
                            Workflow::Idle | Workflow::PageWidth { .. } | Workflow::Ready { .. }
                        ) {
                            continue;
                        }
                        state.workflow = Workflow::PageWidth { rejected_mm: None };
                        if !state.publish(&mut lift, &mut board) {
                            return;
                        }
                    }
                    Inbox::Intent(ControllerIntent::SetPageWidth { millimetres }) => {
                        if !matches!(state.workflow, Workflow::PageWidth { .. }) {
                            continue;
                        }
                        let bounds = &profile.profile().page_width;
                        let width = f32::from(millimetres);
                        state.workflow = if (bounds.minimum_mm..=bounds.maximum_mm).contains(&width)
                        {
                            Workflow::Ready {
                                page_width_mm: millimetres,
                                notice: None,
                            }
                        } else {
                            Workflow::PageWidth {
                                rejected_mm: Some(millimetres),
                            }
                        };
                        if !state.publish(&mut lift, &mut board) {
                            return;
                        }
                    }
                    Inbox::Intent(ControllerIntent::Start) => {
                        // Gates or Position trust lost since the last
                        // snapshot send the workflow back to Setup first.
                        state.reconcile(&lift, board.is_some());
                        let Workflow::Ready { page_width_mm, .. } = state.workflow else {
                            continue;
                        };
                        if capturing.is_some() {
                            continue;
                        }
                        state.workflow = match create_session_directory(
                            profile.data_root(),
                            SystemTime::now(),
                        ) {
                            Err(error) => {
                                log::error!("Start failed: cannot create the Scan session directory: {error}");
                                Workflow::Ready {
                                    page_width_mm,
                                    notice: Some(format!(
                                        "cannot create the Scan session directory: {error}"
                                    )),
                                }
                            }
                            Ok(directory) => {
                                log::info!("Scan session started in {}", directory.display());
                                let mut session = ScanSession::new(directory, page_width_mm);
                                let lit = board
                                    .as_mut()
                                    .map_or(Err("no Monospace connection".into()), |board| {
                                        board.light_on()
                                    });
                                let sent = machine.as_ref().is_some_and(|machine| {
                                    lit.is_ok()
                                        && machine.send(MachineRequest::SessionCapture).is_ok()
                                });
                                match lit {
                                    Err(error) => {
                                        log::error!("Scan session LIGHT ON failed: {error}");
                                        session.capture = CaptureStatus::Blocked {
                                            reason: format!("LIGHT ON failed: {error}"),
                                        }
                                    }
                                    Ok(()) if sent => capturing = Some(CaptureFor::Session),
                                    Ok(()) => {
                                        log::error!("Scan session capture failed: the Camera thread has stopped");
                                        session.capture = CaptureStatus::Blocked {
                                            reason: "the Camera thread has stopped".into(),
                                        }
                                    }
                                }
                                Workflow::Scanning(session)
                            }
                        };
                        if !state.publish(&mut lift, &mut board) {
                            return;
                        }
                    }
                    Inbox::Intent(ControllerIntent::ReconnectLiftAxis) => lift.reconnect(),
                    Inbox::Intent(ControllerIntent::Exit) => {
                        state.screen = MachineScreen::Exited;
                        let _ = state.publish(&mut lift, &mut board);
                        return;
                    }
                    Inbox::Link(event) => {
                        let now = Instant::now();
                        let stop_changed = lift.handle(now, &event, &mut relays(&mut board));
                        let publish = lift.apply(event) || stop_changed || lift.refresh_due(now);
                        if publish && !state.publish(&mut lift, &mut board) {
                            return;
                        }
                    }
                    Inbox::Board(opened) => {
                        match &opened {
                            Ok((_, diagnostic)) => log_ready(diagnostic),
                            Err(blocker) => log::error!("board setup: {}", blocker.summary),
                        }
                        board_setup = Some(opened.map(|(opened, diagnostic)| {
                            board = Some(opened);
                            diagnostic
                        }));
                    }
                    Inbox::Cameras(opened) => {
                        match &opened {
                            Ok(diagnostics) => diagnostics.iter().for_each(log_ready),
                            Err(blockers) => blockers.iter().for_each(|blocker| {
                                log::error!("Camera setup: {}", blocker.summary)
                            }),
                        }
                        state.cameras_live = opened.is_ok();
                        state.set_setup(setup_state(opened, &board_setup));
                        state.opened = true;
                        if !state.publish(&mut lift, &mut board) {
                            return;
                        }
                    }
                    Inbox::Captured(result) => {
                        match capturing.take() {
                            Some(CaptureFor::Session) => {
                                // Attempt 1's first valid complete pair is
                                // canonical. A session Stop already ended
                                // drops its late result.
                                if let Workflow::Scanning(session) = &mut state.workflow {
                                    session.capture = match result {
                                        Ok(pair) => {
                                            log::info!("Scan session captured its first pair");
                                            session.canonical = Some(Arc::new(pair));
                                            CaptureStatus::Ready
                                        }
                                        Err(error) => {
                                            log::error!("Scan session capture failed: {error}");
                                            CaptureStatus::Blocked {
                                                reason: error.to_string(),
                                            }
                                        }
                                    };
                                }
                            }
                            _ => {
                                let status = match result {
                                    Ok(pair) => {
                                        log::info!("captured a complete pair");
                                        state.latest_complete_pair = Some(Arc::new(pair));
                                        CaptureStatus::Ready
                                    }
                                    Err(error) => {
                                        log::error!("CapturePair failed: {error}");
                                        CaptureStatus::Blocked {
                                            reason: error.to_string(),
                                        }
                                    }
                                };
                                // A Setup failure during the capture keeps its screen.
                                if matches!(state.screen, MachineScreen::CapturePreview(_)) {
                                    state.screen =
                                        capture_preview(status, state.latest_complete_pair.clone());
                                }
                            }
                        }
                        if !state.publish(&mut lift, &mut board) {
                            return;
                        }
                    }
                    Inbox::BoardEvent(MachineEvent::Pressure(mbar))
                    | Inbox::Machine(MachineEvent::Pressure(mbar)) => {
                        state.pressure_mbar = Some(mbar);
                        if !state.publish(&mut lift, &mut board) {
                            return;
                        }
                    }
                    Inbox::BoardEvent(MachineEvent::Lost(blocker)) => {
                        log::error!("board lost: {}", blocker.summary);
                        // Stop while the board may still switch its relays off.
                        let cause = StopCause::LinkLost(blocker.summary.clone());
                        lift.stop(Instant::now(), cause, &mut relays(&mut board));
                        board = None;
                        // Captures need the board's light, and the blocker stays shown.
                        state.cameras_live = false;
                        state.set_setup(SetupState::Blocked {
                            reasons: vec![blocker],
                        });
                        if !state.publish(&mut lift, &mut board) {
                            return;
                        }
                    }
                    Inbox::Machine(MachineEvent::Lost(blocker)) => {
                        log::error!("Machine lost: {}", blocker.summary);
                        state.cameras_live = false;
                        state.set_setup(SetupState::Blocked {
                            reasons: vec![blocker],
                        });
                        if !state.publish(&mut lift, &mut board) {
                            return;
                        }
                    }
                    // Mapped to an intent above.
                    Inbox::BoardEvent(MachineEvent::Button)
                    | Inbox::Machine(MachineEvent::Button) => {}
                    Inbox::MachinePanicked => {
                        log::error!("the Machine thread panicked; the controller is stopping");
                        return;
                    }
                }
            }
        })
        .map_err(StartupError::ControllerThreadStart)?;

    Ok(ControllerHandle {
        intents: intent_sender,
        snapshots,
        accepting_intents,
        controller_thread: Some(controller_thread),
    })
}

/// Which request the one Capture pair in flight answers.
enum CaptureFor {
    /// A diagnostic capture from Setup.
    Preview,
    /// The Scan session's first Capture pair.
    Session,
}

struct Projection {
    revision: u64,
    /// Nothing is published before the Machine thread reports its Cameras.
    opened: bool,
    /// Setup or the diagnostic preview; workflow screens replace it.
    screen: MachineScreen,
    /// The live Setup result, restored when Home leaves the preview.
    setup: SetupState,
    latest_complete_pair: Option<Arc<CapturePair>>,
    snapshots: Arc<LatestSnapshot>,
    workflow: Workflow,
    /// The Machine profile's page-width bounds in whole millimetres.
    page_width_mm: (u16, u16),
    cameras_live: bool,
    pressure_mbar: Option<f32>,
    /// The LED colour last sent to the board.
    shown_led: Option<Led>,
}

impl Projection {
    fn set_setup(&mut self, setup: SetupState) {
        self.screen = MachineScreen::Setup(setup.clone());
        self.setup = setup;
    }

    /// The live gates for Home and scanning.
    fn readiness(&self, lift: &LiftAxis, board_live: bool) -> ScanReadiness {
        let reason = if !self.cameras_live {
            "the Cameras are not ready"
        } else if !board_live {
            "Monospace is not connected"
        } else if lift.status.phase != LinkPhase::Verified {
            "the lift axis is not verified"
        } else if lift.stop.status().is_some() {
            "Stop is latched"
        } else {
            return ScanReadiness::Enabled;
        };
        ScanReadiness::Disabled {
            reason: reason.into(),
        }
    }

    /// Return to Setup, dropping any in-memory Scan session, when a live
    /// gate fails or Position trust is lost. Home consent is then needed
    /// again. #30 will keep the interrupted Capture cycle instead.
    fn reconcile(&mut self, lift: &LiftAxis, board_live: bool) {
        let enabled = self.readiness(lift, board_live).is_enabled();
        let keep = match self.workflow {
            // A running Home always ends in exactly one finish, Stop included,
            // so Homing waits for it and the button keeps meaning Stop.
            Workflow::Setup { .. } | Workflow::Homing(_) => true,
            _ => enabled && lift.ops.status().position_trusted,
        };
        if !keep {
            self.workflow = Workflow::setup();
        }
    }

    /// The screen the workflow shows.
    fn project(&self) -> MachineScreen {
        if self.screen == MachineScreen::Exited {
            return MachineScreen::Exited;
        }
        match &self.workflow {
            Workflow::Setup { home_failure: None } => self.screen.clone(),
            Workflow::Setup {
                home_failure: Some(failure),
            } => {
                let mut reasons = match &self.setup {
                    SetupState::Blocked { reasons } => reasons.clone(),
                    SetupState::Ready { .. } => Vec::new(),
                };
                reasons.push(SetupBlocker::new(failure.clone()));
                MachineScreen::Setup(SetupState::Blocked { reasons })
            }
            Workflow::Homing(_) => MachineScreen::Homing,
            Workflow::Idle => MachineScreen::Idle,
            Workflow::PageWidth { rejected_mm } => MachineScreen::PageWidth(PageWidthEntry {
                minimum_mm: self.page_width_mm.0,
                maximum_mm: self.page_width_mm.1,
                rejected_mm: *rejected_mm,
            }),
            Workflow::Ready {
                page_width_mm,
                notice,
            } => MachineScreen::Ready {
                page_width_mm: *page_width_mm,
                notice: notice.clone(),
            },
            Workflow::Scanning(session) => MachineScreen::Scanning(ScanView {
                directory: session.directory.clone(),
                page_width_mm: session.page_width_mm,
                capture_cycle: session.left_sequence / 2 + 1,
                left_sequence: session.left_sequence,
                right_sequence: session.left_sequence + 1,
                attempt: session.attempt,
                capture: capture_preview_state(session.capture.clone(), session.canonical.clone()),
            }),
        }
    }

    /// Reconcile the workflow, show its LED colour, and replace the pending
    /// snapshot with the current projection.
    fn publish(&mut self, lift: &mut LiftAxis, board: &mut Option<Box<dyn MachineBoard>>) -> bool {
        self.reconcile(lift, board.is_some());
        let led = self.workflow.led(lift.stop.status().is_some());
        if let Some(board) = board.as_mut().filter(|_| self.shown_led != Some(led)) {
            // A dead board surfaces as `Lost`; the LED is only a projection.
            if let Err(error) = board.set_led(led) {
                log::warn!("cannot set the status LED to {led:?}: {error}");
            }
            self.shown_led = Some(led);
        }
        if !self.opened {
            return true;
        }
        let snapshot = ControllerSnapshot {
            revision: self.revision,
            screen: self.project(),
            scan_readiness: self.readiness(lift, board.is_some()),
            lift_axis: lift.status(Instant::now()),
            stop: lift.stop.status(),
            pressure_mbar: self.pressure_mbar,
        };
        self.revision += 1;
        self.snapshots.send(snapshot).is_ok()
    }
}

impl ScanReadiness {
    fn is_enabled(&self) -> bool {
        *self == Self::Enabled
    }
}

/// The board's `ALL OFF` port, or a failure when no board is open.
fn relays(
    board: &mut Option<Box<dyn MachineBoard>>,
) -> impl FnMut(Duration) -> Result<(), String> + '_ {
    move |within| match board.as_mut() {
        Some(board) => board.all_off(within),
        None => Err("no Monospace connection".into()),
    }
}

/// The lift-axis submission port; `None` when there is no link.
fn port(
    link: &Option<(LinkHandle, LinkRunner)>,
) -> impl FnMut(LinkCommand, Priority) -> Option<Ticket> + '_ {
    move |command, priority| link.as_ref()?.0.submit(command, priority).ok()
}

/// The controller's view of the lift-axis link and the cross-board Stop.
/// The link runs on its own threads; this folds its events into lift-axis
/// policy and the next snapshot.
struct LiftAxis {
    link: Option<(LinkHandle, LinkRunner)>,
    ops: LiftOperations,
    stop: StopControl,
    /// The Stop status the last snapshot carried.
    shown_stop: Option<StopStatus>,
    /// The Stop status last logged.
    logged_stop: Option<StopStatus>,
    status: LiftAxisStatus,
    latest: Option<TelemetrySample>,
    /// When a snapshot last carried this projection.
    last_publish: Option<Instant>,
    /// Lift operations that finished since the controller last asked.
    finished: Vec<(OperationId, LiftOutcome)>,
}

impl LiftAxis {
    fn start(
        profile: &PreparedMachineProfile,
        opener: Box<dyn FibreOpener>,
        events: Sender<Inbox>,
    ) -> Self {
        let mut lift = Self {
            link: None,
            ops: LiftOperations::new(profile.profile()),
            stop: StopControl::new(profile.profile()),
            shown_stop: None,
            logged_stop: None,
            status: LiftAxisStatus::starting(),
            latest: None,
            last_publish: None,
            finished: Vec::new(),
        };
        let config = match LinkConfig::from_profile(profile.profile()) {
            Ok(config) => config,
            Err(problems) => {
                let reason = problems.join("; ");
                log::error!("lift axis unavailable: {reason}");
                lift.status = LiftAxisStatus::unavailable(reason);
                return lift;
            }
        };
        match spawn_link(config, opener, events) {
            Ok(link) => lift.link = Some(link),
            Err(error) => {
                let reason = format!("cannot start the lift-axis link: {error}");
                log::error!("{reason}");
                lift.status = LiftAxisStatus::unavailable(reason);
            }
        }
        lift
    }

    /// Latch Stop and fan it out to both boards.
    fn stop(&mut self, now: Instant, cause: StopCause, relays: &mut Relays<'_>) {
        let mut link = port(&self.link);
        self.stop.stop(now, cause, &mut self.ops, &mut link, relays);
        drop(link);
        self.settle(now, relays);
    }

    /// Start native Home from idle. It never arms.
    fn home(&mut self, now: Instant) -> Result<OperationId, LiftRejection> {
        let mut link = port(&self.link);
        // Home commands no descent, so vacuum evidence is not needed.
        let vacuum = VacuumEvidence::NotConfirmedOff;
        self.ops.start(now, LiftRequest::Home, vacuum, &mut link)
    }

    /// Lift operations that finished since the last call.
    fn take_finished(&mut self) -> Vec<(OperationId, LiftOutcome)> {
        std::mem::take(&mut self.finished)
    }

    fn clear_stop(&mut self, now: Instant) {
        let mut link = port(&self.link);
        // A refusal is also recorded in the Stop status.
        if let Err(refused) = self.stop.clear(now, &mut self.ops, &mut link) {
            log::error!("Clear Stop refused: {refused:?}");
        }
    }

    /// Feed one link event to lift-axis policy; true when the Stop changed.
    fn handle(&mut self, now: Instant, event: &LinkEvent, relays: &mut Relays<'_>) -> bool {
        let mut link = port(&self.link);
        self.ops.handle_link_event(now, event, &mut link);
        drop(link);
        self.settle(now, relays);
        self.stop_changed()
    }

    /// Process deadlines; true when the Stop changed.
    fn poll(&mut self, now: Instant, relays: &mut Relays<'_>) -> bool {
        let mut link = port(&self.link);
        self.ops.poll(now, &mut link);
        drop(link);
        self.settle(now, relays);
        self.stop_changed()
    }

    fn settle(&mut self, now: Instant, relays: &mut Relays<'_>) {
        while let Some(event) = self.ops.take_event() {
            log_lift_event(&event);
            self.stop.note(&event);
            if let LiftEvent::Finished { id, outcome, .. } = event {
                self.finished.push((id, outcome));
            }
        }
        self.stop.observe(now, &self.ops, relays);
        let stop = self.stop.status();
        if stop != self.logged_stop {
            log_stop_change(self.logged_stop.as_ref(), stop.as_ref());
            self.logged_stop = stop;
        }
    }

    fn stop_changed(&self) -> bool {
        self.stop.status() != self.shown_stop
    }

    /// The earliest lift-axis or Stop deadline.
    fn next_wakeup(&self) -> Option<Instant> {
        self.ops
            .next_wakeup()
            .into_iter()
            .chain(self.stop.next_wakeup())
            .min()
    }

    fn reconnect(&self) {
        match &self.link {
            Some((handle, _)) => handle.reconnect(),
            None => log::error!("ReconnectLiftAxis failed: the lift-axis link is not running"),
        }
    }

    /// Fold one link event in; true when a snapshot should be published.
    fn apply(&mut self, event: LinkEvent) -> bool {
        match event {
            LinkEvent::Status(LinkStatus {
                epoch,
                phase,
                blockers,
                discovery,
                unmatched_replies,
            }) => {
                // A sample never outlives its connection on screen.
                if epoch != self.status.epoch || phase == LinkPhase::Disconnected {
                    self.latest = None;
                }
                self.status = LiftAxisStatus {
                    epoch,
                    phase,
                    blockers,
                    discovery,
                    telemetry: None,
                    unmatched_replies,
                };
                true
            }
            LinkEvent::Telemetry(sample) => {
                // Published by `refresh_due` at most four times a second.
                self.latest = Some(sample);
                false
            }
            LinkEvent::CommandResult { .. } => false,
        }
    }

    /// How long the controller may wait before the next refresh, if a sample is shown.
    fn refresh_wait(&self, now: Instant) -> Option<Duration> {
        self.latest.as_ref()?;
        Some(self.last_publish.map_or(Duration::ZERO, |last| {
            TELEMETRY_PUBLISH_INTERVAL.saturating_sub(now.saturating_duration_since(last))
        }))
    }

    /// A sample is shown and the last snapshot is at least 250 ms old.
    fn refresh_due(&self, now: Instant) -> bool {
        self.refresh_wait(now).is_some_and(|wait| wait.is_zero())
    }

    fn status(&mut self, now: Instant) -> LiftAxisStatus {
        self.last_publish = Some(now);
        self.shown_stop = self.stop.status();
        let mut status = self.status.clone();
        status.telemetry = self
            .latest
            .as_ref()
            .filter(|sample| sample.epoch() == status.epoch)
            .map(|sample| TelemetrySummary {
                values: sample.display_values().clone(),
                age_ms: sample.age(now).as_millis() as u64,
                fresh: sample.fresh_values(now).is_some(),
                cycle_ms: sample.cycle_duration().as_millis() as u64,
            });
        status
    }
}

fn log_ready(diagnostic: &SetupDiagnostic) {
    log::info!("{} ready: {}", diagnostic.component, diagnostic.summary);
}

fn log_lift_event(event: &LiftEvent) {
    match event {
        LiftEvent::Finished {
            id,
            epoch,
            outcome: LiftOutcome::Fault(fault @ LiftFault::Stopped(StopCause::Requested(_))),
        } => log::warn!("lift operation {id:?} on epoch {epoch} ended: {fault:?}"),
        LiftEvent::Finished {
            id,
            epoch,
            outcome: LiftOutcome::Fault(fault),
        } => log::error!("lift operation {id:?} on epoch {epoch} failed: {fault:?}"),
        LiftEvent::Finished { id, epoch, outcome } => {
            log::info!("lift operation {id:?} on epoch {epoch} finished: {outcome:?}")
        }
        LiftEvent::TurnBlowerCutoff { id } => {
            log::debug!("lift operation {id:?} passed the Turn blower cutoff")
        }
    }
}

/// Log a Stop latch, verdict or clear. Clear refusals are logged where they happen.
fn log_stop_change(last: Option<&StopStatus>, stop: Option<&StopStatus>) {
    let Some(stop) = stop else {
        log::info!("Stop cleared");
        return;
    };
    if last.is_none_or(|last| last.cause != stop.cause) {
        match &stop.cause {
            StopCause::Requested(_) => log::warn!("Stop latched: {:?}", stop.cause),
            _ => log::error!("Stop latched: {:?}", stop.cause),
        }
    }
    if last.is_some_and(|last| last.verdict == stop.verdict) {
        return;
    }
    match &stop.verdict {
        StopVerdict::Confirming => {}
        StopVerdict::Confirmed => log::info!("Stop confirmed"),
        StopVerdict::Unconfirmed { reason } => log::error!("Stop unconfirmed: {reason}"),
    }
}

fn capture_preview(
    status: CaptureStatus,
    latest_complete_pair: Option<Arc<CapturePair>>,
) -> MachineScreen {
    MachineScreen::CapturePreview(capture_preview_state(status, latest_complete_pair))
}

fn capture_preview_state(
    status: CaptureStatus,
    latest_complete_pair: Option<Arc<CapturePair>>,
) -> CapturePreview {
    let preview_warning = latest_complete_pair
        .as_deref()
        .and_then(native_detail_preview_warning);
    CapturePreview {
        status,
        latest_complete_pair,
        preview_warning,
    }
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, SystemTime};

    use super::{
        capture_preview, CaptureStatus, ControllerClosed, ControllerSnapshot, LatestSnapshot,
        LiftAxisStatus, MachineScreen, RecvTimeoutError, ScanReadiness, TryRecvError,
    };
    use crate::{CameraRole, CapturePair, CapturedFrame, CropGeometry};

    fn pair(value: u8) -> Arc<CapturePair> {
        let frame = |role| {
            CapturedFrame::from_rgb8(
                role,
                1,
                1,
                vec![value; 3],
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
        Arc::new(CapturePair {
            left: frame(CameraRole::Left),
            right: frame(CameraRole::Right),
        })
    }

    #[test]
    fn slow_snapshot_consumer_gets_latest_pair_and_releases_superseded_frames() {
        let snapshots = LatestSnapshot::default();
        let first = pair(1);
        let first_weak = Arc::downgrade(&first);
        snapshots
            .send(ControllerSnapshot {
                revision: 1,
                screen: capture_preview(CaptureStatus::Ready, Some(first)),
                lift_axis: LiftAxisStatus::starting(),
                scan_readiness: ScanReadiness::Enabled,
                stop: None,
                pressure_mbar: None,
            })
            .unwrap();
        let latest = pair(2);
        let expected = ControllerSnapshot {
            revision: 2,
            screen: capture_preview(CaptureStatus::Ready, Some(latest)),
            lift_axis: LiftAxisStatus::starting(),
            scan_readiness: ScanReadiness::Enabled,
            stop: None,
            pressure_mbar: None,
        };
        snapshots.send(expected.clone()).unwrap();

        assert!(first_weak.upgrade().is_none());
        assert_eq!(snapshots.try_recv(), Ok(expected));
        assert_eq!(snapshots.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(
            snapshots.recv_timeout(Duration::ZERO),
            Err(RecvTimeoutError::Timeout)
        );
    }

    #[test]
    fn closing_wakes_waiters_and_preserves_the_final_snapshot() {
        let snapshots = Arc::new(LatestSnapshot::default());
        let receiver = Arc::clone(&snapshots);
        let waiter = thread::spawn(move || receiver.recv_timeout(Duration::from_secs(1)));
        let exited = ControllerSnapshot {
            revision: 3,
            screen: MachineScreen::Exited,
            lift_axis: LiftAxisStatus::starting(),
            scan_readiness: ScanReadiness::Enabled,
            stop: None,
            pressure_mbar: None,
        };
        snapshots.send(exited.clone()).unwrap();
        snapshots.close();

        assert_eq!(waiter.join().unwrap(), Ok(exited.clone()));
        assert_eq!(snapshots.try_recv(), Err(TryRecvError::Disconnected));
        assert_eq!(
            snapshots.recv_timeout(Duration::from_secs(1)),
            Err(RecvTimeoutError::Disconnected)
        );
        assert_eq!(snapshots.send(exited), Err(ControllerClosed));
    }
}
