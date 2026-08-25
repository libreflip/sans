use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use sans_core::{
    bootstrap, CameraCaptureError, CameraRole, CapturePairMachine, CaptureStatus, CapturedFrame,
    ControllerIntent, CropGeometry, MachineFactory, MachineScreen, PreparedMachineProfile,
    RoleCamera, SetupBlocker, SetupDiagnostic, SetupState,
};

const VALID_PROFILE: &str = include_str!("fixtures/valid-sans.toml");

#[derive(Clone)]
struct ScriptedCamera {
    role: CameraRole,
    captures: Arc<Mutex<VecDeque<Result<CapturedFrame, CameraCaptureError>>>>,
    calls: Arc<Mutex<usize>>,
}

impl RoleCamera for ScriptedCamera {
    fn role(&self) -> CameraRole {
        self.role
    }

    fn capture(&mut self) -> Result<CapturedFrame, CameraCaptureError> {
        *self.calls.lock().unwrap() += 1;
        self.captures.lock().unwrap().pop_front().unwrap()
    }
}

struct FixtureFactory {
    left: ScriptedCamera,
    right: ScriptedCamera,
}

impl MachineFactory for FixtureFactory {
    type Machine = CapturePairMachine;

    fn open(
        self,
        _profile: &PreparedMachineProfile,
    ) -> Result<(Self::Machine, Vec<SetupDiagnostic>), Vec<SetupBlocker>> {
        CapturePairMachine::new(Box::new(self.left), Box::new(self.right))
            .map(|machine| (machine, Vec::new()))
            .map_err(|error| vec![SetupBlocker::new(error.to_string())])
    }
}

fn captured_frame(role: CameraRole, value: u8) -> CapturedFrame {
    CapturedFrame::from_rgb8(
        role,
        4,
        3,
        vec![value; 4 * 3 * 3],
        SystemTime::UNIX_EPOCH + Duration::from_secs(value.into()),
        CropGeometry {
            x: 1,
            y: 0,
            width: 3,
            height: 3,
        },
    )
    .unwrap()
}

fn unrotated_frame(role: CameraRole, rotation_degrees: u16, crop: CropGeometry) -> CapturedFrame {
    let pixels = [1_u8, 2, 3, 4, 5, 6]
        .into_iter()
        .flat_map(|value| [value, value, value])
        .collect();
    CapturedFrame::from_unrotated_rgb8(
        role,
        2,
        3,
        pixels,
        SystemTime::UNIX_EPOCH,
        rotation_degrees,
        crop,
    )
    .unwrap()
}

fn scripted_camera(
    role: CameraRole,
    captures: Vec<Result<CapturedFrame, CameraCaptureError>>,
) -> (ScriptedCamera, Arc<Mutex<usize>>) {
    let calls = Arc::new(Mutex::new(0));
    (
        ScriptedCamera {
            role,
            captures: Arc::new(Mutex::new(captures.into())),
            calls: Arc::clone(&calls),
        },
        calls,
    )
}

fn start_controller(left: ScriptedCamera, right: ScriptedCamera) -> sans_core::ControllerHandle {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    let data_root = temp.path().join("scan-data");
    fs::write(
        &profile_path,
        VALID_PROFILE.replace(
            "data_root = \"scan-data\"",
            &format!("data_root = {:?}", data_root),
        ),
    )
    .unwrap();
    bootstrap(
        Some(Path::new(&profile_path)),
        FixtureFactory { left, right },
    )
    .unwrap()
}

fn expect_ready(controller: &sans_core::ControllerHandle) {
    let ready = controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        ready.screen,
        MachineScreen::Setup(SetupState::Ready { ref diagnostics }) if diagnostics.is_empty()
    ));
}

fn capture_snapshots(
    controller: &sans_core::ControllerHandle,
) -> (sans_core::ControllerSnapshot, sans_core::ControllerSnapshot) {
    controller.send(ControllerIntent::CapturePair).unwrap();
    let capturing = controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        capturing.screen,
        MachineScreen::CapturePreview(ref preview) if preview.status == CaptureStatus::Capturing
    ));
    let captured = controller
        .recv_snapshot_timeout(Duration::from_secs(1))
        .unwrap();
    (capturing, captured)
}

