//! Complete Camera-pair acquisition behind role-specific adapters.

#[cfg(target_os = "linux")]
mod vl_cam;

use std::fmt;
#[cfg(any(target_os = "linux", test))]
use std::fs;
#[cfg(any(target_os = "linux", test))]
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use thiserror::Error;

use crate::config::CropGeometry;
use crate::controller::ControllerMachine;
#[cfg(any(target_os = "linux", test))]
use crate::controller::SetupBlocker;

#[cfg(target_os = "linux")]
pub use self::vl_cam::{capture_configured_camera, CameraDiagnosticError, V4lCameraMachineFactory};

/// The stable physical position assigned to one Camera.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CameraRole {
    /// Camera assigned to the Left page.
    Left,
    /// Camera assigned to the Right page.
    Right,
}

impl fmt::Display for CameraRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Left => "Left",
            Self::Right => "Right",
        })
    }
}

/// A crop clamped to the owned post-rotation frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameCrop {
    /// Horizontal origin from the left edge.
    pub x: u32,
    /// Vertical origin from the top edge.
    pub y: u32,
    /// Clamped nonzero width.
    pub width: u32,
    /// Clamped nonzero height.
    pub height: u32,
}

/// Owned sRGB pixels prepared for one preview view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewImage {
    /// Preview pixel width.
    pub width: u32,
    /// Preview pixel height.
    pub height: u32,
    rgb8: Arc<[u8]>,
}

impl PreviewImage {
    /// Packed sRGB preview pixels in row-major order.
    pub fn rgb8(&self) -> &[u8] {
        &self.rgb8
    }
}

/// An optional 100% preview could not be prepared.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{summary}")]
pub struct NativeDetailPreviewError {
    summary: String,
}

/// One owned, decoded, post-rotation frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedFrame {
    /// Stable Camera role that produced the frame.
    pub role: CameraRole,
    /// Post-rotation pixel width.
    pub width: u32,
    /// Post-rotation pixel height.
    pub height: u32,
    /// Time at which the adapter acquired the frame.
    pub captured_at: SystemTime,
    /// Full-page preview crop, clamped to this frame.
    pub crop: FrameCrop,
    rgb8: Arc<[u8]>,
    full_page_preview: PreviewImage,
    native_detail_preview: Result<PreviewImage, NativeDetailPreviewError>,
}

impl CapturedFrame {
    /// Rotate packed sRGB pixels clockwise, then build an owned frame.
    ///
    /// Camera adapters use this constructor so unrotated pixels do not cross
    /// the role-adapter seam.
    #[allow(clippy::too_many_arguments)]
    pub fn from_unrotated_rgb8(
        role: CameraRole,
        width: u32,
        height: u32,
        rgb8: Vec<u8>,
        captured_at: SystemTime,
        rotation_degrees: u16,
        crop: CropGeometry,
    ) -> Result<Self, FrameBuildError> {
        let expected_len = rgb8_len(width, height)?;
        if rgb8.len() != expected_len {
            return Err(FrameBuildError::PixelLength {
                width,
                height,
                expected: expected_len,
                actual: rgb8.len(),
            });
        }
        let (width, height, rgb8) = rotate_rgb8(width, height, rgb8, rotation_degrees)?;
        Self::from_rgb8(role, width, height, rgb8, captured_at, crop)
    }

    /// Build an owned post-rotation frame from packed sRGB pixels.
    pub fn from_rgb8(
        role: CameraRole,
        width: u32,
        height: u32,
        rgb8: Vec<u8>,
        captured_at: SystemTime,
        crop: CropGeometry,
    ) -> Result<Self, FrameBuildError> {
        let expected_len = rgb8_len(width, height)?;
        if rgb8.len() != expected_len {
            return Err(FrameBuildError::PixelLength {
                width,
                height,
                expected: expected_len,
                actual: rgb8.len(),
            });
        }
        let crop = clamp_crop(crop, width, height)?;
        let full_page_preview = crop_preview(&rgb8, width, crop)?;

        Ok(Self {
            role,
            width,
            height,
            captured_at,
            crop,
            rgb8: rgb8.into(),
            native_detail_preview: Ok(full_page_preview.clone()),
            full_page_preview,
        })
    }

    /// Packed sRGB pixels in row-major order.
    pub fn rgb8(&self) -> &[u8] {
        &self.rgb8
    }

    /// Validated Full-page preview pixels.
    pub fn full_page_preview(&self) -> &PreviewImage {
        &self.full_page_preview
    }

    /// Optional native-resolution source for the centered 100% view.
    pub fn native_detail_preview(&self) -> Result<&PreviewImage, &NativeDetailPreviewError> {
        self.native_detail_preview.as_ref()
    }

