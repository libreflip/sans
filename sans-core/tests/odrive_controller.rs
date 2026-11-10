//! Controller intent and snapshot seam with fake cameras and a fake Fibre device.
//!
//! Fake-backed host behaviour only; no hardware is touched.

mod support;

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use sans_core::{
    bootstrap, CameraCaptureError, CameraRole, CapturePairMachine, CapturedFrame, ControllerHandle,
    ControllerIntent, FibreOpener, LinkPhase, MachineFactory, MachineScreen,
    PreparedMachineProfile, RoleCamera, ScanReadiness, SetupBlocker, SetupDiagnostic, SetupState,
};
use support::fake_fibre::*;
use support::{recv_within, GATE_WAIT};

const VALID_PROFILE: &str = include_str!("fixtures/valid-sans.toml");
const LIFT_AXIS: &str = r#"
[odrive.lift_axis]
direction = "normal"
mm_per_turn = 8.0
travel_minimum_mm = 0.0
travel_maximum_mm = 300.0
current_limit_a = 4.0
velocity_limit_mm_s = 6.0
acceleration_limit_mm_s2 = 4.0
deceleration_limit_mm_s2 = 2.0
"#;

struct NeverCalledCamera(CameraRole);

impl RoleCamera for NeverCalledCamera {
    fn role(&self) -> CameraRole {
        self.0
    }
    fn capture(&mut self) -> Result<CapturedFrame, CameraCaptureError> {
        Err(CameraCaptureError::acquisition(self.0, "fixture camera"))
    }
}

struct Factory {
    opener: FakeOpener,
    opener_requests: Arc<Mutex<usize>>,
    cameras_ready: bool,
}

impl MachineFactory for Factory {
    type Machine = CapturePairMachine;

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
            Box::new(NeverCalledCamera(CameraRole::Left)),
            Box::new(NeverCalledCamera(CameraRole::Right)),
        )
        .map(|machine| (machine, Vec::new()))
        .map_err(|error| vec![SetupBlocker::new(error.to_string())])
    }
}

struct Started {
    controller: ControllerHandle,
    opener: FakeOpener,
    _temp: tempfile::TempDir,
}

/// Widest timing the profile bounds allow. These tests check behaviour through
/// real threads, including under the emulated aarch64 cross job, not timing.
fn with_relaxed_timing(profile: &str) -> String {
    let relaxed = [
        ("poll_interval_ms = 20", "poll_interval_ms = 100"),
        ("telemetry_max_age_ms = 100", "telemetry_max_age_ms = 250"),
        ("request_deadline_ms = 100", "request_deadline_ms = 250"),
        ("urgent_deadline_ms = 50", "urgent_deadline_ms = 100"),
        ("device_open_ms = 5000", "device_open_ms = 60000"),
    ];
    relaxed
        .iter()
        .fold(profile.to_string(), |profile, (from, to)| {
            let replaced = profile.replacen(from, to, 1);
            assert_ne!(replaced, profile, "{from}");
            replaced
        })
}

fn start(profile: &str, opener: FakeOpener, cameras_ready: bool) -> Started {
    let temp = tempfile::tempdir().unwrap();
    let path: PathBuf = temp.path().join("sans.toml");
    fs::write(&path, with_relaxed_timing(profile)).unwrap();
    let controller = bootstrap(
        Some(&path),
        Factory {
            opener: opener.clone(),
            opener_requests: Arc::new(Mutex::new(0)),
            cameras_ready,
        },
    )
    .unwrap();
    Started {
        controller,
        opener,
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
    assert!(matches!(
        snapshot.scan_readiness,
        ScanReadiness::Disabled { .. }
    ));
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

    assert!(matches!(
        reconnected.scan_readiness,
        ScanReadiness::Disabled { .. }
    ));
    let board = started.opener.board.lock().unwrap();
    assert_eq!(board.requested_states(), vec![1, 1]);
    assert!(board.writes_to(INPUT_POS).is_empty());
    assert!(board.writes_to(CLEAR_ERRORS).is_empty());
    assert_eq!(*started.opener.opens.lock().unwrap(), 2);
}
