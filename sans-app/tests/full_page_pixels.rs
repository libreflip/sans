//! Rendered-pixel proofs that each Full-page preview shows its whole crop.

mod support;

use sans_core::CameraRole;
use support::fixtures::{
    clamped_crop, landscape_full_frame, landscape_smaller_crop, portrait_full_frame,
    portrait_smaller_crop, rotated_crop, Fixture, FORBIDDEN,
};
use support::pixels::{count, renderer};
use support::proofs::assert_full_page_markers;
use support::session::{Session, Viewport, T1, T3};

/// Render `pair` at `viewport` and prove both Full-page previews.
fn prove_full_page(proof: &str, (left, right): (Fixture, Fixture), viewport: Viewport) {
    let Some(renderer) = renderer(proof) else {
        return;
    };
    let mut session =
        Session::with_pair_rendered((left.clone(), right.clone()), viewport, Some(renderer));

    let image = session.render();

    assert_full_page_markers(&session, &image, &left, CameraRole::Left);
    assert_full_page_markers(&session, &image, &right, CameraRole::Right);
    assert_eq!(
        count(&image, FORBIDDEN),
        0,
        "pixels outside the crop leaked"
    );
}

#[test]
fn portrait_full_frame_shows_all_four_edges_of_both_cameras() {
    for viewport in [T1, T3] {
        prove_full_page("portrait full frame", portrait_full_frame(), viewport);
    }
}

#[test]
fn landscape_full_frame_shows_all_four_edges_of_both_cameras() {
    for viewport in [T1, T3] {
        prove_full_page("landscape full frame", landscape_full_frame(), viewport);
    }
}

#[test]
fn smaller_portrait_crop_shows_exactly_that_rectangle() {
    prove_full_page("portrait smaller crop", portrait_smaller_crop(), T1);
}

#[test]
fn smaller_landscape_crop_shows_exactly_that_rectangle() {
    prove_full_page("landscape smaller crop", landscape_smaller_crop(), T1);
}

#[test]
fn crop_after_a_quarter_turn_shows_exactly_that_rectangle() {
    prove_full_page("rotated crop", rotated_crop(), T1);
}

#[test]
fn crop_past_the_frame_shows_the_clamped_rectangle() {
    prove_full_page("clamped crop", clamped_crop(), T1);
}
