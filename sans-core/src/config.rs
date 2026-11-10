//! Global Machine profile discovery and startup validation.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The parseable source used for the repository example and first startup.
pub const MACHINE_PROFILE_TEMPLATE: &str = include_str!("../../sans.example.toml");

/// The only Machine-profile schema version this build reads.
pub const MACHINE_PROFILE_SCHEMA_VERSION: u32 = 2;
const SCHEMA_VERSION_INTEGER: i64 = MACHINE_PROFILE_SCHEMA_VERSION as i64;
const MAX_TOUCHDOWN_PRESS_PERCENT: f32 = 50.0;
const MAX_CLASSIFICATION_START_PERCENT: f32 = 10.0;
const MAX_WIGGLE_START_PERCENT: f32 = 65.0;
const MAX_WIGGLE_RETREAT_PERCENT: f32 = 5.0;
const MAX_BLOWER_ON_PERCENT: f32 = 80.0;
const MAX_VACUUM_OFF_PERCENT: f32 = 100.0;
const MAX_FINAL_LIFT_PERCENT: f32 = 105.0;
const MAX_BLOWER_OFF_DESCENT_PERCENT: f32 = 80.0;
const MAX_STOP_TERMINAL_MS: u64 = 500;
/// Immutable bounds on `odrive.poll_interval_ms`. Provisional and unmeasured; #60 reviews them.
pub const ODRIVE_POLL_INTERVAL_BOUNDS_MS: (u64, u64) = (5, 100);
/// Immutable ceiling on `odrive.telemetry_max_age_ms`. Provisional and unmeasured.
pub const ODRIVE_MAX_TELEMETRY_AGE_CEILING_MS: u64 = 250;
/// Immutable bounds on `odrive.request_deadline_ms`. Provisional and unmeasured.
pub const ODRIVE_REQUEST_DEADLINE_BOUNDS_MS: (u64, u64) = (1, 250);
/// Immutable bounds on `odrive.urgent_deadline_ms`. Provisional and unmeasured.
pub const ODRIVE_URGENT_DEADLINE_BOUNDS_MS: (u64, u64) = (1, 100);
/// Software policy cap on the commissioned motor current limit, in amps.
///
/// This is an arbitrary conservative starting bound chosen as host policy. It is
/// not derived from the mechanism and makes no physical safety qualification.
/// It matches the `current_lim` observed on the board on 2026-11-10, which is
/// provenance only. #60 reviews it before any change.
pub const ODRIVE_CURRENT_LIMIT_CAP_A: f32 = 4.8;
/// Software policy cap on commissioned velocity, in motor turns per second.
///
/// Arbitrary conservative host policy, not a measured or qualified limit. #60 reviews it.
pub const ODRIVE_VELOCITY_CAP_TURNS_PER_S: f32 = 1.0;
/// Software policy cap on commissioned acceleration and deceleration, in motor
/// turns per second squared.
///
/// Arbitrary conservative host policy, not a measured or qualified limit. #60 reviews it.
pub const ODRIVE_ACCELERATION_CAP_TURNS_PER_S2: f32 = 1.0;
const CAMERA_FRAME_WIDTH: u32 = 3840;
const CAMERA_FRAME_HEIGHT: u32 = 2160;

/// Complete, versioned configuration for one Sans machine.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MachineProfile {
    /// On-disk schema version. This build accepts only version 2.
    pub schema_version: u32,
    /// Absolute or profile-relative root for Scan session directories.
    pub data_root: PathBuf,
    /// Stable path for the Monospace relay board.
    pub boards: BoardProfile,
    /// Identity, link timing and optional commissioning for the ODrive lift axis.
    pub odrive: OdriveProfile,
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

/// Stable serial-device path for the Monospace relay board.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BoardProfile {
    /// Stable serial-device path for the Monospace relay board.
    pub monospace_path: String,
}

