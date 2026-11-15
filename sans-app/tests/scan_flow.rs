//! The operator's journey from Home consent to a Scan session's first
//! Capture pair, tapped through the real application.
//!
//! Fake-backed: scripted cameras, a Monospace that answers every command and
//! a fake ODrive whose native Home ends cleanly.

mod support;

use support::fixtures::portrait_full_frame;
use support::session::{Session, T1};

#[test]
fn home_new_book_page_width_and_start_reach_the_first_pair() {
    let mut session = Session::scanner(portrait_full_frame(), T1);

    session.tap_label("Home lift axis");
    session.wait_for_label("Idle");
    session.tap_label("New Book");
    session.wait_for_label("Enter page width");
    for key in ["2", "1", "0"] {
        session.tap_label(key);
    }
    session.tap_label("Set 210 mm");
    session.wait_for_label("Page width: 210 mm");
    assert!(session.has("Start"));

    session.tap_label("Start");
    session.wait_for_label("First pair captured");

    for label in [
        "Stop",
        "Capture cycle 1  attempt 1  Sequence 0000/0001",
        "Pressure: no reading",
        "Left Full-page preview",
        "Right Full-page preview",
    ] {
        assert!(session.has(label), "{label}");
    }
    assert_eq!(session.calls.left(), 1);
}

#[test]
fn a_page_width_outside_the_profile_bounds_is_rejected() {
    let mut session = Session::scanner(portrait_full_frame(), T1);
    session.tap_label("Home lift axis");
    session.wait_for_label("Idle");
    session.tap_label("New Book");
    session.wait_for_label("Enter page width");

    for key in ["9", "9"] {
        session.tap_label(key);
    }
    session.tap_label("Set 99 mm");

    session.wait_for_label("99 mm is outside 100–350 mm.");
}
