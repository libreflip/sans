//! Controller intent and snapshot seam with fake cameras and a fake Fibre device.
//!
//! Fake-backed host behaviour only; no hardware is touched.

mod support;

use std::fs;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use sans_core::{
    bootstrap, CameraCaptureError, CameraRole, CapturePairMachine, CapturedFrame, ControllerHandle,
    ControllerIntent, FibreOpener, LinkPhase, MachineBoard, MachineEvent, MachineFactory,
    MachineScreen, OpenedBoard, PreparedMachineProfile, RoleCamera, ScanReadiness, SetupBlocker,
    SetupDiagnostic, SetupState, StopCause, StopVerdict,
};
use support::fake_fibre::*;
use support::{recv_within, with_relaxed_timing, GATE_WAIT, LIFT_AXIS};

const VALID_PROFILE: &str = include_str!("fixtures/valid-sans.toml");

/// A camera that fails every capture, after waiting for its gate.
struct FailingCamera(CameraRole, Arc<Mutex<()>>);

impl RoleCamera for FailingCamera {
    fn role(&self) -> CameraRole {
        self.0
    }
    fn capture(&mut self) -> Result<CapturedFrame, CameraCaptureError> {
        let _open = self.1.lock().unwrap();
        Err(CameraCaptureError::acquisition(self.0, "fixture camera"))
    }
}

/// A scripted Monospace: `ALL OFF` always answers `OK`.
struct Board {
    all_offs: Arc<Mutex<Vec<Duration>>>,
    events: mpsc::Receiver<MachineEvent>,
}

impl MachineBoard for Board {
    fn all_off(&mut self, within: Duration) -> Result<(), String> {
        self.all_offs.lock().unwrap().push(within);
        Ok(())
    }

    fn poll(&mut self) -> Option<MachineEvent> {
        self.events.try_recv().ok()
    }
}

struct Factory {
    opener: FakeOpener,
    opener_requests: Arc<Mutex<usize>>,
    cameras_ready: bool,
    board: Option<Board>,
    capture_gate: Arc<Mutex<()>>,
}

impl MachineFactory for Factory {
    type Machine = CapturePairMachine;

    fn open_board(&mut self, _profile: &PreparedMachineProfile) -> Option<OpenedBoard> {
        let board: Box<dyn MachineBoard> = Box::new(self.board.take()?);
        Some(Ok((board, SetupDiagnostic::ready("Monospace", "scripted"))))
    }

    fn odrive_opener(&mut self) -> Box<dyn FibreOpener> {
        *self.opener_requests.lock().unwrap() += 1;
        Box::new(self.opener.clone())
    }

    fn open(
        self,
        _profile: &PreparedMachineProfile,
    ) -> Result<(Self::Machine, Vec<SetupDiagnostic>), Vec<SetupBlocker>> {
        if !self.cameras_ready {
            return Err(vec![SetupBlocker::new("fixture cameras are unplugged")]);
        }
        CapturePairMachine::new(
            Box::new(FailingCamera(CameraRole::Left, self.capture_gate)),
            Box::new(FailingCamera(CameraRole::Right, Arc::default())),
        )
        .map(|machine| (machine, Vec::new()))
        .map_err(|error| vec![SetupBlocker::new(error.to_string())])
    }
}

struct Started {
    controller: ControllerHandle,
    opener: FakeOpener,
    all_offs: Arc<Mutex<Vec<Duration>>>,
    machine_events: mpsc::Sender<MachineEvent>,
    /// Holding this lock holds every Left capture.
    capture_gate: Arc<Mutex<()>>,
    _temp: tempfile::TempDir,
}

fn start(profile: &str, opener: FakeOpener, cameras_ready: bool) -> Started {
    let temp = tempfile::tempdir().unwrap();
    let path: PathBuf = temp.path().join("sans.toml");
    fs::write(&path, with_relaxed_timing(profile)).unwrap();
    let all_offs = Arc::new(Mutex::new(Vec::new()));
    let (machine_events, events) = mpsc::channel();
    let capture_gate = Arc::new(Mutex::new(()));
    let controller = bootstrap(
        Some(&path),
        Factory {
            opener: opener.clone(),
            opener_requests: Arc::new(Mutex::new(0)),
            cameras_ready,
            board: Some(Board {
                all_offs: Arc::clone(&all_offs),
                events,
            }),
            capture_gate: Arc::clone(&capture_gate),
        },
    )
    .unwrap();
    Started {
        controller,
        opener,
        all_offs,
        machine_events,
        capture_gate,
        _temp: temp,
    }
}

