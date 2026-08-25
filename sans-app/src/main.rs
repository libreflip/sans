//! Native portrait touchscreen entry point for Sans.

mod preview;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use eframe::egui;
#[cfg(target_os = "linux")]
use sans_core::V4lCameraMachineFactory;
use sans_core::{
    bootstrap, CapturePair, CapturePairError, ControllerHandle, ControllerIntent,
    ControllerMachine, ControllerSnapshot, MachineFactory, MachineScreen, MonospaceClient,
    MonospaceConnection, MonospaceEventKind, MonospaceFault, PreparedMachineProfile, SetupBlocker,
    SetupDiagnostic, SetupState,
};
#[cfg(not(target_os = "linux"))]
use sans_core::{CameraCaptureError, CameraRole};

use crate::preview::{render_capture_preview, sync_preview_textures, PreviewTextures};

const PORTRAIT_WIDTH: f32 = 600.0;
const PORTRAIT_HEIGHT: f32 = 1_024.0;
const EXIT_FALLBACK_TIMEOUT: Duration = Duration::from_secs(3);
const MONOSPACE_BOOT_DELAY: Duration = Duration::from_millis(2_500);

#[derive(Debug, Parser)]
#[command(about = "Run the native Sans touchscreen application")]
struct Arguments {
    /// Use an explicit Machine profile instead of the normal user config path.
    #[arg(long)]
    config: Option<PathBuf>,
}

struct BootstrapMachineFactory<CameraFactory> {
    cameras: CameraFactory,
}

struct BootstrapMachine<CameraMachine> {
    cameras: CameraMachine,
    monospace: MonospaceConnection,
}

impl<CameraMachine: ControllerMachine> ControllerMachine for BootstrapMachine<CameraMachine> {
    fn capture_pair(&mut self) -> Result<CapturePair, CapturePairError> {
        self.cameras.capture_pair()
    }
}

impl<CameraFactory: MachineFactory> MachineFactory for BootstrapMachineFactory<CameraFactory> {
    type Machine = BootstrapMachine<CameraFactory::Machine>;

    fn open(
        self,
        profile: &PreparedMachineProfile,
    ) -> Result<(Self::Machine, Vec<SetupDiagnostic>), Vec<SetupBlocker>> {
        let cameras = self.cameras.open(profile);
        let path = &profile.profile().boards.monospace_path;
        let open_timeout = Duration::from_millis(profile.profile().timeouts.device_open_ms);
        let reply_timeout = Duration::from_millis(profile.profile().timeouts.command_ms);
        let monospace =
            MonospaceClient::connect(path, MONOSPACE_BOOT_DELAY, open_timeout, reply_timeout)
                .map_err(|error| vec![SetupBlocker::new(format!("Monospace at {path}: {error}"))]);

        match (cameras, monospace) {
            (Ok((cameras, mut diagnostics)), Ok(monospace)) => {
                let epoch = monospace.client.epoch().get();
                diagnostics.push(SetupDiagnostic::ready(
                    "Monospace",
                    format!("Ready at {path} on connection epoch {epoch}"),
                ));
                Ok((BootstrapMachine { cameras, monospace }, diagnostics))
            }
            (Err(mut camera_blockers), Err(mut monospace_blockers)) => {
                camera_blockers.append(&mut monospace_blockers);
                Err(camera_blockers)
            }
            (Err(camera_blockers), Ok(_)) => Err(camera_blockers),
            (Ok(_), Err(monospace_blockers)) => Err(monospace_blockers),
        }
    }

    fn poll_setup(machine: &mut Self::Machine) -> Option<SetupBlocker> {
        if let Some(blocker) = CameraFactory::poll_setup(&mut machine.cameras) {
            return Some(blocker);
        }
        while let Ok(event) = machine.monospace.events.try_recv() {
            if let Some(blocker) = monospace_setup_blocker(event.epoch.get(), event.kind) {
                return Some(blocker);
            }
        }
        None
    }
}

fn monospace_setup_blocker(epoch: u64, event: MonospaceEventKind) -> Option<SetupBlocker> {
    match event {
        MonospaceEventKind::Pressure(_) | MonospaceEventKind::ButtonPressed => None,
        MonospaceEventKind::UnknownEvent(payload) => {
            eprintln!("ignored unknown Monospace event on epoch {epoch}: {payload:?}");
            None
        }
        MonospaceEventKind::Disconnected => Some(SetupBlocker::new(format!(
            "Monospace disconnected on connection epoch {epoch}"
        ))),
        MonospaceEventKind::Fault(fault) => Some(SetupBlocker::new(format!(
            "Monospace connection epoch {epoch} is blocked: {}",
            monospace_fault_summary(&fault)
        ))),
    }
}

fn monospace_fault_summary(fault: &MonospaceFault) -> String {
    match fault {
        MonospaceFault::MalformedFrame(frame) => format!("malformed frame {frame:?}"),
        MonospaceFault::ReplyTimeout => "reply timeout poisoned response correlation".into(),
        MonospaceFault::UnexpectedReply(reply) => format!("unexpected reply {reply:?}"),
        MonospaceFault::AmbiguousUrgentWrite => {
            "urgent writing poisoned response correlation".into()
        }
        MonospaceFault::SerialIo(error) => format!("serial I/O failure: {error}"),
    }
}

