use std::{env, fs, path::PathBuf};

use sans_core::{prepare_machine_profile, resolve_machine_profile_path, StartupError};

const VALID_PROFILE: &str = include_str!("fixtures/valid-sans.toml");

#[test]
fn missing_profile_creates_parseable_template_and_stops_startup() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("nested/sans.toml");

    let result = prepare_machine_profile(Some(&profile_path));

    assert!(matches!(
        result,
        Err(StartupError::TemplateCreated { path }) if path == profile_path
    ));
    let template = fs::read_to_string(&profile_path).unwrap();
    let parsed = toml::from_str::<toml::Value>(&template).unwrap();
    assert_eq!(parsed["schema_version"].as_integer(), Some(2));
    assert!(parsed["odrive"].get("lift_axis").is_none());
    assert!(parsed["boards"].get("ligature_path").is_none());
    let error = prepare_machine_profile(Some(&profile_path)).unwrap_err();
    assert!(matches!(error, StartupError::ProfileInvalid { .. }));
    let message = error.to_string();
    assert!(message.contains("odrive.usb_serial"), "{message}");
    assert!(
        message.contains("odrive.api_fingerprint_sha256"),
        "{message}"
    );
}

#[test]
fn normal_profile_path_uses_xdg_or_user_config_location() {
    let expected = env::var_os("XDG_CONFIG_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map(|path| path.join("sans/sans.toml"))
        .or_else(|| {
            env::var_os("HOME")
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .map(|path| path.join(".config/sans/sans.toml"))
        });

    match expected {
        Some(expected) => assert_eq!(resolve_machine_profile_path(None).unwrap(), expected),
        None => assert!(matches!(
            resolve_machine_profile_path(None),
            Err(StartupError::ConfigHomeUnavailable)
        )),
    }
}

#[test]
fn valid_profile_resolves_and_creates_relative_data_root() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("config/sans.toml");
    fs::create_dir_all(profile_path.parent().unwrap()).unwrap();
    fs::write(&profile_path, VALID_PROFILE).unwrap();

    let prepared = prepare_machine_profile(Some(&profile_path)).unwrap();

    assert_eq!(prepared.profile_path(), profile_path);
    assert_eq!(prepared.data_root(), temp.path().join("config/scan-data"));
    assert!(prepared.data_root().is_dir());
    let odrive = &prepared.profile().odrive;
    assert_eq!(odrive.usb_serial, "316536713335");
    assert_eq!(odrive.axis, 0);
    assert!(odrive.lift_axis.is_none());
}

