//! Full-page previews use the body height the window actually gives them.

mod support;

use support::cameras::Step;
use support::fixtures::{
    clamped_crop, landscape_full_frame, landscape_smaller_crop, portrait_full_frame,
    portrait_smaller_crop, rotated_crop,
};
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

#[test]
fn captions_say_full_frame_or_crop_with_dimensions() {
    let cases = [
        (portrait_full_frame(), "Full frame, 360x640 px"),
        (landscape_full_frame(), "Full frame, 640x360 px"),
        (
            portrait_smaller_crop(),
            "Crop 301x521 px of 400x700 px frame",
        ),
        (
            landscape_smaller_crop(),
            "Crop 523x297 px of 700x400 px frame",
        ),
        (rotated_crop(), "Crop 311x547 px of 360x640 px frame"),
        (clamped_crop(), "Crop 319x557 px of 360x640 px frame"),
    ];
    for (pair, caption) in cases {
        let session = Session::with_pair(pair, T1);

        let shown = session
            .labelled_rects()
            .into_iter()
            .filter(|(label, rect)| label == caption && session.contains(session.body(), *rect))
            .count();

        assert_eq!(shown, 2, "{caption:?} under both Full-page labels");
    }
}

#[test]
fn each_capture_replaces_the_shown_pair_with_the_new_one() {
    let (first_left, first_right) = portrait_full_frame();
    let (second_left, second_right) = landscape_full_frame();
    let (third_left, third_right) = portrait_smaller_crop();
    let mut session = Session::cameras(
        vec![
            Step::Frame(first_left.frame),
            Step::Frame(second_left.frame),
            Step::Frame(third_left.frame),
        ],
        vec![
            Step::Frame(first_right.frame),
            Step::Frame(second_right.frame),
            Step::Frame(third_right.frame),
        ],
        T1,
    );

    for caption in [
        "Full frame, 360x640 px",
        "Full frame, 640x360 px",
        "Crop 301x521 px of 400x700 px frame",
    ] {
        session.capture_pair();
        let shown = session
            .labelled_rects()
            .into_iter()
            .filter(|(label, _)| label == caption)
            .count();
        assert_eq!(shown, 2, "after a capture, both captions read {caption:?}");
    }
}
