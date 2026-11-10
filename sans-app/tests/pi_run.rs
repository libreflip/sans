#![cfg(unix)]
//! Drives the real `scripts/pi-run` through fake `cross` and `ssh` adapters
//! resolved from `PATH`.
//!
//! The fake `ssh` runs the remote command with `sh -c` against an isolated
//! remote home and runtime directory, so the tests observe the script only
//! through its exit status, stderr, and the files it leaves on the "Pi".

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

const SYSTEM_PATH: &str = "/usr/bin:/bin";
const HOST: &str = "pi@sans-pi";
const RECORDED_ENV: &[&str] = &[
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XDG_CONFIG_HOME",
    "XDG_RUNTIME_DIR",
    "SANS_TEST_VALUE",
];

/// An isolated developer checkout, fake tool directory, and fake Pi.
struct Harness {
    root: TempDir,
    /// Exit status of the fake `cross`; `None` leaves `cross` off `PATH`.
    cross_status: Option<i32>,
    /// Environment of the remote login session beyond `HOME` and `PATH`.
    remote_env: Vec<(String, String)>,
    /// Number of upload bytes the fake connection delivers before EOF.
    truncate_upload: Option<usize>,
    /// Bytes the fake `cross` writes as the built `sans` binary.
    built_binary: Vec<u8>,
}

impl Harness {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let harness = Self {
            root,
            cross_status: Some(0),
            remote_env: Vec::new(),
            truncate_upload: None,
            built_binary: fake_app(0),
        };
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/pi-run");
        fs::create_dir_all(harness.repo().join("scripts")).unwrap();
        fs::copy(&script, harness.repo().join("scripts/pi-run")).unwrap();
        for dir in [
            harness.bin(),
            harness.log(),
            harness.home(),
            harness.runtime(),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        let runtime = harness.runtime();
        harness.set_remote_env("XDG_RUNTIME_DIR", runtime.to_str().unwrap())
    }

    /// A harness whose remote login session already has an X11 display.
    fn with_desktop() -> Self {
        Self::new().set_remote_env("DISPLAY", ":0")
    }

    fn set_remote_env(mut self, name: &str, value: &str) -> Self {
        self.remote_env.retain(|(existing, _)| existing != name);
        self.remote_env.push((name.to_owned(), value.to_owned()));
        self
    }

    fn unset_remote_env(mut self, name: &str) -> Self {
        self.remote_env.retain(|(existing, _)| existing != name);
        self
    }

    fn with_wayland_socket(self, name: &str) -> Self {
        let listener = UnixListener::bind(self.runtime().join(name)).unwrap();
        std::mem::forget(listener);
        self
    }

    fn repo(&self) -> PathBuf {
        self.root.path().join("repo")
    }

    fn bin(&self) -> PathBuf {
        self.root.path().join("bin")
    }

    fn log(&self) -> PathBuf {
        self.root.path().join("log")
    }

    /// Remote `HOME` on the fake Pi.
    fn home(&self) -> PathBuf {
        self.root.path().join("pi-home")
    }

    /// Remote `XDG_RUNTIME_DIR` on the fake Pi.
    fn runtime(&self) -> PathBuf {
        self.root.path().join("run")
    }

    fn remote_binary(&self) -> PathBuf {
        self.home().join(".local/bin/sans-dev")
    }

    fn write_local(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.path().join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with(&self.repo(), &[], args)
    }

    fn run_with(&self, cwd: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
        self.write_fake_tools();
        let mut command = Command::new(self.repo().join("scripts/pi-run"));
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", format!("{}:{SYSTEM_PATH}", self.bin().display()))
            .env("HOME", self.root.path().join("dev-home"))
            .stdin(Stdio::null());
        for (name, value) in env {
            command.env(name, value);
        }
        command.output().unwrap()
    }

