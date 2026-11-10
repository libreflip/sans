//! Native portrait touchscreen application for Sans.
//!
//! The binary parses arguments and bootstraps the controller. This library
//! owns the application, its screens, and the startup window request so tests
//! can drive the same application headlessly.

mod lift_axis;
mod preview;
mod screen;

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use sans_core::{
    CapturePair, ControllerHandle, ControllerIntent, ControllerSnapshot, LinkPhase, MachineScreen,
    SetupState, StartupError,
};

use crate::lift_axis::{reconnect_action, show_lift_axis};
use crate::preview::{show_capture_preview, sync_preview_textures, PreviewTextures};
use crate::screen::Header;

/// Initial size hint, used only when no window manager honours the maximize request.
const PORTRAIT_SIZE_HINT: [f32; 2] = [600.0, 1_024.0];
/// Small enough that the window manager is never forced past the work area,
/// even at scale factor 2 on the 600 pixel wide display with window borders.
const MINIMUM_INNER_SIZE: [f32; 2] = [280.0, 240.0];
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
        // A drag that starts on selectable text selects it instead of scrolling.
        context.all_styles_mut(|style| style.interaction.selectable_labels = false);
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
        match &mut self.startup {
            StartupView::Fatal(message) => show_fatal(ui, &context, message),
            StartupView::Controller {
                handle,
                latest,
                exit_requested_at,
            } => {
                while let Ok(snapshot) = handle.try_snapshot() {
                    *latest = Some(snapshot);
                }
                let latest_pair = latest.as_ref().and_then(latest_complete_pair);
                sync_preview_textures(&context, &mut self.preview_textures, latest_pair.cloned());
                show_controller(
                    ui,
                    &context,
                    handle,
                    latest.as_ref(),
                    exit_requested_at,
                    self.preview_textures.as_ref(),
                );
            }
        }
    }
}

fn latest_complete_pair(snapshot: &ControllerSnapshot) -> Option<&Arc<CapturePair>> {
    match &snapshot.screen {
        MachineScreen::CapturePreview(preview) => preview.latest_complete_pair.as_ref(),
        MachineScreen::Setup(_) | MachineScreen::Exited => None,
    }
}

