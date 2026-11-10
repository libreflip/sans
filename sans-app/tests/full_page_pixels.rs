//! Rendered-pixel proofs that each Full-page preview shows its whole crop.

mod support;

use image::RgbaImage;
use sans_core::CameraRole;
use support::fixtures::{
    landscape_full_frame, palette, portrait_full_frame, Fixture, Marker, BAND, FORBIDDEN,
};
use support::pixels::{count, find, renderer, PixelBox};
use support::session::{Session, Viewport, T1, T3};

/// Allowed disagreement, in physical pixels, between rendered extents and expected ones.
const EDGE_TOLERANCE: f32 = 1.0;
/// Linear filtering blends one more pixel at a band's inner edge.
const BAND_TOLERANCE: f32 = 2.0;

fn close(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() <= EDGE_TOLERANCE
}

fn band_close(actual: u32, expected: f32) -> bool {
    (actual as f32 - expected).abs() <= BAND_TOLERANCE
}

/// Assert the `role` Full-page preview shows every marker of `fixture` on its own edge.
///
/// Markers are searched only inside the role's own preview, so a swapped or
/// duplicated view fails because its own palette is missing.
fn assert_full_page_markers(
    session: &Session,
    image: &RgbaImage,
    fixture: &Fixture,
    role: CameraRole,
) {
    let at = session.viewport.name;
    let pixels_per_point = session.viewport.pixels_per_point;
    let node = session.rect(&format!("{role} Full-page preview"));
    let region = PixelBox::from_points(node, pixels_per_point, image).expand(2, image);

    let found: Vec<(Marker, PixelBox)> = palette(role)
        .markers()
        .map(|(marker, colour)| {
            let found = find(image, region, colour)
                .unwrap_or_else(|| panic!("{role} {marker:?} marker is missing at {at}"));
            (marker, found)
        })
        .collect();
    let drawn = found
        .iter()
        .map(|(_, found)| *found)
        .reduce(PixelBox::union)
        .unwrap();

    let expected = node * pixels_per_point;
    assert!(
        close(drawn.min_x as f32, expected.min.x)
            && close(drawn.min_y as f32, expected.min.y)
            && close((drawn.max_x + 1) as f32, expected.max.x)
            && close((drawn.max_y + 1) as f32, expected.max.y),
        "{role} markers span {drawn:?} but the preview is {expected:?} at {at}"
    );
    let crop = fixture.image.crop;
    let crop_aspect_width = expected.height() * crop.width as f32 / crop.height as f32;
    assert!(
        close(drawn.width() as f32, expected.width())
            && close(drawn.height() as f32, expected.height())
            && close(expected.width(), crop_aspect_width),
        "{role} preview draws {}x{} pixels in a {:?} rectangle for a {}x{} crop at {at}",
        drawn.width(),
        drawn.height(),
        expected.size(),
        crop.width,
        crop.height
    );

    let band = BAND as f32 * drawn.height() as f32 / crop.height as f32;
    for (marker, found) in found {
        let touches = match marker {
            Marker::Top => found.min_y == drawn.min_y && band_close(found.height(), band),
            Marker::Bottom => found.max_y == drawn.max_y && band_close(found.height(), band),
            Marker::Left => found.min_x == drawn.min_x && band_close(found.width(), band),
            Marker::Right => found.max_x == drawn.max_x && band_close(found.width(), band),
            Marker::TopLeft => found.min_x == drawn.min_x && found.min_y == drawn.min_y,
            Marker::TopRight => found.max_x == drawn.max_x && found.min_y == drawn.min_y,
            Marker::BottomLeft => found.min_x == drawn.min_x && found.max_y == drawn.max_y,
            Marker::BottomRight => found.max_x == drawn.max_x && found.max_y == drawn.max_y,
        };
        assert!(
            touches,
            "{role} {marker:?} marker at {found:?} does not sit on its edge of {drawn:?} at {at}"
        );
    }
}

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
