//! Assertions over rendered frames: what an operator would see in each view.

use image::RgbaImage;
use sans_core::CameraRole;

use super::fixtures::{palette, Fixture, Marker, BAND};
use super::pixels::{find, rgb, PixelBox};
use super::session::Session;

/// Fewest visible block rows a 100% proof accepts.
pub const MIN_VISIBLE_ROWS: u32 = 40;

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
pub fn assert_full_page_markers(
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

/// Assert the `role` 100% preview shows the centred source block one-to-one.
///
/// The expected block comes from the issue's contract: the centred region of
/// the crop, at most the tile side times pixels per point in each dimension,
/// floored to whole pixels. It must match the rendered frame exactly, with
/// zero differing pixels, at exactly one position below the role's 100% label.
/// Tile pixels around a block smaller than the tile must be black.
///
/// Only rows inside the body viewport are compared, because the body may have
/// scrolled part of the tile out of view. At least [`MIN_VISIBLE_ROWS`] rows
/// must be visible.
pub fn assert_detail_one_to_one(
    session: &Session,
    image: &RgbaImage,
    fixture: &Fixture,
    role: CameraRole,
) {
    let at = format!(
        "{} x{}",
        session.viewport.name, session.viewport.pixels_per_point
    );
    let pixels_per_point = session.viewport.pixels_per_point;
    let tile = session.rect(&format!("{role} 100% preview"));
    let label = session.rect(&format!("{role} 100%"));
    let crop = fixture.image.crop;
    let source_side = (tile.width() * pixels_per_point).floor() as u32;
    let (width, height) = (source_side.min(crop.width), source_side.min(crop.height));
    let (offset_x, offset_y) = ((crop.width - width) / 2, (crop.height - height) / 2);
    let expected = |x: u32, y: u32| fixture.image.crop_pixel(offset_x + x, offset_y + y);

    let body = session.body();
    let visible_rows = (body.min.y * pixels_per_point).ceil() as u32
        ..(body.max.y * pixels_per_point).floor() as u32;
    let region = PixelBox::from_points(tile, pixels_per_point, image).expand(2, image);
    let mut matches = Vec::new();
    for top in region.min_y..=(region.max_y + 1).saturating_sub(height) {
        let rows: Vec<u32> = (0..height)
            .filter(|y| visible_rows.contains(&(top + y)))
            .collect();
        if (rows.len() as u32) < MIN_VISIBLE_ROWS {
            continue;
        }
        for left in region.min_x..=(region.max_x + 1).saturating_sub(width) {
            let exact = rows
                .iter()
                .all(|&y| (0..width).all(|x| rgb(image, left + x, top + y) == expected(x, y)));
            if exact {
                matches.push((left, top));
            }
        }
    }
    assert_eq!(
        matches.len(),
        1,
        "{role} 100% block of {width}x{height} matched at {matches:?} at {at}"
    );
    let (left, top) = matches[0];
    assert!(
        top as f32 >= label.max.y * pixels_per_point,
        "{role} 100% block sits above its label at {at}"
    );

    let inside = PixelBox {
        min_x: (tile.min.x * pixels_per_point).ceil() as u32,
        min_y: (tile.min.y * pixels_per_point).ceil() as u32,
        max_x: (tile.max.x * pixels_per_point).floor() as u32 - 1,
        max_y: (tile.max.y * pixels_per_point).floor() as u32 - 1,
    };
    for y in (inside.min_y..=inside.max_y).filter(|y| visible_rows.contains(y)) {
        for x in inside.min_x..=inside.max_x {
            let in_block = (left..left + width).contains(&x) && (top..top + height).contains(&y);
            assert!(
                in_block || rgb(image, x, y) == [0, 0, 0],
                "{role} 100% tile pixel ({x}, {y}) outside the block is not black at {at}"
            );
        }
    }
}
