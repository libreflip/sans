use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::Duration;

use sans_core::{
    bootstrap, CapturePair, CapturePairError, ControllerClosed, ControllerHandle, ControllerIntent,
    ControllerMachine, MachineFactory, MachineScreen, PreparedMachineProfile, SetupBlocker,
    SetupDiagnostic, SetupState,
};

const VALID_PROFILE: &str = include_str!("fixtures/valid-sans.toml");
type OpenThreads = Arc<Mutex<Vec<(ThreadId, Option<String>)>>>;

#[derive(Clone)]
struct NoCaptureMachine;

impl ControllerMachine for NoCaptureMachine {
    fn capture_pair(&mut self) -> Result<CapturePair, CapturePairError> {
        panic!("this startup fixture must not capture")
    }
}

struct NonSendMachine {
    _marker: Rc<()>,
}

impl ControllerMachine for NonSendMachine {
    fn capture_pair(&mut self) -> Result<CapturePair, CapturePairError> {
        panic!("this startup fixture must not capture")
    }
}

struct RecordingFactory {
    open_threads: OpenThreads,
    result: Result<(NoCaptureMachine, Vec<SetupDiagnostic>), Vec<SetupBlocker>>,
}

impl MachineFactory for RecordingFactory {
    type Machine = NoCaptureMachine;

    fn open(
        self,
        _profile: &PreparedMachineProfile,
    ) -> Result<(Self::Machine, Vec<SetupDiagnostic>), Vec<SetupBlocker>> {
        let current_thread = thread::current();
        self.open_threads.lock().unwrap().push((
            current_thread.id(),
            current_thread.name().map(str::to_owned),
        ));
        self.result.clone()
    }
}

struct NonSendMachineFactory;

impl MachineFactory for NonSendMachineFactory {
    type Machine = NonSendMachine;

    fn open(
        self,
        _profile: &PreparedMachineProfile,
    ) -> Result<(Self::Machine, Vec<SetupDiagnostic>), Vec<SetupBlocker>> {
        Ok((
            NonSendMachine {
                _marker: Rc::new(()),
            },
            vec![SetupDiagnostic::ready(
                "Monospace",
                "Ready on connection epoch 7",
            )],
        ))
    }
}

struct BlockingFactory {
    started: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

struct BlockingDropMachine {
    started: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

impl Drop for BlockingDropMachine {
    fn drop(&mut self) {
        self.started.send(()).unwrap();
        self.release.recv().unwrap();
    }
}

impl ControllerMachine for BlockingDropMachine {
    fn capture_pair(&mut self) -> Result<CapturePair, CapturePairError> {
        panic!("this startup fixture must not capture")
    }
}

struct BlockingDropFactory {
    started: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

struct PanickingFactory;

struct PollingFactory {
    faults: mpsc::Receiver<SetupBlocker>,
    epoch: u64,
}

struct PollingMachine {
    faults: mpsc::Receiver<SetupBlocker>,
}

impl ControllerMachine for PollingMachine {
    fn capture_pair(&mut self) -> Result<CapturePair, CapturePairError> {
        panic!("this startup fixture must not capture")
    }
}

impl MachineFactory for PollingFactory {
    type Machine = PollingMachine;

    fn open(
        self,
        _profile: &PreparedMachineProfile,
    ) -> Result<(Self::Machine, Vec<SetupDiagnostic>), Vec<SetupBlocker>> {
        Ok((
            PollingMachine {
                faults: self.faults,
            },
            vec![SetupDiagnostic::ready(
                "Monospace",
                format!("Ready on connection epoch {}", self.epoch),
            )],
        ))
    }