#[cfg(not(target_os = "linux"))]
struct UnsupportedCameraFactory;

#[cfg(not(target_os = "linux"))]
struct UnsupportedCameraMachine;

#[cfg(not(target_os = "linux"))]
impl ControllerMachine for UnsupportedCameraMachine {
    fn capture_pair(&mut self) -> Result<CapturePair, CapturePairError> {
        Err(CameraCaptureError::Profile {
            role: CameraRole::Left,
            detail: "V4L2 Camera support requires Linux".into(),
        }
        .into())
    }
}

#[cfg(not(target_os = "linux"))]
impl MachineFactory for UnsupportedCameraFactory {
    type Machine = UnsupportedCameraMachine;

    fn open(
        self,
        _profile: &PreparedMachineProfile,
    ) -> Result<(Self::Machine, Vec<SetupDiagnostic>), Vec<SetupBlocker>> {
        Err(vec![SetupBlocker::new(
            "Live Camera readiness requires Linux V4L2; diagnostics remain available.",
        )])
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
    preview_textures: Option<PreviewTextures>,
}

impl SansApp {
    fn new(context: &eframe::CreationContext<'_>, startup: StartupView) -> Self {
        context.egui_ctx.set_visuals(egui::Visuals::dark());
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
    let screen = snapshot.map(|snapshot| &snapshot.screen);
    if screen_needs_polling(screen) {
        context.request_repaint_after(Duration::from_millis(16));
    }

    match screen {
        None => {
            ui.spinner();
            ui.label("Checking Camera readiness...");
            context.request_repaint_after(Duration::from_millis(16));
        }
        Some(MachineScreen::Setup(SetupState::Blocked { reasons })) => {
            ui.colored_label(egui::Color32::YELLOW, "Setup blocked");
            ui.label("Scanning is disabled. Non-actuating diagnostics remain available.");
            ui.add_space(12.0);
            for reason in reasons {
                ui.label(format!("• {}", reason.summary));
            }
        }
        Some(MachineScreen::Setup(SetupState::Ready { diagnostics })) => {
            ui.colored_label(egui::Color32::LIGHT_GREEN, "Machine ready");
            for diagnostic in diagnostics {
                ui.label(format!("{}: {}", diagnostic.component, diagnostic.summary));
            }
            ui.add_space(12.0);
            if large_button(ui, "Capture pair").clicked() {
                if handle.send(ControllerIntent::CapturePair).is_err() {
                    context.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                context.request_repaint_after(Duration::from_millis(16));
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

fn main() -> eframe::Result {
    let arguments = Arguments::parse();
    #[cfg(target_os = "linux")]
    let camera_factory = V4lCameraMachineFactory;
    #[cfg(not(target_os = "linux"))]
    let camera_factory = UnsupportedCameraFactory;
    let startup = bootstrap(
        arguments.config.as_deref(),
        BootstrapMachineFactory {
            cameras: camera_factory,
        },
    );
    let startup = match startup {
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
            .with_min_inner_size([PORTRAIT_WIDTH, PORTRAIT_HEIGHT]),
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

    use sans_core::{MachineScreen, MonospaceEventKind, MonospaceFault, SetupState};

    use super::{
        exit_fallback_elapsed, monospace_fault_summary, monospace_setup_blocker,
        screen_needs_polling, EXIT_FALLBACK_TIMEOUT,
    };

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
    fn setup_fault_summaries_preserve_the_failure_class() {
        assert_eq!(
            monospace_fault_summary(&MonospaceFault::ReplyTimeout),
            "reply timeout poisoned response correlation"
        );
        assert_eq!(
            monospace_fault_summary(&MonospaceFault::MalformedFrame("broken".into())),
            "malformed frame \"broken\""
        );
        assert_eq!(
            monospace_fault_summary(&MonospaceFault::UnexpectedReply("OK 1".into())),
            "unexpected reply \"OK 1\""
        );
        assert_eq!(
            monospace_fault_summary(&MonospaceFault::AmbiguousUrgentWrite),
            "urgent writing poisoned response correlation"
        );
    }

    #[test]
    fn typed_monospace_events_map_to_setup_without_promoting_unknown_buttons() {
        assert!(monospace_setup_blocker(8, MonospaceEventKind::Pressure(1_013.0)).is_none());
        assert!(monospace_setup_blocker(8, MonospaceEventKind::ButtonPressed).is_none());
        assert!(
            monospace_setup_blocker(8, MonospaceEventKind::UnknownEvent("BUTTON UP".into()))
                .is_none()
        );
        assert_eq!(
            monospace_setup_blocker(8, MonospaceEventKind::Disconnected)
                .unwrap()
                .summary,
            "Monospace disconnected on connection epoch 8"
        );
        assert_eq!(
            monospace_setup_blocker(9, MonospaceEventKind::Fault(MonospaceFault::ReplyTimeout))
                .unwrap()
                .summary,
            "Monospace connection epoch 9 is blocked: reply timeout poisoned response correlation"
        );
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
