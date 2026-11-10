//! Marked fixture frames that make clipping, stretching, and resampling visible.
//!
//! Pixels outside the commissioned crop are [`FORBIDDEN`]. Inside the crop,
//! each edge has a band and each corner has a block in a distinct saturated
//! colour. Left and Right use different palettes. The interior is
//! deterministic grey noise, which no marker can be mistaken for.

use std::time::SystemTime;

use sans_core::{CameraRole, CapturedFrame, CropGeometry};

/// Thickness of each edge band in source pixels.
pub const BAND: u32 = 12;
/// Side of each corner block in source pixels.
pub const CORNER: u32 = 24;

/// The colour of every pixel outside the commissioned crop.
///
/// No blend of the in-crop colours can produce it: every in-crop colour has
/// a red or blue channel below 255 or a green channel above 0.
pub const FORBIDDEN: Rgb = [255, 0, 255];

/// One packed sRGB pixel.
pub type Rgb = [u8; 3];

/// Where a marker sits on the crop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Marker {
    Top,
    Right,
    Bottom,
    Left,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Marker {
    /// Every marker, edges first.
    pub const ALL: [Self; 8] = [
        Self::Top,
        Self::Right,
        Self::Bottom,
        Self::Left,
        Self::TopLeft,
        Self::TopRight,
        Self::BottomLeft,
        Self::BottomRight,
    ];
}

/// Marker colours for one Camera role.
#[derive(Clone, Copy, Debug)]
pub struct Palette([Rgb; 8]);

impl Palette {
    /// The colour painted for `marker`.
    pub fn colour(&self, marker: Marker) -> Rgb {
        self.0[Marker::ALL.iter().position(|m| *m == marker).unwrap()]
    }

    /// Every marker with its colour.
    pub fn markers(&self) -> impl Iterator<Item = (Marker, Rgb)> + '_ {
        Marker::ALL
            .into_iter()
            .map(|marker| (marker, self.colour(marker)))
    }
}

/// Palette for the Left role, in [`Marker::ALL`] order.
pub const LEFT_PALETTE: Palette = Palette([
    [255, 0, 0],
    [255, 255, 0],
    [0, 0, 255],
    [0, 255, 0],
    [0, 255, 255],
    [255, 128, 0],
    [128, 0, 255],
    [128, 255, 255],
]);

/// Palette for the Right role, in [`Marker::ALL`] order.
pub const RIGHT_PALETTE: Palette = Palette([
    [0, 128, 255],
    [128, 255, 0],
    [255, 0, 128],
    [0, 255, 128],
    [128, 0, 0],
    [0, 128, 0],
    [0, 0, 128],
    [128, 128, 0],
]);

/// The palette a role's fixture uses.
pub fn palette(role: CameraRole) -> Palette {
    match role {
        CameraRole::Left => LEFT_PALETTE,
        CameraRole::Right => RIGHT_PALETTE,
    }
}

/// A rectangle in source pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// A marked post-rotation image and the crop the core is expected to keep.
#[derive(Clone, Debug)]
pub struct MarkedImage {
    pub width: u32,
    pub height: u32,
    /// The crop after clamping to the frame, computed independently of the core.
    pub crop: PixelRect,
    pub rgb8: Vec<u8>,
}

impl MarkedImage {
    /// Paint a `width`x`height` image whose `crop`, clamped to the frame, is marked for `role`.
    pub fn new(role: CameraRole, width: u32, height: u32, crop: CropGeometry) -> Self {
        let crop = PixelRect {
            x: crop.x,
            y: crop.y,
            width: crop.width.min(width - crop.x),
            height: crop.height.min(height - crop.y),
        };
        let palette = palette(role);
        let mut rgb8 = Vec::with_capacity((width * height * 3) as usize);
        for y in 0..height {
            for x in 0..width {
                rgb8.extend_from_slice(&marked_pixel(palette, crop, x, y));
            }
        }
        Self {
            width,
            height,
            crop,
            rgb8,
        }
    }

    /// The pixel at `(x, y)` relative to the crop's top-left corner.
    pub fn crop_pixel(&self, x: u32, y: u32) -> Rgb {
        let index = (((self.crop.y + y) * self.width + self.crop.x + x) * 3) as usize;
        [self.rgb8[index], self.rgb8[index + 1], self.rgb8[index + 2]]
    }
}

fn marked_pixel(palette: Palette, crop: PixelRect, x: u32, y: u32) -> Rgb {
    if x < crop.x || y < crop.y || x >= crop.x + crop.width || y >= crop.y + crop.height {
        return FORBIDDEN;
    }
    let (x, y) = (x - crop.x, y - crop.y);
    let left = x < CORNER;
    let right = x >= crop.width - CORNER;
    let top = y < CORNER;
    let bottom = y >= crop.height - CORNER;
    let marker = match (top, bottom, left, right) {
        (true, _, true, _) => Some(Marker::TopLeft),
        (true, _, _, true) => Some(Marker::TopRight),
        (_, true, true, _) => Some(Marker::BottomLeft),
        (_, true, _, true) => Some(Marker::BottomRight),
        _ if y < BAND => Some(Marker::Top),
        _ if y >= crop.height - BAND => Some(Marker::Bottom),
        _ if x < BAND => Some(Marker::Left),
        _ if x >= crop.width - BAND => Some(Marker::Right),
        _ => None,
    };
    match marker {
        Some(marker) => palette.colour(marker),
        None => [noise(x, y); 3],
    }
}