    fn poll_setup(machine: &mut Self::Machine) -> Option<SetupBlocker> {
        machine.faults.try_recv().ok()
    }
}

fn bootstrap_polling_controller(
    profile_path: &Path,
    epoch: u64,
) -> (
    mpsc::Sender<SetupBlocker>,
    ControllerHandle,
    Vec<SetupDiagnostic>,
) {
    let (fault_sender, fault_receiver) = mpsc::channel();
    let controller = bootstrap(
        Some(profile_path),
        PollingFactory {
            faults: fault_receiver,
            epoch,
        },
    )
    .unwrap();
    let ready = controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();
    let diagnostics = match ready.screen {
        MachineScreen::Setup(SetupState::Ready { diagnostics }) => diagnostics,
        other => panic!("expected ready Setup snapshot, got {other:?}"),
    };
    (fault_sender, controller, diagnostics)
}

fn assert_polling_controller_blocks(
    fault_sender: &mpsc::Sender<SetupBlocker>,
    controller: &ControllerHandle,
    failure: &str,
) {
    fault_sender.send(SetupBlocker::new(failure)).unwrap();
    let blocked = controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        blocked.screen,
        MachineScreen::Setup(SetupState::Blocked {
            reasons: vec![SetupBlocker::new(failure)]
        })
    );
}

impl MachineFactory for PanickingFactory {
    type Machine = NoCaptureMachine;

    fn open(
        self,
        _profile: &PreparedMachineProfile,
    ) -> Result<(Self::Machine, Vec<SetupDiagnostic>), Vec<SetupBlocker>> {
        panic!("simulated controller startup panic");
    }
}

impl MachineFactory for BlockingDropFactory {
    type Machine = BlockingDropMachine;

    fn open(
        self,
        _profile: &PreparedMachineProfile,
    ) -> Result<(Self::Machine, Vec<SetupDiagnostic>), Vec<SetupBlocker>> {
        Ok((
            BlockingDropMachine {
                started: self.started,
                release: self.release,
            },
            Vec::new(),
        ))
    }
}

impl MachineFactory for BlockingFactory {
    type Machine = NoCaptureMachine;

    fn open(
        self,
        _profile: &PreparedMachineProfile,
    ) -> Result<(Self::Machine, Vec<SetupDiagnostic>), Vec<SetupBlocker>> {
        self.started.send(()).unwrap();
        self.release.recv().unwrap();
        Ok((NoCaptureMachine, Vec::new()))
    }
}

#[test]
fn invalid_profile_never_invokes_the_machine_factory() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    fs::write(
        &profile_path,
        VALID_PROFILE.replace("schema_version = 1", "schema_version = 2"),
    )
    .unwrap();
    let open_threads = Arc::new(Mutex::new(Vec::new()));
    let factory = RecordingFactory {
        open_threads: Arc::clone(&open_threads),
        result: Ok((NoCaptureMachine, Vec::new())),
    };

    let result = bootstrap(Some(Path::new(&profile_path)), factory);

    assert!(result.is_err());
    assert!(open_threads.lock().unwrap().is_empty());
}

#[test]
fn typed_intent_drives_fake_backed_controller_thread_end_to_end() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    fs::write(&profile_path, VALID_PROFILE).unwrap();
    let caller_thread = thread::current().id();
    let open_threads = Arc::new(Mutex::new(Vec::new()));
    let factory = RecordingFactory {
        open_threads: Arc::clone(&open_threads),
        result: Err(vec![SetupBlocker::new("Ligature is not connected")]),
    };

    let controller = bootstrap(Some(&profile_path), factory).unwrap();
    let setup = controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();

    assert_eq!(
        setup.screen,
        MachineScreen::Setup(SetupState::Blocked {
            reasons: vec![SetupBlocker::new("Ligature is not connected")]
        })
    );
    let opened_on = open_threads.lock().unwrap().clone();
    assert_eq!(opened_on.len(), 1);
    assert_ne!(opened_on[0].0, caller_thread);
    assert_eq!(opened_on[0].1.as_deref(), Some("sans-controller"));

    controller.send(ControllerIntent::Exit).unwrap();
    let exited = controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(exited.screen, MachineScreen::Exited);
}

#[test]
fn controller_can_own_a_non_send_machine() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    fs::write(&profile_path, VALID_PROFILE).unwrap();

    let controller = bootstrap(Some(&profile_path), NonSendMachineFactory).unwrap();
    let setup = controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();

    assert_eq!(
        setup.screen,
        MachineScreen::Setup(SetupState::Ready {
            diagnostics: vec![SetupDiagnostic::ready(
                "Monospace",
                "Ready on connection epoch 7"
            )]
        })
    );
    controller.send(ControllerIntent::Exit).unwrap();
}

