//! Four-view presentation of the latest complete Capture pair.

use std::sync::Arc;
use std::time::Duration;

use eframe::egui;
use sans_core::{
    CapturePair, CapturePreview, CaptureStatus, CapturedFrame, ControllerClosed, ControllerHandle,
    ControllerIntent, PreviewImage,
};

use crate::screen::{self, Header};

const FULL_PAGE_MAX_HEIGHT: f32 = 360.0;
/// Largest side of a 100% tile.
const DETAIL_MAX_SIDE: f32 = 220.0;
/// Gap between the Full-page row and the 100% row.
const ROW_GAP: f32 = 8.0;

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
        egui::TextureOptions::LINEAR,
    );
    let right_full_page = load_texture(
        context,
        "latest-right-full-page",
        pair.right.full_page_preview(),
        egui::TextureOptions::LINEAR,
    );
    let left_detail = pair.left.native_detail_preview().ok().map(|image| {
        load_texture(
            context,
            "latest-left-100-percent",
            image,
            egui::TextureOptions::NEAREST,
        )
    });
    let right_detail = pair.right.native_detail_preview().ok().map(|image| {
        load_texture(
            context,
            "latest-right-100-percent",
            image,
            egui::TextureOptions::NEAREST,
        )
    });
    *textures = Some(PreviewTextures {
        pair,
        left_full_page,
        right_full_page,
        left_detail,
        right_detail,
    });
}

fn load_texture(
    context: &egui::Context,
    name: &str,
    image: &PreviewImage,
    options: egui::TextureOptions,
) -> egui::TextureHandle {
    let color_image =
        egui::ColorImage::from_rgb([image.width as usize, image.height as usize], image.rgb8());
    context.load_texture(name, color_image, options)
}

/// Show the Capture preview screen for `preview`.
///
/// The action bar holds the state's capture action followed by `exit`.
pub(crate) fn show_capture_preview(
    ui: &mut egui::Ui,
    context: &egui::Context,
    handle: &ControllerHandle,
    preview: &CapturePreview,
    textures: Option<&PreviewTextures>,
    exit: impl FnOnce(&mut egui::Ui),
) -> Result<(), ControllerClosed> {
    let (status, colour) = match &preview.status {
        CaptureStatus::Ready => ("Cameras ready", egui::Color32::LIGHT_GREEN),
        CaptureStatus::Capturing => ("Capturing both Camera roles", egui::Color32::LIGHT_BLUE),
        CaptureStatus::Blocked { .. } => ("Capture blocked", egui::Color32::LIGHT_RED),
    };
    let header = Header::new(status, colour).warning_tag(
        preview
            .preview_warning
            .as_ref()
            .map(|_| "100% preview warning"),
    );
    if preview.status == CaptureStatus::Capturing {
        context.request_repaint_after(Duration::from_millis(16));
    }

    let mut result = Ok(());
    screen::show(
        ui,
        header,
        |ui| {
            result = show_capture_action(ui, context, handle, &preview.status);
            exit(ui);
        },
        |ui, viewport_height| {
            if let CaptureStatus::Blocked { reason } = &preview.status {
                ui.colored_label(egui::Color32::LIGHT_RED, reason);
            }
            if let Some(warning) = &preview.preview_warning {
                ui.colored_label(egui::Color32::YELLOW, warning);
            }
            match (preview.latest_complete_pair.as_ref(), textures) {
                (Some(_), Some(textures)) => render_four_views(ui, textures, viewport_height),
                _ => {
                    ui.label("No complete Capture pair yet.");
                }
            }
        },
    );
    result
}

fn show_capture_action(
    ui: &mut egui::Ui,
    context: &egui::Context,
    handle: &ControllerHandle,
    status: &CaptureStatus,
) -> Result<(), ControllerClosed> {
    let label = match status {
        CaptureStatus::Capturing => {
            screen::action_indicator(ui, "Capturing...");
            return Ok(());
        }
        CaptureStatus::Ready => "Capture pair",
        CaptureStatus::Blocked { .. } => "Retry Capture",
    };
    if screen::action_button(ui, label).clicked() {
        handle.send(ControllerIntent::CapturePair)?;
        context.request_repaint_after(Duration::from_millis(16));
    }
    Ok(())
}

/// Lay out both Full-page previews over both 100% previews.
fn render_four_views(ui: &mut egui::Ui, textures: &PreviewTextures, _viewport_height: f32) {
    let column_width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
    let detail_side = column_width.min(DETAIL_MAX_SIDE);
    let full_page_height = FULL_PAGE_MAX_HEIGHT;
    let full_page_area = egui::vec2(column_width, full_page_height);

    ui.columns(2, |columns| {
        render_full_page(
            &mut columns[0],
            "Left",
            &textures.left_full_page,
            &textures.pair.left,
            full_page_area,
        );
        render_full_page(
            &mut columns[1],
            "Right",
            &textures.right_full_page,
            &textures.pair.right,
            full_page_area,
        );
    });
    ui.add_space(ROW_GAP);
    ui.columns(2, |columns| {
        render_detail(
            &mut columns[0],
            "Left",
            textures.left_detail.as_ref(),
            textures.pair.left.native_detail_preview().ok(),
            detail_side,
        );
        render_detail(
            &mut columns[1],
            "Right",
            textures.right_detail.as_ref(),
            textures.pair.right.native_detail_preview().ok(),
            detail_side,
        );
    });
}

