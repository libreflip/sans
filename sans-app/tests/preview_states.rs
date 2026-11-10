//! Every current screen stays usable across the viewport matrix.
//!
//! Usable means: the actions stay inside the window and still while the body
//! scrolls, nothing is wider than the window, and the end of the body can be
//! brought into view.

mod support;

use support::cameras::{Gate, Step};
use support::fixtures::portrait_full_frame;
use support::session::{Session, MATRIX_AND_MINIMUM};

/// Assert `actions` stay put, nothing overflows sideways, and the node whose
/// label contains `last` can be scrolled until its bottom is in the body.
fn assert_usable(session: &mut Session, actions: &[&str], last: &str) {
    let at = session.viewport.name;
    let fixed: Vec<_> = actions
        .iter()
        .map(|label| (*label, session.rect(label)))
        .collect();
    let mut reached_end = false;

    session.wheel_through(60.0, |session| {
        let window = session.window();
        for (label, expected) in &fixed {
            let rect = session.rect(label);
            assert!(
                session.contains(window, rect),
                "{label} leaves the window at {at}"
            );
            assert_eq!(rect, *expected, "{label} moved at {at}");
        }
        for rect in session.all_rects() {
            assert!(
                rect.min.x >= window.min.x - 0.5 && rect.max.x <= window.max.x + 0.5,
                "a node at {rect:?} is wider than the {at} window"
            );
        }
        let end = session.rect_containing(last);
        let body = session.body();
        reached_end |= end.max.y <= body.max.y + 0.5 && end.max.y > body.min.y;
    });

    assert!(reached_end, "{last:?} never came into view at {at}");
}

#[test]
fn machine_ready_shows_capture_pair_and_diagnostics_without_scrolling() {
    for viewport in MATRIX_AND_MINIMUM {
        let (left, right) = portrait_full_frame();
        let mut session = Session::cameras(
            vec![Step::Frame(left.frame)],
            vec![Step::Frame(right.frame)],
            viewport,
        );

        assert!(session.contains(session.window(), session.rect("Capture pair")));
        assert!(session.in_body("Left Camera: Ready at fixture"));
        assert!(session.in_body("Right Camera: Ready at fixture"));
        assert_usable(
            &mut session,
            &["Capture pair", "Exit"],
            "Right Camera: Ready at fixture",
        );
    }
}

#[test]
fn ready_state_with_a_pair_is_usable() {
    for viewport in MATRIX_AND_MINIMUM {
        let mut session = Session::with_pair(portrait_full_frame(), viewport);

        assert_usable(
            &mut session,
            &["Capture pair", "Exit"],
            "Right 100% preview",
        );
    }
}

#[test]
fn capturing_keeps_the_previous_pair_visible_and_scrollable() {
    for viewport in MATRIX_AND_MINIMUM {
        let (left, right) = portrait_full_frame();
        let gate = Gate::default();
        let mut session = Session::cameras(
            vec![
                Step::Frame(left.frame.clone()),
                Step::Gated(left.frame, gate.clone()),
            ],
            vec![Step::Frame(right.frame)],
            viewport,
        );
        session.capture_pair();
        session.tap_label("Capture pair");
        session.wait_for_label("Capturing...");

        assert!(session.has("Left Full-page preview"));
        assert_usable(
            &mut session,
            &["Capturing...", "Exit"],
            "Right 100% preview",
        );

        gate.open();
        session.wait_for_label("Cameras ready");
    }
}

#[test]
fn warning_state_marks_the_unavailable_100_percent_preview() {
    for viewport in MATRIX_AND_MINIMUM {
        let (left, right) = portrait_full_frame();
        let left = left.without_native_detail("fixture detail allocation failed");
        let mut session = Session::with_pair((left, right), viewport);

        assert!(
            session.has("100% preview warning"),
            "header warning tag at {}",
            viewport.name
        );
        assert!(session.in_body(&session_warning(&session)));
        assert_usable(
            &mut session,
            &["Capture pair", "Exit"],
            "Left 100% preview unavailable",
        );
        assert!(session.has("Right 100% preview"));
    }
}

/// The full warning text in the body.
fn session_warning(session: &Session) -> String {
    session
        .labelled_rects()
        .into_iter()
        .map(|(label, _)| label)
        .find(|label| label.contains("Left 100% preview is unavailable"))
        .expect("the full warning text is in the body")
}

fn long_reason() -> String {
    "Right Camera stopped answering before the frame arrived. ".repeat(8)
}

#[test]
fn blocked_state_without_a_pair_keeps_retry_capture_on_screen() {
    for viewport in MATRIX_AND_MINIMUM {
        let (_, right) = portrait_full_frame();
        let mut session = Session::cameras(
            vec![Step::Fail(long_reason())],
            vec![Step::Frame(right.frame)],
            viewport,
        );
        session.tap_label("Capture pair");
        session.wait_for_label("Retry Capture");

        assert_usable(
            &mut session,
            &["Retry Capture", "Exit"],
            "No complete Capture pair yet.",
        );
    }
}

#[test]
fn blocked_state_with_a_previous_pair_keeps_retry_capture_on_screen() {
    for viewport in MATRIX_AND_MINIMUM {
        let (left, right) = portrait_full_frame();
        let mut session = Session::cameras(
            vec![Step::Frame(left.frame), Step::Fail(long_reason())],
            vec![Step::Frame(right.frame)],
            viewport,
        );
        session.capture_pair();
        session.tap_label("Capture pair");
        session.wait_for_label("Retry Capture");

        assert!(session.has("Left Full-page preview"));
        assert_usable(
            &mut session,
            &["Retry Capture", "Exit"],
            "Right 100% preview",
        );
    }
}

#[test]
fn setup_blocked_scrolls_through_many_reasons_with_exit_fixed() {
    for viewport in MATRIX_AND_MINIMUM {
        let reasons = (1..=40)
            .map(|reason| format!("Fixture Setup blocker number {reason}"))
            .collect();
        let mut session = Session::setup_blocked(reasons, viewport);

        assert_usable(&mut session, &["Exit"], "Fixture Setup blocker number 40");
    }
}

#[test]
fn fatal_screen_scrolls_a_long_message_with_close_sans_fixed() {
    for viewport in MATRIX_AND_MINIMUM {
        let message = (1..=60)
            .map(|line| format!("Startup diagnostic line {line}: profile field is invalid."))
            .collect::<Vec<_>>()
            .join("\n");
        let mut session = Session::fatal(&message, viewport);
        session.harness.run();

        assert_usable(&mut session, &["Close Sans"], "Startup diagnostic line 60");
    }
}
