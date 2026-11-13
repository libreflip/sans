//! Controller-owned Machine state exposed as typed intents and snapshots.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::camera::{CapturePair, CapturePairError};
use crate::lift::{LiftEvent, LiftOperations, LiftOutcome, StopCause};
use crate::odrive::{
    spawn_link, DiscoveryRecord, FibreOpener, LinkCommand, LinkConfig, LinkEvent, LinkHandle,
    LinkPhase, LinkRunner, LinkStatus, Priority, TelemetrySample, TelemetryValues, Ticket,
};
use crate::stop::{Relays, StopControl, StopStatus, StopVerdict};
use crate::{prepare_machine_profile, PreparedMachineProfile, StartupError};

mod machine;

use machine::{setup_state, spawn_machine, MachineRequest};

/// Telemetry-only snapshot changes publish at most four times a second.
const TELEMETRY_PUBLISH_INTERVAL: Duration = Duration::from_millis(250);

/// The complete set of UI-to-controller actions.
///
/// No intent arms, homes or moves the lift axis. Only `ClearStop` clears
/// its errors, and only after a Confirmed Stop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerIntent {
    /// Latch Stop: urgently disarm the lift axis and switch Monospace `ALL OFF`.
    Stop,
    /// Clear a Confirmed Stop's lift-axis errors and latch. Never arms.
    ClearStop,
    /// Acquire one complete stationary Capture pair.
    CapturePair,
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

/// Whether an operator can start a Scan session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScanReadiness {
    /// Production scanning is disabled in this build whatever the lift-axis
    /// phase or the profile says.
    Disabled {
        /// Why scanning is unavailable.
        reason: String,
    },
}

impl ScanReadiness {
    fn current() -> Self {
        Self::Disabled {
            reason: "production scanning is not available in this build; a verified lift axis \
                     enables diagnostics only"
                .into(),
        }
    }
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
    /// Lift-axis link status, independent of the Camera screen.
    pub lift_axis: LiftAxisStatus,
    /// Scanning stays disabled in this build.
    pub scan_readiness: ScanReadiness,
    /// The latched Stop, if any.
    pub stop: Option<StopStatus>,
}

/// Machine-thread-owned Camera behavior. Blocking work runs here, never on
/// the controller thread, so it cannot delay Stop.
pub trait ControllerMachine: 'static {
    /// Acquire one complete pair or reject it without publishing a partial pair.
    fn capture_pair(&mut self) -> Result<CapturePair, CapturePairError>;
}

/// The relay board as the Stop path uses it. It is owned by the controller
/// thread, beside the lift axis, so no Camera work can delay it.
pub trait MachineBoard: Send + 'static {
    /// Send `ALL OFF` and wait at most `within` for its exact `OK`.
    fn all_off(&mut self, within: Duration) -> Result<(), String>;

    /// The next button press or failure, without blocking.
    fn poll(&mut self) -> Option<MachineEvent>;
}

/// The board as the factory opened it, or why it did not open.
pub type OpenedBoard = Result<(Box<dyn MachineBoard>, SetupDiagnostic), SetupBlocker>;