/// The MKS ODrive Mini that drives the lift axis over native Fibre USB.
///
/// Timing values are provisional and unmeasured; #60 reviews them. Their bounds
/// are immutable in code so a profile edit cannot make the link slower than the
/// Stop contract tolerates.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OdriveProfile {
    /// USB serial-number string as hex text, for example `316536713335`.
    pub usb_serial: String,
    /// Axis index on the board, 0 or 1.
    pub axis: u8,
    /// Commissioned SHA-256 of the flattened endpoint schema, 64 lowercase hex digits.
    pub api_fingerprint_sha256: String,
    /// Interval between telemetry cycle starts, 5 to 100 ms.
    pub poll_interval_ms: u64,
    /// Oldest usable telemetry sample, at least twice the poll interval and at most 250 ms.
    pub telemetry_max_age_ms: u64,
    /// Deadline for one ordinary request, 1 to 250 ms.
    pub request_deadline_ms: u64,
    /// Deadline for one urgent request, 1 to 100 ms and not above the request deadline.
    pub urgent_deadline_ms: u64,
    /// Commissioned mechanism values. Absent means uncommissioned, and the lift axis stays blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lift_axis: Option<LiftAxisProfile>,
}

/// Which way positive motor turns move the lift axis.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LiftDirection {
    /// Millimetres equal motor turns times millimetres per turn.
    Normal,
    /// Millimetres equal negated motor turns times millimetres per turn.
    Reversed,
}

/// Commissioned lift-axis mechanism values and limits.
///
/// Limits are bounded by software policy caps in motor units, so a wrong
/// `mm_per_turn` cannot raise motor speed or current past the cap.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LiftAxisProfile {
    /// Commissioned motor direction.
    pub direction: LiftDirection,
    /// Millimetres of lift travel per motor turn; positive and finite.
    pub mm_per_turn: f32,
    /// Lowest commissioned soft bound for absolute moves.
    pub travel_minimum_mm: f32,
    /// Highest commissioned soft bound for absolute moves.
    pub travel_maximum_mm: f32,
    /// Motor current limit written to the board; positive, at most the 4.8 A policy cap.
    pub current_limit_a: f32,
    /// Velocity limit; at most 1 motor turn per second after conversion.
    pub velocity_limit_mm_s: f32,
    /// Trajectory acceleration limit; at most 1 motor turn per second squared after conversion.
    pub acceleration_limit_mm_s2: f32,
    /// Trajectory deceleration limit; at most 1 motor turn per second squared after conversion.
    pub deceleration_limit_mm_s2: f32,
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
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

/// Commissioned limits converted to the board's motor units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotorLimits {
    /// `motor.config.current_lim` in amps.
    pub current_a: f32,
    /// `controller.config.vel_limit` and `trap_traj.config.vel_limit` in turns per second.
    pub velocity_turns_per_s: f32,
    /// `trap_traj.config.accel_limit` in turns per second squared.
    pub acceleration_turns_per_s2: f32,
    /// `trap_traj.config.decel_limit` in turns per second squared.
    pub deceleration_turns_per_s2: f32,
}