fn capture(controller: &sans_core::ControllerHandle) -> Arc<sans_core::CapturePair> {
    let (_, captured) = capture_snapshots(controller);
    match captured.screen {
        MachineScreen::CapturePreview(preview) => {
            assert_eq!(preview.status, CaptureStatus::Ready);
            preview.latest_complete_pair.unwrap()
        }
        screen => panic!("expected a Capture preview, got {screen:?}"),
    }
}

#[test]
fn controller_retries_only_the_failed_camera_and_publishes_one_complete_pair() {
    let left_frame = captured_frame(CameraRole::Left, 10);
    let right_frame = captured_frame(CameraRole::Right, 20);
    let (left, left_calls) = scripted_camera(CameraRole::Left, vec![Ok(left_frame.clone())]);
    let (right, right_calls) = scripted_camera(
        CameraRole::Right,
        vec![
            Err(CameraCaptureError::decode(
                CameraRole::Right,
                "truncated fixture",
            )),
            Ok(right_frame.clone()),
        ],
    );
    let controller = start_controller(left, right);
    expect_ready(&controller);

    let pair = capture(&controller);

    assert_eq!(pair.left, left_frame);
    assert_eq!(pair.right, right_frame);
    assert_eq!(*left_calls.lock().unwrap(), 1);
    assert_eq!(*right_calls.lock().unwrap(), 2);
}

#[test]
fn exhausted_retry_blocks_capture_and_retains_the_previous_complete_pair() {
    let first_left = captured_frame(CameraRole::Left, 10);
    let first_right = captured_frame(CameraRole::Right, 20);
    let (left, left_calls) = scripted_camera(
        CameraRole::Left,
        vec![Ok(first_left), Ok(captured_frame(CameraRole::Left, 11))],
    );
    let (right, right_calls) = scripted_camera(
        CameraRole::Right,
        vec![
            Ok(first_right),
            Err(CameraCaptureError::acquisition(
                CameraRole::Right,
                "fixture timeout",
            )),
            Err(CameraCaptureError::decode(
                CameraRole::Right,
                "truncated fixture",
            )),
        ],
    );
    let controller = start_controller(left, right);
    expect_ready(&controller);
    let expected_pair = capture(&controller);

    let (capturing, blocked) = capture_snapshots(&controller);
    assert!(matches!(
        capturing.screen,
        MachineScreen::CapturePreview(ref preview)
            if preview.status == CaptureStatus::Capturing
                && preview.latest_complete_pair.as_deref() == Some(expected_pair.as_ref())
    ));
    assert!(matches!(
        blocked.screen,
        MachineScreen::CapturePreview(ref preview)
            if matches!(preview.status, CaptureStatus::Blocked { .. })
                && preview.latest_complete_pair.as_deref() == Some(expected_pair.as_ref())
    ));
    assert_eq!(*left_calls.lock().unwrap(), 2);
    assert_eq!(*right_calls.lock().unwrap(), 3);
}

#[test]
fn controller_publishes_rotated_frames_with_crops_clamped_to_each_role() {
    let left_frame = unrotated_frame(
        CameraRole::Left,
        90,
        CropGeometry {
            x: 2,
            y: 0,
            width: 10,
            height: 10,
        },
    );
    let right_frame = unrotated_frame(
        CameraRole::Right,
        270,
        CropGeometry {
            x: 0,
            y: 1,
            width: 10,
            height: 10,
        },
    );
    let (left, _) = scripted_camera(CameraRole::Left, vec![Ok(left_frame.clone())]);
    let (right, _) = scripted_camera(CameraRole::Right, vec![Ok(right_frame.clone())]);
    let controller = start_controller(left, right);
    expect_ready(&controller);

    let pair = capture(&controller);

    assert_eq!(pair.left, left_frame);
    assert_eq!((pair.left.crop.width, pair.left.crop.height), (1, 2));
    assert_eq!(
        pair.left
            .rgb8()
            .iter()
            .step_by(3)
            .copied()
            .collect::<Vec<_>>(),
        vec![5, 3, 1, 6, 4, 2]
    );
    assert_eq!(pair.right, right_frame);
    assert_eq!((pair.right.crop.width, pair.right.crop.height), (3, 1));
    assert_eq!(
        pair.right
            .rgb8()
            .iter()
            .step_by(3)
            .copied()
            .collect::<Vec<_>>(),
        vec![2, 4, 6, 1, 3, 5]
    );
}

