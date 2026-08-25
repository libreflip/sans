//! Four-view presentation of the latest complete Capture pair.

use std::sync::Arc;
use std::time::Duration;

use eframe::egui;
use sans_core::{
    CapturePair, CapturePreview, CaptureStatus, ControllerHandle, ControllerIntent, PreviewImage,
};

const FULL_PAGE_MAX_HEIGHT: f32 = 360.0;
const DETAIL_MAX_SIDE: f32 = 220.0;

pub(crate) struct PreviewTextures {
    pair: Arc<CapturePair>,
    left_full_page: egui::TextureHandle,
    right_full_page: egui::TextureHandle,
    left_detail: Option<egui::TextureHandle>,
    right_detail: Option<egui::TextureHandle>,
}

pub(crate) fn sync_preview_textures(
    context: &egui::Context,
    textures: &mut Option<PreviewTextures>,
    pair: Option<Arc<CapturePair>>,
) {
    let Some(pair) = pair else {
        *textures = None;
        return;
    };
    if textures
        .as_ref()
        .is_some_and(|textures| Arc::ptr_eq(&textures.pair, &pair))
    {
        return;
    }
    let left_full_page = load_texture(
        context,
        "latest-left-full-page",
        pair.left.full_page_preview(),
    );
    let right_full_page = load_texture(
        context,
        "latest-right-full-page",
        pair.right.full_page_preview(),
    );
    let left_detail = pair
        .left
        .native_detail_preview()
        .ok()
        .map(|image| load_texture(context, "latest-left-100-percent", image));
    let right_detail = pair
        .right
        .native_detail_preview()
        .ok()
        .map(|image| load_texture(context, "latest-right-100-percent", image));
    *textures = Some(PreviewTextures {
        pair,
        left_full_page,
        right_full_page,
        left_detail,
        right_detail,
    });
}

fn load_texture(context: &egui::Context, name: &str, image: &PreviewImage) -> egui::TextureHandle {
    let color_image =
        egui::ColorImage::from_rgb([image.width as usize, image.height as usize], image.rgb8());
    context.load_texture(name, color_image, egui::TextureOptions::LINEAR)
}

pub(crate) fn render_capture_preview(
    ui: &mut egui::Ui,
    context: &egui::Context,
    handle: &ControllerHandle,
    preview: &CapturePreview,
    textures: Option<&PreviewTextures>,
) {
    match &preview.status {
        CaptureStatus::Ready => {
            ui.colored_label(egui::Color32::LIGHT_GREEN, "Cameras ready");
            if large_button(ui, "Capture pair").clicked() {
                let _ = handle.send(ControllerIntent::CapturePair);
                context.request_repaint_after(Duration::from_millis(16));
            }
        }
        CaptureStatus::Capturing => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Capturing both Camera roles. The previous pair stays visible.");
            });
            context.request_repaint_after(Duration::from_millis(16));
        }
        CaptureStatus::Blocked { reason } => {
            ui.colored_label(egui::Color32::LIGHT_RED, "Capture blocked");
            ui.label(reason);
            if large_button(ui, "Retry Capture").clicked() {
                let _ = handle.send(ControllerIntent::CapturePair);
                context.request_repaint_after(Duration::from_millis(16));
            }
        }
    }
    if let Some(warning) = &preview.preview_warning {
        ui.colored_label(egui::Color32::YELLOW, warning);
    }
    ui.add_space(10.0);

    match (preview.latest_complete_pair.as_ref(), textures) {
        (Some(_), Some(textures)) => render_four_views(ui, textures),
        _ => {
            ui.label("No complete Capture pair yet.");
        }
    }
}

fn render_four_views(ui: &mut egui::Ui, textures: &PreviewTextures) {
    ui.columns(2, |columns| {
        render_full_page(
            &mut columns[0],
            "Left Full-page",
            &textures.left_full_page,
            textures.pair.left.full_page_preview(),
        );
        render_full_page(
            &mut columns[1],
            "Right Full-page",
            &textures.right_full_page,
            textures.pair.right.full_page_preview(),
        );
    });
    ui.add_space(8.0);
    ui.columns(2, |columns| {
        render_detail(
            &mut columns[0],
            "Left 100%",
            textures.left_detail.as_ref(),
            textures.pair.left.native_detail_preview().ok(),
        );
        render_detail(
            &mut columns[1],
            "Right 100%",
            textures.right_detail.as_ref(),
            textures.pair.right.native_detail_preview().ok(),
        );
    });
}