fn commissioned() -> String {
    format!("{VALID_PROFILE}{LIFT_AXIS}")
}

/// The Camera machine opened, so capture and preview stay available.
fn camera_preview_ready(screen: &MachineScreen) -> bool {
    matches!(screen, MachineScreen::Setup(SetupState::Ready { .. }))
}

#[test]
fn invalid_or_version_1_profile_never_asks_the_factory_for_the_odrive_opener() {
    for profile in [
        VALID_PROFILE.replace("schema_version = 2", "schema_version = 1"),
        VALID_PROFILE.replace("poll_interval_ms = 20", "poll_interval_ms = 1"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sans.toml");
        fs::write(&path, profile).unwrap();
        let requests = Arc::new(Mutex::new(0));
        let opener = FakeOpener::new(FakeFibre::compatible());

        let result = bootstrap(
            Some(&path),
            Factory {
                opener: opener.clone(),
                opener_requests: Arc::clone(&requests),
                cameras_ready: true,
                board: None,
                capture_gate: Arc::default(),
            },
        );

        assert!(result.is_err());
        assert_eq!(*requests.lock().unwrap(), 0);
        assert_eq!(*opener.opens.lock().unwrap(), 0);
    }
}

#[test]
fn compatible_device_and_commissioned_profile_reach_verified_with_the_preview_ready() {
    let started = start(
        &commissioned(),
        FakeOpener::new(FakeFibre::compatible()),
        true,
    );

    let snapshot = recv_within(&started.controller, GATE_WAIT, |snapshot| {
        snapshot.lift_axis.phase == LinkPhase::Verified
    });

    assert_eq!(snapshot.lift_axis.epoch, 1);
    assert!(camera_preview_ready(&snapshot.screen));
    // Live gates pass; Home still needs the operator's consent.
    assert_eq!(snapshot.scan_readiness, ScanReadiness::Enabled);
    let discovery = snapshot.lift_axis.discovery.as_ref().unwrap();
    assert_eq!(discovery.descriptor_crc, Some(39744));
    assert_eq!(discovery.endpoint_count, Some(535));
    let telemetry = recv_within(&started.controller, GATE_WAIT, |snapshot| {
        snapshot.lift_axis.telemetry.is_some()
    })
    .lift_axis
    .telemetry
    .unwrap();
    assert_eq!(telemetry.values.position_mm, Some(0.0));
    let board = started.opener.board.lock().unwrap();
    let writes: Vec<u16> = board.writes.iter().map(|(endpoint, _)| *endpoint).collect();
    assert_eq!(
        writes,
        vec![
            REQUESTED_STATE,
            CURRENT_LIM,
            CONTROLLER_VEL_LIMIT,
            TRAP_VEL_LIMIT,
            TRAP_ACCEL_LIMIT,
            TRAP_DECEL_LIMIT,
        ]
    );
    assert_eq!(board.requested_states(), vec![1]);
}

#[test]
fn each_blocker_is_shown_while_the_camera_preview_stays_available() {
    let descriptor = String::from_utf8(DESCRIPTOR.to_vec()).unwrap().replacen(
        r#"{"name":"input_pos","id":194,"type":"float","access":"rw"}"#,
        r#"{"name":"input_pos","id":194,"type":"float","access":"r"}"#,
        1,
    );
    let cases = [
        (
            commissioned(),
            FakeOpener::new(FakeFibre::compatible().with_descriptor(descriptor.into_bytes())),
            "endpoint axis0.controller.input_pos has access r, Sans needs read and write",
        ),
        (
            commissioned(),
            FakeOpener::absent(),
            "no ODrive with USB serial 316536713335 is connected",
        ),
        (
            VALID_PROFILE.to_string(),
            FakeOpener::new(FakeFibre::compatible()),
            "lift axis is uncommissioned",
        ),
    ];

    for (profile, opener, expected) in cases {
        let started = start(&profile, opener, true);

        let snapshot = recv_within(&started.controller, GATE_WAIT, |snapshot| {
            snapshot.lift_axis.phase == LinkPhase::Blocked
        });

        assert!(
            snapshot
                .lift_axis
                .blockers
                .iter()
                .any(|blocker| blocker.contains(expected)),
            "{expected}: {:?}",
            snapshot.lift_axis.blockers
        );
        assert!(camera_preview_ready(&snapshot.screen));
        let board = started.opener.board.lock().unwrap();
        assert!(board.requested_states().iter().all(|state| *state == 1));
        assert!(board.writes_to(CURRENT_LIM).is_empty());
    }
}

#[test]
fn camera_setup_blocker_and_lift_axis_status_are_independent() {
    let started = start(
        &commissioned(),
        FakeOpener::new(FakeFibre::compatible()),
        false,
    );

    let snapshot = recv_within(&started.controller, GATE_WAIT, |snapshot| {
        snapshot.lift_axis.phase == LinkPhase::Verified
    });

    assert!(matches!(snapshot.screen, MachineScreen::Setup(_)));
}

#[test]
fn cutting_the_connection_then_reconnecting_reaches_verified_in_epoch_2_without_motion() {
    let started = start(
        &commissioned(),
        FakeOpener::new(FakeFibre::compatible()),
        true,
    );
    recv_within(&started.controller, GATE_WAIT, |snapshot| {
        snapshot.lift_axis.phase == LinkPhase::Verified
    });

    *started.opener.cut.lock().unwrap() = true;
    let cut = recv_within(&started.controller, GATE_WAIT, |snapshot| {
        snapshot.lift_axis.phase == LinkPhase::Disconnected
    });
    assert_eq!(cut.lift_axis.epoch, 1);
    assert!(camera_preview_ready(&cut.screen));
    assert!(
        cut.lift_axis.telemetry.is_none(),
        "a disconnected link must not show its last sample: {:#?}",
        cut.lift_axis
    );

    started
        .controller
        .send(ControllerIntent::ReconnectLiftAxis)
        .unwrap();
    let reconnected = recv_within(&started.controller, GATE_WAIT, |snapshot| {
        snapshot.lift_axis.phase == LinkPhase::Verified && snapshot.lift_axis.epoch == 2
    });

    assert_eq!(reconnected.scan_readiness, ScanReadiness::Enabled);
    let board = started.opener.board.lock().unwrap();
    assert_eq!(board.requested_states(), vec![1, 1]);
    assert!(board.writes_to(INPUT_POS).is_empty());
    assert!(board.writes_to(CLEAR_ERRORS).is_empty());
    assert_eq!(*started.opener.opens.lock().unwrap(), 2);
}

#[test]
fn shown_telemetry_keeps_ageing_after_the_link_falls_silent() {
    let started = start(
        &commissioned(),
        FakeOpener::new(FakeFibre::compatible()),
        true,
    );
    recv_within(&started.controller, GATE_WAIT, |snapshot| {
        snapshot
            .lift_axis
            .telemetry
            .as_ref()
            .is_some_and(|telemetry| telemetry.fresh)
    });

    // Every request now stalls: the link faults once and then sends no events.
    started.opener.block_writes();
    let stale = recv_within(&started.controller, GATE_WAIT, |snapshot| {
        snapshot
            .lift_axis
            .telemetry
            .as_ref()
            .is_some_and(|telemetry| !telemetry.fresh)
    });
    let stale_age = stale.lift_axis.telemetry.as_ref().unwrap().age_ms;
    // No link event follows the fault; only the controller's refresh can show
    // the sample getting older.
    let later = recv_within(&started.controller, GATE_WAIT, |snapshot| {
        snapshot
            .lift_axis
            .telemetry
            .as_ref()
            .is_some_and(|telemetry| telemetry.age_ms > stale_age)
    });
    started.opener.release_writes();

    assert!(!later.lift_axis.telemetry.as_ref().unwrap().fresh);
}

fn verified() -> Started {
    let started = start(
        &commissioned(),
        FakeOpener::new(FakeFibre::compatible()),
        true,
    );
    recv_within(&started.controller, GATE_WAIT, |snapshot| {
        snapshot.lift_axis.phase == LinkPhase::Verified
    });
    started
}

fn stop_verdict(snapshot: &sans_core::ControllerSnapshot) -> Option<StopVerdict> {
    snapshot.stop.as_ref().map(|stop| stop.verdict.clone())
}

#[test]
fn stop_intent_confirms_on_both_boards_then_clear_stop_unlatches_without_arming() {
    let started = verified();

    started.controller.send(ControllerIntent::Stop).unwrap();
    let confirmed = recv_within(&started.controller, GATE_WAIT, |snapshot| {
        stop_verdict(snapshot) == Some(StopVerdict::Confirmed)
    });

    assert_eq!(
        confirmed.stop.unwrap().cause,
        StopCause::Requested("touchscreen Stop".into())
    );
    assert_eq!(
        *started.all_offs.lock().unwrap(),
        vec![Duration::from_millis(500)]
    );
    // The gate's idle request, then Stop's urgent Disarm.
    assert_eq!(
        started.opener.board.lock().unwrap().requested_states(),
        vec![1, 1]
    );

    started
        .controller
        .send(ControllerIntent::ClearStop)
        .unwrap();
    recv_within(&started.controller, GATE_WAIT, |snapshot| {
        snapshot.stop.is_none()
    });
    let board = started.opener.board.lock().unwrap();
    assert_eq!(board.writes_to(CLEAR_ERRORS).len(), 1);
    assert_eq!(board.requested_states(), vec![1, 1], "clear never arms");
}

#[test]
fn a_lost_machine_stops_the_machine() {
    // The physical button's state-dependent meaning is covered in scan_workflow.rs.
    let started = verified();
    started
        .machine_events
        .send(MachineEvent::Lost(SetupBlocker::new(
            "Monospace disconnected",
        )))
        .unwrap();
    let lost = recv_within(&started.controller, GATE_WAIT, |snapshot| {
        snapshot.stop.is_some()
    });
    assert_eq!(
        lost.stop.unwrap().cause,
        StopCause::LinkLost("Monospace disconnected".into())
    );
    assert!(matches!(
        lost.screen,
        MachineScreen::Setup(SetupState::Blocked { .. })
    ));
    assert_eq!(started.all_offs.lock().unwrap().len(), 1);

    // Capture needs the lost board's light, so no snapshot after the
    // request may replace the blocker with a capture.
    started
        .controller
        .send(ControllerIntent::CapturePair)
        .unwrap();
    let watch_until = std::time::Instant::now() + Duration::from_millis(500);
    while let Some(left) = watch_until.checked_duration_since(std::time::Instant::now()) {
        let snapshot = match started.controller.recv_snapshot_timeout(left) {
            Ok(snapshot) => snapshot,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(error) => panic!("the controller closed: {error}"),
        };
        assert!(
            matches!(
                snapshot.screen,
                MachineScreen::Setup(SetupState::Blocked { .. })
            ),
            "{:?}",
            snapshot.screen
        );
    }
}

#[test]
fn stop_confirms_while_a_capture_is_still_running() {
    let started = verified();
    let held = started.capture_gate.lock().unwrap();
    started
        .controller
        .send(ControllerIntent::CapturePair)
        .unwrap();
    recv_within(&started.controller, GATE_WAIT, |snapshot| {
        matches!(
            &snapshot.screen,
            MachineScreen::CapturePreview(preview)
                if preview.status == sans_core::CaptureStatus::Capturing
        )
    });

    started.controller.send(ControllerIntent::Stop).unwrap();
    let confirmed = recv_within(&started.controller, GATE_WAIT, |snapshot| {
        stop_verdict(snapshot) == Some(StopVerdict::Confirmed)
    });

    assert!(matches!(
        &confirmed.screen,
        MachineScreen::CapturePreview(preview)
            if preview.status == sans_core::CaptureStatus::Capturing
    ));
    drop(held);
    recv_within(&started.controller, GATE_WAIT, |snapshot| {
        matches!(
            &snapshot.screen,
            MachineScreen::CapturePreview(preview)
                if matches!(preview.status, sans_core::CaptureStatus::Blocked { .. })
        )
    });
}
