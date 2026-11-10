//! Reaching every Capture-pair view by wheel and drag when the body overflows.

mod support;

use std::collections::BTreeSet;

use eframe::egui;
use support::cameras::Step;
use support::fixtures::portrait_full_frame;
use support::session::{Session, SCROLLING};

/// Every label and view of the four-view composition, top to bottom.
const VIEWS: [&str; 8] = [
    "Left Full-page",
    "Right Full-page",
    "Left Full-page preview",
    "Right Full-page preview",
    "Left 100%",
    "Right 100%",
    "Left 100% preview",
    "Right 100% preview",
];

fn views_in_body(session: &Session, seen: &mut BTreeSet<&'static str>) {
    for view in VIEWS {
        if session.in_body(view) {
            seen.insert(view);
        }
    }
}

#[test]
fn wheel_scrolling_brings_every_view_into_the_body() {
    for viewport in SCROLLING {
        let mut session = Session::with_pair(portrait_full_frame(), viewport);
        let mut seen = BTreeSet::new();

        session.wheel_through(45.0, |session| views_in_body(session, &mut seen));

        assert_eq!(
            seen,
            BTreeSet::from(VIEWS),
            "views reached by wheel at {}",
            viewport.name
        );
    }
}

/// Drag from low in the body to high in it until the content stops moving.
fn drag_through(session: &mut Session, touch: bool, mut visit: impl FnMut(&Session)) {
    session.wheel(-100_000.0);
    visit(session);
    for _ in 0..100 {
        let body = session.body();
        let before = session.labelled_rects();
        session.drag(
            body.center_bottom() - egui::vec2(0.0, body.height() * 0.2),
            body.center_top() + egui::vec2(0.0, body.height() * 0.2),
            touch,
        );
        visit(session);
        if session.labelled_rects() == before {
            return;
        }
    }
    panic!(
        "dragging never reached the end at {}",
        session.viewport.name
    );
}

#[test]
fn pointer_drag_without_touch_events_brings_every_view_into_the_body() {
    for viewport in SCROLLING {
        let mut session = Session::with_pair(portrait_full_frame(), viewport);
        let mut seen = BTreeSet::new();

        drag_through(&mut session, false, |session| {
            views_in_body(session, &mut seen)
        });

        assert_eq!(
            seen,
            BTreeSet::from(VIEWS),
            "views reached by drag at {}",
            viewport.name
        );
    }
}

#[test]
fn pointer_drag_with_touch_events_brings_every_view_into_the_body() {
    for viewport in SCROLLING {
        let mut session = Session::with_pair(portrait_full_frame(), viewport);
        let mut seen = BTreeSet::new();

        drag_through(&mut session, true, |session| {
            views_in_body(session, &mut seen)
        });

        assert_eq!(
            seen,
            BTreeSet::from(VIEWS),
            "views reached by touch at {}",
            viewport.name
        );
    }
}

/// Drag 80 points upward from the visible centre of `start_on` and assert the body followed.
///
/// The body must move by 40 points, or by all the room it had left if that is less.
fn assert_drag_scrolls(session: &mut Session, start_on: egui::Rect, what: &str) {
    let from = start_on.intersect(session.body()).center();
    let probe = "Left 100%";
    let before = session.rect(probe).top();

    session.drag(from, from - egui::vec2(0.0, 80.0), false);
    let moved = before - session.rect(probe).top();
    session.wheel(100_000.0);
    let room = before - session.rect(probe).top();

    assert!(
        room > 5.0 && moved >= room.min(40.0) - 0.5,
        "a drag starting on {what} moved the body {moved} of {room} points at {}",
        session.viewport.name
    );
}

#[test]
fn drags_that_start_on_images_or_text_scroll_the_body() {
    for viewport in SCROLLING {
        for start_on in [
            "Left Full-page preview",
            "Left Full-page",
            "Right 100% preview",
        ] {
            let mut session = Session::with_pair(portrait_full_frame(), viewport);
            if start_on.contains("100%") {
                // Bring the tile into view, then leave room to scroll further.
                session.wheel(100_000.0);
                session.wheel(-60.0);
            }
            let rect = session.rect(start_on);

            assert_drag_scrolls(&mut session, rect, start_on);
        }
    }
}

#[test]
fn a_drag_that_starts_on_blocked_reason_text_scrolls_the_body() {
    let reason = "fixture timeout ".repeat(12);
    for viewport in SCROLLING {
        let (left, right) = portrait_full_frame();
        let mut session = Session::cameras(
            vec![Step::Frame(left.frame), Step::Fail(reason.clone())],
            vec![Step::Frame(right.frame)],
            viewport,
        );
        session.capture_pair();
        session.tap_label("Capture pair");
        session.wait_for_label("Retry Capture");
        let reason_text = session.rect_containing("fixture timeout");

        assert_drag_scrolls(&mut session, reason_text, "the blocked reason");
    }
}

#[test]
fn the_body_settles_after_a_drag_ends() {
    for viewport in SCROLLING {
        let mut session = Session::with_pair(portrait_full_frame(), viewport);
        let body = session.body();

        session.drag(body.center(), body.center_top(), false);

        assert!(
            session.harness.try_run().is_ok(),
            "the run did not converge at {}",
            viewport.name
        );
        let settled = session.labelled_rects();
        session.harness.run();
        assert_eq!(session.labelled_rects(), settled);
    }
}