#[test]
fn frame_tagged_for_the_wrong_camera_role_blocks_the_entire_pair() {
    let (left, left_calls) = scripted_camera(
        CameraRole::Left,
        vec![Ok(captured_frame(CameraRole::Right, 10))],
    );
    let (right, right_calls) = scripted_camera(
        CameraRole::Right,
        vec![Ok(captured_frame(CameraRole::Right, 20))],
    );
    let controller = start_controller(left, right);
    expect_ready(&controller);

    let (_, blocked) = capture_snapshots(&controller);

    assert!(matches!(
        blocked.screen,
        MachineScreen::CapturePreview(ref preview)
            if matches!(preview.status, CaptureStatus::Blocked { ref reason }
                if reason.contains("Left Camera profile mismatch: adapter returned a Right frame"))
                && preview.latest_complete_pair.is_none()
    ));
    assert_eq!(*left_calls.lock().unwrap(), 1);
    assert_eq!(*right_calls.lock().unwrap(), 1);
}

#[test]
fn missing_native_detail_preview_warns_without_rejecting_the_complete_pair() {
    let left_frame = captured_frame(CameraRole::Left, 10)
        .without_native_detail_preview("fixture detail allocation failed");
    let right_frame = captured_frame(CameraRole::Right, 20);
    let (left, _) = scripted_camera(CameraRole::Left, vec![Ok(left_frame)]);
    let (right, _) = scripted_camera(CameraRole::Right, vec![Ok(right_frame)]);
    let controller = start_controller(left, right);
    expect_ready(&controller);

    let (_, captured) = capture_snapshots(&controller);

    assert!(matches!(
        captured.screen,
        MachineScreen::CapturePreview(ref preview)
            if preview.status == CaptureStatus::Ready
                && preview.latest_complete_pair.is_some()
                && preview.preview_warning.as_deref().is_some_and(|warning|
                    warning.contains("Left 100% preview is unavailable")
                        && warning.contains("fixture detail allocation failed"))
    ));
}

#[test]
fn empty_post_rotation_crop_blocks_the_entire_pair() {
    let geometry_error = CapturedFrame::from_rgb8(
        CameraRole::Left,
        4,
        3,
        vec![0; 4 * 3 * 3],
        SystemTime::UNIX_EPOCH,
        CropGeometry {
            x: 4,
            y: 0,
            width: 1,
            height: 1,
        },
    )
    .unwrap_err();
    let (left, left_calls) = scripted_camera(
        CameraRole::Left,
        vec![Err(CameraCaptureError::Geometry {
            role: CameraRole::Left,
            detail: geometry_error.to_string(),
        })],
    );
    let (right, right_calls) = scripted_camera(
        CameraRole::Right,
        vec![Ok(captured_frame(CameraRole::Right, 20))],
    );
    let controller = start_controller(left, right);
    expect_ready(&controller);

    let (_, blocked) = capture_snapshots(&controller);

    assert!(matches!(
        blocked.screen,
        MachineScreen::CapturePreview(ref preview)
            if matches!(preview.status, CaptureStatus::Blocked { ref reason }
                if reason.contains("crop") && reason.contains("empty"))
                && preview.latest_complete_pair.is_none()
    ));
    assert_eq!(*left_calls.lock().unwrap(), 1);
    assert_eq!(*right_calls.lock().unwrap(), 1);
}