fn render_full_page(
    ui: &mut egui::Ui,
    role: &str,
    texture: &egui::TextureHandle,
    frame: &CapturedFrame,
    area: egui::Vec2,
) {
    ui.vertical_centered(|ui| {
        ui.label(format!("{role} Full-page"));
        let image = frame.full_page_preview();
        let layout = full_page_layout(area, image);
        let (rect, response) = ui.allocate_exact_size(layout.draw_size, egui::Sense::hover());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Image,
                true,
                format!("{role} Full-page preview"),
            )
        });
        ui.painter()
            .image(texture.id(), rect, layout.uv, egui::Color32::WHITE);
    });
}

fn render_detail(
    ui: &mut egui::Ui,
    role: &str,
    texture: Option<&egui::TextureHandle>,
    image: Option<&PreviewImage>,
    side: f32,
) {
    ui.vertical_centered(|ui| {
        ui.label(format!("{role} 100%"));
        let (tile, response) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
        ui.painter().rect_filled(tile, 0.0, egui::Color32::BLACK);
        match (texture, image) {
            (Some(texture), Some(image)) => {
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Image,
                        true,
                        format!("{role} 100% preview"),
                    )
                });
                let pixels_per_point = ui.ctx().pixels_per_point();
                let layout = native_detail_layout(side, pixels_per_point, image);
                let rect = egui::Rect::from_center_size(tile.center(), layout.draw_size);
                ui.painter()
                    .image(texture.id(), rect, layout.uv, egui::Color32::WHITE);
            }
            _ => {
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Image,
                        true,
                        format!("{role} 100% preview unavailable"),
                    )
                });
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::SystemTime;

    use sans_core::{CameraRole, CapturePair, CapturedFrame, CropGeometry};

    use super::{full_page_layout, native_detail_layout, sync_preview_textures, PreviewTextures};

    fn frame(role: CameraRole) -> CapturedFrame {
        CapturedFrame::from_rgb8(
            role,
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
        let frame = frame(CameraRole::Left);
        let layout = full_page_layout(eframe::egui::vec2(100.0, 100.0), frame.full_page_preview());

        assert_eq!(layout.draw_size, eframe::egui::vec2(100.0, 50.0));
        assert_eq!(layout.uv.min, eframe::egui::Pos2::ZERO);
        assert_eq!(layout.uv.max, eframe::egui::pos2(1.0, 1.0));
    }

    #[test]
    fn detail_preview_centers_one_image_pixel_per_physical_display_pixel() {
        let frame = frame(CameraRole::Left);
        let layout = native_detail_layout(80.0, 2.0, frame.native_detail_preview().unwrap());

        assert_eq!(layout.draw_size, eframe::egui::vec2(80.0, 50.0));
        assert_eq!(layout.uv.min, eframe::egui::pos2(20.0 / 200.0, 0.0));
        assert_eq!(layout.uv.max, eframe::egui::pos2(180.0 / 200.0, 1.0));
    }

    #[test]
    fn full_page_and_detail_textures_use_their_required_filters() {
        let context = eframe::egui::Context::default();
        let pair = Arc::new(CapturePair {
            left: frame(CameraRole::Left),
            right: frame(CameraRole::Right),
        });
        let mut textures = None;

        context.begin_pass(Default::default());
        sync_preview_textures(&context, &mut textures, Some(pair));
        let mut output = context.end_pass();
        let PreviewTextures {
            left_full_page,
            right_full_page,
            left_detail,
            right_detail,
            ..
        } = textures.unwrap();
        let options_for = |texture: &eframe::egui::TextureHandle| {
            output
                .textures_delta
                .set
                .iter()
                .find(|(id, _)| **id == texture.id())
                .and_then(|(_, deltas)| deltas.last())
                .map(|delta| delta.options)
                .unwrap()
        };

        assert_eq!(
            options_for(&left_full_page),
            eframe::egui::TextureOptions::LINEAR
        );
        assert_eq!(
            options_for(&right_full_page),
            eframe::egui::TextureOptions::LINEAR
        );
        assert_eq!(
            options_for(left_detail.as_ref().unwrap()),
            eframe::egui::TextureOptions::NEAREST
        );
        assert_eq!(
            options_for(right_detail.as_ref().unwrap()),
            eframe::egui::TextureOptions::NEAREST
        );
        output.textures_delta.clear();
    }
}
