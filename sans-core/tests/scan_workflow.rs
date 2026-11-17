//! From startup through Home consent, Idle, page width and Ready to a Scan
//! session's first Capture pair, through the controller's intent and
//! snapshot seam.
//!
//! Fake-backed host behaviour only: a fake Fibre board, a recording
//! Monospace, scripted cameras and a temporary storage root.

mod support;

use std::collections::VecDeque;
use std::fs;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use sans_core::{
    bootstrap, CameraCaptureError, CameraRole, CapturePair, CapturePairError, CaptureStatus,
    CapturedFrame, ControllerHandle, ControllerIntent, ControllerMachine, ControllerSnapshot,
    CropGeometry, FibreOpener, Led, LinkPhase, MachineBoard, MachineEvent, MachineFactory,
    MachineScreen, OpenedBoard, PreparedMachineProfile, Relay, ScanReadiness, SetupBlocker,
    SetupDiagnostic, SetupState, StopCause, StopVerdict,
};
use support::fake_fibre::*;
use support::{recv_within, with_relaxed_timing, GATE_WAIT, LIFT_AXIS};

const VALID_PROFILE: &str = include_str!("fixtures/valid-sans.toml");

/// Board commands and captures in the order they happened.
type Log = Arc<Mutex<Vec<String>>>;

/// The pressure every reading starts from, in millibar.
const AMBIENT_MBAR: f32 = 1_000.0;

struct Board {
    log: Log,
    events: mpsc::Receiver<MachineEvent>,
    /// The Pressure drop streamed while the vacuum is on.
    vacuum_drop: Arc<Mutex<f32>>,
    vacuum: bool,
    streaming: bool,
    last_reading: Option<Instant>,
}

impl Board {
    fn push(&self, command: &str) {
        self.log.lock().unwrap().push(command.into());
    }

    fn pressure(&self) -> f32 {
        let drop = if self.vacuum {
            *self.vacuum_drop.lock().unwrap()
        } else {
            0.0
        };
        AMBIENT_MBAR - drop
    }
}

impl MachineBoard for Board {
    fn all_off(&mut self, _within: Duration) -> Result<(), String> {
        self.vacuum = false;
        self.push("ALL OFF");
        Ok(())
    }

    fn poll(&mut self) -> Option<MachineEvent> {
        if let Ok(event) = self.events.try_recv() {
            return Some(event);
        }
        // Stream a reading every 10 ms; always answering would starve the inbox.
        let due = self
            .last_reading
            .is_none_or(|last| last.elapsed() >= Duration::from_millis(10));
        if !(self.streaming && due) {
            return None;
        }
        self.last_reading = Some(Instant::now());
        Some(MachineEvent::Pressure(self.pressure()))
    }

    fn set_relay(&mut self, relay: Relay, on: bool) -> Result<(), String> {
        let name = match relay {
            Relay::Vacuum => {
                self.vacuum = on;
                "VACUUM"
            }
            Relay::FlutterFan => "FAN",
            Relay::TurnBlower => "BLOWER",
        };
        self.push(&format!("{name} {}", if on { "ON" } else { "OFF" }));
        Ok(())
    }

    fn read_pressure(&mut self) -> Result<f32, String> {
        self.push("PRESS?");
        Ok(self.pressure())
    }

    fn set_pressure_stream(&mut self, on: bool) -> Result<(), String> {
        self.streaming = on;
        self.push(if on { "PRESS START" } else { "PRESS STOP" });
        Ok(())
    }

    fn set_light(&mut self, on: bool) -> Result<(), String> {
        let command = if on { "LIGHT ON" } else { "LIGHT OFF" };
        self.log.lock().unwrap().push(command.into());
        Ok(())
    }

    fn set_led(&mut self, led: Led) -> Result<(), String> {
        self.log.lock().unwrap().push(format!("LED {led:?}"));
        Ok(())
    }
}

