//! Linux V4L2 adapters for the fixed Sans Camera profile.

use std::io;
use std::path::Path;
use std::time::SystemTime;

use rscam::{
    Camera, Config, CtrlData, IntervalInfo, ResolutionInfo, CID_AUTOGAIN, CID_AUTO_WHITE_BALANCE,
    CID_EXPOSURE_ABSOLUTE, CID_EXPOSURE_AUTO, CID_GAIN, CID_WHITE_BALANCE_TEMPERATURE,
    EXPOSURE_MANUAL,
};
use thiserror::Error;

use super::{
    acquisition::{capture_fresh_frame, CAPTURE_BUFFERS},
    jpeg::decode_camera_jpeg,
    resolve_live_camera_identities, resolve_live_camera_identity, CameraCaptureError, CameraRole,
    CapturePairMachine, CapturedFrame, RoleCamera,
};
use crate::config::{CameraRoleProfile, PreparedMachineProfile};
use crate::controller::{MachineFactory, SetupBlocker, SetupDiagnostic};

const FRAME_WIDTH: u32 = 3840;
const FRAME_HEIGHT: u32 = 2160;
const MJPEG: &[u8; 4] = b"MJPG";

/// Opens the two commissioned V4L2 Camera identities on the controller thread.
#[derive(Clone, Copy, Debug, Default)]
pub struct V4lCameraMachineFactory;

/// Failure from the non-persistent single-role Camera diagnostic path.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CameraDiagnosticError {
    /// The configured Camera identity or fixed profile is not ready.
    #[error("{0}")]
    Setup(String),
    /// Acquisition, decode, rotation, or crop processing failed.
    #[error(transparent)]
    Capture(#[from] CameraCaptureError),
}

impl From<SetupBlocker> for CameraDiagnosticError {
    fn from(blocker: SetupBlocker) -> Self {
        Self::Setup(blocker.summary)
    }
}

/// Capture one configured Camera role for a standalone diagnostic.
///
/// The caller decides whether and where to write the owned post-rotation frame.
pub fn capture_configured_camera(
    prepared: &PreparedMachineProfile,
    role: CameraRole,
) -> Result<CapturedFrame, CameraDiagnosticError> {
    let profile = match role {
        CameraRole::Left => &prepared.profile().cameras.left,
        CameraRole::Right => &prepared.profile().cameras.right,
    };
    let path = resolve_live_camera_identity(role, Path::new(&profile.identity))?;
    let mut camera = V4lRoleCamera::open(role, &path, profile)?;
    camera.capture().map_err(CameraDiagnosticError::from)
}

impl MachineFactory for V4lCameraMachineFactory {
    type Machine = CapturePairMachine;

    fn odrive_opener(&mut self) -> Box<dyn crate::FibreOpener> {
        Box::new(crate::UsbFibreOpener)
    }

    fn open(
        self,
        prepared: &PreparedMachineProfile,
    ) -> Result<(Self::Machine, Vec<SetupDiagnostic>), Vec<SetupBlocker>> {
        let cameras = &prepared.profile().cameras;
        let (left_path, right_path) = resolve_live_camera_identities(
            Path::new(&cameras.left.identity),
            Path::new(&cameras.right.identity),
        )?;
        let left = V4lRoleCamera::open(CameraRole::Left, &left_path, &cameras.left);
        let right = V4lRoleCamera::open(CameraRole::Right, &right_path, &cameras.right);
        let mut blockers = Vec::new();
        if let Err(blocker) = &left {
            blockers.push(blocker.clone());
        }
        if let Err(blocker) = &right {
            blockers.push(blocker.clone());
        }
        if !blockers.is_empty() {
            return Err(blockers);
        }

        let machine = CapturePairMachine::new(Box::new(left.unwrap()), Box::new(right.unwrap()))
            .map_err(|error| vec![SetupBlocker::new(error.to_string())])?;
        Ok((
            machine,
            vec![
                SetupDiagnostic::ready("Left Camera", format!("Ready at {}", left_path.display())),
                SetupDiagnostic::ready(
                    "Right Camera",
                    format!("Ready at {}", right_path.display()),
                ),
            ],
        ))
    }
}

