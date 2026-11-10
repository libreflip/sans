//! Rendered-pixel proofs that each 100% preview stays one-to-one while the body scrolls.
//!
//! The test renderer filters every texture bilinearly and ignores the
//! nearest-neighbour setting. Only a draw at exactly one-to-one scale on the
//! physical pixel grid reproduces the source exactly under that filter.

mod support;

use eframe::egui;
use sans_core::CameraRole;
use support::fixtures::{crop_smaller_than_tile, portrait_full_frame, Fixture};
use support::pixels::renderer;
use support::proofs::{assert_detail_one_to_one, assert_full_page_markers, MIN_VISIBLE_ROWS};
use support::session::{Session, Viewport, T2, T3, T4};

/// Windows whose body overflows, at scale factors 1.0, 1.25, 1.5, and 2.0.
const SCALED: [Viewport; 4] = [
    T2,
    Viewport {
        name: "T1 at 1.25",
        size: egui::vec2(480.0, 761.6),
        pixels_per_point: 1.25,
    },
    T3,
    T4,
];

/// Whether enough of both 100% tiles is inside the body viewport to prove them.
fn tiles_visible(session: &Session) -> bool {
    let body = session.body();
    let rows = MIN_VISIBLE_ROWS as f32 / session.viewport.pixels_per_point;
    ["Left 100% preview", "Right 100% preview"]
        .iter()
        .all(|tile| body.intersect(session.rect(tile)).height() >= rows + 1.0)
}

fn prove_both_details(session: &mut Session, left: &Fixture, right: &Fixture) {
    let image = session.render();
    assert_detail_one_to_one(session, &image, left, CameraRole::Left);
    assert_detail_one_to_one(session, &image, right, CameraRole::Right);
}

#[test]
fn detail_previews_stay_one_to_one_after_fractional_scrolls_and_drags() {
    for viewport in SCALED {
        let Some(renderer) = renderer("100% preview after scrolling") else {
            return;
        };
        let (left, right) = portrait_full_frame();
        let mut session =
            Session::with_pair_rendered((left.clone(), right.clone()), viewport, Some(renderer));
        session.wheel(100_000.0);
        assert!(
            tiles_visible(&session),
            "tiles hidden at the end of the body"
        );
        prove_both_details(&mut session, &left, &right);

        let mut proved = 1;
        for scroll in [-37.3, -13.7, -21.9, -0.45] {
            session.wheel(scroll);
            if tiles_visible(&session) {
                prove_both_details(&mut session, &left, &right);
                proved += 1;
            }
        }
        assert_eq!(proved, 5, "scroll positions that showed both tiles");

        session.wheel(100_000.0);
        let body = session.body();
        session.drag(body.center(), body.center() + egui::vec2(0.0, 23.4), false);
        assert!(tiles_visible(&session), "tiles hidden after the drag");
        prove_both_details(&mut session, &left, &right);
    }
}

#[test]
fn a_crop_smaller_than_the_tile_shows_whole_and_centred_on_black() {
    for viewport in SCALED {
        let Some(renderer) = renderer("100% preview of a small crop") else {
            return;
        };
        let (left, right) = crop_smaller_than_tile();
        let mut session =
            Session::with_pair_rendered((left.clone(), right.clone()), viewport, Some(renderer));
        session.wheel(100_000.0);

        prove_both_details(&mut session, &left, &right);
    }
}

#[test]
fn an_unavailable_detail_preview_leaves_the_other_three_views_intact() {
    let Some(renderer) = renderer("unavailable 100% preview") else {
        return;
    };
    let (left, right) = portrait_full_frame();
    let left = left.without_native_detail("fixture detail allocation failed");
    let mut session =
        Session::with_pair_rendered((left.clone(), right.clone()), T2, Some(renderer));

    let image = session.render();
    assert_full_page_markers(&session, &image, &left, CameraRole::Left);
    assert_full_page_markers(&session, &image, &right, CameraRole::Right);

    session.wheel(100_000.0);
    let image = session.render();
    assert!(session.in_body("Left 100% preview unavailable"));
    assert_detail_one_to_one(&session, &image, &right, CameraRole::Right);
}