struct Machine {
    log: Log,
    results: VecDeque<Result<CapturePair, CapturePairError>>,
    /// Holding this lock holds every capture.
    gate: Arc<Mutex<()>>,
    /// A blocker sent here reports the Cameras lost.
    lost: mpsc::Receiver<SetupBlocker>,
}

impl Machine {
    fn next(&mut self, kind: &str) -> Result<CapturePair, CapturePairError> {
        let _open = self.gate.lock().unwrap();
        self.log.lock().unwrap().push(kind.into());
        self.results.pop_front().expect("no scripted capture left")
    }
}

impl ControllerMachine for Machine {
    fn capture_pair(&mut self) -> Result<CapturePair, CapturePairError> {
        self.next("diagnostic capture")
    }

    fn capture_session_pair(&mut self) -> Result<CapturePair, CapturePairError> {
        self.next("session capture")
    }
}

struct Factory {
    opener: FakeOpener,
    board: Option<Board>,
    machine: Machine,
}

impl MachineFactory for Factory {
    type Machine = Machine;

    fn odrive_opener(&mut self) -> Box<dyn FibreOpener> {
        Box::new(self.opener.clone())
    }

    fn open_board(&mut self, _profile: &PreparedMachineProfile) -> Option<OpenedBoard> {
        let board: Box<dyn MachineBoard> = Box::new(self.board.take()?);
        Some(Ok((board, SetupDiagnostic::ready("Monospace", "scripted"))))
    }

    fn open(
        self,
        _profile: &PreparedMachineProfile,
    ) -> Result<(Self::Machine, Vec<SetupDiagnostic>), Vec<SetupBlocker>> {
        Ok((self.machine, Vec::new()))
    }

    fn poll_machine(machine: &mut Self::Machine) -> Option<MachineEvent> {
        machine.lost.try_recv().ok().map(MachineEvent::Lost)
    }
}

fn frame(role: CameraRole, value: u8) -> CapturedFrame {
    CapturedFrame::from_rgb8(
        role,
        4,
        3,
        vec![value; 4 * 3 * 3],
        SystemTime::UNIX_EPOCH,
        CropGeometry {
            x: 0,
            y: 0,
            width: 4,
            height: 3,
        },
    )
    .unwrap()
}

fn pair(value: u8) -> Result<CapturePair, CapturePairError> {
    Ok(CapturePair {
        left: frame(CameraRole::Left, value),
        right: frame(CameraRole::Right, value),
    })
}

fn failed() -> Result<CapturePair, CapturePairError> {
    Err(CameraCaptureError::acquisition(CameraRole::Right, "fixture camera").into())
}

/// A fake board whose native Home ends idle at 0 with clean evidence.
fn homing_board() -> FakeFibre {
    let mut board = FakeFibre::compatible();
    board.settle_after_one_read.insert(11, 1);
    board.set_bool(IS_HOMED, false);
    board.set_bool(TRAJECTORY_DONE, true);
    board
}

struct Started {
    controller: ControllerHandle,
    opener: FakeOpener,
    log: Log,
    button: mpsc::Sender<MachineEvent>,
    gate: Arc<Mutex<()>>,
    lose_cameras: mpsc::Sender<SetupBlocker>,
    vacuum_drop: Arc<Mutex<f32>>,
    data_root: std::path::PathBuf,
    markers: std::cell::Cell<u16>,
    _temp: tempfile::TempDir,
}

impl Started {
    fn send(&self, intent: ControllerIntent) {
        self.controller.send(intent).unwrap();
    }