    fn write_fake_tools(&self) {
        let log = self.log();
        let mut remote_env = format!(
            "HOME={} PATH={SYSTEM_PATH}",
            sh_quote(self.home().to_str().unwrap())
        );
        for (name, value) in &self.remote_env {
            remote_env.push_str(&format!(" {}", sh_quote(&format!("{name}={value}"))));
        }
        let upload_filter = match self.truncate_upload {
            Some(bytes) => format!("head -c {bytes}"),
            None => "cat".to_owned(),
        };
        write_executable(
            &self.bin().join("ssh"),
            &format!(
                r#"#!/bin/sh
log={log}
n=$(ls "$log" | grep -c '^ssh\.' || true)
printf '%s\0' "$@" > "$log/ssh.$n"
tty=
while [ $# -gt 0 ]; do
    case $1 in
        --) shift; break ;;
        -T) tty=none ;;
        -tt) tty=forced ;;
        *) echo "fake ssh: unexpected argument $1" >&2; exit 255 ;;
    esac
    shift
done
[ "$1" = {host} ] || {{ echo "fake ssh: unexpected host $1" >&2; exit 255; }}
shift
if [ "$tty" = none ]; then
    {upload_filter} | env -i {remote_env} sh -c "$*"
else
    env -i {remote_env} sh -c "$*"
fi
"#,
                log = sh_quote(log.to_str().unwrap()),
                host = sh_quote(HOST),
            ),
        );
        let cross = self.bin().join("cross");
        match self.cross_status {
            None => {
                let _ = fs::remove_file(cross);
            }
            Some(status) => {
                let built = self.write_local("built-sans", &self.built_binary);
                write_executable(
                    &cross,
                    &format!(
                        r#"#!/bin/sh
printf '%s\0' "$@" > {log}/cross.argv
pwd -P > {log}/cross.cwd
[ {status} -eq 0 ] || exit {status}
target=${{CARGO_TARGET_DIR:-target}}
mkdir -p "$target/aarch64-unknown-linux-gnu/release"
cp {built} "$target/aarch64-unknown-linux-gnu/release/sans"
"#,
                        log = sh_quote(log.to_str().unwrap()),
                        built = sh_quote(built.to_str().unwrap()),
                    ),
                );
            }
        }
    }

    /// Argument vectors of every `ssh` invocation, in order.
    fn ssh_calls(&self) -> Vec<Vec<String>> {
        (0..)
            .map(|n| self.log().join(format!("ssh.{n}")))
            .take_while(|path| path.exists())
            .map(|path| split_nul(&fs::read(path).unwrap()))
            .collect()
    }

    fn cross_call(&self) -> Option<(Vec<String>, PathBuf)> {
        let argv = fs::read(self.log().join("cross.argv")).ok()?;
        let cwd = fs::read_to_string(self.log().join("cross.cwd")).unwrap();
        Some((split_nul(&argv), PathBuf::from(cwd.trim_end())))
    }

    fn app_argv(&self) -> Option<Vec<String>> {
        let argv = fs::read(self.home().join("app.argv")).ok()?;
        Some(split_nul(&argv))
    }

    /// Recorded application environment; unset variables map to `None`.
    fn app_env(&self) -> BTreeMap<String, Option<String>> {
        split_nul(&fs::read(self.home().join("app.env")).unwrap())
            .into_iter()
            .map(|entry| match entry.split_once('=') {
                Some((name, value)) => (name.to_owned(), Some(value.to_owned())),
                None => (entry, None),
            })
            .collect()
    }
}

