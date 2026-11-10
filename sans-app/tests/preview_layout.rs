//! Full-page previews use the body height the window actually gives them.

mod support;

use support::fixtures::portrait_full_frame;
use support::session::{Session, T0, T1};

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

#[test]
fn the_four_views_fit_without_scrolling_on_the_target_display_with_chrome() {
    let mut session = Session::with_pair(portrait_full_frame(), T1);
    for view in VIEWS {
        assert!(session.in_body(view), "{view} is outside the body at T1");
    }
    let before = session.labelled_rects();

    session.wheel(200.0);

    assert_eq!(session.labelled_rects(), before, "the T1 body scrolled");
}

#[test]
fn full_page_previews_grow_with_the_available_height() {
    let height_at = |viewport| {
        Session::with_pair(portrait_full_frame(), viewport)
            .rect("Left Full-page preview")
            .height()
    };

    let (t0, t1) = (height_at(T0), height_at(T1));

    assert!(t0 > t1, "Full-page preview is {t0} at T0 and {t1} at T1");
}