#[test]
fn live_connection_fault_replaces_ready_setup_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    fs::write(&profile_path, VALID_PROFILE).unwrap();
    let (fault_sender, controller, _diagnostics) = bootstrap_polling_controller(&profile_path, 12);

    assert_polling_controller_blocks(
        &fault_sender,
        &controller,
        "Monospace reply timeout poisoned connection epoch 12",
    );
    controller.send(ControllerIntent::Exit).unwrap();
}

#[test]
fn controller_setup_snapshots_preserve_monospace_failure_categories() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    fs::write(&profile_path, VALID_PROFILE).unwrap();
    let failures = [
        "Monospace firmware error: RELAY_FAULT",
        "Monospace reply timeout poisoned connection epoch 21",
        "Monospace malformed frame on connection epoch 21",
        "Monospace unexpected reply on connection epoch 21",
        "Monospace disconnected on connection epoch 21",
        "Monospace urgent writing poisoned connection epoch 21",
    ];

    for failure in failures {
        let (fault_sender, controller, _diagnostics) =
            bootstrap_polling_controller(&profile_path, 21);
        assert_polling_controller_blocks(&fault_sender, &controller, failure);
        controller.send(ControllerIntent::Exit).unwrap();
    }
}

#[test]
fn controller_setup_snapshot_uses_the_reconnected_epoch() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    fs::write(&profile_path, VALID_PROFILE).unwrap();

    let ready_summary = |epoch| {
        let (_fault_sender, controller, diagnostics) =
            bootstrap_polling_controller(&profile_path, epoch);
        controller.send(ControllerIntent::Exit).unwrap();
        diagnostics[0].summary.clone()
    };

    assert_eq!(ready_summary(30), "Ready on connection epoch 30");
    assert_eq!(ready_summary(31), "Ready on connection epoch 31");
}

#[test]
fn dropping_controller_does_not_wait_for_blocked_machine_open() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    fs::write(&profile_path, VALID_PROFILE).unwrap();
    let (started_sender, started_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    let factory = BlockingFactory {
        started: started_sender,
        release: release_receiver,
    };
    let controller = bootstrap(Some(&profile_path), factory).unwrap();
    started_receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    let (dropped_sender, dropped_receiver) = mpsc::channel();

    let dropper = thread::spawn(move || {
        drop(controller);
        dropped_sender.send(()).unwrap();
    });
    let dropped_promptly = dropped_receiver
        .recv_timeout(Duration::from_millis(500))
        .is_ok();
    release_sender.send(()).unwrap();
    dropper.join().unwrap();

    assert!(
        dropped_promptly,
        "dropping the controller blocked on Machine open"
    );
}

#[test]
fn controller_rejects_intents_after_exit_is_accepted() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    fs::write(&profile_path, VALID_PROFILE).unwrap();
    let (drop_started_sender, drop_started_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    let controller = bootstrap(
        Some(&profile_path),
        BlockingDropFactory {
            started: drop_started_sender,
            release: release_receiver,
        },
    )
    .unwrap();
    controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();

    controller.send(ControllerIntent::Exit).unwrap();
    assert_eq!(
        controller.send(ControllerIntent::CapturePair),
        Err(ControllerClosed)
    );
    let exited = controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(exited.screen, MachineScreen::Exited);
    drop_started_receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    let post_exit_send = controller.send(ControllerIntent::Exit);
    release_sender.send(()).unwrap();

    assert_eq!(post_exit_send, Err(ControllerClosed));
}

#[test]
fn controller_rejects_intents_after_worker_panic() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    fs::write(&profile_path, VALID_PROFILE).unwrap();
    let controller = bootstrap(Some(&profile_path), PanickingFactory).unwrap();

    let snapshot = controller.recv_snapshot_timeout(Duration::from_secs(1));

    assert!(snapshot.is_err());
    assert_eq!(
        controller.send(ControllerIntent::Exit),
        Err(ControllerClosed)
    );
}