/// A shell script standing in for the aarch64 `sans` binary. It records its
/// argv and selected environment in the remote home, then exits with `status`.
fn fake_app(status: i32) -> Vec<u8> {
    let mut script = String::from(
        "#!/bin/sh\nfor arg in \"$@\"; do printf '%s\\0' \"$arg\"; done > \"$HOME/app.argv\"\n\
         : > \"$HOME/app.env\"\n",
    );
    for name in RECORDED_ENV {
        script.push_str(&format!(
            "if [ \"${{{name}+set}}\" ]; then printf '%s\\0' \"{name}=${name}\"; \
             else printf '%s\\0' {name}; fi >> \"$HOME/app.env\"\n"
        ));
    }
    script.push_str(&format!("exit {status}\n"));
    script.into_bytes()
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn split_nul(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    match text.strip_suffix('\0') {
        Some(body) => body.split('\0').map(str::to_owned).collect(),
        None => Vec::new(),
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn rejects_invalid_usage_before_calling_tools() {
    let harness = Harness::new();
    let cases: &[&[&str]] = &[
        &[],
        &[""],
        &["-v"],
        &["--wat", HOST],
        &["--binary"],
        &["--env"],
        &["--env", "NOEQUALS", HOST],
        &["--env", "1BAD=x", HOST],
        &["--env", "BAD-NAME=x", HOST],
        &["--env", "=x", HOST],
        &[HOST, "extra"],
        &["--", HOST],
    ];
    for args in cases {
        let output = harness.run(args);
        assert_eq!(output.status.code(), Some(2), "args {args:?}");
        assert!(stderr(&output).contains("usage:"), "args {args:?}");
    }
    assert!(harness.cross_call().is_none());
    assert!(harness.ssh_calls().is_empty());
}

#[test]
fn missing_binary_fails_before_ssh() {
    let harness = Harness::new();
    let missing = harness.root.path().join("no-such-sans");
    let directory = harness.root.path().to_path_buf();
    for path in [&missing, &directory] {
        let output = harness.run(&["--binary", path.to_str().unwrap(), HOST]);
        assert_eq!(output.status.code(), Some(1), "path {path:?}");
        assert!(stderr(&output).contains(path.to_str().unwrap()));
    }
    assert!(harness.cross_call().is_none());
    assert!(harness.ssh_calls().is_empty());
}

#[test]
fn builds_release_binary_in_repository_root_and_uploads_it() {
    let harness = Harness::with_desktop();
    let elsewhere = harness.root.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();

    let output = harness.run_with(&elsewhere, &[], &[HOST]);

    assert!(output.status.success(), "{}", stderr(&output));
    let (argv, cwd) = harness.cross_call().unwrap();
    assert_eq!(
        argv,
        [
            "build",
            "--package",
            "sans",
            "--release",
            "--locked",
            "--target",
            "aarch64-unknown-linux-gnu"
        ]
    );
    assert_eq!(cwd, harness.repo().canonicalize().unwrap());
    assert_eq!(fs::read(harness.remote_binary()).unwrap(), fake_app(0));
}

#[test]
fn build_failure_propagates_without_ssh() {
    let mut harness = Harness::new();
    harness.cross_status = Some(101);

    let output = harness.run(&[HOST]);

    assert_eq!(output.status.code(), Some(101));
    assert!(stderr(&output).contains("cross build failed"));
    assert!(harness.ssh_calls().is_empty());
}

#[test]
fn missing_cross_suggests_binary_option() {
    let mut harness = Harness::new();
    harness.cross_status = None;

    let output = harness.run(&[HOST]);

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("--binary"));
    assert!(harness.ssh_calls().is_empty());
}

#[test]
fn relative_cargo_target_dir_resolves_from_invoking_directory() {
    let harness = Harness::with_desktop();
    let elsewhere = harness.root.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();

    let output = harness.run_with(
        &elsewhere,
        &[("CARGO_TARGET_DIR", "custom-target")],
        &[HOST],
    );

    assert!(output.status.success(), "{}", stderr(&output));
    let built = elsewhere.join("custom-target/aarch64-unknown-linux-gnu/release/sans");
    assert_eq!(fs::read(built).unwrap(), fake_app(0));
    assert_eq!(fs::read(harness.remote_binary()).unwrap(), fake_app(0));
}

#[test]
fn binary_option_skips_build_and_resolves_from_invoking_directory() {
    let mut harness = Harness::with_desktop();
    harness.cross_status = None;
    let elsewhere = harness.root.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("ci-sans"), fake_app(0)).unwrap();

    let output = harness.run_with(&elsewhere, &[], &["--binary", "ci-sans", HOST]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(fs::read(harness.remote_binary()).unwrap(), fake_app(0));
}

/// Every byte value after the fake app's `exit`, so the upload must be
/// byte-identical rather than text-safe.
fn binary_with_all_bytes() -> Vec<u8> {
    let mut bytes = fake_app(0);
    bytes.extend(0..=255u8);
    bytes.extend(b"\r\n\n");
    bytes
}

fn bin_dir_entries(harness: &Harness) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(harness.home().join(".local/bin"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn upload_atomically_replaces_development_binary_byte_for_byte() {
    let harness = Harness::with_desktop();
    fs::create_dir_all(harness.home().join(".local/bin")).unwrap();
    fs::write(harness.remote_binary(), b"previous sans-dev").unwrap();
    let old_link = harness.home().join(".local/bin/old-link");
    fs::hard_link(harness.remote_binary(), &old_link).unwrap();
    let local = harness.write_local("ci-sans", &binary_with_all_bytes());

    let output = harness.run(&["--binary", local.to_str().unwrap(), HOST]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        fs::read(harness.remote_binary()).unwrap(),
        binary_with_all_bytes()
    );
    let mode = fs::metadata(harness.remote_binary()).unwrap().mode() & 0o7777;
    assert_eq!(mode, 0o755);
    assert_eq!(fs::read(&old_link).unwrap(), b"previous sans-dev");
    assert_eq!(bin_dir_entries(&harness), ["old-link", "sans-dev"]);
    let upload = &harness.ssh_calls()[0];
    assert_eq!(upload[..3], ["-T", "--", HOST]);
}

#[test]
fn truncated_upload_keeps_previous_binary_and_skips_run() {
    let mut harness = Harness::new();
    harness.truncate_upload = Some(10);
    fs::create_dir_all(harness.home().join(".local/bin")).unwrap();
    fs::write(harness.remote_binary(), b"previous sans-dev").unwrap();
    let local = harness.write_local("ci-sans", &binary_with_all_bytes());

    let output = harness.run(&["--binary", local.to_str().unwrap(), HOST]);

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("truncated"));
    assert_eq!(
        fs::read(harness.remote_binary()).unwrap(),
        b"previous sans-dev"
    );
    assert_eq!(bin_dir_entries(&harness), ["sans-dev"]);
    assert_eq!(harness.ssh_calls().len(), 1);
    assert!(harness.app_argv().is_none());
}

#[test]
fn runs_development_binary_with_forced_tty_and_remote_machine_profile() {
    let harness = Harness::with_desktop();
    let profile = harness.home().join(".config/sans/sans.toml");
    fs::create_dir_all(profile.parent().unwrap()).unwrap();
    fs::write(&profile, "# commissioned Machine profile\n").unwrap();
    let local_config = harness.root.path().join("dev-config");

    let output = harness.run_with(
        &harness.repo(),
        &[("XDG_CONFIG_HOME", local_config.to_str().unwrap())],
        &[HOST],
    );

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(harness.app_argv().unwrap(), Vec::<String>::new());
    assert_eq!(harness.app_env()["XDG_CONFIG_HOME"], None);
    assert_eq!(
        fs::read_to_string(&profile).unwrap(),
        "# commissioned Machine profile\n"
    );
    assert!(!local_config.exists());
    let calls = harness.ssh_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1][..3], ["-tt", "--", HOST]);
}

#[test]
fn application_exit_status_propagates() {
    let mut harness = Harness::with_desktop();
    harness.built_binary = fake_app(7);

    let output = harness.run(&[HOST]);

    assert_eq!(output.status.code(), Some(7));
    assert!(harness.app_argv().is_some());
}

#[test]
fn arguments_and_environment_values_reach_application_literally() {
    let harness = Harness::with_desktop();
    let args = [
        "",
        "two words",
        "$(touch pwned)",
        "`touch pwned`",
        "it's",
        "\"",
        "\\",
        "*",
        "first\nsecond",
        "trailing newline\n",
        "--config",
        "; exit 9",
        "%s%n",
        "~",
    ];
    let value = "it's $HOME \"quoted\" `touch pwned` *\ntrailing\n";
    let env = format!("SANS_TEST_VALUE={value}");
    let mut cli = vec!["--env", env.as_str(), HOST, "--"];
    cli.extend(args);

    let output = harness.run(&cli);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(harness.app_argv().unwrap(), args);
    assert_eq!(harness.app_env()["SANS_TEST_VALUE"].as_deref(), Some(value));
    assert!(!harness.home().join("pwned").exists());
    assert!(!harness.repo().join("pwned").exists());
}

#[test]
fn selects_wayland_socket_from_remote_runtime_dir_ignoring_lock_files() {
    let harness = Harness::new().with_wayland_socket("wayland-1");
    fs::write(harness.runtime().join("wayland-0"), "").unwrap();
    fs::write(harness.runtime().join("wayland-0.lock"), "").unwrap();
    fs::write(harness.runtime().join("wayland-1.lock"), "").unwrap();

    let output = harness.run(&[HOST]);

    assert!(output.status.success(), "{}", stderr(&output));
    let env = harness.app_env();
    assert_eq!(env["WAYLAND_DISPLAY"].as_deref(), Some("wayland-1"));
    assert_eq!(env["DISPLAY"], None);
    assert_eq!(
        env["XDG_RUNTIME_DIR"].as_deref(),
        harness.runtime().to_str()
    );
}

#[test]
fn missing_display_fails_with_explicit_env_hints() {
    let harness = Harness::new();
    fs::write(harness.runtime().join("wayland-0.lock"), "").unwrap();

    let output = harness.run(&[HOST]);

    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr(&output);
    assert!(stderr.contains("--env DISPLAY=:0"), "{stderr}");
    assert!(stderr.contains("--env WAYLAND_DISPLAY="), "{stderr}");
    assert!(harness.app_argv().is_none());
}

/// One display-precedence case. Each runs with a detectable `wayland-1` socket.
struct DisplayCase {
    label: &'static str,
    session: &'static [(&'static str, &'static str)],
    options: &'static [&'static str],
    display: Option<&'static str>,
    wayland: Option<&'static str>,
}