fn render_full_page(
    ui: &mut egui::Ui,
    label: &str,
    texture: &egui::TextureHandle,
    image: &PreviewImage,
) {
    ui.vertical_centered(|ui| {
        ui.label(label);
        let available = egui::vec2(ui.available_width(), FULL_PAGE_MAX_HEIGHT);
        let layout = full_page_layout(available, image);
        let (rect, _) = ui.allocate_exact_size(layout.draw_size, egui::Sense::hover());
        ui.painter()
            .image(texture.id(), rect, layout.uv, egui::Color32::WHITE);
    });
}

fn render_detail(
    ui: &mut egui::Ui,
    label: &str,
    texture: Option<&egui::TextureHandle>,
    image: Option<&PreviewImage>,
) {
    ui.vertical_centered(|ui| {
        ui.label(label);
        let side = ui.available_width().min(DETAIL_MAX_SIDE);
        let (tile, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
        ui.painter().rect_filled(tile, 0.0, egui::Color32::BLACK);
        match (texture, image) {
            (Some(texture), Some(image)) => {
                let layout = native_detail_layout(side, ui.ctx().pixels_per_point(), image);
                let rect = egui::Rect::from_center_size(tile.center(), layout.draw_size);
                ui.painter()
                    .image(texture.id(), rect, layout.uv, egui::Color32::WHITE);
            }
            _ => {
                ui.painter().text(
                    tile.center(),
                    egui::Align2::CENTER_CENTER,
                    "100% preview\nunavailable",
                    egui::FontId::proportional(18.0),
                    egui::Color32::YELLOW,
                );
            }
        }
    });
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PreviewLayout {
    draw_size: egui::Vec2,
    uv: egui::Rect,
}

fn full_page_layout(available: egui::Vec2, image: &PreviewImage) -> PreviewLayout {
    let image_size = egui::vec2(image.width as f32, image.height as f32);
    let scale = (available.x / image_size.x)
        .min(available.y / image_size.y)
        .min(1.0);
    PreviewLayout {
        draw_size: image_size * scale,
        uv: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
    }
}

fn native_detail_layout(side: f32, pixels_per_point: f32, image: &PreviewImage) -> PreviewLayout {
    let source_side = (side * pixels_per_point).floor();
    let width = source_side.min(image.width as f32) as u32;
    let height = source_side.min(image.height as f32) as u32;
    let x = (image.width - width) / 2;
    let y = (image.height - height) / 2;
    PreviewLayout {
        draw_size: egui::vec2(
            width as f32 / pixels_per_point,
            height as f32 / pixels_per_point,
        ),
        uv: egui::Rect::from_min_max(
            egui::pos2(
                x as f32 / image.width as f32,
                y as f32 / image.height as f32,
            ),
            egui::pos2(
                (x + width) as f32 / image.width as f32,
                (y + height) as f32 / image.height as f32,
            ),
        ),
    }
}

fn large_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add_sized([240.0, 56.0], egui::Button::new(label))
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use sans_core::{CameraRole, CapturedFrame, CropGeometry};

    use super::{full_page_layout, native_detail_layout};

    fn frame() -> CapturedFrame {
        CapturedFrame::from_rgb8(
            CameraRole::Left,
            400,
            300,
            vec![0; 400 * 300 * 3],
            SystemTime::UNIX_EPOCH,
            CropGeometry {
                x: 50,
                y: 20,
                width: 200,
                height: 100,
            },
        )
        .unwrap()
    }

    #[test]
    fn full_page_preview_aspect_fits_without_distortion() {
        let frame = frame();
        let layout = full_page_layout(eframe::egui::vec2(100.0, 100.0), frame.full_page_preview());

        assert_eq!(layout.draw_size, eframe::egui::vec2(100.0, 50.0));
        assert_eq!(layout.uv.min, eframe::egui::Pos2::ZERO);
        assert_eq!(layout.uv.max, eframe::egui::pos2(1.0, 1.0));
    }

    #[test]
    fn detail_preview_centers_one_image_pixel_per_physical_display_pixel() {
        let frame = frame();
        let layout = native_detail_layout(80.0, 2.0, frame.native_detail_preview().unwrap());

        assert_eq!(layout.draw_size, eframe::egui::vec2(80.0, 50.0));
        assert_eq!(layout.uv.min, eframe::egui::pos2(20.0 / 200.0, 0.0));
        assert_eq!(layout.uv.max, eframe::egui::pos2(180.0 / 200.0, 1.0));
    }
}
