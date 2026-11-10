//! Rendering the application to pixels and finding fixture markers in them.
//!
//! Pixel proofs need a wgpu adapter. [`renderer`] checks for one before
//! creating a renderer, because creating one without an adapter panics.
//! Without an adapter the proof prints a skip notice and passes, unless
//! `SANS_REQUIRE_PIXEL_PROOF=1` is set, in which case it fails.

use std::io::Write;
use std::sync::Once;

use eframe::{egui, egui_wgpu, wgpu};
use egui_kittest::wgpu::{create_render_state, default_wgpu_setup, WgpuTestRenderer};
use image::RgbaImage;

use super::fixtures::Rgb;
use super::session::Session;

/// Largest per-channel difference still treated as a marker colour.
const MARKER_TOLERANCE: u8 = 2;

static ADAPTER_NOTICE: Once = Once::new();

/// A renderer for one pixel proof, or `None` when the proof must be skipped.
///
/// `proof` names the test in the skip notice.
pub fn renderer(proof: &str) -> Option<WgpuTestRenderer> {
    let required = std::env::var("SANS_REQUIRE_PIXEL_PROOF").is_ok_and(|value| value == "1");
    let setup = default_wgpu_setup();
    let instance = pollster::block_on(setup.new_instance());
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    if !adapters
        .iter()
        .any(|adapter| adapter.get_info().backend != wgpu::Backend::Noop)
    {
        assert!(
            !required,
            "pixel proof {proof} cannot run: no wgpu adapter and SANS_REQUIRE_PIXEL_PROOF=1"
        );
        // Write past the test harness's output capture so the skip is always visible.
        let _ = writeln!(
            std::io::stderr(),
            "SKIPPED pixel proof {proof}: no wgpu adapter. Set SANS_REQUIRE_PIXEL_PROOF=1 to fail instead."
        );
        return None;
    }
    let state = create_render_state(setup, egui_wgpu::RendererOptions::PREDICTABLE);
    ADAPTER_NOTICE.call_once(|| {
        let info = state.adapter.get_info();
        let _ = writeln!(
            std::io::stderr(),
            "pixel proofs render on {} ({:?}, {:?}, driver {:?})",
            info.name,
            info.backend,
            info.device_type,
            info.driver
        );
    });
    Some(WgpuTestRenderer::from_render_state(state))
}

/// An inclusive rectangle of physical pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PixelBox {
    pub min_x: u32,
    pub min_y: u32,
    pub max_x: u32,
    pub max_y: u32,
}

impl PixelBox {
    /// The physical pixels a rectangle in points covers, clamped to `image`.
    pub fn from_points(rect: egui::Rect, pixels_per_point: f32, image: &RgbaImage) -> Self {
        let rect = rect * pixels_per_point;
        Self {
            min_x: rect.min.x.floor().max(0.0) as u32,
            min_y: rect.min.y.floor().max(0.0) as u32,
            max_x: (rect.max.x.ceil() as u32).min(image.width()) - 1,
            max_y: (rect.max.y.ceil() as u32).min(image.height()) - 1,
        }
    }

    /// Grow by `pixels` on every side, clamped to `image`.
    pub fn expand(self, pixels: u32, image: &RgbaImage) -> Self {
        Self {
            min_x: self.min_x.saturating_sub(pixels),
            min_y: self.min_y.saturating_sub(pixels),
            max_x: (self.max_x + pixels).min(image.width() - 1),
            max_y: (self.max_y + pixels).min(image.height() - 1),
        }
    }

    /// The smallest box holding both.
    pub fn union(self, other: Self) -> Self {
        Self {
            min_x: self.min_x.min(other.min_x),
            min_y: self.min_y.min(other.min_y),
            max_x: self.max_x.max(other.max_x),
            max_y: self.max_y.max(other.max_y),
        }
    }

    pub fn width(&self) -> u32 {
        self.max_x - self.min_x + 1
    }

    pub fn height(&self) -> u32 {
        self.max_y - self.min_y + 1
    }
}

/// The rendered pixel at `(x, y)` without alpha.
pub fn rgb(image: &RgbaImage, x: u32, y: u32) -> Rgb {
    let [r, g, b, _] = image.get_pixel(x, y).0;
    [r, g, b]
}

fn matches(pixel: Rgb, colour: Rgb) -> bool {
    pixel
        .iter()
        .zip(colour)
        .all(|(pixel, colour)| pixel.abs_diff(colour) <= MARKER_TOLERANCE)
}

/// The bounding box of every pixel in `region` that shows `colour`.
pub fn find(image: &RgbaImage, region: PixelBox, colour: Rgb) -> Option<PixelBox> {
    let mut found: Option<PixelBox> = None;
    for y in region.min_y..=region.max_y {
        for x in region.min_x..=region.max_x {
            if matches(rgb(image, x, y), colour) {
                let pixel = PixelBox {
                    min_x: x,
                    min_y: y,
                    max_x: x,
                    max_y: y,
                };
                found = Some(found.map_or(pixel, |found| found.union(pixel)));
            }
        }
    }
    found
}

/// How many pixels in the whole image show `colour`.
pub fn count(image: &RgbaImage, colour: Rgb) -> usize {
    image
        .pixels()
        .filter(|pixel| matches([pixel.0[0], pixel.0[1], pixel.0[2]], colour))
        .count()
}

impl Session {
    /// Render the current frame without the test pointer drawn over it.
    pub fn render(&mut self) -> RgbaImage {
        self.harness.remove_cursor();
        self.harness.step();
        self.settle();
        self.harness.render().expect("render the application")
    }
}
