//! Controller-owned Machine state exposed as typed intents and snapshots.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::camera::{CapturePair, CapturePairError};
use crate::odrive::{
    spawn_link, DiscoveryRecord, FibreOpener, LinkConfig, LinkEvent, LinkHandle, LinkPhase,
    LinkRunner, LinkStatus, TelemetrySample, TelemetryValues,
};
use crate::{prepare_machine_profile, PreparedMachineProfile, StartupError};

/// Telemetry-only snapshot changes publish at most four times a second.
const TELEMETRY_PUBLISH_INTERVAL: Duration = Duration::from_millis(250);

/// The complete set of UI-to-controller actions.
///
/// No intent arms, homes, moves or clears errors on the lift axis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerIntent {
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

    /// Hand over the ODrive device opener. Called once on the controller
    /// thread, only after the profile passed validation; no USB device is
    /// touched before then. Production returns the USB opener.
    fn odrive_opener(&mut self) -> Box<dyn FibreOpener>;

    /// Open live resources after structural startup checks and report nonfatal Setup blockers.
    fn open(
        self,
        profile: &PreparedMachineProfile,
    ) -> Result<(Self::Machine, Vec<SetupDiagnostic>), Vec<SetupBlocker>>;

    /// Poll controller-owned resources for a live Setup failure.
    fn poll_setup(_machine: &mut Self::Machine) -> Option<SetupBlocker> {
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

/// One controller inbox for intents and lift-axis link events.
enum Inbox {
    Intent(ControllerIntent),
    Link(LinkEvent),
}

impl From<LinkEvent> for Inbox {
    fn from(event: LinkEvent) -> Self {
        Self::Link(event)
    }
}

/// How often the controller polls live Machine resources for Setup failures.
const SETUP_POLL_INTERVAL: Duration = Duration::from_millis(16);

fn spawn_controller<Factory: MachineFactory>(
    profile: PreparedMachineProfile,
    mut factory: Factory,
) -> Result<ControllerHandle, StartupError> {
    let (intent_sender, inbox) = mpsc::channel();
    let link_events = intent_sender.clone();
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
            // The ODrive opener is requested only after the profile validated,
            // and before live Camera and board resources are opened.
            let mut lift = LiftAxis::start(&profile, factory.odrive_opener(), link_events);
            let (setup, mut active_machine) = match factory.open(&profile) {
                Ok((machine, diagnostics)) => (SetupState::Ready { diagnostics }, Some(machine)),
                Err(reasons) => (SetupState::Blocked { reasons }, None),
            };
            let mut state = Projection {
                revision: 0,
                screen: MachineScreen::Setup(setup),
                latest_complete_pair: None,
                snapshots: snapshot_sender,
            };
            if !state.publish(&lift) {
                return;
            }

            loop {
                if let Some(machine) = active_machine.as_mut() {
                    if let Some(blocker) = Factory::poll_setup(machine) {
                        state.screen = MachineScreen::Setup(SetupState::Blocked {
                            reasons: vec![blocker],
                        });
                        if !state.publish(&lift) {
                            return;
                        }
                        active_machine = None;
                    }
                }

                // A live Machine is polled for Setup failures; otherwise the
                // controller sleeps until an intent or a link event arrives.
                let message = if active_machine.is_some() {
                    match inbox.recv_timeout(SETUP_POLL_INTERVAL) {
                        Ok(message) => message,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                } else {
                    match inbox.recv() {
                        Ok(message) => message,
                        Err(_) => return,
                    }
                };

                match message {
                    Inbox::Intent(ControllerIntent::CapturePair) => {
                        let Some(machine) = active_machine.as_mut() else {
                            continue;
                        };
                        state.screen = capture_preview(
                            CaptureStatus::Capturing,
                            state.latest_complete_pair.clone(),
                        );
                        if !state.publish(&lift) {
                            return;
                        }
                        let status = match machine.capture_pair() {
                            Ok(pair) => {
                                state.latest_complete_pair = Some(Arc::new(pair));
                                CaptureStatus::Ready
                            }
                            Err(error) => CaptureStatus::Blocked {
                                reason: error.to_string(),
                            },
                        };
                        state.screen = capture_preview(status, state.latest_complete_pair.clone());
                        if !state.publish(&lift) {
                            return;
                        }
                    }
                    Inbox::Intent(ControllerIntent::ReconnectLiftAxis) => lift.reconnect(),
                    Inbox::Intent(ControllerIntent::Exit) => {
                        state.screen = MachineScreen::Exited;
                        let _ = state.publish(&lift);
                        return;
                    }
                    Inbox::Link(event) => {
                        if lift.apply(event) && !state.publish(&lift) {
                            return;
                        }
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
    screen: MachineScreen,
    latest_complete_pair: Option<Arc<CapturePair>>,
    snapshots: Arc<LatestSnapshot>,
}

impl Projection {
    /// Replace the pending snapshot with the current projection.
    fn publish(&mut self, lift: &LiftAxis) -> bool {
        let snapshot = ControllerSnapshot {
            revision: self.revision,
            screen: self.screen.clone(),
            lift_axis: lift.status(Instant::now()),
            scan_readiness: ScanReadiness::current(),
        };
        self.revision += 1;
        self.snapshots.send(snapshot).is_ok()
    }
}

/// The controller's view of the lift-axis link. The link runs on its own
/// threads; this only folds its events into the next snapshot.
struct LiftAxis {
    link: Option<(LinkHandle, LinkRunner)>,
    status: LiftAxisStatus,
    latest: Option<TelemetrySample>,
    last_telemetry_publish: Option<Instant>,
}

impl LiftAxis {
    fn start(
        profile: &PreparedMachineProfile,
        opener: Box<dyn FibreOpener>,
        events: Sender<Inbox>,
    ) -> Self {
        let mut lift = Self {
            link: None,
            status: LiftAxisStatus::starting(),
            latest: None,
            last_telemetry_publish: None,
        };
        let config = match LinkConfig::from_profile(profile.profile()) {
            Ok(config) => config,
            Err(problems) => {
                lift.status = LiftAxisStatus::unavailable(problems.join("; "));
                return lift;
            }
        };
        match spawn_link(config, opener, events) {
            Ok(link) => lift.link = Some(link),
            Err(error) => {
                lift.status =
                    LiftAxisStatus::unavailable(format!("cannot start the lift-axis link: {error}"))
            }
        }
        lift
    }

    fn reconnect(&self) {
        if let Some((handle, _)) = &self.link {
            handle.reconnect();
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
                if epoch != self.status.epoch {
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
                self.latest = Some(sample);
                let now = Instant::now();
                let due = self
                    .last_telemetry_publish
                    .is_none_or(|last| now.duration_since(last) >= TELEMETRY_PUBLISH_INTERVAL);
                if due {
                    self.last_telemetry_publish = Some(now);
                }
                due
            }
            LinkEvent::CommandResult { .. } => false,
        }
    }

    fn status(&self, now: Instant) -> LiftAxisStatus {
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
            })
            .unwrap();
        let latest = pair(2);
        let expected = ControllerSnapshot {
            revision: 2,
            screen: capture_preview(CaptureStatus::Ready, Some(latest)),
            lift_axis: LiftAxisStatus::starting(),
            scan_readiness: ScanReadiness::current(),
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
