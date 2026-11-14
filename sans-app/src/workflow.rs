//! Workflow screens: Home consent, Homing, Idle, page-width entry, Ready and
//! the running Scan session.
//!
//! Every screen keeps Stop first in the fixed action bar. The page-width
//! draft is the only state held here; the controller checks the value.

use std::time::Duration;

use eframe::egui;
use sans_core::{
    ControllerClosed, ControllerHandle, ControllerIntent, PageWidthEntry, ScanView, StopStatus,
};

use crate::preview::{render_four_views, PreviewTextures};
use crate::screen::{self, Header};
use crate::stop::{show_stop, stop_actions, stop_header};

/// Longest page-width draft in digits.
const DRAFT_DIGITS: usize = 4;

/// Send `intent` and keep frames running until its snapshot arrives.
fn send(
    ui: &egui::Ui,
    handle: &ControllerHandle,
    intent: ControllerIntent,
) -> Result<(), ControllerClosed> {
    handle.send(intent)?;
    ui.ctx().request_repaint_after(Duration::from_millis(16));
    Ok(())
}

/// A full-height action that sends `intent` when pressed.
fn intent_action(
    ui: &mut egui::Ui,
    handle: &ControllerHandle,
    label: &str,
    intent: ControllerIntent,
) -> Result<(), ControllerClosed> {
    if screen::action_button(ui, label).clicked() {
        send(ui, handle, intent)?;
    }
    Ok(())
}

/// The Home consent action for the Setup screens.
pub(crate) fn home_action(
    ui: &mut egui::Ui,
    handle: &ControllerHandle,
) -> Result<(), ControllerClosed> {
    intent_action(ui, handle, "Home lift axis", ControllerIntent::Home)
}

/// What Home consent means, for the Setup screen bodies.
pub(crate) fn show_home_consent(ui: &mut egui::Ui) {
    ui.label(
        "Home is required. The suction box moves up to its Home reference. \
         Keep hands clear of the machine, then press Home lift axis.",
    );
    ui.add_space(12.0);
}

pub(crate) fn show_homing(
    ui: &mut egui::Ui,
    handle: &ControllerHandle,
    stop: Option<&StopStatus>,
) -> Result<(), ControllerClosed> {
    let mut result = Ok(());
    screen::show(
        ui,
        stop_header(stop)
            .unwrap_or_else(|| Header::new("Homing lift axis...", egui::Color32::YELLOW).busy()),
        |ui| {
            result = stop_actions(ui, handle, stop, |ui| {
                screen::action_indicator(ui, "Homing...");
            });
        },
        |ui, _| {
            show_stop(ui, stop);
            ui.label("The suction box is moving to its Home reference. Press Stop to halt it.");
        },
    );
    result
}

pub(crate) fn show_idle(
    ui: &mut egui::Ui,
    handle: &ControllerHandle,
    stop: Option<&StopStatus>,
    exit: impl FnOnce(&mut egui::Ui),
) -> Result<(), ControllerClosed> {
    let mut result = Ok(());
    screen::show(
        ui,
        stop_header(stop).unwrap_or_else(|| Header::new("Idle", egui::Color32::LIGHT_BLUE)),
        |ui| {
            let mut new_book = Ok(());
            result = stop_actions(ui, handle, stop, |ui| {
                new_book = intent_action(ui, handle, "New Book", ControllerIntent::NewBook);
            })
            .and(new_book);
            exit(ui);
        },
        |ui, _| {
            show_stop(ui, stop);
            ui.label("Position trust is held. Press New Book or the green button to scan a book.");
        },
    );
    result
}

pub(crate) fn show_page_width(
    ui: &mut egui::Ui,
    handle: &ControllerHandle,
    stop: Option<&StopStatus>,
    entry: &PageWidthEntry,
    draft: &mut String,
    exit: impl FnOnce(&mut egui::Ui),
) -> Result<(), ControllerClosed> {
    // The bar edits the draft; the body shows it as it was at the frame's start.
    let shown = if draft.is_empty() {
        "—".to_string()
    } else {
        draft.clone()
    };
    let mut result = Ok(());
    screen::show(
        ui,
        stop_header(stop)
            .unwrap_or_else(|| Header::new("Enter page width", egui::Color32::LIGHT_BLUE)),
        |ui| {
            let mut set = Ok(());
            result = stop_actions(ui, handle, stop, |ui| {
                let millimetres = draft.parse::<u16>().ok();
                let label =
                    millimetres.map_or("Set page width".into(), |mm| format!("Set {mm} mm"));
                let pressed = ui
                    .add_enabled_ui(millimetres.is_some(), |ui| {
                        screen::action_button(ui, &label)
                    })
                    .inner
                    .clicked();
                if let Some(millimetres) = millimetres.filter(|_| pressed) {
                    set = send(ui, handle, ControllerIntent::SetPageWidth { millimetres });
                }
            })
            .and(set);
            show_keypad(ui, draft);
            exit(ui);
        },
        |ui, _| {
            show_stop(ui, stop);
            ui.label(format!(
                "Measure the width of one page in millimetres, between {} and {} mm.",
                entry.minimum_mm, entry.maximum_mm
            ));
            ui.add_space(12.0);
            ui.heading(format!("Page width: {shown} mm"));
            if let Some(rejected) = entry.rejected_mm {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    format!(
                        "{rejected} mm is outside {}–{} mm.",
                        entry.minimum_mm, entry.maximum_mm
                    ),
                );
            }
        },
    );
    result
}

