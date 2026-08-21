use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::Duration;

use sans_core::{
    bootstrap, ControllerIntent, MachineFactory, MachineScreen, PreparedMachineProfile,
    SetupBlocker, SetupState,
};

const VALID_PROFILE: &str = include_str!("fixtures/valid-sans.toml");

struct RecordingFactory {
    open_threads: Arc<Mutex<Vec<ThreadId>>>,
    result: Result<(), Vec<SetupBlocker>>,
}

impl MachineFactory for RecordingFactory {
    type Machine = ();

    fn open(self, _profile: &PreparedMachineProfile) -> Result<Self::Machine, Vec<SetupBlocker>> {
        self.open_threads
            .lock()
            .unwrap()
            .push(thread::current().id());
        self.result.clone()
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
        result: Ok(()),
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
    assert_ne!(open_threads.lock().unwrap().as_slice(), &[caller_thread]);

    controller.send(ControllerIntent::Exit).unwrap();
    let exited = controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();
    assert_eq!(exited.screen, MachineScreen::Exited);
}