/// Something a live board or Machine reported between intents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MachineEvent {
    /// The physical button was pressed; it means Stop.
    StopButton,
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
            let mut cameras_live = false;
            let mut capturing = false;
            let mut state = Projection {
                revision: 0,
                opened: false,
                screen: MachineScreen::Setup(SetupState::Blocked {
                    reasons: Vec::new(),
                }),
                latest_complete_pair: None,
                snapshots: snapshot_sender,
            };
            // Without a Machine thread the lift axis and Stop still run.
            let machine = match machine {
                Ok(machine) => Some(machine),
                Err(error) => {
                    log::error!("cannot start the Machine thread: {error}");
                    state.opened = true;
                    state.screen = MachineScreen::Setup(SetupState::Blocked {
                        reasons: vec![SetupBlocker::new(format!(
                            "cannot start the Machine thread: {error}"
                        ))],
                    });
                    if !state.publish(&mut lift) {
                        return;
                    }
                    None
                }
            };

            loop {
                // Lift deadlines run on every pass: a busy inbox never times out.
                if lift.poll(Instant::now(), &mut relays(&mut board)) && !state.publish(&mut lift) {
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
                            if lift.refresh_due(Instant::now()) && !state.publish(&mut lift) {
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
                match message {
                    Inbox::Intent(ControllerIntent::Stop) => {
                        let cause = StopCause::Requested("touchscreen Stop".into());
                        lift.stop(Instant::now(), cause, &mut relays(&mut board));
                        if !state.publish(&mut lift) {
                            return;
                        }
                    }
                    Inbox::Intent(ControllerIntent::ClearStop) => {
                        lift.clear_stop(Instant::now());
                        if !state.publish(&mut lift) {
                            return;
                        }
                    }
                    Inbox::Intent(ControllerIntent::CapturePair) => {
                        if !cameras_live {
                            log::warn!("ignored CapturePair: the Cameras are not live");
                            continue;
                        }
                        if capturing {
                            log::warn!("ignored CapturePair: a capture is already running");
                            continue;
                        }
                        let sent = machine.as_ref().is_some_and(|machine| {
                            machine.send(MachineRequest::CapturePair).is_ok()
                        });
                        if !sent {
                            log::error!("CapturePair failed: the Machine thread is gone");
                            cameras_live = false;
                            continue;
                        }
                        capturing = true;
                        state.screen = capture_preview(
                            CaptureStatus::Capturing,
                            state.latest_complete_pair.clone(),
                        );
                        if !state.publish(&mut lift) {
                            return;
                        }
                    }
                    Inbox::Intent(ControllerIntent::ReconnectLiftAxis) => lift.reconnect(),
                    Inbox::Intent(ControllerIntent::Exit) => {
                        state.screen = MachineScreen::Exited;
                        let _ = state.publish(&mut lift);
                        return;
                    }
                    Inbox::Link(event) => {
                        let now = Instant::now();
                        let stop_changed = lift.handle(now, &event, &mut relays(&mut board));
                        let publish = lift.apply(event) || stop_changed || lift.refresh_due(now);
                        if publish && !state.publish(&mut lift) {
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
                        cameras_live = opened.is_ok();
                        state.screen = MachineScreen::Setup(setup_state(opened, &board_setup));
                        state.opened = true;
                        if !state.publish(&mut lift) {
                            return;
                        }
                    }
                    Inbox::Captured(result) => {
                        capturing = false;
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
                        if !state.publish(&mut lift) {
                            return;
                        }
                    }
                    Inbox::BoardEvent(MachineEvent::StopButton)
                    | Inbox::Machine(MachineEvent::StopButton) => {
                        log::info!("physical Stop button pressed");
                        let cause = StopCause::Requested("physical button".into());
                        lift.stop(Instant::now(), cause, &mut relays(&mut board));
                        if !state.publish(&mut lift) {
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
                        cameras_live = false;
                        state.screen = MachineScreen::Setup(SetupState::Blocked {
                            reasons: vec![blocker],
                        });
                        if !state.publish(&mut lift) {
                            return;
                        }
                    }
                    Inbox::Machine(MachineEvent::Lost(blocker)) => {
                        log::error!("Machine lost: {}", blocker.summary);
                        cameras_live = false;
                        state.screen = MachineScreen::Setup(SetupState::Blocked {
                            reasons: vec![blocker],
                        });
                        if !state.publish(&mut lift) {
                            return;
                        }
                    }
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

struct Projection {
    revision: u64,
    /// Nothing is published before the Machine thread reports its Cameras.
    opened: bool,
    screen: MachineScreen,
    latest_complete_pair: Option<Arc<CapturePair>>,
    snapshots: Arc<LatestSnapshot>,
}

impl Projection {
    /// Replace the pending snapshot with the current projection.
    fn publish(&mut self, lift: &mut LiftAxis) -> bool {
        if !self.opened {
            return true;
        }
        let snapshot = ControllerSnapshot {
            revision: self.revision,
            screen: self.screen.clone(),
            lift_axis: lift.status(Instant::now()),
            scan_readiness: ScanReadiness::current(),
            stop: lift.stop.status(),
        };
        self.revision += 1;
        self.snapshots.send(snapshot).is_ok()
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
                scan_readiness: ScanReadiness::current(),
                stop: None,
            })
            .unwrap();
        let latest = pair(2);
        let expected = ControllerSnapshot {
            revision: 2,
            screen: capture_preview(CaptureStatus::Ready, Some(latest)),
            lift_axis: LiftAxisStatus::starting(),
            scan_readiness: ScanReadiness::current(),
            stop: None,
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
            scan_readiness: ScanReadiness::current(),
            stop: None,
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