impl LiftAxisProfile {
    /// Convert the commissioned limits to motor units, the values written to the board.
    pub fn motor_limits(&self) -> MotorLimits {
        MotorLimits {
            current_a: self.current_limit_a,
            velocity_turns_per_s: self.velocity_limit_mm_s / self.mm_per_turn,
            acceleration_turns_per_s2: self.acceleration_limit_mm_s2 / self.mm_per_turn,
            deceleration_turns_per_s2: self.deceleration_limit_mm_s2 / self.mm_per_turn,
        }
    }
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
    /// The profile uses schema version 1, which named the retired Ligature board.
    #[error(
        "Machine profile at {path} uses retired schema version 1: remove boards.ligature_path, \
         add the required [odrive] section and set schema_version = 2; compare with the \
         regenerated template (move this file aside and restart Sans to write a fresh one)"
    )]
    ProfileVersionRetired {
        /// Selected Machine-profile path.
        path: PathBuf,
    },
    /// The profile's schema version is missing or not one this build reads.
    #[error("Machine profile at {path} has unsupported schema_version {found}; this build reads only schema_version = 2")]
    ProfileVersionUnsupported {
        /// Selected Machine-profile path.
        path: PathBuf,
        /// The value found, or `missing`.
        found: String,
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
    check_schema_version(&profile_path, &source)?;
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

/// Read `schema_version` before strict decoding so retired layouts get guidance
/// instead of an unknown-field error.
fn check_schema_version(path: &Path, source: &str) -> Result<(), StartupError> {
    let table: toml::Table =
        toml::from_str(source).map_err(|source| StartupError::ProfileParse {
            path: path.to_path_buf(),
            source,
        })?;
    match table.get("schema_version") {
        Some(toml::Value::Integer(SCHEMA_VERSION_INTEGER)) => Ok(()),
        Some(toml::Value::Integer(1)) => Err(StartupError::ProfileVersionRetired {
            path: path.to_path_buf(),
        }),
        found => Err(StartupError::ProfileVersionUnsupported {
            path: path.to_path_buf(),
            found: found.map_or_else(|| "missing".into(), ToString::to_string),
        }),
    }
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

    if profile.schema_version != MACHINE_PROFILE_SCHEMA_VERSION {
        problems.push(format!(
            "schema_version must be {MACHINE_PROFILE_SCHEMA_VERSION}"
        ));
    }
    validate_board_path(
        "boards.monospace_path",
        &profile.boards.monospace_path,
        &mut problems,
    );
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
    problems.extend(odrive_profile_problems(&profile.odrive));
    problems
}

/// Every structural problem in an ODrive section, each named by its profile path.
pub(crate) fn odrive_profile_problems(odrive: &OdriveProfile) -> Vec<String> {
    let mut problems = Vec::new();
    let serial = &odrive.usb_serial;
    if serial.is_empty() || serial.len() > 16 || !serial.bytes().all(|b| b.is_ascii_hexdigit()) {
        problems.push("odrive.usb_serial must be 1 to 16 hex digits".into());
    }
    if odrive.axis > 1 {
        problems.push("odrive.axis must be 0 or 1".into());
    }
    let fingerprint = &odrive.api_fingerprint_sha256;
    if fingerprint.len() != 64
        || !fingerprint
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        problems.push("odrive.api_fingerprint_sha256 must be 64 lowercase hex digits".into());
    }
    validate_bounded_ms(
        "odrive.poll_interval_ms",
        odrive.poll_interval_ms,
        ODRIVE_POLL_INTERVAL_BOUNDS_MS,
        &mut problems,
    );
    if odrive.telemetry_max_age_ms < odrive.poll_interval_ms.saturating_mul(2) {
        problems.push(
            "odrive.telemetry_max_age_ms must be at least twice odrive.poll_interval_ms".into(),
        );
    }
    if odrive.telemetry_max_age_ms > ODRIVE_MAX_TELEMETRY_AGE_CEILING_MS {
        problems.push(format!(
            "odrive.telemetry_max_age_ms exceeds immutable {ODRIVE_MAX_TELEMETRY_AGE_CEILING_MS} ms ceiling"
        ));
    }
    validate_bounded_ms(
        "odrive.request_deadline_ms",
        odrive.request_deadline_ms,
        ODRIVE_REQUEST_DEADLINE_BOUNDS_MS,
        &mut problems,
    );
    validate_bounded_ms(
        "odrive.urgent_deadline_ms",
        odrive.urgent_deadline_ms,
        ODRIVE_URGENT_DEADLINE_BOUNDS_MS,
        &mut problems,
    );
    if odrive.urgent_deadline_ms > odrive.request_deadline_ms {
        problems
            .push("odrive.urgent_deadline_ms must not exceed odrive.request_deadline_ms".into());
    }
    if let Some(lift_axis) = &odrive.lift_axis {
        validate_lift_axis(lift_axis, &mut problems);
    }
    problems
}