    /// Mark only the optional 100% preview unavailable.
    ///
    /// Adapters use this after an optional detail-processing failure. The
    /// archival frame and Full-page preview remain valid.
    pub fn without_native_detail_preview(mut self, summary: impl Into<String>) -> Self {
        self.native_detail_preview = Err(NativeDetailPreviewError {
            summary: summary.into(),
        });
        self
    }
}

/// An invalid owned frame or post-rotation crop.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum FrameBuildError {
    /// The pixel count cannot be represented on this host.
    #[error("{width}x{height} frame dimensions overflow the pixel buffer size")]
    DimensionsOverflow {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// Packed RGB data does not match the declared dimensions.
    #[error("{width}x{height} RGB frame needs {expected} bytes but the adapter returned {actual}")]
    PixelLength {
        /// Declared width.
        width: u32,
        /// Declared height.
        height: u32,
        /// Required packed RGB byte count.
        expected: usize,
        /// Supplied byte count.
        actual: usize,
    },
    /// The commissioned crop has no pixels inside the frame.
    #[error("crop at ({x}, {y}) with size {width}x{height} is empty in a {frame_width}x{frame_height} frame")]
    EmptyCrop {
        /// Requested horizontal origin.
        x: u32,
        /// Requested vertical origin.
        y: u32,
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
        /// Available frame width.
        frame_width: u32,
        /// Available frame height.
        frame_height: u32,
    },
    /// The configured rotation is not a clockwise quarter turn.
    #[error("rotation must be 0, 90, 180, or 270 degrees, got {0}")]
    UnsupportedRotation(u16),
    /// Memory for a validated Full-page preview could not be reserved.
    #[error("cannot allocate {width}x{height} Full-page preview")]
    FullPagePreviewAllocation {
        /// Preview width.
        width: u32,
        /// Preview height.
        height: u32,
    },
}

fn rgb8_len(width: u32, height: u32) -> Result<usize, FrameBuildError> {
    usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or(FrameBuildError::DimensionsOverflow { width, height })
}

fn rotate_rgb8(
    width: u32,
    height: u32,
    rgb8: Vec<u8>,
    rotation_degrees: u16,
) -> Result<(u32, u32, Vec<u8>), FrameBuildError> {
    if rotation_degrees == 0 {
        return Ok((width, height, rgb8));
    }
    if !matches!(rotation_degrees, 90 | 180 | 270) {
        return Err(FrameBuildError::UnsupportedRotation(rotation_degrees));
    }

    let (rotated_width, rotated_height) = if matches!(rotation_degrees, 90 | 270) {
        (height, width)
    } else {
        (width, height)
    };
    let source_width = usize::try_from(width).unwrap();
    let destination_width = usize::try_from(rotated_width).unwrap();
    let mut rotated = vec![0; rgb8.len()];
    for y in 0..height {
        for x in 0..width {
            let (rotated_x, rotated_y) = match rotation_degrees {
                90 => (height - 1 - y, x),
                180 => (width - 1 - x, height - 1 - y),
                270 => (y, width - 1 - x),
                _ => unreachable!(),
            };
            let source =
                (usize::try_from(y).unwrap() * source_width + usize::try_from(x).unwrap()) * 3;
            let destination = (usize::try_from(rotated_y).unwrap() * destination_width
                + usize::try_from(rotated_x).unwrap())
                * 3;
            rotated[destination..destination + 3].copy_from_slice(&rgb8[source..source + 3]);
        }
    }
    Ok((rotated_width, rotated_height, rotated))
}

fn clamp_crop(
    crop: CropGeometry,
    frame_width: u32,
    frame_height: u32,
) -> Result<FrameCrop, FrameBuildError> {
    let width = frame_width.saturating_sub(crop.x).min(crop.width);
    let height = frame_height.saturating_sub(crop.y).min(crop.height);
    if width == 0 || height == 0 {
        return Err(FrameBuildError::EmptyCrop {
            x: crop.x,
            y: crop.y,
            width: crop.width,
            height: crop.height,
            frame_width,
            frame_height,
        });
    }

    Ok(FrameCrop {
        x: crop.x,
        y: crop.y,
        width,
        height,
    })
}

fn crop_preview(
    frame: &[u8],
    frame_width: u32,
    crop: FrameCrop,
) -> Result<PreviewImage, FrameBuildError> {
    let preview_len = rgb8_len(crop.width, crop.height)?;
    let mut rgb8 = Vec::new();
    rgb8.try_reserve_exact(preview_len).map_err(|_| {
        FrameBuildError::FullPagePreviewAllocation {
            width: crop.width,
            height: crop.height,
        }
    })?;
    let frame_width = usize::try_from(frame_width).unwrap();
    let crop_x = usize::try_from(crop.x).unwrap();
    let crop_width = usize::try_from(crop.width).unwrap();
    for y in crop.y..crop.y + crop.height {
        let row_start = (usize::try_from(y).unwrap() * frame_width + crop_x) * 3;
        let row_end = row_start + crop_width * 3;
        rgb8.extend_from_slice(&frame[row_start..row_end]);
    }
    debug_assert_eq!(rgb8.len(), preview_len);
    Ok(PreviewImage {
        width: crop.width,
        height: crop.height,
        rgb8: rgb8.into(),
    })
}

