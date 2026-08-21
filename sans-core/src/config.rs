//! Global Machine profile discovery and startup validation.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The parseable source used for the repository example and first startup.
pub const MACHINE_PROFILE_TEMPLATE: &str = include_str!("../../sans.example.toml");

const MAX_TOUCHDOWN_PRESS_PERCENT: f32 = 50.0;
const MAX_CLASSIFICATION_START_PERCENT: f32 = 10.0;
const MAX_WIGGLE_START_PERCENT: f32 = 65.0;
const MAX_WIGGLE_RETREAT_PERCENT: f32 = 5.0;
const MAX_BLOWER_ON_PERCENT: f32 = 80.0;
const MAX_VACUUM_OFF_PERCENT: f32 = 100.0;
const MAX_FINAL_LIFT_PERCENT: f32 = 105.0;
const MAX_BLOWER_OFF_DESCENT_PERCENT: f32 = 80.0;
const MAX_STOP_TERMINAL_MS: u64 = 500;

/// Complete, versioned configuration for one Sans machine.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MachineProfile {
    /// On-disk schema version. MVPrototype accepts only version 1.
    pub schema_version: u32,
    /// Absolute or profile-relative root for Scan session directories.
    pub data_root: PathBuf,
    /// Stable paths for the two controller boards.
    pub boards: BoardProfile,
    /// Stable identities and geometry for the Left and Right Camera roles.
    pub cameras: CameraProfiles,
    /// Commissioned operator input bounds for page width.
    pub page_width: PageWidthProfile,
    /// Positive I/O deadlines, bounded where the safety contract requires it.
    pub timeouts: TimeoutProfile,
    /// Pressure-classification tuning values.
    pub pickup: PickupProfile,
    /// Safety-bounded Touchdown and page-turn tuning values.
    pub motion: MotionProfile,
}

impl MachineProfile {
    /// Save the complete profile with a direct truncating write.
    ///
    /// This deliberately makes no atomic-replacement or storage-synchronization claim.
    pub fn save(&self, path: &Path) -> Result<(), ProfileSaveError> {
        let encoded = toml::to_string_pretty(self)?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .map_err(|source| ProfileSaveError::Open {
                path: path.to_path_buf(),
                source,
            })?;
        file.write_all(encoded.as_bytes())
            .map_err(|source| ProfileSaveError::Write {
                path: path.to_path_buf(),
                source,
            })
    }
}

/// A commissioning profile save failure.
#[derive(Debug, Error)]
pub enum ProfileSaveError {
    /// The complete typed profile could not be represented as TOML.
    #[error("cannot serialize the complete Machine profile: {0}")]
    Serialize(#[from] toml::ser::Error),
    /// The destination could not be opened for a truncating write.
    #[error("cannot open Machine profile {path} for a truncating write: {source}")]
    Open {
        /// Requested profile destination.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// Writing the serialized complete profile failed.
    #[error("cannot write complete Machine profile {path}: {source}")]
    Write {
        /// Requested profile destination.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
}

/// Stable serial-device paths for both controller boards.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BoardProfile {
    /// Stable serial-device path for the Ligature motion board.
    pub ligature_path: String,
    /// Stable serial-device path for the Monospace relay board.
    pub monospace_path: String,
}

/// Left and Right Camera-role configuration.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CameraProfiles {
    /// Configuration for the physical Left Camera role.
    pub left: CameraRoleProfile,
    /// Configuration for the physical Right Camera role.
    pub right: CameraRoleProfile,
}

/// Stable identity and post-rotation geometry for one Camera role.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CameraRoleProfile {
    /// Persistent `/dev/v4l/by-id` or commissioned `/dev/v4l/by-path` identity.
    pub identity: String,
    /// Clockwise quarter-turn rotation in degrees.
    pub rotation_degrees: u16,
    /// Full-page preview crop in post-rotation pixels.
    pub crop: CropGeometry,
}

/// A post-rotation Camera crop rectangle in pixels.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CropGeometry {
    /// Horizontal origin from the left edge.
    pub x: u32,
    /// Vertical origin from the top edge.
    pub y: u32,
    /// Nonzero crop width.
    pub width: u32,
    /// Nonzero crop height.
    pub height: u32,
}

/// Commissioned page-width input bounds in millimetres.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PageWidthProfile {
    /// Smallest accepted operator-entered page width.
    pub minimum_mm: f32,
    /// Largest accepted operator-entered page width.
    pub maximum_mm: f32,
}