struct V4lRoleCamera {
    role: CameraRole,
    backend: Camera,
    rotation_degrees: u16,
    crop: crate::CropGeometry,
}

impl V4lRoleCamera {
    fn open(
        role: CameraRole,
        path: &Path,
        profile: &CameraRoleProfile,
    ) -> Result<Self, SetupBlocker> {
        let path_text = path.to_str().ok_or_else(|| {
            SetupBlocker::new(format!(
                "{role} Camera device path {} is not valid UTF-8",
                path.display()
            ))
        })?;
        let mut backend = Camera::new(path_text).map_err(|error| {
            SetupBlocker::new(format!(
                "{role} Camera device {} cannot be opened: {error}",
                path.display()
            ))
        })?;
        verify_fixed_profile(role, path, &backend)?;
        apply_fixed_controls(role, path, &backend, profile)?;
        let interval = select_interval(role, path, &backend)?;
        backend.start(&capture_config(interval)).map_err(|error| {
            SetupBlocker::new(format!(
                "{role} Camera device {} rejected 3840x2160 MJPEG: {error}",
                path.display()
            ))
        })?;
        Ok(Self {
            role,
            backend,
            rotation_degrees: profile.rotation_degrees,
            crop: profile.crop,
        })
    }
}

impl RoleCamera for V4lRoleCamera {
    fn role(&self) -> CameraRole {
        self.role
    }

    fn capture(&mut self) -> Result<CapturedFrame, CameraCaptureError> {
        let frame = capture_fresh_frame(|| self.backend.capture())
            .map_err(|error| CameraCaptureError::acquisition(self.role, error.to_string()))?;
        let captured_at = SystemTime::now();
        if frame.resolution != (FRAME_WIDTH, FRAME_HEIGHT) || frame.format != *MJPEG {
            return Err(CameraCaptureError::Profile {
                role: self.role,
                detail: format!(
                    "expected 3840x2160 MJPEG, received {}x{} {:?}",
                    frame.resolution.0, frame.resolution.1, frame.format
                ),
            });
        }
        let jpeg = frame.to_vec();
        drop(frame);

        let decoded = decode_camera_jpeg(&jpeg)
            .map_err(|error| CameraCaptureError::decode(self.role, error))?;
        if decoded.dimensions() != (FRAME_WIDTH, FRAME_HEIGHT) {
            return Err(CameraCaptureError::Profile {
                role: self.role,
                detail: format!(
                    "decoded MJPEG dimensions are {}x{}, expected 3840x2160",
                    decoded.width(),
                    decoded.height()
                ),
            });
        }

        CapturedFrame::from_unrotated_rgb8(
            self.role,
            decoded.width(),
            decoded.height(),
            decoded.into_raw(),
            captured_at,
            self.rotation_degrees,
            self.crop,
        )
        .map_err(|error| CameraCaptureError::Geometry {
            role: self.role,
            detail: error.to_string(),
        })
    }
}

fn capture_config(interval: (u32, u32)) -> Config<'static> {
    Config {
        interval,
        resolution: (FRAME_WIDTH, FRAME_HEIGHT),
        format: MJPEG,
        nbuffers: CAPTURE_BUFFERS,
        ..Default::default()
    }
}

