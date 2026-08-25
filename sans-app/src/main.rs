//! Native portrait touchscreen entry point for Sans.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Parser;
use eframe::egui;
use sans_core::{
    bootstrap, ControllerHandle, ControllerIntent, ControllerSnapshot, MachineFactory,
    MachineScreen, PreparedMachineProfile, SetupBlocker, SetupState,
};

const PORTRAIT_WIDTH: f32 = 800.0;
const PORTRAIT_HEIGHT: f32 = 1_280.0;
const EXIT_FALLBACK_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Parser)]
#[command(about = "Run the native Sans touchscreen application")]
struct Arguments {
    /// Use an explicit Machine profile instead of the normal user config path.
    #[arg(long)]
    config: Option<PathBuf>,
}

struct BootstrapMachineFactory;

impl MachineFactory for BootstrapMachineFactory {
    type Machine = ();

    fn open(self, _profile: &PreparedMachineProfile) -> Result<Self::Machine, Vec<SetupBlocker>> {
        Err(vec![
            SetupBlocker::new("Machine connections are not configured in this tracer bullet."),
            SetupBlocker::new("Use direct diagnostics only while Sans is stopped."),
        ])
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

struct SansApp {
    startup: StartupView,
}

impl SansApp {
    fn new(context: &eframe::CreationContext<'_>, startup: StartupView) -> Self {
        context.egui_ctx.set_visuals(egui::Visuals::dark());
        Self { startup }
    }
}

impl eframe::App for SansApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = ui.ctx().clone();
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(32.0);
            ui.heading("Sans");
            ui.label("MVPrototype machine setup");
            ui.add_space(32.0);

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
                    render_controller(ui, &context, handle, latest.as_ref(), exit_requested_at);
                }
            }
        });
    }
}

fn render_fatal(ui: &mut egui::Ui, context: &egui::Context, message: &str) {
    ui.colored_label(egui::Color32::LIGHT_RED, "Fatal startup error");
    ui.add_space(16.0);
    ui.label(message);
    ui.add_space(24.0);
    if ui
        .add_sized([240.0, 64.0], egui::Button::new("Close Sans"))
        .clicked()
    {
        context.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

fn render_controller(
    ui: &mut egui::Ui,
    context: &egui::Context,
    handle: &ControllerHandle,
    snapshot: Option<&ControllerSnapshot>,
    exit_requested_at: &mut Option<Instant>,
) {
    match snapshot.map(|snapshot| &snapshot.screen) {
        None => {
            ui.spinner();
            ui.label("Checking machine readiness…");
        }
        Some(MachineScreen::Setup(SetupState::Blocked { reasons })) => {
            ui.colored_label(egui::Color32::YELLOW, "Setup blocked");
            ui.label("Sans will not actuate the machine.");
            ui.add_space(16.0);
            for reason in reasons {
                ui.label(format!("• {}", reason.summary));
            }
        }
        Some(MachineScreen::Setup(SetupState::Ready)) => {
            ui.colored_label(egui::Color32::LIGHT_GREEN, "Machine profile ready");
            ui.label("No scan workflow is active.");
        }
        Some(MachineScreen::Exited) => {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
    }

    ui.add_space(32.0);
    if let Some(requested_at) = *exit_requested_at {
        if exit_fallback_elapsed(requested_at, Instant::now()) {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        ui.spinner();
        ui.label("Exiting Sans…");
        context.request_repaint_after(Duration::from_millis(16));
    } else if ui
        .add_sized([240.0, 64.0], egui::Button::new("Exit"))
        .clicked()
    {
        if handle.send(ControllerIntent::Exit).is_ok() {
            *exit_requested_at = Some(Instant::now());
            context.request_repaint();
        } else {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn exit_fallback_elapsed(requested_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(requested_at) >= EXIT_FALLBACK_TIMEOUT
}

fn main() -> eframe::Result {
    let arguments = Arguments::parse();
    let startup = match bootstrap(arguments.config.as_deref(), BootstrapMachineFactory) {
        Ok(handle) => StartupView::Controller {
            handle,
            latest: None,
            exit_requested_at: None,
        },
        Err(error) => StartupView::Fatal(error.to_string()),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Sans")
            .with_inner_size([PORTRAIT_WIDTH, PORTRAIT_HEIGHT])
            .with_min_inner_size([600.0, 900.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Sans",
        options,
        Box::new(move |context| Ok(Box::new(SansApp::new(context, startup)))),
    )
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