/// A complete stationary acquisition step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturePair {
    /// Owned Left Captured frame.
    pub left: CapturedFrame,
    /// Owned Right Captured frame.
    pub right: CapturedFrame,
}

/// A role adapter that never exposes source MJPEG or unrotated pixels.
pub trait RoleCamera: 'static {
    /// Stable role assigned when this adapter was opened.
    fn role(&self) -> CameraRole;

    /// Acquire, decode, rotate, and validate one owned frame.
    fn capture(&mut self) -> Result<CapturedFrame, CameraCaptureError>;
}

/// One Camera-role acquisition failure.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CameraCaptureError {
    /// The device could not return an encoded frame. One retry is allowed.
    #[error("{role} Camera acquisition failed: {detail}")]
    Acquisition {
        /// Failed role.
        role: CameraRole,
        /// Device diagnostic.
        detail: String,
    },
    /// The returned MJPEG frame could not be decoded. One retry is allowed.
    #[error("{role} Camera decode failed: {detail}")]
    Decode {
        /// Failed role.
        role: CameraRole,
        /// Decoder diagnostic.
        detail: String,
    },
    /// The live frame did not match the fixed Camera profile.
    #[error("{role} Camera profile mismatch: {detail}")]
    Profile {
        /// Failed role.
        role: CameraRole,
        /// Mismatch diagnostic.
        detail: String,
    },
    /// Rotation or crop processing could not produce a valid frame.
    #[error("{role} Camera geometry failed: {detail}")]
    Geometry {
        /// Failed role.
        role: CameraRole,
        /// Geometry diagnostic.
        detail: String,
    },
}

impl CameraCaptureError {
    /// Construct a retryable acquisition failure.
    pub fn acquisition(role: CameraRole, detail: impl Into<String>) -> Self {
        Self::Acquisition {
            role,
            detail: detail.into(),
        }
    }

    /// Construct a retryable decode failure.
    pub fn decode(role: CameraRole, detail: impl Into<String>) -> Self {
        Self::Decode {
            role,
            detail: detail.into(),
        }
    }

    fn is_retryable(&self) -> bool {
        matches!(self, Self::Acquisition { .. } | Self::Decode { .. })
    }
}

/// Invalid Camera-role adapters supplied to a pair Machine.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CameraPairSetupError {
    /// An adapter was placed in the wrong stable role.
    #[error("expected the {expected} Camera adapter but received {actual}")]
    WrongRole {
        /// Required role for the slot.
        expected: CameraRole,
        /// Role reported by the adapter.
        actual: CameraRole,
    },
}

/// A complete-pair acquisition failure.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CapturePairError {
    /// The selected Machine adapter does not provide Camera capture.
    #[error("complete Capture pair is unavailable")]
    Unavailable,
    /// One role failed, including its permitted retry when applicable.
    #[error("complete Capture pair rejected: {0}")]
    Camera(#[from] CameraCaptureError),
    /// Both roles failed during the same stationary acquisition step.
    #[error("complete Capture pair rejected: {left}; {right}")]
    Both {
        /// Left Camera failure.
        left: CameraCaptureError,
        /// Right Camera failure.
        right: CameraCaptureError,
    },
}

/// Controller-owned adapters for the two stable Camera roles.
pub struct CapturePairMachine {
    left: Box<dyn RoleCamera>,
    right: Box<dyn RoleCamera>,
}

impl CapturePairMachine {
    /// Build a pair Machine from explicitly assigned role adapters.
    pub fn new(
        left: Box<dyn RoleCamera>,
        right: Box<dyn RoleCamera>,
    ) -> Result<Self, CameraPairSetupError> {
        if left.role() != CameraRole::Left {
            return Err(CameraPairSetupError::WrongRole {
                expected: CameraRole::Left,
                actual: left.role(),
            });
        }
        if right.role() != CameraRole::Right {
            return Err(CameraPairSetupError::WrongRole {
                expected: CameraRole::Right,
                actual: right.role(),
            });
        }
        Ok(Self { left, right })
    }
}

