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