fn verify_fixed_profile(
    role: CameraRole,
    path: &Path,
    backend: &Camera,
) -> Result<(), SetupBlocker> {
    let formats = backend
        .formats()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            SetupBlocker::new(format!(
                "{role} Camera device {} cannot list pixel formats: {error}",
                path.display()
            ))
        })?;
    if !formats.iter().any(|format| format.format == *MJPEG) {
        return Err(SetupBlocker::new(format!(
            "{role} Camera device {} does not expose MJPEG",
            path.display()
        )));
    }
    let resolutions = backend.resolutions(MJPEG).map_err(|error| {
        SetupBlocker::new(format!(
            "{role} Camera device {} cannot list MJPEG resolutions: {error}",
            path.display()
        ))
    })?;
    if !resolution_supported(&resolutions, (FRAME_WIDTH, FRAME_HEIGHT)) {
        return Err(SetupBlocker::new(format!(
            "{role} Camera device {} does not expose 3840x2160 MJPEG",
            path.display()
        )));
    }
    Ok(())
}

/// Fixes the image controls the profile names; omitted ones keep the Camera's own behaviour.
///
/// Every value is checked against its control's reported range before any control is written,
/// so a profile value the Camera cannot take leaves all of its controls as they were.
fn apply_fixed_controls(
    role: CameraRole,
    path: &Path,
    backend: &Camera,
    profile: &CameraRoleProfile,
) -> Result<(), SetupBlocker> {
    let blocker = |detail: String| {
        SetupBlocker::new(format!("{role} Camera device {} {detail}", path.display()))
    };
    let check = |id: u32, control: &str, field: &str, value: u32| {
        let control = backend
            .get_control(id)
            .map_err(|error| blocker(format!("has no {control} control: {error}")))?;
        integer_value_problem(&control.data, field, value)
            .map_or(Ok(()), |problem| Err(blocker(problem)))
    };
    let set = |id: u32, value: i64, what: &str| {
        backend
            .set_control(id, value)
            .map_err(|error| blocker(format!("rejected {what}: {error}")))
    };
    // Turns an automatic mode off when the Camera has one; a Camera without it has nothing to
    // override the fixed value.
    let disable_if_present = |id: u32, what: &str| match backend.get_control(id) {
        Ok(_) => set(id, 0, what),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
        Err(error) => Err(blocker(format!("cannot query {what}: {error}"))),
    };

    if let Some(exposure) = profile.exposure_100us {
        check(
            CID_EXPOSURE_ABSOLUTE,
            "absolute exposure",
            "exposure_100us",
            exposure,
        )?;
    }
    if let Some(kelvin) = profile.white_balance_kelvin {
        check(
            CID_WHITE_BALANCE_TEMPERATURE,
            "white balance temperature",
            "white_balance_kelvin",
            kelvin,
        )?;
    }
    if let Some(gain) = profile.gain {
        check(CID_GAIN, "gain", "gain", gain)?;
    }

    if let Some(exposure) = profile.exposure_100us {
        set(
            CID_EXPOSURE_AUTO,
            i64::from(EXPOSURE_MANUAL),
            "manual exposure mode",
        )?;
        set(
            CID_EXPOSURE_ABSOLUTE,
            i64::from(exposure),
            &format!("exposure_100us {exposure}"),
        )?;
    }
    if let Some(kelvin) = profile.white_balance_kelvin {
        disable_if_present(CID_AUTO_WHITE_BALANCE, "automatic white balance")?;
        set(
            CID_WHITE_BALANCE_TEMPERATURE,
            i64::from(kelvin),
            &format!("white_balance_kelvin {kelvin}"),
        )?;
    }
    if let Some(gain) = profile.gain {
        disable_if_present(CID_AUTOGAIN, "automatic gain")?;
        set(CID_GAIN, i64::from(gain), &format!("gain {gain}"))?;
    }
    Ok(())
}

/// Describes why `value` cannot be written to an integer control, or `None` when it can.
fn integer_value_problem(data: &CtrlData, field: &str, value: u32) -> Option<String> {
    let CtrlData::Integer {
        minimum,
        maximum,
        step,
        ..
    } = *data
    else {
        return Some(format!("reports a non-integer control for {field}"));
    };
    let value = i64::from(value);
    let (minimum, maximum, step) = (i64::from(minimum), i64::from(maximum), i64::from(step));
    if !(minimum..=maximum).contains(&value) {
        return Some(format!(
            "accepts {field} from {minimum} to {maximum}, not {value}"
        ));
    }
    if step > 1 && (value - minimum) % step != 0 {
        return Some(format!(
            "accepts {field} in steps of {step} from {minimum}, not {value}"
        ));
    }
    None
}

