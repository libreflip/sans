//! Capture pair, Retry Capture, and Exit stay on screen and pressable while the body scrolls.

mod support;

use eframe::egui;
use support::cameras::Step;
use support::fixtures::portrait_full_frame;
use support::session::{Session, MATRIX, SCROLLING};

fn assert_actions_fixed(session: &Session, actions: &[(&str, egui::Rect)]) {
    for (label, expected) in actions {
        let rect = session.rect(label);
        assert!(
            session.contains(session.window(), rect),
            "{label} at {rect:?} leaves the {} window",
            session.viewport.name
        );
        assert_eq!(
            rect, *expected,
            "{label} moved at {}",
            session.viewport.name
        );
    }
}

#[test]
fn actions_stay_inside_the_window_and_still_at_every_scroll_step() {
    for viewport in MATRIX {
        let mut session = Session::with_pair(portrait_full_frame(), viewport);
        let actions = ["Capture pair", "Exit"].map(|label| (label, session.rect(label)));

        session.wheel_through(45.0, |session| assert_actions_fixed(session, &actions));
    }
}

#[test]
fn tapping_capture_pair_captures_once_at_any_scroll_position() {
    for viewport in SCROLLING {
        let mut session = Session::with_pair(portrait_full_frame(), viewport);
        for scroll in [0.0, 150.0, 100_000.0] {
            session.wheel(-100_000.0);
            session.wheel(scroll);
            let (left, right) = (session.calls.left(), session.calls.right());

            session.capture_pair();

            assert_eq!(
                (session.calls.left(), session.calls.right()),
                (left + 1, right + 1),
                "captures after tapping at scroll {scroll} at {}",
                viewport.name
            );
        }
    }
}

#[test]
fn tapping_retry_capture_captures_once_from_the_blocked_state() {
    for viewport in SCROLLING {
        let (left, right) = portrait_full_frame();
        let mut session = Session::cameras(
            vec![
                Step::Frame(left.frame.clone()),
                Step::Fail("fixture timeout".into()),
                Step::Fail("fixture timeout".into()),
                Step::Frame(left.frame),
            ],
            vec![Step::Frame(right.frame)],
            viewport,
        );
        session.capture_pair();
        session.tap_label("Capture pair");
        session.wait_for_label("Retry Capture");
        session.wheel(100_000.0);
        let (left, right) = (session.calls.left(), session.calls.right());

        session.tap_and_wait_for_capture("Retry Capture");

        assert_eq!(
            (session.calls.left(), session.calls.right()),
            (left + 1, right + 1),
            "captures after Retry Capture at {}",
            viewport.name
        );
    }
}

#[test]
fn tapping_exit_at_the_end_of_the_body_closes_the_window() {
    for viewport in SCROLLING {
        let mut session = Session::with_pair(portrait_full_frame(), viewport);
        session.wheel(100_000.0);

        session.tap_label("Exit");

        session.wait_until("a close command", Session::close_requested);
    }
}
