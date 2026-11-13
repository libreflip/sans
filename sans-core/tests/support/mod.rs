//! Shared integration-test support.
#![allow(dead_code)]

pub mod fake_fibre;

use std::time::{Duration, Instant};

use sans_core::{ControllerHandle, ControllerSnapshot};

/// Receive snapshots until one satisfies `predicate`, within one second.
///
/// Link status changes publish snapshots of their own, so tests wait for the
/// projection they care about instead of counting snapshots.
pub fn recv_until(
    controller: &ControllerHandle,
    predicate: impl FnMut(&ControllerSnapshot) -> bool,
) -> ControllerSnapshot {
    recv_within(controller, Duration::from_secs(1), predicate)
}

/// Bound for waits that include a whole connection gate: about 1273 descriptor
/// round trips through real threads, which is slow under the emulated cross
/// test job. Natively it completes in well under a second.
pub const GATE_WAIT: Duration = Duration::from_secs(10);

/// Receive snapshots until one satisfies `predicate`, within `timeout`.
pub fn recv_within(
    controller: &ControllerHandle,
    timeout: Duration,
    mut predicate: impl FnMut(&ControllerSnapshot) -> bool,
) -> ControllerSnapshot {
    let deadline = Instant::now() + timeout;
    let mut last = None;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!("no matching snapshot before the deadline; last was {last:#?}");
        }
        match controller.recv_snapshot_timeout(remaining) {
            Ok(snapshot) if predicate(&snapshot) => return snapshot,
            Ok(snapshot) => last = Some(snapshot),
            Err(error) => panic!("no matching snapshot ({error}); last was {last:#?}"),
        }
    }
}

/// A commissioned `[odrive.lift_axis]` table for the fake Fibre board.
pub const LIFT_AXIS: &str = r#"
[odrive.lift_axis]
direction = "normal"
mm_per_turn = 8.0
travel_minimum_mm = 0.0
travel_maximum_mm = 300.0
current_limit_a = 4.0
velocity_limit_mm_s = 6.0
acceleration_limit_mm_s2 = 4.0
deceleration_limit_mm_s2 = 2.0
position_tolerance_mm = 0.5
stationary_velocity_mm_s = 0.2
move_timeout_ms = 2000
home_timeout_ms = 3000
cancel_timeout_ms = 1000
touchdown_grace_ms = 100
touchdown_dwell_ms = 100
touchdown_no_progress_mm = 1.0
touchdown_saturation_percent = 90.0
touchdown_hold_depth_mm = 2.0
"#;

/// Widest timing the profile bounds allow. These tests check behaviour through
/// real threads, including under the emulated aarch64 cross job, not timing.
pub fn with_relaxed_timing(profile: &str) -> String {
    let relaxed = [
        ("poll_interval_ms = 20", "poll_interval_ms = 100"),
        ("telemetry_max_age_ms = 100", "telemetry_max_age_ms = 250"),
        ("request_deadline_ms = 100", "request_deadline_ms = 250"),
        ("urgent_deadline_ms = 50", "urgent_deadline_ms = 100"),
        ("device_open_ms = 5000", "device_open_ms = 60000"),
    ];
    relaxed
        .iter()
        .fold(profile.to_string(), |profile, (from, to)| {
            let replaced = profile.replacen(from, to, 1);
            assert_ne!(replaced, profile, "{from}");
            replaced
        })
}