fn show_fatal(ui: &mut egui::Ui, context: &egui::Context, message: &str) {
    screen::show(
        ui,
        Header::new("Fatal startup error", egui::Color32::LIGHT_RED),
        |ui| {
            if screen::action_button(ui, "Close Sans").clicked() {
                context.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        },
        |ui, _| {
            ui.label(message);
        },
    );
}

fn show_controller(
    ui: &mut egui::Ui,
    context: &egui::Context,
    handle: &ControllerHandle,
    snapshot: Option<&ControllerSnapshot>,
    exit_requested_at: &mut Option<Instant>,
    textures: Option<&PreviewTextures>,
) {
    let screen = snapshot.map(|snapshot| &snapshot.screen);
    if matches!(screen, Some(MachineScreen::Exited)) {
        context.send_viewport_cmd(egui::ViewportCommand::Close);
        return;
    }
    if let Some(requested_at) = *exit_requested_at {
        if exit_fallback_elapsed(requested_at, Instant::now()) {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        context.request_repaint_after(Duration::from_millis(16));
    }
    if screen_needs_polling(screen) {
        // Keep draining snapshots so live connection faults reach the screen.
        context.request_repaint_after(Duration::from_millis(16));
    }
    // The lift-axis status changes without operator input on every screen.
    let lift_axis = snapshot.map(|snapshot| &snapshot.lift_axis);
    if lift_axis.is_some_and(|status| status.phase == LinkPhase::Connecting) {
        context.request_repaint_after(Duration::from_millis(100));
    } else if lift_axis.is_some_and(|status| status.telemetry.is_some()) {
        context.request_repaint_after(Duration::from_millis(250));
    }
    // Shown on the Setup screens only: the four preview views must fit and
    // scroll as they are.
    let show_lift = |ui: &mut egui::Ui| {
        if let Some(snapshot) = snapshot {
            ui.add_space(12.0);
            show_lift_axis(ui, &snapshot.lift_axis, &snapshot.scan_readiness);
        }
    };
    let mut closed = false;
    let mut exit = |ui: &mut egui::Ui| show_exit_action(ui, context, handle, exit_requested_at);

    match screen {
        None | Some(MachineScreen::Exited) => {
            screen::show(
                ui,
                Header::new("Checking Camera readiness...", egui::Color32::GRAY).busy(),
                exit,
                |_, _| {},
            );
        }
        Some(MachineScreen::Setup(SetupState::Blocked { reasons })) => {
            screen::show(
                ui,
                Header::new("Setup blocked", egui::Color32::YELLOW),
                |ui| {
                    closed = reconnect_action(ui, handle).is_err();
                    exit(ui);
                },
                |ui, _| {
                    ui.label("Scanning is disabled. Non-actuating diagnostics remain available.");
                    ui.add_space(12.0);
                    for reason in reasons {
                        ui.label(format!("• {}", reason.summary));
                    }
                    show_lift(ui);
                },
            );
            if closed {
                context.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        Some(MachineScreen::Setup(SetupState::Ready { diagnostics })) => {
            screen::show(
                ui,
                Header::new("Machine ready", egui::Color32::LIGHT_GREEN),
                |ui| {
                    if screen::action_button(ui, "Capture pair").clicked() {
                        closed = handle.send(ControllerIntent::CapturePair).is_err();
                        context.request_repaint_after(Duration::from_millis(16));
                    }
                    closed |= reconnect_action(ui, handle).is_err();
                    exit(ui);
                },
                |ui, _| {
                    for diagnostic in diagnostics {
                        ui.label(format!("{}: {}", diagnostic.component, diagnostic.summary));
                    }
                    show_lift(ui);
                },
            );
            if closed {
                context.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        Some(MachineScreen::CapturePreview(preview)) => {
            if show_capture_preview(ui, context, handle, preview, textures, exit).is_err() {
                context.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

fn show_exit_action(
    ui: &mut egui::Ui,
    context: &egui::Context,
    handle: &ControllerHandle,
    exit_requested_at: &mut Option<Instant>,
) {
    if exit_requested_at.is_some() {
        screen::action_indicator(ui, "Exiting Sans...");
    } else if screen::action_button(ui, "Exit").clicked() {
        if handle.send(ControllerIntent::Exit).is_ok() {
            *exit_requested_at = Some(Instant::now());
            context.request_repaint();
        } else {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// Whether the screen can change without operator input, so frames must keep running.
fn screen_needs_polling(screen: Option<&MachineScreen>) -> bool {
    matches!(
        screen,
        None | Some(
            MachineScreen::Setup(SetupState::Ready { .. }) | MachineScreen::CapturePreview(_)
        )
    )
}

fn exit_fallback_elapsed(requested_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(requested_at) >= EXIT_FALLBACK_TIMEOUT
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use sans_core::{MachineScreen, SetupState};

    use super::{exit_fallback_elapsed, screen_needs_polling, EXIT_FALLBACK_TIMEOUT};

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

    #[test]
    fn live_machine_screens_keep_polling_for_connection_faults() {
        let ready = MachineScreen::Setup(SetupState::Ready {
            diagnostics: Vec::new(),
        });
        let blocked = MachineScreen::Setup(SetupState::Blocked {
            reasons: Vec::new(),
        });

        assert!(screen_needs_polling(None));
        assert!(screen_needs_polling(Some(&ready)));
        assert!(screen_needs_polling(Some(&MachineScreen::CapturePreview(
            sans_core::CapturePreview {
                status: sans_core::CaptureStatus::Ready,
                latest_complete_pair: None,
                preview_warning: None,
            }
        ))));
        assert!(!screen_needs_polling(Some(&blocked)));
        assert!(!screen_needs_polling(Some(&MachineScreen::Exited)));
    }
}