/// Deterministic grey noise between 64 and 191.
fn noise(x: u32, y: u32) -> u8 {
    let mut hash = x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA77);
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(0xC2B2_AE3D);
    hash ^= hash >> 13;
    64 + (hash & 0x7F) as u8
}

/// A post-rotation frame with no rotation applied.
pub fn marked_frame(role: CameraRole, width: u32, height: u32, crop: CropGeometry) -> Fixture {
    let image = MarkedImage::new(role, width, height, crop);
    let frame = CapturedFrame::from_rgb8(
        role,
        width,
        height,
        image.rgb8.clone(),
        SystemTime::UNIX_EPOCH,
        crop,
    )
    .unwrap();
    Fixture { image, frame }
}

/// A landscape source that the core rotates a quarter turn clockwise into
/// the marked `width`x`height` portrait image. `crop` is in post-rotation
/// coordinates.
pub fn rotated_marked_frame(
    role: CameraRole,
    width: u32,
    height: u32,
    crop: CropGeometry,
) -> Fixture {
    let image = MarkedImage::new(role, width, height, crop);
    // A clockwise quarter turn moves source (x, y) to (source_height - 1 - y, x).
    let (source_width, source_height) = (height, width);
    let mut source = Vec::with_capacity(image.rgb8.len());
    for y in 0..source_height {
        for x in 0..source_width {
            let (rotated_x, rotated_y) = (source_height - 1 - y, x);
            let index = ((rotated_y * width + rotated_x) * 3) as usize;
            source.extend_from_slice(&image.rgb8[index..index + 3]);
        }
    }
    let frame = CapturedFrame::from_unrotated_rgb8(
        role,
        source_width,
        source_height,
        source,
        SystemTime::UNIX_EPOCH,
        90,
        crop,
    )
    .unwrap();
    Fixture { image, frame }
}

/// A frame for the controller plus the independent description of what it shows.
#[derive(Clone, Debug)]
pub struct Fixture {
    pub image: MarkedImage,
    pub frame: CapturedFrame,
}

impl Fixture {
    /// Mark only the 100% preview unavailable.
    pub fn without_native_detail(mut self, summary: &str) -> Self {
        self.frame = self.frame.without_native_detail_preview(summary);
        self
    }
}

/// A crop covering the whole frame.
pub fn full_crop(width: u32, height: u32) -> CropGeometry {
    CropGeometry {
        x: 0,
        y: 0,
        width,
        height,
    }
}

/// A Left and Right pair of full-frame fixtures.
pub fn full_frame_pair(width: u32, height: u32) -> (Fixture, Fixture) {
    (
        marked_frame(CameraRole::Left, width, height, full_crop(width, height)),
        marked_frame(CameraRole::Right, width, height, full_crop(width, height)),
    )
}

/// Portrait full-frame crop.
pub fn portrait_full_frame() -> (Fixture, Fixture) {
    full_frame_pair(360, 640)
}

/// Portrait full-frame crop whose Left 100% preview is unavailable.
pub fn portrait_without_left_detail() -> (Fixture, Fixture) {
    let (left, right) = portrait_full_frame();
    (
        left.without_native_detail("fixture detail allocation failed"),
        right,
    )
}

/// Landscape full-frame crop.
pub fn landscape_full_frame() -> (Fixture, Fixture) {
    full_frame_pair(640, 360)
}

/// A pair with the same smaller crop at an odd offset in each role.
pub fn smaller_crop_pair(width: u32, height: u32, crop: CropGeometry) -> (Fixture, Fixture) {
    (
        marked_frame(CameraRole::Left, width, height, crop),
        marked_frame(CameraRole::Right, width, height, crop),
    )
}

/// Portrait frame with a smaller crop at an odd offset.
pub fn portrait_smaller_crop() -> (Fixture, Fixture) {
    smaller_crop_pair(
        400,
        700,
        CropGeometry {
            x: 37,
            y: 53,
            width: 301,
            height: 521,
        },
    )
}

/// Landscape frame with a smaller crop at an odd offset.
pub fn landscape_smaller_crop() -> (Fixture, Fixture) {
    smaller_crop_pair(
        700,
        400,
        CropGeometry {
            x: 61,
            y: 29,
            width: 523,
            height: 297,
        },
    )
}

/// Landscape sources rotated into portrait, cropped in post-rotation coordinates.
pub fn rotated_crop() -> (Fixture, Fixture) {
    let crop = CropGeometry {
        x: 21,
        y: 45,
        width: 311,
        height: 547,
    };
    (
        rotated_marked_frame(CameraRole::Left, 360, 640, crop),
        rotated_marked_frame(CameraRole::Right, 360, 640, crop),
    )
}

/// A crop that extends past the frame, which the core clamps.
pub fn clamped_crop() -> (Fixture, Fixture) {
    smaller_crop_pair(
        360,
        640,
        CropGeometry {
            x: 41,
            y: 83,
            width: 1_000,
            height: 2_000,
        },
    )
}

/// A crop smaller than the 100% tile at every scale factor.
pub fn crop_smaller_than_tile() -> (Fixture, Fixture) {
    smaller_crop_pair(
        360,
        640,
        CropGeometry {
            x: 101,
            y: 203,
            width: 97,
            height: 131,
        },
    )
}
