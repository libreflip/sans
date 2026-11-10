//! Linux V4L2 adapters for the fixed Sans Camera profile.

use std::path::Path;
use std::time::SystemTime;

use image::ImageFormat;
use rscam::{Camera, Config, IntervalInfo, ResolutionInfo};
use thiserror::Error;

use super::{
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
        let interval = select_interval(role, path, &backend)?;
        backend
            .start(&Config {
                interval,
                resolution: (FRAME_WIDTH, FRAME_HEIGHT),
                format: MJPEG,
                ..Default::default()
            })
            .map_err(|error| {
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
        let frame = self
            .backend
            .capture()
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
        let decoded = image::load_from_memory_with_format(&frame, ImageFormat::Jpeg)
            .map_err(|error| CameraCaptureError::decode(self.role, error.to_string()))?
            .to_rgb8();
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
    use rscam::ResolutionInfo;

    use super::resolution_supported;

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