#[test]
fn explicit_and_session_display_values_take_precedence_over_detection() {
    let cases = [
        DisplayCase {
            label: "explicit X11",
            session: &[],
            options: &["--env", "DISPLAY=:1"],
            display: Some(":1"),
            wayland: None,
        },
        DisplayCase {
            label: "explicit Wayland",
            session: &[],
            options: &["--env", "WAYLAND_DISPLAY=wayland-9"],
            display: None,
            wayland: Some("wayland-9"),
        },
        DisplayCase {
            label: "session X11",
            session: &[("DISPLAY", ":0")],
            options: &[],
            display: Some(":0"),
            wayland: None,
        },
        DisplayCase {
            label: "session Wayland",
            session: &[("WAYLAND_DISPLAY", "wayland-5")],
            options: &[],
            display: None,
            wayland: Some("wayland-5"),
        },
        DisplayCase {
            label: "explicit overrides session",
            session: &[("WAYLAND_DISPLAY", "wayland-5")],
            options: &["--env", "WAYLAND_DISPLAY=wayland-9"],
            display: None,
            wayland: Some("wayland-9"),
        },
    ];
    for case in cases {
        let mut harness = Harness::new().with_wayland_socket("wayland-1");
        for (name, value) in case.session {
            harness = harness.set_remote_env(name, value);
        }
        let mut cli = case.options.to_vec();
        cli.push(HOST);

        let output = harness.run(&cli);

        let label = case.label;
        assert!(output.status.success(), "{label}: {}", stderr(&output));
        let env = harness.app_env();
        assert_eq!(env["DISPLAY"].as_deref(), case.display, "{label}");
        assert_eq!(env["WAYLAND_DISPLAY"].as_deref(), case.wayland, "{label}");
    }
}

#[test]
fn explicit_runtime_dir_is_applied_before_wayland_detection() {
    let harness = Harness::new()
        .unset_remote_env("XDG_RUNTIME_DIR")
        .with_wayland_socket("wayland-0");
    let runtime = format!("XDG_RUNTIME_DIR={}", harness.runtime().display());

    let output = harness.run(&["--env", &runtime, HOST]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        harness.app_env()["WAYLAND_DISPLAY"].as_deref(),
        Some("wayland-0")
    );
}