/// Positive Machine-operation deadlines in milliseconds.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TimeoutProfile {
    /// Maximum wait while opening a live device.
    pub device_open_ms: u64,
    /// Maximum wait for an ordinary board command.
    pub command_ms: u64,
    /// Maximum wait for one Camera-role capture.
    pub camera_capture_ms: u64,
    /// Maximum current-epoch terminal wait during Stop; capped at 500 ms.
    pub stop_terminal_ms: u64,
}

/// Pressure-drop classifier tuning for one Capture attempt.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PickupProfile {
    /// Minimum Pressure drop showing that the vacuum system works.
    pub vacuum_working_drop_mbar: f32,
    /// Higher Pressure drop showing that a page is sealed.
    pub pickup_drop_mbar: f32,
    /// Consecutive samples required for a classification.
    pub confirmation_samples: u32,
}

/// Touchdown press and page-turn Lift-percentage tuning.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MotionProfile {
    /// Normalized Touchdown press command, capped by the immutable 50% ceiling.
    pub touchdown_press_percent: f32,
    /// Lift percentage at which pressure classification begins.
    pub classification_start_percent: f32,
    /// Lift percentage at which the downward wiggle begins.
    pub wiggle_start_percent: f32,
    /// Downward wiggle distance as a page-width percentage.
    pub wiggle_retreat_percent: f32,
    /// Lift percentage after which the Turn blower is enabled.
    pub blower_on_percent: f32,
    /// Lift percentage after which vacuum is disabled.
    pub vacuum_off_percent: f32,
    /// Final successful Lift percentage.
    pub final_lift_percent: f32,
    /// Descending Lift percentage at which the Turn blower is disabled.
    pub blower_off_descent_percent: f32,
}

/// A Machine profile that has passed structural and storage validation.
#[derive(Debug)]
pub struct PreparedMachineProfile {
    profile_path: PathBuf,
    data_root: PathBuf,
    profile: MachineProfile,
}

impl PreparedMachineProfile {
    /// The selected Machine-profile path.
    pub fn profile_path(&self) -> &Path {
        &self.profile_path
    }

    /// The checked storage root, resolved against the profile location.
    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    /// The structurally validated profile.
    pub fn profile(&self) -> &MachineProfile {
        &self.profile
    }
}

/// A fatal error encountered before device adapters may be opened.
#[derive(Debug, Error)]
pub enum StartupError {
    /// Neither standard environment variable can locate a user config directory.
    #[error("cannot resolve the Machine profile: neither XDG_CONFIG_HOME nor HOME is set")]
    ConfigHomeUnavailable,
    /// The first-start template could not be created.
    #[error("cannot create Machine profile template at {path}: {source}")]
    TemplateCreate {
        /// Selected Machine-profile path.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// A first-start template was created and must be commissioned before restart.
    #[error("created Machine profile template at {path}; fill every placeholder and restart Sans")]
    TemplateCreated {
        /// Selected path at which the template was written.
        path: PathBuf,
    },
    /// An existing Machine profile could not be read.
    #[error("cannot read Machine profile at {path}: {source}")]
    ProfileRead {
        /// Selected Machine-profile path.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The selected file was not a complete supported TOML profile.
    #[error("cannot parse Machine profile at {path}: {source}")]
    ProfileParse {
        /// Selected Machine-profile path.
        path: PathBuf,
        /// TOML decoding failure.
        #[source]
        source: toml::de::Error,
    },
    /// The decoded profile failed one or more structural checks.
    #[error("Machine profile at {path} is invalid: {}", .problems.join("; "))]
    ProfileInvalid {
        /// Selected Machine-profile path.
        path: PathBuf,
        /// Exhaustive structural problems found before hardware startup.
        problems: Vec<String>,
    },
    /// The resolved data root could not be created and write-checked.
    #[error("data root {path} cannot be created or written: {source}")]
    DataRootUnavailable {
        /// Resolved data-root path.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The operating system refused to create the controller thread.
    #[error("cannot create the Sans controller thread: {0}")]
    ControllerThreadStart(#[source] io::Error),
}

/// Resolve, load, and validate the global Machine profile without opening hardware.
pub fn prepare_machine_profile(
    explicit_path: Option<&Path>,
) -> Result<PreparedMachineProfile, StartupError> {
    let profile_path = resolve_machine_profile_path(explicit_path)?;
    if !profile_path.exists() {
        create_template(&profile_path)?;
        return Err(StartupError::TemplateCreated { path: profile_path });
    }

    let source = fs::read_to_string(&profile_path).map_err(|source| StartupError::ProfileRead {
        path: profile_path.clone(),
        source,
    })?;
    let profile: MachineProfile =
        toml::from_str(&source).map_err(|source| StartupError::ProfileParse {
            path: profile_path.clone(),
            source,
        })?;
    let problems = validate_profile(&profile);
    if !problems.is_empty() {
        return Err(StartupError::ProfileInvalid {
            path: profile_path,
            problems,
        });
    }

    let data_root = if profile.data_root.is_absolute() {
        profile.data_root.clone()
    } else {
        profile_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(&profile.data_root)
    };
    ensure_writable_data_root(&data_root)?;

    Ok(PreparedMachineProfile {
        profile_path,
        data_root,
        profile,
    })
}

/// Resolve an explicit path or the normal XDG/user configuration location.
pub fn resolve_machine_profile_path(explicit_path: Option<&Path>) -> Result<PathBuf, StartupError> {
    if let Some(path) = explicit_path {
        return Ok(path.to_path_buf());
    }

    if let Some(config_home) = env::var_os("XDG_CONFIG_HOME").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(config_home).join("sans/sans.toml"));
    }

    env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".config/sans/sans.toml"))
        .ok_or(StartupError::ConfigHomeUnavailable)
}