/// Digits, delete and clear, three to a row at touch height.
fn show_keypad(ui: &mut egui::Ui, draft: &mut String) {
    for row in [
        ["1", "2", "3"],
        ["4", "5", "6"],
        ["7", "8", "9"],
        ["Clear", "0", "Delete"],
    ] {
        ui.columns(3, |columns| {
            for (column, key) in columns.iter_mut().zip(row) {
                if !screen::action_button(column, key).clicked() {
                    continue;
                }
                match key {
                    "Clear" => draft.clear(),
                    "Delete" => {
                        draft.pop();
                    }
                    digit if draft.len() < DRAFT_DIGITS => draft.push_str(digit),
                    _ => {}
                }
            }
        });
    }
}

pub(crate) fn show_ready(
    ui: &mut egui::Ui,
    handle: &ControllerHandle,
    stop: Option<&StopStatus>,
    page_width_mm: u16,
    notice: Option<&str>,
    exit: impl FnOnce(&mut egui::Ui),
) -> Result<(), ControllerClosed> {
    let mut result = Ok(());
    screen::show(
        ui,
        stop_header(stop).unwrap_or_else(|| Header::new("Ready", egui::Color32::LIGHT_GREEN)),
        |ui| {
            let mut start = Ok(());
            result = stop_actions(ui, handle, stop, |ui| {
                start = intent_action(ui, handle, "Start", ControllerIntent::Start);
            })
            .and(start)
            .and_then(|()| {
                intent_action(ui, handle, "Change page width", ControllerIntent::NewBook)
            });
            exit(ui);
        },
        |ui, _| {
            show_stop(ui, stop);
            ui.heading(format!("Page width: {page_width_mm} mm"));
            ui.label(
                "Press Start or the green button to create the Scan session, \
                 switch the light on and capture the first pages.",
            );
            if let Some(notice) = notice {
                ui.colored_label(egui::Color32::LIGHT_RED, notice);
            }
        },
    );
    result
}

pub(crate) fn show_scanning(
    ui: &mut egui::Ui,
    handle: &ControllerHandle,
    stop: Option<&StopStatus>,
    view: &ScanView,
    pressure_mbar: Option<f32>,
    textures: Option<&PreviewTextures>,
    exit: impl FnOnce(&mut egui::Ui),
) -> Result<(), ControllerClosed> {
    let capture = &view.capture;
    let (status, colour) = match &capture.status {
        sans_core::CaptureStatus::Capturing => ("Scanning: capturing", egui::Color32::LIGHT_BLUE),
        sans_core::CaptureStatus::Ready => ("Scanning", egui::Color32::LIGHT_GREEN),
        sans_core::CaptureStatus::Blocked { .. } => ("Capture blocked", egui::Color32::LIGHT_RED),
    };
    let header = stop_header(stop).unwrap_or_else(|| {
        Header::new(status, colour).warning_tag(
            capture
                .preview_warning
                .as_ref()
                .map(|_| "100% preview warning"),
        )
    });
    let mut result = Ok(());
    screen::show(
        ui,
        header,
        |ui| {
            result = stop_actions(ui, handle, stop, |ui| {
                let label = match &capture.status {
                    sans_core::CaptureStatus::Capturing => {
                        return screen::action_indicator(ui, "Capturing...");
                    }
                    sans_core::CaptureStatus::Ready => "First pair captured",
                    sans_core::CaptureStatus::Blocked { .. } => "Capture blocked",
                };
                // A status in the action's place; #27 continues the Capture cycle.
                ui.add_enabled_ui(false, |ui| screen::action_button(ui, label));
            });
            exit(ui);
        },
        |ui, viewport_height| {
            show_stop(ui, stop);
            ui.label(format!(
                "Capture cycle {}  attempt {}  Sequence {:04}/{:04}",
                view.capture_cycle, view.attempt, view.left_sequence, view.right_sequence
            ));
            ui.label(pressure_mbar.map_or_else(
                || "Pressure: no reading".to_string(),
                |mbar| format!("Pressure {mbar:.1} mbar"),
            ));
            ui.small(format!(
                "Scan session {}  page width {} mm",
                view.directory.display(),
                view.page_width_mm
            ));
            if let sans_core::CaptureStatus::Blocked { reason } = &capture.status {
                ui.colored_label(egui::Color32::LIGHT_RED, reason);
            }
            if let Some(warning) = &capture.preview_warning {
                ui.colored_label(egui::Color32::YELLOW, warning);
            }
            match (capture.latest_complete_pair.as_ref(), textures) {
                (Some(_), Some(textures)) => render_four_views(ui, textures, viewport_height),
                _ => {
                    ui.label("No complete Capture pair yet.");
                }
            }
        },
    );
    result
}
