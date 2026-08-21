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
    toml::from_str::<toml::Value>(&template).unwrap();
    assert!(matches!(
        prepare_machine_profile(Some(&profile_path)),
        Err(StartupError::ProfileInvalid { .. })
    ));
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
}

#[test]
fn structural_mistakes_are_reported_with_the_selected_profile_path() {
    let cases = [
        ("schema_version = 1", "schema_version = 2", "schema_version"),
        (
            "ligature_path = \"/dev/serial/by-id/ligature\"",
            "ligature_path = \"\"",
            "boards.ligature_path",
        ),
        (
            "identity = \"/dev/v4l/by-id/left-camera\"",
            "identity = \"/dev/video0\"",
            "stable /dev/v4l",
        ),
        (
            "rotation_degrees = 90",
            "rotation_degrees = 45",
            "quarter turn",
        ),
        ("width = 2000", "width = 0", "crop dimensions"),
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
    prepare_machine_profile(Some(&profile_path)).unwrap();

    let directory_path = temp.path().join("not-a-file");
    fs::create_dir(&directory_path).unwrap();
    assert!(prepared.profile().save(&directory_path).is_err());
}
