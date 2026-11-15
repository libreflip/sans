//! The Stop action, Clear Stop, and how a latched Stop is shown.
//!
//! Stop is the first action on every controller screen. It shares the
//! first row with the screen's leading action, so the bar keeps its height.
//! Clear Stop appears only for a Confirmed Stop. An Unconfirmed Stop asks
//! for manual power intervention and another Stop instead.

use std::time::Duration;

use eframe::egui;
use sans_core::{
    ControllerClosed, ControllerHandle, ControllerIntent, StopCause, StopStatus, StopVerdict,
};

use crate::screen::{self, Header};

/// Stop with `beside` to its right, then Clear Stop below them when the
/// latched Stop is confirmed.
pub(crate) fn stop_actions(
    ui: &mut egui::Ui,
    handle: &ControllerHandle,
    stop: Option<&StopStatus>,
    beside: impl FnOnce(&mut egui::Ui),
) -> Result<(), ControllerClosed> {
    let mut pressed = false;
    ui.columns(2, |columns| {
        let label = egui::RichText::new("Stop")
            .strong()
            .color(egui::Color32::WHITE);
        let button = egui::Button::new(label).fill(egui::Color32::from_rgb(170, 20, 20));
        pressed = screen::action(&mut columns[0], button).clicked();
        beside(&mut columns[1]);
    });
    if pressed {
        handle.send(ControllerIntent::Stop)?;
        ui.ctx().request_repaint_after(Duration::from_millis(16));
    }
    let confirmed = stop.is_some_and(|stop| stop.verdict == StopVerdict::Confirmed);
    if confirmed && screen::action_button(ui, "Clear Stop").clicked() {
        handle.send(ControllerIntent::ClearStop)?;
    }
    Ok(())
}

/// The header for a latched Stop, which replaces the screen's own status.
pub(crate) fn stop_header(stop: Option<&StopStatus>) -> Option<Header<'static>> {
    let header = match stop?.verdict {
        StopVerdict::Confirming => Header::new("Stopping...", egui::Color32::LIGHT_RED).busy(),
        StopVerdict::Confirmed => Header::new("Stop confirmed", egui::Color32::YELLOW),
        StopVerdict::Unconfirmed { .. } => Header::new(
            "UNCONFIRMED STOP: switch off machine power",
            egui::Color32::LIGHT_RED,
        ),
    };
    Some(header)
}

/// Cause and detail of a latched Stop, at the top of a screen body.
pub(crate) fn show_stop(ui: &mut egui::Ui, stop: Option<&StopStatus>) {
    let Some(stop) = stop else {
        return;
    };
    ui.colored_label(
        egui::Color32::LIGHT_RED,
        format!("Stop latched: {}", describe(&stop.cause)),
    );
    match &stop.verdict {
        StopVerdict::Confirming => {
            ui.label("Waiting for stopped motion and ALL OFF.");
        }
        StopVerdict::Confirmed => {
            ui.label(
                "Motion stopped and ALL OFF processed. Clear Stop to recover; Home before moving.",
            );
        }
        StopVerdict::Unconfirmed { reason } => {
            ui.colored_label(egui::Color32::LIGHT_RED, reason);
            ui.label("Switch off machine power and check the machine, then press Stop again.");
        }
    }
    if let Some(notice) = &stop.notice {
        ui.colored_label(
            egui::Color32::YELLOW,
            format!("Clear Stop failed: {notice}"),
        );
    }
    ui.add_space(12.0);
}

fn describe(cause: &StopCause) -> String {
    match cause {
        StopCause::Requested(by) => format!("requested by {by}"),
        StopCause::LinkLost(reason) => format!("link lost: {reason}"),
        StopCause::Uncertain(reason) => format!("uncertain motion: {reason}"),
        StopCause::AxisError(errors) => errors.clone(),
    }
}