fn resolution_supported(info: &ResolutionInfo, requested: (u32, u32)) -> bool {
    match info {
        ResolutionInfo::Discretes(resolutions) => resolutions.contains(&requested),
        ResolutionInfo::Stepwise { min, max, step } => {
            requested.0 >= min.0
                && requested.0 <= max.0
                && requested.1 >= min.1
                && requested.1 <= max.1
                && step_matches(requested.0, min.0, step.0)
                && step_matches(requested.1, min.1, step.1)
        }
    }
}

fn step_matches(value: u32, minimum: u32, step: u32) -> bool {
    step == 0 || (value - minimum).is_multiple_of(step)
}

fn select_interval(
    role: CameraRole,
    path: &Path,
    backend: &Camera,
) -> Result<(u32, u32), SetupBlocker> {
    let intervals = backend
        .intervals(MJPEG, (FRAME_WIDTH, FRAME_HEIGHT))
        .map_err(|error| {
            SetupBlocker::new(format!(
                "{role} Camera device {} cannot list 3840x2160 MJPEG frame intervals: {error}",
                path.display()
            ))
        })?;
    match intervals {
        IntervalInfo::Discretes(intervals) => intervals.first().copied().ok_or_else(|| {
            SetupBlocker::new(format!(
                "{role} Camera device {} exposes no 3840x2160 MJPEG frame interval",
                path.display()
            ))
        }),
        IntervalInfo::Stepwise { min, .. } => Ok(min),
    }
}

#[cfg(test)]
mod tests {
    use rscam::{CtrlData, ResolutionInfo};

    use super::{integer_value_problem, resolution_supported};

    fn integer(minimum: i32, maximum: i32, step: i32) -> CtrlData {
        CtrlData::Integer {
            value: minimum,
            default: minimum,
            minimum,
            maximum,
            step,
        }
    }

    #[test]
    fn fixed_control_values_must_fit_the_reported_range_and_step() {
        let temperature = integer(2800, 6500, 10);
        assert_eq!(
            integer_value_problem(&temperature, "white_balance_kelvin", 4600),
            None
        );
        assert_eq!(
            integer_value_problem(&temperature, "white_balance_kelvin", 2700).as_deref(),
            Some("accepts white_balance_kelvin from 2800 to 6500, not 2700")
        );
        assert_eq!(
            integer_value_problem(&temperature, "white_balance_kelvin", 4605).as_deref(),
            Some("accepts white_balance_kelvin in steps of 10 from 2800, not 4605")
        );
        assert_eq!(integer_value_problem(&integer(0, 100, 1), "gain", 0), None);
        assert_eq!(
            integer_value_problem(&integer(0, 100, 0), "gain", u32::MAX).as_deref(),
            Some("accepts gain from 0 to 100, not 4294967295")
        );
        assert_eq!(
            integer_value_problem(
                &CtrlData::Boolean {
                    value: false,
                    default: false
                },
                "gain",
                1
            )
            .as_deref(),
            Some("reports a non-integer control for gain")
        );
    }

    #[test]
    fn fixed_resolution_must_be_explicitly_supported() {
        assert!(resolution_supported(
            &ResolutionInfo::Discretes(vec![(1920, 1080), (3840, 2160)]),
            (3840, 2160)
        ));
        assert!(!resolution_supported(
            &ResolutionInfo::Discretes(vec![(1920, 1080)]),
            (3840, 2160)
        ));
        assert!(resolution_supported(
            &ResolutionInfo::Stepwise {
                min: (640, 480),
                max: (3840, 2160),
                step: (16, 16),
            },
            (3840, 2160)
        ));
    }
}