fn create_template(path: &Path) -> Result<(), StartupError> {
    let result = (|| -> io::Result<()> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(MACHINE_PROFILE_TEMPLATE.as_bytes())
    })();

    result.map_err(|source| StartupError::TemplateCreate {
        path: path.to_path_buf(),
        source,
    })
}

fn validate_profile(profile: &MachineProfile) -> Vec<String> {
    let mut problems = Vec::new();

    if profile.schema_version != 1 {
        problems.push("schema_version must be 1".into());
    }
    validate_required_string(
        "boards.ligature_path",
        &profile.boards.ligature_path,
        &mut problems,
    );
    validate_required_string(
        "boards.monospace_path",
        &profile.boards.monospace_path,
        &mut problems,
    );
    if profile.boards.ligature_path == profile.boards.monospace_path {
        problems.push("board paths must be distinct".into());
    }
    validate_camera("cameras.left", &profile.cameras.left, &mut problems);
    validate_camera("cameras.right", &profile.cameras.right, &mut problems);
    if profile.cameras.left.identity == profile.cameras.right.identity {
        problems.push("Camera-role identities must be distinct".into());
    }
    if profile.data_root.as_os_str().is_empty() {
        problems.push("data_root must not be empty".into());
    }

    validate_positive_finite(
        "page_width.minimum_mm",
        profile.page_width.minimum_mm,
        &mut problems,
    );
    validate_positive_finite(
        "page_width.maximum_mm",
        profile.page_width.maximum_mm,
        &mut problems,
    );
    if profile.page_width.minimum_mm >= profile.page_width.maximum_mm {
        problems.push("page-width bounds must be ordered".into());
    }
    validate_positive_timeout(
        "timeouts.device_open_ms",
        profile.timeouts.device_open_ms,
        &mut problems,
    );
    validate_positive_timeout(
        "timeouts.command_ms",
        profile.timeouts.command_ms,
        &mut problems,
    );
    validate_positive_timeout(
        "timeouts.camera_capture_ms",
        profile.timeouts.camera_capture_ms,
        &mut problems,
    );
    validate_timeout(
        "timeouts.stop_terminal_ms",
        profile.timeouts.stop_terminal_ms,
        MAX_STOP_TERMINAL_MS,
        &mut problems,
    );

    validate_positive_finite(
        "pickup.vacuum_working_drop_mbar",
        profile.pickup.vacuum_working_drop_mbar,
        &mut problems,
    );
    validate_positive_finite(
        "pickup.pickup_drop_mbar",
        profile.pickup.pickup_drop_mbar,
        &mut problems,
    );
    if profile.pickup.vacuum_working_drop_mbar >= profile.pickup.pickup_drop_mbar {
        problems.push("pickup thresholds must be ordered".into());
    }
    if profile.pickup.confirmation_samples == 0 {
        problems.push("pickup.confirmation_samples must be positive".into());
    }

    validate_motion(&profile.motion, &mut problems);
    problems
}

fn validate_required_string(name: &str, value: &str, problems: &mut Vec<String>) {
    if value.trim().is_empty() || value.contains('<') || value.contains('>') {
        problems.push(format!("{name} must be commissioned and nonempty"));
    }
}

