//! Reaching every Capture-pair view by wheel and drag when the body overflows.

mod support;

use std::collections::BTreeSet;

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