impl ControllerMachine for CapturePairMachine {
    fn capture_pair(&mut self) -> Result<CapturePair, CapturePairError> {
        let left = capture_role(self.left.as_mut(), CameraRole::Left);
        let right = capture_role(self.right.as_mut(), CameraRole::Right);

        let pair = match (left, right) {
            (Ok(left), Ok(right)) => Ok(CapturePair { left, right }),
            (Ok(left), Err(error)) if error.is_retryable() => {
                capture_role(self.right.as_mut(), CameraRole::Right)
                    .map(|right| CapturePair { left, right })
            }
            (Err(error), Ok(right)) if error.is_retryable() => {
                capture_role(self.left.as_mut(), CameraRole::Left)
                    .map(|left| CapturePair { left, right })
            }
            (Ok(_), Err(error)) | (Err(error), Ok(_)) => Err(error),
            (Err(left), Err(right)) => return Err(CapturePairError::Both { left, right }),
        };
        pair.map_err(CapturePairError::from)
    }
}

fn capture_role(
    camera: &mut dyn RoleCamera,
    expected: CameraRole,
) -> Result<CapturedFrame, CameraCaptureError> {
    let frame = camera.capture()?;
    if frame.role != expected {
        return Err(CameraCaptureError::Profile {
            role: expected,
            detail: format!("adapter returned a {} frame", frame.role),
        });
    }
    Ok(frame)
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn resolve_live_camera_identities(
    left: &Path,
    right: &Path,
) -> Result<(PathBuf, PathBuf), Vec<SetupBlocker>> {
    let mut blockers = Vec::new();
    let left = resolve_live_camera_identity(CameraRole::Left, left).map_err(|blocker| {
        blockers.push(blocker);
    });
    let right = resolve_live_camera_identity(CameraRole::Right, right).map_err(|blocker| {
        blockers.push(blocker);
    });
    if !blockers.is_empty() {
        return Err(blockers);
    }
    let (left, right) = (left.unwrap(), right.unwrap());
    if left == right {
        return Err(vec![SetupBlocker::new(format!(
            "Left and Right Camera identities resolve to the same live device {}",
            left.display()
        ))]);
    }
    Ok((left, right))
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn resolve_live_camera_identity(
    role: CameraRole,
    identity: &Path,
) -> Result<PathBuf, SetupBlocker> {
    let resolved = match fs::canonicalize(identity) {
        Ok(resolved) => resolved,
        Err(error) => {
            return Err(SetupBlocker::new(format!(
                "{role} Camera identity {} cannot be resolved: {error}",
                identity.display()
            )));
        }
    };
    let is_video_node = resolved
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("video"))
        .is_some_and(|index| !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit()));
    if !is_video_node {
        return Err(SetupBlocker::new(format!(
            "{role} Camera identity {} resolves to {}, not a videoN device node",
            identity.display(),
            resolved.display()
        )));
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::time::SystemTime;

    use super::{resolve_live_camera_identities, CameraRole, CapturedFrame};
    use crate::CropGeometry;

    #[test]
    fn half_turn_rotation_returns_owned_post_rotation_pixels() {
        let pixels = [1_u8, 2, 3, 4, 5, 6]
            .into_iter()
            .flat_map(|value| [value, value, value])
            .collect();

        let frame = CapturedFrame::from_unrotated_rgb8(
            CameraRole::Left,
            2,
            3,
            pixels,
            SystemTime::UNIX_EPOCH,
            180,
            CropGeometry {
                x: 0,
                y: 0,
                width: 2,
                height: 3,
            },
        )
        .unwrap();

        assert_eq!((frame.width, frame.height), (2, 3));
        assert_eq!(
            frame.rgb8().iter().step_by(3).copied().collect::<Vec<_>>(),
            vec![6, 5, 4, 3, 2, 1]
        );
    }

    #[test]
    fn live_camera_identities_must_resolve_to_distinct_video_devices() {
        let temp = tempfile::tempdir().unwrap();
        let device = temp.path().join("video0");
        let left = temp.path().join("left-camera");
        let right = temp.path().join("right-camera");
        fs::write(&device, []).unwrap();
        symlink(&device, &left).unwrap();
        symlink(&device, &right).unwrap();

        let blockers = resolve_live_camera_identities(&left, &right).unwrap_err();

        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].summary.contains("same live device"));
    }

    #[test]
    fn missing_and_mismatched_live_camera_identities_report_both_roles() {
        let temp = tempfile::tempdir().unwrap();
        let left = temp.path().join("missing-left");
        let non_camera = temp.path().join("ttyACM0");
        let right = temp.path().join("right-camera");
        fs::write(&non_camera, []).unwrap();
        symlink(&non_camera, &right).unwrap();

        let blockers = resolve_live_camera_identities(&left, &right).unwrap_err();

        assert_eq!(blockers.len(), 2);
        assert!(blockers[0].summary.contains("Left Camera identity"));
        assert!(blockers[0].summary.contains("cannot be resolved"));
        assert!(blockers[1].summary.contains("Right Camera identity"));
        assert!(blockers[1].summary.contains("not a videoN device node"));
    }
}