fn validate_lift_axis(lift_axis: &LiftAxisProfile, problems: &mut Vec<String>) {
    let name = "odrive.lift_axis";
    validate_positive_finite(
        &format!("{name}.mm_per_turn"),
        lift_axis.mm_per_turn,
        problems,
    );
    if !(lift_axis.travel_minimum_mm.is_finite()
        && lift_axis.travel_maximum_mm.is_finite()
        && lift_axis.travel_minimum_mm < lift_axis.travel_maximum_mm)
    {
        problems.push(format!("{name} travel bounds must be finite and ordered"));
    }
    validate_positive_finite(
        &format!("{name}.current_limit_a"),
        lift_axis.current_limit_a,
        problems,
    );
    if lift_axis.current_limit_a > ODRIVE_CURRENT_LIMIT_CAP_A {
        problems.push(format!(
            "{name}.current_limit_a exceeds the {ODRIVE_CURRENT_LIMIT_CAP_A} A software policy cap"
        ));
    }
    let limits = lift_axis.motor_limits();
    for (field, value, converted, cap, unit) in [
        (
            "velocity_limit_mm_s",
            lift_axis.velocity_limit_mm_s,
            limits.velocity_turns_per_s,
            ODRIVE_VELOCITY_CAP_TURNS_PER_S,
            "motor turn per second",
        ),
        (
            "acceleration_limit_mm_s2",
            lift_axis.acceleration_limit_mm_s2,
            limits.acceleration_turns_per_s2,
            ODRIVE_ACCELERATION_CAP_TURNS_PER_S2,
            "motor turn per second squared",
        ),
        (
            "deceleration_limit_mm_s2",
            lift_axis.deceleration_limit_mm_s2,
            limits.deceleration_turns_per_s2,
            ODRIVE_ACCELERATION_CAP_TURNS_PER_S2,
            "motor turn per second squared",
        ),
    ] {
        validate_positive_finite(&format!("{name}.{field}"), value, problems);
        if converted.is_nan() || converted > cap {
            problems.push(format!(
                "{name}.{field} exceeds the {cap} {unit} software policy cap after conversion"
            ));
        }
    }
}

fn validate_bounded_ms(
    name: &str,
    value: u64,
    (low, high): (u64, u64),
    problems: &mut Vec<String>,
) {
    if !(low..=high).contains(&value) {
        problems.push(format!(
            "{name} must be between {low} and {high} ms (immutable bounds)"
        ));
    }
}

fn validate_board_path(name: &str, value: &str, problems: &mut Vec<String>) {
    validate_required_string(name, value, problems);
    if !is_direct_device_identity(value, &["/dev/serial/by-id", "/dev/serial/by-path"]) {
        problems.push(format!(
            "{name} must name one device directly under /dev/serial/by-id or /dev/serial/by-path"
        ));
    }
}

fn validate_required_string(name: &str, value: &str, problems: &mut Vec<String>) {
    if value.trim().is_empty() {
        problems.push(format!("{name} must be nonempty"));
    }
}

fn validate_camera(name: &str, camera: &CameraRoleProfile, problems: &mut Vec<String>) {
    validate_required_string(&format!("{name}.identity"), &camera.identity, problems);
    if !is_direct_device_identity(&camera.identity, &["/dev/v4l/by-id", "/dev/v4l/by-path"]) {
        problems.push(format!(
            "{name}.identity must name one device directly under /dev/v4l/by-id or /dev/v4l/by-path"
        ));
    }
    let frame_dimensions = match camera.rotation_degrees {
        0 | 180 => Some((CAMERA_FRAME_WIDTH, CAMERA_FRAME_HEIGHT)),
        90 | 270 => Some((CAMERA_FRAME_HEIGHT, CAMERA_FRAME_WIDTH)),
        _ => {
            problems.push(format!("{name}.rotation_degrees must be a quarter turn"));
            None
        }
    };
    if camera.crop.width == 0 || camera.crop.height == 0 {
        problems.push(format!("{name}.crop dimensions must be nonzero"));
    }
    if let Some((frame_width, frame_height)) = frame_dimensions {
        if camera.crop.x >= frame_width || camera.crop.y >= frame_height {
            problems.push(format!(
                "{name}.crop must intersect the {frame_width}x{frame_height} post-rotation frame"
            ));
        }
    }
}

fn is_direct_device_identity(identity: &str, directories: &[&str]) -> bool {
    directories.iter().any(|directory| {
        let Ok(relative) = Path::new(identity).strip_prefix(directory) else {
            return false;
        };
        let mut components = relative.components();
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
    })
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
        && motion.vacuum_off_percent < motion.final_lift_percent
        && motion.blower_off_descent_percent <= motion.blower_on_percent)
    {
        problems.push("motion Lift-percentage anchors must be safely ordered".into());
    }
}

fn ensure_writable_data_root(path: &Path) -> Result<(), StartupError> {
    let result = (|| -> io::Result<()> {
        fs::create_dir_all(path)?;
        let mut probe = tempfile::Builder::new()
            .prefix(".sans-write-check-")
            .tempfile_in(path)?;
        probe.write_all(b"writable")
    })();

    result.map_err(|source| StartupError::DataRootUnavailable {
        path: path.to_path_buf(),
        source,
    })
}
