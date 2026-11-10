//! Native portrait touchscreen application for Sans.
//!
//! The binary parses arguments and bootstraps the controller. This library
//! owns the application, its screens, and the startup window request so tests
//! can drive the same application headlessly.

mod preview;

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use sans_core::{
    CapturePair, ControllerHandle, ControllerIntent, ControllerSnapshot, MachineScreen, SetupState,
    StartupError,
};

use crate::preview::{render_capture_preview, sync_preview_textures, PreviewTextures};

/// Initial size hint, used only when no window manager honours the maximize request.
const PORTRAIT_SIZE_HINT: [f32; 2] = [600.0, 1_024.0];
/// Small enough that the window manager is never forced past the work area,
/// even at scale factor 2 on the 600 pixel wide display.
const MINIMUM_INNER_SIZE: [f32; 2] = [300.0, 240.0];
const EXIT_FALLBACK_TIMEOUT: Duration = Duration::from_secs(3);

/// The startup window request for the native application.
///
/// Sans opens maximized and decorated, so the desktop decides the usable size
/// after its title bar and menu bar. Sizes are in points and the reported
/// scale factor is respected. Screens lay out from whatever rectangle they
/// receive.
pub fn window_request() -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title("Sans")
        .with_maximized(true)
        .with_decorations(true)
        .with_inner_size(PORTRAIT_SIZE_HINT)
        .with_min_inner_size(MINIMUM_INNER_SIZE)
}

/// What the application shows after startup validation.
pub enum Startup {
    /// Startup failed before a controller existed.
    Fatal(String),
    /// A controller is running and publishes snapshots.
    Controller(ControllerHandle),
}

impl Startup {
    /// Map the result of [`sans_core::bootstrap`] to the first screen.
    pub fn from_bootstrap(result: Result<ControllerHandle, StartupError>) -> Self {
        match result {
            Ok(handle) => Self::Controller(handle),
            Err(error) => Self::Fatal(error.to_string()),
        }
    }
}

enum StartupView {
    Fatal(String),
    Controller {
        handle: ControllerHandle,
        latest: Option<ControllerSnapshot>,
        exit_requested_at: Option<Instant>,
    },
}

/// The native Sans application.
pub struct SansApp {
    startup: StartupView,
    preview_textures: Option<PreviewTextures>,
}

impl SansApp {
    /// Configure the egui context and show the first screen for `startup`.
    pub fn new(context: &egui::Context, startup: Startup) -> Self {
        context.set_visuals(egui::Visuals::dark());
        let startup = match startup {
            Startup::Fatal(message) => StartupView::Fatal(message),
            Startup::Controller(handle) => StartupView::Controller {
                handle,
                latest: None,
                exit_requested_at: None,
            },
        };
        Self {
            startup,
            preview_textures: None,
        }
    }
}

impl eframe::App for SansApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = ui.ctx().clone();
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Sans");

            match &mut self.startup {
                StartupView::Fatal(message) => render_fatal(ui, &context, message),
                StartupView::Controller {
                    handle,
                    latest,
                    exit_requested_at,
                } => {
                    while let Ok(snapshot) = handle.try_snapshot() {
                        *latest = Some(snapshot);
                    }
                    let latest_pair = latest.as_ref().and_then(latest_complete_pair);
                    sync_preview_textures(
                        &context,
                        &mut self.preview_textures,
                        latest_pair.cloned(),
                    );
                    render_controller(
                        ui,
                        &context,
                        handle,
                        latest.as_ref(),
                        exit_requested_at,
                        self.preview_textures.as_ref(),
                    );
                }
            }
        });
    }
}

fn latest_complete_pair(snapshot: &ControllerSnapshot) -> Option<&Arc<CapturePair>> {
    match &snapshot.screen {
        MachineScreen::CapturePreview(preview) => preview.latest_complete_pair.as_ref(),
        MachineScreen::Setup(_) | MachineScreen::Exited => None,
    }
}

fn render_fatal(ui: &mut egui::Ui, context: &egui::Context, message: &str) {
    ui.colored_label(egui::Color32::LIGHT_RED, "Fatal startup error");
    ui.add_space(16.0);
    ui.label(message);
    ui.add_space(24.0);
    if large_button(ui, "Close Sans").clicked() {
        context.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

fn render_controller(
    ui: &mut egui::Ui,
    context: &egui::Context,
    handle: &ControllerHandle,
    snapshot: Option<&ControllerSnapshot>,
    exit_requested_at: &mut Option<Instant>,
    textures: Option<&PreviewTextures>,
) {
    match snapshot.map(|snapshot| &snapshot.screen) {
        None => {
            ui.spinner();
            ui.label("Checking Camera readiness...");
            context.request_repaint_after(Duration::from_millis(16));
        }
        Some(MachineScreen::Setup(SetupState::Blocked { reasons })) => {
            ui.colored_label(egui::Color32::YELLOW, "Setup blocked");
            ui.label("Scanning is disabled. Camera diagnostics remain available.");
            ui.add_space(12.0);
            for reason in reasons {
                ui.label(format!("• {}", reason.summary));
            }
        }
        Some(MachineScreen::CapturePreview(preview)) => {
            if render_capture_preview(ui, context, handle, preview, textures).is_err() {
                context.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
        }
        Some(MachineScreen::Exited) => {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
    }

    ui.add_space(16.0);
    if let Some(requested_at) = *exit_requested_at {
        if exit_fallback_elapsed(requested_at, Instant::now()) {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        ui.spinner();
        ui.label("Exiting Sans...");
        context.request_repaint_after(Duration::from_millis(16));
    } else if large_button(ui, "Exit").clicked() {
        if handle.send(ControllerIntent::Exit).is_ok() {
            *exit_requested_at = Some(Instant::now());
            context.request_repaint();
        } else {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn large_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add_sized([240.0, 56.0], egui::Button::new(label))
}

fn exit_fallback_elapsed(requested_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(requested_at) >= EXIT_FALLBACK_TIMEOUT
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{exit_fallback_elapsed, EXIT_FALLBACK_TIMEOUT};

    #[test]
    fn exit_fallback_closes_after_controller_deadline() {
        let requested_at = Instant::now();

        assert!(!exit_fallback_elapsed(
            requested_at,
            requested_at + EXIT_FALLBACK_TIMEOUT - Duration::from_millis(1)
        ));
        assert!(exit_fallback_elapsed(
            requested_at,
            requested_at + EXIT_FALLBACK_TIMEOUT
        ));
    }
}