#[test]
fn structural_mistakes_are_reported_with_the_selected_profile_path() {
    let cases = [
        (
            "schema_version = 2",
            "schema_version = 3",
            "unsupported schema_version 3",
        ),
        (
            "schema_version = 2",
            "schema_version = \"2\"",
            "unsupported schema_version",
        ),
        (
            "monospace_path = \"/dev/serial/by-id/monospace\"",
            "monospace_path = \"\"",
            "boards.monospace_path",
        ),
        (
            "monospace_path = \"/dev/serial/by-id/monospace\"",
            "monospace_path = \"<set-stable-monospace-device-path>\"",
            "directly under /dev/serial",
        ),
        (
            "monospace_path = \"/dev/serial/by-id/monospace\"",
            "monospace_path = \"/dev/serial/by-id/\"",
            "directly under /dev/serial",
        ),
        (
            "monospace_path = \"/dev/serial/by-id/monospace\"",
            "monospace_path = \"/dev/serial/by-id/../ttyACM0\"",
            "directly under /dev/serial",
        ),
        (
            "monospace_path = \"/dev/serial/by-id/monospace\"",
            "ligature_path = \"/dev/serial/by-id/ligature\"\nmonospace_path = \"/dev/serial/by-id/monospace\"",
            "ligature_path",
        ),
        (
            "usb_serial = \"316536713335\"",
            "usb_serial = \"\"",
            "odrive.usb_serial must be 1 to 16 hex digits",
        ),
        (
            "usb_serial = \"316536713335\"",
            "usb_serial = \"<set-odrive-usb-serial-hex>\"",
            "odrive.usb_serial must be 1 to 16 hex digits",
        ),
        (
            "usb_serial = \"316536713335\"",
            "usb_serial = \"31653671333531653\"",
            "odrive.usb_serial must be 1 to 16 hex digits",
        ),
        ("axis = 0", "axis = 2", "odrive.axis must be 0 or 1"),
        (
            "api_fingerprint_sha256 = \"a36d84437bd2bf5925c0119a1df53d88de8ebe1aa8df0a338d2f6dd56fd5e52b\"",
            "api_fingerprint_sha256 = \"<set-commissioned-api-fingerprint-sha256>\"",
            "odrive.api_fingerprint_sha256 must be 64 lowercase hex digits",
        ),
        (
            "api_fingerprint_sha256 = \"a36d84437bd2bf5925c0119a1df53d88de8ebe1aa8df0a338d2f6dd56fd5e52b\"",
            "api_fingerprint_sha256 = \"A36D84437BD2BF5925C0119A1DF53D88DE8EBE1AA8DF0A338D2F6DD56FD5E52B\"",
            "odrive.api_fingerprint_sha256 must be 64 lowercase hex digits",
        ),
        (
            "api_fingerprint_sha256 = \"a36d84437bd2bf5925c0119a1df53d88de8ebe1aa8df0a338d2f6dd56fd5e52b\"",
            "api_fingerprint_sha256 = \"a36d84437bd2bf5925c0119a1df53d88\"",
            "odrive.api_fingerprint_sha256 must be 64 lowercase hex digits",
        ),
        (
            "poll_interval_ms = 20",
            "poll_interval_ms = 4",
            "odrive.poll_interval_ms must be between 5 and 100 ms",
        ),
        (
            "poll_interval_ms = 20",
            "poll_interval_ms = 101",
            "odrive.poll_interval_ms must be between 5 and 100 ms",
        ),
        (
            "telemetry_max_age_ms = 100",
            "telemetry_max_age_ms = 39",
            "odrive.telemetry_max_age_ms must be at least twice odrive.poll_interval_ms",
        ),
        (
            "telemetry_max_age_ms = 100",
            "telemetry_max_age_ms = 251",
            "odrive.telemetry_max_age_ms exceeds immutable 250 ms ceiling",
        ),
        (
            "request_deadline_ms = 100",
            "request_deadline_ms = 0",
            "odrive.request_deadline_ms must be between 1 and 250 ms",
        ),
        (
            "request_deadline_ms = 100",
            "request_deadline_ms = 251",
            "odrive.request_deadline_ms must be between 1 and 250 ms",
        ),
        (
            "urgent_deadline_ms = 50",
            "urgent_deadline_ms = 0",
            "odrive.urgent_deadline_ms must be between 1 and 100 ms",
        ),
        (
            "urgent_deadline_ms = 50",
            "urgent_deadline_ms = 101",
            "odrive.urgent_deadline_ms must be between 1 and 100 ms",
        ),
        (
            "request_deadline_ms = 100",
            "request_deadline_ms = 40",
            "odrive.urgent_deadline_ms must not exceed odrive.request_deadline_ms",
        ),
        (
            "identity = \"/dev/v4l/by-id/left-camera\"",
            "identity = \"<set-/dev/v4l/by-id-or-by-path-identity>\"",
            "directly under /dev/v4l",
        ),
        (
            "identity = \"/dev/v4l/by-id/left-camera\"",
            "identity = \"/dev/video0\"",
            "directly under /dev/v4l",
        ),
        (
            "identity = \"/dev/v4l/by-id/left-camera\"",
            "identity = \"/dev/v4l/by-id/\"",
            "directly under /dev/v4l",
        ),
        (
            "identity = \"/dev/v4l/by-id/left-camera\"",
            "identity = \"/dev/v4l/by-id/../video0\"",
            "directly under /dev/v4l",
        ),
        (
            "rotation_degrees = 90",
            "rotation_degrees = 45",
            "quarter turn",
        ),
        ("width = 2000", "width = 0", "crop dimensions"),
        ("x = 10", "x = 2160", "crop must intersect"),
        ("minimum_mm = 100.0", "minimum_mm = 0.0", "minimum_mm"),
        ("command_ms = 2000", "command_ms = 0", "command_ms"),
        (
            "stop_terminal_ms = 500",
            "stop_terminal_ms = 501",
            "immutable 500 ms",
        ),
        (
            "vacuum_working_drop_mbar = 0.5",
            "vacuum_working_drop_mbar = 1.3",
            "pickup thresholds",
        ),
        (
            "confirmation_samples = 3",
            "confirmation_samples = 0",
            "confirmation_samples",
        ),
        (
            "touchdown_press_percent = 50.0",
            "touchdown_press_percent = 50.1",
            "immutable 50%",
        ),
        (
            "final_lift_percent = 105.0",
            "final_lift_percent = 105.1",
            "immutable 105%",
        ),
        (
            "blower_on_percent = 80.0",
            "blower_on_percent = 70.0",
            "anchors must be safely ordered",
        ),
        ("data_root = \"scan-data\"", "data_root = \"\"", "data_root"),
    ];

    for (index, (from, to, expected)) in cases.iter().copied().enumerate() {
        let temp = tempfile::tempdir().unwrap();
        let profile_path = temp.path().join(format!("case-{index}.toml"));
        let profile = VALID_PROFILE.replacen(from, to, 1);
        assert_ne!(profile, VALID_PROFILE, "invalid test replacement: {from}");
        fs::write(&profile_path, profile).unwrap();

        let error = prepare_machine_profile(Some(&profile_path)).unwrap_err();

        let message = error.to_string();
        assert!(message.contains(&profile_path.display().to_string()));
        assert!(
            message.contains(expected),
            "expected {:?} in {:?}",
            expected,
            message
        );
    }
}