    fn until(&self, predicate: impl FnMut(&ControllerSnapshot) -> bool) -> ControllerSnapshot {
        recv_within(&self.controller, GATE_WAIT, predicate)
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    fn requested_states(&self) -> Vec<i32> {
        self.opener.board.lock().unwrap().requested_states()
    }

    fn press_button(&self) {
        self.button.send(MachineEvent::Button).unwrap();
    }

    /// Press the button and wait until the controller has handled it: board
    /// events arrive in order, so a pressure reading sent after it marks it done.
    fn press_button_and_settle(&self) {
        self.press_button();
        self.markers.set(self.markers.get() + 1);
        let marker = 900.0 + f32::from(self.markers.get());
        self.button.send(MachineEvent::Pressure(marker)).unwrap();
        self.until(|snapshot| snapshot.pressure_mbar == Some(marker));
    }

    fn sessions(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.data_root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }
}

/// Start with a commissioned profile and wait until Home may be given.
fn start(board: FakeFibre, captures: Vec<Result<CapturePair, CapturePairError>>) -> Started {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sans.toml");
    fs::write(
        &path,
        with_relaxed_timing(&format!("{VALID_PROFILE}{LIFT_AXIS}")),
    )
    .unwrap();
    let log = Log::default();
    let (button, events) = mpsc::channel();
    let gate = Arc::new(Mutex::new(()));
    let (lose_cameras, lost) = mpsc::channel();
    let opener = FakeOpener::new(board);
    let vacuum_drop = Arc::new(Mutex::new(0.0));
    let controller = bootstrap(
        Some(&path),
        Factory {
            opener: opener.clone(),
            board: Some(Board {
                log: Arc::clone(&log),
                events,
                vacuum_drop: Arc::clone(&vacuum_drop),
                vacuum: false,
                streaming: false,
                last_reading: None,
            }),
            machine: Machine {
                log: Arc::clone(&log),
                results: captures.into(),
                gate: Arc::clone(&gate),
                lost,
            },
        },
    )
    .unwrap();
    let started = Started {
        controller,
        opener,
        log,
        button,
        gate,
        lose_cameras,
        vacuum_drop,
        // The fixture's relative data root resolves beside the profile.
        data_root: temp.path().join("scan-data"),
        markers: Default::default(),
        _temp: temp,
    };
    started.until(|snapshot| snapshot.scan_readiness == ScanReadiness::Enabled);
    started
}

fn home(started: &Started) {
    started.send(ControllerIntent::Home);
    started.until(|snapshot| snapshot.screen == MachineScreen::Idle);
}

fn ready(started: &Started, millimetres: u16) {
    started.send(ControllerIntent::NewBook);
    started.until(|snapshot| matches!(snapshot.screen, MachineScreen::PageWidth(_)));
    started.send(ControllerIntent::SetPageWidth { millimetres });
    started.until(|snapshot| matches!(snapshot.screen, MachineScreen::Ready { .. }));
}

fn scanning(snapshot: &ControllerSnapshot) -> Option<&sans_core::ScanView> {
    match &snapshot.screen {
        MachineScreen::Scanning(view) => Some(view),
        _ => None,
    }
}

#[test]
fn boot_waits_in_setup_for_home_consent_and_never_homes_by_itself() {
    let started = start(homing_board(), vec![]);

    let snapshot = started.until(|snapshot| snapshot.lift_axis.phase == LinkPhase::Verified);

    assert!(matches!(
        snapshot.screen,
        MachineScreen::Setup(SetupState::Ready { .. })
    ));
    assert_eq!(started.requested_states(), vec![1], "only the gate's idle");
    assert_eq!(started.log(), vec!["LED Blue"]);
}

#[test]
fn consented_home_enters_idle_and_new_book_does_not_home_again() {
    let started = start(homing_board(), vec![]);

    started.send(ControllerIntent::Home);
    started.until(|snapshot| snapshot.screen == MachineScreen::Homing);
    home(&started);
    ready(&started, 200);
    started.send(ControllerIntent::NewBook);
    started.until(|snapshot| matches!(snapshot.screen, MachineScreen::PageWidth(_)));

    assert_eq!(started.requested_states(), vec![1, 11], "one Home, no Arm");
}

#[test]
fn clean_home_failure_returns_to_setup_blocked_without_stop_and_home_can_be_retried() {
    for endpoint in [IS_HOMED, TRAJECTORY_DONE] {
        let mut board = homing_board();
        board.readback_override.insert(endpoint, vec![0]);
        let started = start(board, vec![]);

        started.send(ControllerIntent::Home);
        let failed = started.until(|snapshot| {
            matches!(&snapshot.screen, MachineScreen::Setup(SetupState::Blocked { reasons })
            if reasons.iter().any(|reason| reason.summary.starts_with("Home failed")))
        });

        assert_eq!(failed.stop, None);
        assert_eq!(failed.scan_readiness, ScanReadiness::Enabled);
        started
            .opener
            .board
            .lock()
            .unwrap()
            .readback_override
            .remove(&endpoint);
        home(&started);
        assert_eq!(started.requested_states(), vec![1, 11, 11]);
    }
}

#[test]
fn ambiguous_home_evidence_enters_stop() {
    let mut board = homing_board();
    // The board never shows homing, so the Home's outcome is uncertain.
    board.ignore_state_requests.push(11);
    let started = start(board, vec![]);

    started.send(ControllerIntent::Home);
    let stopped = started.until(|snapshot| snapshot.stop.is_some());

    assert!(matches!(
        stopped.stop.unwrap().cause,
        StopCause::Uncertain(_)
    ));
    let setup = started.until(|snapshot| matches!(snapshot.screen, MachineScreen::Setup(_)));
    assert!(setup.stop.is_some());
}

#[test]
fn page_width_outside_the_profile_bounds_is_rejected_and_ready_is_separate() {
    let started = start(homing_board(), vec![]);
    home(&started);
    started.send(ControllerIntent::NewBook);
    let entry = started.until(|snapshot| matches!(snapshot.screen, MachineScreen::PageWidth(_)));
    let MachineScreen::PageWidth(entry) = entry.screen else {
        unreachable!()
    };
    assert_eq!((entry.minimum_mm, entry.maximum_mm), (100, 350));

    for rejected in [99, 351] {
        started.send(ControllerIntent::SetPageWidth {
            millimetres: rejected,
        });
        started.until(|snapshot| {
            matches!(&snapshot.screen, MachineScreen::PageWidth(entry)
                if entry.rejected_mm == Some(rejected))
        });
    }
    started.send(ControllerIntent::SetPageWidth { millimetres: 350 });
    let ready = started.until(|snapshot| matches!(snapshot.screen, MachineScreen::Ready { .. }));

    assert_eq!(
        ready.screen,
        MachineScreen::Ready {
            page_width_mm: 350,
            notice: None
        }
    );
    assert!(
        started.sessions().is_empty(),
        "Ready creates no Scan session"
    );
    assert!(!started.log().contains(&"LIGHT ON".to_string()));
}

#[test]
fn start_creates_a_utc_scan_session_lights_and_keeps_the_first_pair_as_canonical() {
    let started = start(homing_board(), vec![pair(7)]);
    home(&started);
    ready(&started, 210);

    // A second Start finds the workflow already Scanning.
    started.send(ControllerIntent::Start);
    started.send(ControllerIntent::Start);
    let snapshot = started.until(|snapshot| {
        scanning(snapshot).is_some_and(|view| view.capture.status == CaptureStatus::Ready)
    });

    let view = scanning(&snapshot).unwrap();
    assert_eq!(
        (view.left_sequence, view.right_sequence, view.attempt),
        (0, 1, 1)
    );
    assert_eq!(view.page_width_mm, 210);
    let canonical = view.capture.latest_complete_pair.as_ref().unwrap();
    assert_eq!(**canonical, pair(7).unwrap());
    let sessions = started.sessions();
    assert_eq!(sessions.len(), 1);
    let name = &sessions[0];
    assert!(
        name.len() == 16 && name.ends_with('Z') && name.as_bytes()[8] == b'T',
        "{name}"
    );
    assert_eq!(view.directory, started.data_root.join(name));
    assert!(fs::read_dir(&view.directory).unwrap().next().is_none());
    let log = started.log();
    let light = log.iter().position(|entry| entry == "LIGHT ON").unwrap();
    let capture = log.iter().position(|entry| entry == "session capture");
    assert!(capture.is_some_and(|capture| capture > light), "{log:?}");
    assert!(!log.contains(&"diagnostic capture".to_string()));
}

#[test]
fn a_failed_first_capture_blocks_without_a_canonical_pair() {
    let started = start(homing_board(), vec![failed()]);
    home(&started);
    ready(&started, 200);

    started.send(ControllerIntent::Start);
    let snapshot = started.until(|snapshot| {
        scanning(snapshot)
            .is_some_and(|view| matches!(view.capture.status, CaptureStatus::Blocked { .. }))
    });

    let view = scanning(&snapshot).unwrap();
    assert_eq!(view.capture.latest_complete_pair, None);
    assert_eq!((view.left_sequence, view.attempt), (0, 1));
    // A blocked first capture does not leave the light on.
    assert!(
        started.log().ends_with(&["LIGHT OFF".to_string()]),
        "{:?}",
        started.log()
    );
}

#[test]
fn the_green_button_is_new_book_in_idle_start_in_ready_and_stop_while_active() {
    let started = start(homing_board(), vec![pair(1)]);
    let gate = started.gate.lock().unwrap();
    // Inert in Setup: nothing homes and nothing stops.
    started.press_button_and_settle();
    home(&started);
    assert_eq!(started.requested_states(), vec![1, 11]);

    started.press_button();
    started.until(|snapshot| matches!(snapshot.screen, MachineScreen::PageWidth(_)));
    // Inert during page-width entry.
    started.press_button_and_settle();
    started.send(ControllerIntent::SetPageWidth { millimetres: 200 });
    started.until(|snapshot| matches!(snapshot.screen, MachineScreen::Ready { .. }));

    started.press_button();
    started.until(|snapshot| {
        scanning(snapshot).is_some_and(|view| view.capture.status == CaptureStatus::Capturing)
    });
    started.press_button();
    let stopped = started.until(|snapshot| snapshot.stop.is_some());
    drop(gate);

    assert_eq!(
        stopped.stop.unwrap().cause,
        StopCause::Requested("physical button".into())
    );
    assert_eq!(started.sessions().len(), 1);
}

#[test]
fn losing_the_cameras_mid_home_keeps_homing_until_the_home_finishes() {
    let mut board = homing_board();
    // The board stays homing until the test ends the run.
    board.settle_after_one_read.clear();
    let started = start(board, vec![]);
    started.send(ControllerIntent::Home);
    started.until(|snapshot| snapshot.screen == MachineScreen::Homing);

    started
        .lose_cameras
        .send(SetupBlocker::new("Left Camera unplugged"))
        .unwrap();
    started.until(|snapshot| {
        snapshot.scan_readiness != ScanReadiness::Enabled
            && snapshot.screen == MachineScreen::Homing
    });
    started
        .opener
        .board
        .lock()
        .unwrap()
        .set_i32(CURRENT_STATE, 1);

    // Home ends with trust, then the lost Cameras send the workflow to Setup.
    let setup = started.until(|snapshot| {
        matches!(&snapshot.screen, MachineScreen::Setup(SetupState::Blocked { reasons })
            if reasons[0].summary == "Left Camera unplugged")
    });
    assert_eq!(setup.stop, None);
}

#[test]
fn the_led_follows_setup_idle_ready_scanning_and_stop() {
    let started = start(homing_board(), vec![pair(1)]);
    home(&started);
    ready(&started, 200);
    started.send(ControllerIntent::Start);
    started.until(|snapshot| {
        scanning(snapshot).is_some_and(|view| view.capture.status == CaptureStatus::Ready)
    });
    started.send(ControllerIntent::Stop);
    started.until(|snapshot| snapshot.stop.is_some());

    let leds: Vec<String> = started
        .log()
        .into_iter()
        .filter(|entry| entry.starts_with("LED"))
        .collect();
    assert_eq!(
        leds,
        [
            "LED Blue",
            "LED Amber",
            "LED Blue",
            "LED Green",
            "LED Amber",
            "LED Red"
        ]
    );
}

#[test]
fn stop_during_the_first_capture_switches_the_light_off_and_does_not_block_home() {
    let started = start(homing_board(), vec![pair(1), pair(2)]);
    home(&started);
    ready(&started, 200);
    // The Cameras never answer while the test holds the gate.
    let gate = started.gate.lock().unwrap();
    started.send(ControllerIntent::Start);
    started.until(|snapshot| {
        scanning(snapshot).is_some_and(|view| view.capture.status == CaptureStatus::Capturing)
    });

    started.send(ControllerIntent::Stop);
    started.until(|snapshot| {
        snapshot.stop.as_ref().map(|stop| &stop.verdict) == Some(&StopVerdict::Confirmed)
    });
    let log = started.log();
    let on = log.iter().position(|entry| entry == "LIGHT ON").unwrap();
    assert!(log[on..].contains(&"LIGHT OFF".to_string()), "{log:?}");
    started.send(ControllerIntent::ClearStop);
    started.until(|snapshot| snapshot.stop.is_none());

    home(&started);
    drop(gate);
    // The late pair belongs to the ended session; the workflow carries on.
    ready(&started, 200);
    started.send(ControllerIntent::Start);
    started.until(|snapshot| scanning(snapshot).is_some());
}

#[test]
fn stop_then_clear_stop_returns_to_idle_without_another_home() {
    let started = start(homing_board(), vec![pair(1)]);
    home(&started);
    ready(&started, 200);
    // Held captures keep the Capture cycle from arming before the Stop.
    let gate = started.gate.lock().unwrap();
    started.send(ControllerIntent::Start);
    started.until(|snapshot| scanning(snapshot).is_some());

    started.send(ControllerIntent::Stop);
    let confirmed = started.until(|snapshot| {
        snapshot.stop.as_ref().map(|stop| &stop.verdict) == Some(&StopVerdict::Confirmed)
    });
    drop(gate);
    assert!(matches!(confirmed.screen, MachineScreen::Setup(_)));
    started.send(ControllerIntent::ClearStop);
    let cleared = started.until(|snapshot| snapshot.stop.is_none());

    assert_eq!(cleared.screen, MachineScreen::Idle);
    assert_eq!(
        started.sessions().len(),
        1,
        "the stopped session is not resumed"
    );
    assert_eq!(
        started.requested_states(),
        vec![1, 11, 1],
        "clear neither homes nor arms"
    );
}

#[test]
fn a_lift_axis_reconnect_accepts_the_new_epochs_native_homed_report() {
    let started = start(homing_board(), vec![]);
    home(&started);
    ready(&started, 200);

    started.send(ControllerIntent::ReconnectLiftAxis);
    let snapshot = started.until(|snapshot| matches!(snapshot.screen, MachineScreen::Setup(_)));
    assert_eq!(snapshot.stop, None, "an idle axis needs no Stop");

    started.until(|snapshot| {
        snapshot.lift_axis.epoch == 2 && snapshot.scan_readiness == ScanReadiness::Enabled
    });
    started.until(|snapshot| snapshot.screen == MachineScreen::Idle);
    assert_eq!(
        started.requested_states(),
        vec![1, 11, 1],
        "reconnect only idles the axis"
    );
}

#[test]
fn pressure_readings_reach_the_snapshot() {
    let started = start(homing_board(), vec![]);

    started
        .button
        .send(MachineEvent::Pressure(1_013.5))
        .unwrap();

    started.until(|snapshot| snapshot.pressure_mbar == Some(1_013.5));
}

#[test]
fn already_homed_startup_skips_home_but_manual_home_is_still_available() {
    let mut board = homing_board();
    board.set_bool(IS_HOMED, true);
    let started = start(board, vec![]);
    started.until(|snapshot| snapshot.screen == MachineScreen::Idle);
    assert_eq!(
        started.requested_states(),
        vec![1],
        "only the connection gate idles"
    );
    assert_eq!(started.log(), vec!["LED Blue"]);
    home(&started);
    assert_eq!(
        started.requested_states(),
        vec![1, 11],
        "Home from Idle is explicit"
    );
}

/// The fake axis contacts the book at this height, in millimetres.
const CONTACT_MM: f32 = -250.0;

/// A homing board whose axis moves at once and touches the book at `CONTACT_MM`.
fn book_board() -> FakeFibre {
    let mut board = homing_board();
    // The support profile has 8 mm per motor turn.
    board.contact_turns = Some(CONTACT_MM / 8.0);
    board
}

fn attempt_log(directory: &std::path::Path) -> Vec<serde_json::Value> {
    fs::read_to_string(directory.join("scan.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The board commands and captures, without LED projections.
fn commands(started: &Started) -> Vec<String> {
    started
        .log()
        .into_iter()
        .filter(|entry| !entry.starts_with("LED"))
        .collect()
}

#[test]
fn one_capture_cycle_turns_the_page_saves_both_page_records_and_starts_the_next() {
    let started = start(book_board(), vec![pair(7), pair(8), pair(9)]);
    *started.vacuum_drop.lock().unwrap() = 2.0;
    home(&started);
    ready(&started, 200);

    started.send(ControllerIntent::Start);
    let next = started.until(|snapshot| {
        scanning(snapshot).is_some_and(|view| {
            view.left_sequence == 2 && view.capture.status == CaptureStatus::Ready
        })
    });
    let view = scanning(&next).unwrap();
    assert_eq!(
        (view.capture_cycle, view.right_sequence, view.attempt),
        (2, 3, 1)
    );
    assert_eq!(
        **view.capture.latest_complete_pair.as_ref().unwrap(),
        pair(8).unwrap(),
        "the second cycle's own canonical pair"
    );
    // The second Touchdown switches the Turn blower off at its cutoff. The
    // cycle keeps going, so only the first two cycles' commands are compared.
    let expected = [
        "LIGHT ON",
        "PRESS START",
        "session capture",
        "VACUUM OFF",
        "PRESS?",
        "VACUUM ON",
        "FAN ON",
        "FAN OFF",
        "BLOWER ON",
        "VACUUM OFF",
        "session capture",
        "VACUUM OFF",
        "PRESS?",
        "BLOWER OFF",
        "VACUUM ON",
        "FAN ON",
    ];
    started.until(|_| commands(&started).len() >= expected.len());
    assert_eq!(commands(&started)[..expected.len()], expected);
    // Arm once, then the Touchdown and every leg from the same contact height:
    // 10, 65, 60, 80, 100 and 105% of 200 mm above it.
    let board = started.opener.board.lock().unwrap();
    assert_eq!(board.requested_states(), vec![1, 11, 8]);
    let targets: Vec<f32> = board
        .writes_to(INPUT_POS)
        .into_iter()
        .map(|payload| 8.0 * f32::from_le_bytes(payload.try_into().unwrap()))
        .collect();
    drop(board);
    for anchor in [-230.0, -120.0, -130.0, -90.0, -50.0, -40.0] {
        assert!(
            targets.iter().any(|target| (target - anchor).abs() < 0.01),
            "{anchor} not in {targets:?}"
        );
    }
    let directory = &view.directory;
    for (sequence, value) in [("0000", 7), ("0001", 7)] {
        let bytes = fs::read(directory.join(format!("{sequence}.jpg"))).unwrap();
        let image = image::load_from_memory(&bytes).unwrap().to_rgb8();
        assert_eq!(image.dimensions(), (4, 3));
        assert!(i16::from(image.get_pixel(0, 0)[0]).abs_diff(value) <= 2);
    }
    assert!(!directory.join("0002.jpg").exists());
    let log = attempt_log(directory);
    assert_eq!(log.len(), 1, "{log:?}");
    let record = &log[0];
    assert_eq!(record["result"], "turned");
    assert_eq!(
        (record["left_sequence"].as_u64(), record["attempt"].as_u64()),
        (Some(0), Some(1))
    );
    assert_eq!(record["baseline_mbar"], 1000.0);
    assert_eq!(record["touchdown_mm"], f64::from(CONTACT_MM));
    assert_eq!(record["pickup_threshold_mbar"], 1.2);
    let at =
        |key: &str| chrono::DateTime::parse_from_rfc3339(record[key].as_str().unwrap()).unwrap();
    assert!(at("baseline_at") - at("vacuum_off_at") >= chrono::Duration::seconds(1));
}

#[test]
fn a_vacuum_fault_switches_all_off_logs_the_attempt_and_blocks_at_the_same_sequence_pair() {
    let started = start(book_board(), vec![pair(7)]);
    home(&started);
    ready(&started, 200);

    started.send(ControllerIntent::Start);
    let blocked = started.until(|snapshot| {
        scanning(snapshot)
            .is_some_and(|view| matches!(view.capture.status, CaptureStatus::Blocked { .. }))
    });

    let view = scanning(&blocked).unwrap();
    assert_eq!(
        view.capture.status,
        CaptureStatus::Blocked {
            reason: "pickup failed: the vacuum is not working".into()
        }
    );
    assert_eq!((view.left_sequence, view.attempt), (0, 1));
    assert!(commands(&started).contains(&"ALL OFF".to_string()));
    assert_eq!(blocked.stop, None, "a pickup failure is not a Stop");
    let directory = view.directory.clone();
    started.until(|_| !attempt_log(&directory).is_empty());
    let log = attempt_log(&directory);
    assert_eq!(log[0]["result"], "vacuum_fault");
    assert!(log[0]["lowest_mbar"].as_f64().is_some());
    assert!(!directory.join("0000.jpg").exists());
}

/// Start a Scan session and return inside its first one-second
/// Pressure-baseline wait, with the session directory.
fn mid_baseline_wait() -> (Started, std::path::PathBuf) {
    let started = start(book_board(), vec![pair(7)]);
    home(&started);
    ready(&started, 200);
    started.send(ControllerIntent::Start);
    started.until(|_| commands(&started).contains(&"VACUUM OFF".to_string()));
    let snapshot = started.until(|snapshot| scanning(snapshot).is_some());
    let directory = scanning(&snapshot).unwrap().directory.clone();
    (started, directory)
}

#[test]
fn stop_mid_attempt_logs_the_attempt_as_stopped() {
    let (started, directory) = mid_baseline_wait();

    started.send(ControllerIntent::Stop);
    started.until(|snapshot| snapshot.stop.is_some());
    started.until(|_| !attempt_log(&directory).is_empty());

    let log = attempt_log(&directory);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0]["result"], "stopped");
    assert!(log[0]["vacuum_off_at"].is_string());
    assert!(log[0]["baseline_mbar"].is_null());
    assert!(
        !commands(&started).contains(&"PRESS?".to_string()),
        "no baseline after Stop"
    );
}

#[test]
fn exit_mid_attempt_still_logs_the_attempt() {
    let (started, directory) = mid_baseline_wait();

    started.send(ControllerIntent::Exit);
    started.until(|snapshot| snapshot.screen == MachineScreen::Exited);
    let deadline = Instant::now() + Duration::from_secs(2);
    while attempt_log(&directory).is_empty() && Instant::now() < deadline {
        std::thread::yield_now();
    }

    assert_eq!(attempt_log(&directory)[0]["result"], "stopped");
}