fn validate_camera(name: &str, camera: &CameraRoleProfile, problems: &mut Vec<String>) {
    validate_required_string(&format!("{name}.identity"), &camera.identity, problems);
    if !camera.identity.starts_with("/dev/v4l/by-id/")
        && !camera.identity.starts_with("/dev/v4l/by-path/")
    {
        problems.push(format!(
            "{name}.identity must use a stable /dev/v4l/by-id or /dev/v4l/by-path identity"
        ));
    }
    if !matches!(camera.rotation_degrees, 0 | 90 | 180 | 270) {
        problems.push(format!("{name}.rotation_degrees must be a quarter turn"));
    }
    if camera.crop.width == 0 || camera.crop.height == 0 {
        problems.push(format!("{name}.crop dimensions must be nonzero"));
    }
    if camera.crop.x.checked_add(camera.crop.width).is_none()
        || camera.crop.y.checked_add(camera.crop.height).is_none()
    {
        problems.push(format!("{name}.crop geometry overflows"));
    }
}

fn validate_timeout(name: &str, value: u64, ceiling: u64, problems: &mut Vec<String>) {
    if value == 0 {
        problems.push(format!("{name} must be positive"));
    } else if value > ceiling {
        problems.push(format!("{name} exceeds immutable {ceiling} ms ceiling"));
    }
}

fn validate_positive_timeout(name: &str, value: u64, problems: &mut Vec<String>) {
    if value == 0 {
        problems.push(format!("{name} must be positive"));
    }
}

fn validate_positive_finite(name: &str, value: f32, problems: &mut Vec<String>) {
    if !value.is_finite() || value <= 0.0 {
        problems.push(format!("{name} must be positive and finite"));
    }
}

fn validate_motion(motion: &MotionProfile, problems: &mut Vec<String>) {
    for (name, value, ceiling) in [
        (
            "classification_start_percent",
            motion.classification_start_percent,
            MAX_CLASSIFICATION_START_PERCENT,
        ),
        (
            "wiggle_start_percent",
            motion.wiggle_start_percent,
            MAX_WIGGLE_START_PERCENT,
        ),
        (
            "wiggle_retreat_percent",
            motion.wiggle_retreat_percent,
            MAX_WIGGLE_RETREAT_PERCENT,
        ),
        (
            "blower_on_percent",
            motion.blower_on_percent,
            MAX_BLOWER_ON_PERCENT,
        ),
        (
            "vacuum_off_percent",
            motion.vacuum_off_percent,
            MAX_VACUUM_OFF_PERCENT,
        ),
        (
            "final_lift_percent",
            motion.final_lift_percent,
            MAX_FINAL_LIFT_PERCENT,
        ),
        (
            "blower_off_descent_percent",
            motion.blower_off_descent_percent,
            MAX_BLOWER_OFF_DESCENT_PERCENT,
        ),
    ] {
        validate_positive_finite(&format!("motion.{name}"), value, problems);
        if value > ceiling {
            problems.push(format!(
                "motion.{name} exceeds immutable {ceiling}% ceiling"
            ));
        }
    }
    validate_positive_finite(
        "motion.touchdown_press_percent",
        motion.touchdown_press_percent,
        problems,
    );
    if motion.touchdown_press_percent > MAX_TOUCHDOWN_PRESS_PERCENT {
        problems.push(format!(
            "motion.touchdown_press_percent exceeds immutable {MAX_TOUCHDOWN_PRESS_PERCENT}% ceiling"
        ));
    }
    if !(motion.classification_start_percent
        < motion.wiggle_start_percent - motion.wiggle_retreat_percent
        && motion.wiggle_retreat_percent < motion.wiggle_start_percent
        && motion.wiggle_start_percent < motion.blower_on_percent
        && motion.blower_on_percent < motion.vacuum_off_percent
        && motion.vacuum_off_percent < motion.final_lift_percent)
    {
        problems.push("motion Lift-percentage anchors must be safely ordered".into());
    }
}

fn ensure_writable_data_root(path: &Path) -> Result<(), StartupError> {
    let result = (|| -> io::Result<()> {
        fs::create_dir_all(path)?;
        let probe = path.join(format!(".sans-write-check-{}", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)?;
        file.write_all(b"writable")?;
        fs::remove_file(probe)
    })();

    result.map_err(|source| StartupError::DataRootUnavailable {
        path: path.to_path_buf(),
        source,
    })
}