#[test]
fn crop_extending_past_the_rotated_frame_is_valid_for_capture_time_clamping() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    let profile = VALID_PROFILE
        .replacen("width = 2000", "width = 5000", 1)
        .replacen("height = 3600", "height = 5000", 1);
    fs::write(&profile_path, profile).unwrap();

    let prepared = prepare_machine_profile(Some(&profile_path)).unwrap();

    assert_eq!(prepared.profile().cameras.left.crop.width, 5000);
    assert_eq!(prepared.profile().cameras.left.crop.height, 5000);
}

#[test]
fn startup_fails_when_data_root_cannot_be_created_as_a_directory() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    fs::write(&profile_path, VALID_PROFILE).unwrap();
    fs::write(temp.path().join("scan-data"), "occupied by a file").unwrap();

    let error = prepare_machine_profile(Some(&profile_path)).unwrap_err();

    assert!(
        matches!(error, StartupError::DataRootUnavailable { path, .. } if path == temp.path().join("scan-data"))
    );
}

#[test]
fn stale_legacy_write_probe_does_not_block_a_writable_data_root() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    let data_root = temp.path().join("scan-data");
    fs::create_dir(&data_root).unwrap();
    let stale_probe = data_root.join(format!(".sans-write-check-{}", std::process::id()));
    fs::write(&stale_probe, "keep me").unwrap();
    fs::write(&profile_path, VALID_PROFILE).unwrap();

    prepare_machine_profile(Some(&profile_path)).unwrap();

    assert_eq!(fs::read_to_string(stale_probe).unwrap(), "keep me");
}

#[test]
fn complete_profile_save_truncates_old_content_and_propagates_open_errors() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    fs::write(&profile_path, VALID_PROFILE).unwrap();
    let prepared = prepare_machine_profile(Some(&profile_path)).unwrap();
    fs::write(
        &profile_path,
        format!("{VALID_PROFILE}\n[obsolete]\nstale = true\n"),
    )
    .unwrap();

    prepared.profile().save(&profile_path).unwrap();

    let saved = fs::read_to_string(&profile_path).unwrap();
    assert!(!saved.contains("obsolete"));
    assert!(!saved.contains("lift_axis"));
    let reloaded = prepare_machine_profile(Some(&profile_path)).unwrap();
    assert!(reloaded.profile().odrive.lift_axis.is_none());

    let directory_path = temp.path().join("not-a-file");
    fs::create_dir(&directory_path).unwrap();
    assert!(prepared.profile().save(&directory_path).is_err());
}

#[test]
fn legitimate_device_names_may_contain_angle_brackets() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    let profile = VALID_PROFILE
        .replacen(
            "monospace_path = \"/dev/serial/by-id/monospace\"",
            "monospace_path = \"/dev/serial/by-id/monospace<primary>\"",
            1,
        )
        .replacen(
            "identity = \"/dev/v4l/by-id/left-camera\"",
            "identity = \"/dev/v4l/by-id/left-camera<primary>\"",
            1,
        );
    fs::write(&profile_path, profile).unwrap();

    prepare_machine_profile(Some(&profile_path)).unwrap();
}

const VERSION_1_PROFILE: &str = r#"schema_version = 1
data_root = "scan-data"

[boards]
ligature_path = "/dev/serial/by-id/ligature"
monospace_path = "/dev/serial/by-id/monospace"
"#;

