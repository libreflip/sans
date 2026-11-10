//! The shared layout every Sans screen uses: header, scrolling body, action bar.
//!
//! [`show`] lays a screen out from the rectangle the window actually receives,
//! at whatever scale factor the desktop reports.
//!
//! - The header is one line with the application name and the current status.
//!   It never scrolls.
//! - The action bar is fixed to the bottom and holds the screen's actions,
//!   stacked vertically at touch height. It never scrolls and cannot be resized.
//! - The body fills the space between them and scrolls vertically by pointer
//!   drag, mouse wheel, or a solid scroll bar when its content is taller.
//!   Every screen scrolls the same way: the bar is always solid and shown
//!   whenever the body overflows, and the body edges never fade. A floating
//!   bar is invisible on a touchscreen, and a fade would dim 100% preview
//!   pixels; one policy for all screens keeps them consistent.
//!
//! Contract for later screens: anything that must stay reachable goes in the
//! action bar. Stop comes first of all. Body content may scroll out of view at
//! any time, so it must never hold an action the operator depends on.
//!
//! The bar keeps actions on screen and still. It does not make them pressable
//! while another finger holds a drag: egui treats one touch as the only
//! pointer, so a second finger's tap is not a click until the first lifts.

use eframe::egui;

/// Touch height of every action.
pub(crate) const ACTION_HEIGHT: f32 = 56.0;
/// Widest an action grows.
const ACTION_MAX_WIDTH: f32 = 240.0;

/// Accessibility label of the body region.
const BODY_LABEL: &str = "Screen body";

/// The one-line status shown in the header.
pub(crate) struct Header<'a> {
    status: &'a str,
    colour: egui::Color32,
    busy: bool,
    warning_tag: Option<&'a str>,
}

impl<'a> Header<'a> {
    /// A status line in `colour`.
    pub(crate) fn new(status: &'a str, colour: egui::Color32) -> Self {
        Self {
            status,
            colour,
            busy: false,
            warning_tag: None,
        }
    }

    /// Show a spinner before the status.
    pub(crate) fn busy(mut self) -> Self {
        self.busy = true;
        self
    }

    /// Add a short warning tag after the status. The full text belongs in the body.
    pub(crate) fn warning_tag(mut self, tag: Option<&'a str>) -> Self {
        self.warning_tag = tag;
        self
    }
}

/// Lay out one screen.
///
/// `actions` draws into the fixed action bar. `body` draws into the scrolling
/// body and receives the height of the body viewport in points.
pub(crate) fn show(
    ui: &mut egui::Ui,
    header: Header<'_>,
    actions: impl FnOnce(&mut egui::Ui),
    body: impl FnOnce(&mut egui::Ui, f32),
) {
    let scroll = &mut ui.style_mut().spacing.scroll;
    *scroll = egui::style::ScrollStyle::solid();
    // The scroll policy in the module documentation: solid bar, no edge fade.
    scroll.fade.strength = 0.0;

    egui::Panel::top("screen-header")
        .resizable(false)
        .show(ui, |ui| show_header(ui, &header));

    egui::Panel::bottom("screen-actions")
        .resizable(false)
        .show(ui, |ui| {
            ui.add_space(4.0);
            ui.vertical_centered(actions);
            ui.add_space(4.0);
        });

    egui::CentralPanel::default().show(ui, |ui| {
        let viewport_height = ui.available_height();
        let output = egui::ScrollArea::vertical()
            .id_salt("screen-body")
            .scroll_source(egui::scroll_area::ScrollSource::ALL)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
            .auto_shrink(false)
            .show(ui, |ui| body(ui, viewport_height));
        // Name the region the operator sees, so tests and assistive tools can find it.
        ui.interact(
            output.inner_rect,
            output.id.with(BODY_LABEL),
            egui::Sense::hover(),
        )
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, BODY_LABEL));
    });
}

fn show_header(ui: &mut egui::Ui, header: &Header<'_>) {
    ui.horizontal(|ui| {
        ui.heading("Sans");
        ui.add_space(8.0);
        if header.busy {
            ui.spinner();
        }
        // The status comes first so a narrow window truncates the tag, not the status.
        ui.add(
            egui::Label::new(egui::RichText::new(header.status).color(header.colour)).truncate(),
        );
        if let Some(tag) = header.warning_tag {
            ui.add(
                egui::Label::new(egui::RichText::new(tag).color(egui::Color32::YELLOW)).truncate(),
            );
        }
    });
}

/// A full-height action button.
pub(crate) fn action_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add_sized(action_size(ui), egui::Button::new(label))
}

/// A non-interactive placeholder that keeps an action's place in the bar.
pub(crate) fn action_indicator(ui: &mut egui::Ui, label: &str) {
    ui.allocate_ui_with_layout(
        action_size(ui),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spinner();
            ui.label(label);
        },
    );
}

fn action_size(ui: &egui::Ui) -> egui::Vec2 {
    egui::vec2(ui.available_width().min(ACTION_MAX_WIDTH), ACTION_HEIGHT)
}

#[cfg(test)]
mod tests;