#[test]
fn version_1_profile_is_rejected_with_odrive_migration_guidance() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    fs::write(&profile_path, VERSION_1_PROFILE).unwrap();

    let error = prepare_machine_profile(Some(&profile_path)).unwrap_err();

    assert!(matches!(error, StartupError::ProfileVersionRetired { .. }));
    let message = error.to_string();
    assert!(message.contains(&profile_path.display().to_string()));
    assert!(message.contains("boards.ligature_path"), "{message}");
    assert!(message.contains("[odrive]"), "{message}");
    assert!(message.contains("template"), "{message}");
}

const LIFT_AXIS_TABLE: &str = r#"
[odrive.lift_axis]
direction = "reversed"
mm_per_turn = 8.0
travel_minimum_mm = 0.0
travel_maximum_mm = 300.0
current_limit_a = 4.8
velocity_limit_mm_s = 8.0
acceleration_limit_mm_s2 = 8.0
deceleration_limit_mm_s2 = 4.0
"#;

#[test]
fn saved_commissioned_profile_round_trips_its_lift_axis_table() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    fs::write(&profile_path, format!("{VALID_PROFILE}{LIFT_AXIS_TABLE}")).unwrap();
    let prepared = prepare_machine_profile(Some(&profile_path)).unwrap();

    prepared.profile().save(&profile_path).unwrap();

    let reloaded = prepare_machine_profile(Some(&profile_path)).unwrap();
    assert_eq!(
        reloaded.profile().odrive.lift_axis,
        prepared.profile().odrive.lift_axis
    );
}

#[test]
fn complete_lift_axis_table_prepares_as_commissioned() {
    let temp = tempfile::tempdir().unwrap();
    let profile_path = temp.path().join("sans.toml");
    fs::write(&profile_path, format!("{VALID_PROFILE}{LIFT_AXIS_TABLE}")).unwrap();

    let prepared = prepare_machine_profile(Some(&profile_path)).unwrap();

    let lift_axis = prepared.profile().odrive.lift_axis.as_ref().unwrap();
    assert_eq!(lift_axis.direction, sans_core::LiftDirection::Reversed);
    assert_eq!(lift_axis.mm_per_turn, 8.0);
    assert_eq!(lift_axis.current_limit_a, 4.8);
}

#[test]
fn lift_axis_limits_above_software_policy_caps_are_reported() {
    let cases = [
        (
            "current_limit_a = 4.8",
            "current_limit_a = 4.81",
            "odrive.lift_axis.current_limit_a exceeds the 4.8 A software policy cap",
        ),
        (
            "current_limit_a = 4.8",
            "current_limit_a = 0.0",
            "odrive.lift_axis.current_limit_a must be positive and finite",
        ),
        (
            "velocity_limit_mm_s = 8.0",
            "velocity_limit_mm_s = 8.1",
            "odrive.lift_axis.velocity_limit_mm_s exceeds the 1 motor turn per second software policy cap",
        ),
        (
            "acceleration_limit_mm_s2 = 8.0",
            "acceleration_limit_mm_s2 = 8.1",
            "odrive.lift_axis.acceleration_limit_mm_s2 exceeds the 1 motor turn per second squared software policy cap",
        ),
        (
            "deceleration_limit_mm_s2 = 4.0",
            "deceleration_limit_mm_s2 = 8.1",
            "odrive.lift_axis.deceleration_limit_mm_s2 exceeds the 1 motor turn per second squared software policy cap",
        ),
        (
            "travel_maximum_mm = 300.0",
            "travel_maximum_mm = -1.0",
            "odrive.lift_axis travel bounds must be finite and ordered",
        ),
        (
            "mm_per_turn = 8.0",
            "mm_per_turn = 0.0",
            "odrive.lift_axis.mm_per_turn must be positive and finite",
        ),
        (
            "direction = \"reversed\"",
            "direction = \"sideways\"",
            "direction",
        ),
        (
            "deceleration_limit_mm_s2 = 4.0\n",
            "",
            "deceleration_limit_mm_s2",
        ),
    ];

    for (index, (from, to, expected)) in cases.iter().copied().enumerate() {
        let temp = tempfile::tempdir().unwrap();
        let profile_path = temp.path().join(format!("lift-axis-{index}.toml"));
        let table = LIFT_AXIS_TABLE.replacen(from, to, 1);
        assert_ne!(table, LIFT_AXIS_TABLE, "invalid test replacement: {from}");
        fs::write(&profile_path, format!("{VALID_PROFILE}{table}")).unwrap();

        let message = prepare_machine_profile(Some(&profile_path))
            .unwrap_err()
            .to_string();

        assert!(message.contains(&profile_path.display().to_string()));
        assert!(
            message.contains(expected),
            "expected {expected:?} in {message:?}"
        );
    }
}
