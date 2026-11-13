//! Native portrait touchscreen entry point for Sans.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use clap::Parser;
use sans::{window_request, SansApp, Startup};
#[cfg(target_os = "linux")]
use sans_core::V4lCameraMachineFactory;
use sans_core::{
    bootstrap, capture_lit_pair, CapturePair, CapturePairError, ControllerMachine, MachineBoard,
    MachineEvent, MachineFactory, MonospaceClient, MonospaceConnection, MonospaceEventKind,
    MonospaceFault, OpenedBoard, PreparedMachineProfile, SetupBlocker, SetupDiagnostic,
    StartupError, CAPTURE_LIGHT_SETTLE,
};
#[cfg(not(target_os = "linux"))]
use sans_core::{CameraCaptureError, CameraRole};

const MONOSPACE_BOOT_DELAY: Duration = Duration::from_millis(2_500);

#[derive(Debug, Parser)]
#[command(about = "Run the native Sans touchscreen application")]
struct Arguments {
    /// Use an explicit Machine profile instead of the normal user config path.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Log more to stderr: `-v` operator operations, Stop and link changes;
    /// `-vv` also every Monospace and ODrive command; `-vvv` also ODrive
    /// reads. Errors and warnings are always logged. `RUST_LOG` overrides.
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

/// The `env_logger` filter for a `-v` count: other crates stay at warnings.
fn log_filter(verbose: u8) -> String {
    let level = match verbose {
        0 => return "warn".into(),
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    format!("warn,sans={level},sans_core={level}")
}

/// Log to stderr. `RUST_LOG`, when set, replaces the `-v` filter.
fn init_logging(verbose: u8) {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(log_filter(verbose)),
    )
    .init();
}

/// One Monospace connection, shared by the controller's board and the
/// Machine's capture light. Each command holds the lock only for its own
/// reply, so the light never holds up `ALL OFF` for longer than one command.
type SharedMonospace = Arc<Mutex<MonospaceConnection>>;

fn lock(monospace: &SharedMonospace) -> MutexGuard<'_, MonospaceConnection> {
    monospace.lock().unwrap_or_else(PoisonError::into_inner)
}

struct BootstrapMachineFactory<CameraFactory> {
    cameras: CameraFactory,
    /// The opened Monospace, handed to the Machine for the capture light.
    monospace: Option<SharedMonospace>,
}

/// Monospace as the controller's relay board.
struct MonospaceBoard(SharedMonospace);

impl MachineBoard for MonospaceBoard {
    fn all_off(&mut self, within: Duration) -> Result<(), String> {
        lock(&self.0)
            .client
            .all_off_within(within)
            .map_err(|error| error.to_string())
    }

    fn poll(&mut self) -> Option<MachineEvent> {
        let monospace = lock(&self.0);
        while let Ok(event) = monospace.events.try_recv() {
            if let Some(event) = monospace_machine_event(event.epoch.get(), event.kind) {
                return Some(event);
            }
        }
        None
    }
}

/// The Cameras, lit by Monospace while they capture.
struct BootstrapMachine<CameraMachine> {
    cameras: CameraMachine,
    light: Option<SharedMonospace>,
}

impl<CameraMachine: ControllerMachine> ControllerMachine for BootstrapMachine<CameraMachine> {
    fn capture_pair(&mut self) -> Result<CapturePair, CapturePairError> {
        let Some(light) = &self.light else {
            return self.cameras.capture_pair();
        };
        let cameras = &mut self.cameras;
        capture_lit_pair(
            |on| lock(light).client.set_light(on),
            CAPTURE_LIGHT_SETTLE,
            || cameras.capture_pair(),
        )
    }
}

impl<CameraFactory: MachineFactory> MachineFactory for BootstrapMachineFactory<CameraFactory> {
    type Machine = BootstrapMachine<CameraFactory::Machine>;

    fn odrive_opener(&mut self) -> Box<dyn sans_core::FibreOpener> {
        self.cameras.odrive_opener()
    }

    fn open_board(&mut self, profile: &PreparedMachineProfile) -> Option<OpenedBoard> {
        let path = &profile.profile().boards.monospace_path;
        let open_timeout = Duration::from_millis(profile.profile().timeouts.device_open_ms);
        let reply_timeout = Duration::from_millis(profile.profile().timeouts.command_ms);
        let opened =
            MonospaceClient::connect(path, MONOSPACE_BOOT_DELAY, open_timeout, reply_timeout)
                .map(|monospace| {
                    let epoch = monospace.client.epoch().get();
                    let monospace = Arc::new(Mutex::new(monospace));
                    self.monospace = Some(Arc::clone(&monospace));
                    let board: Box<dyn MachineBoard> = Box::new(MonospaceBoard(monospace));
                    let diagnostic = SetupDiagnostic::ready(
                        "Monospace",
                        format!("Ready at {path} on connection epoch {epoch}"),
                    );
                    (board, diagnostic)
                })
                .map_err(|error| SetupBlocker::new(format!("Monospace at {path}: {error}")));
        Some(opened)
    }

    fn open(
        self,
        profile: &PreparedMachineProfile,
    ) -> Result<(Self::Machine, Vec<SetupDiagnostic>), Vec<SetupBlocker>> {
        let (cameras, diagnostics) = self.cameras.open(profile)?;
        let machine = BootstrapMachine {
            cameras,
            light: self.monospace,
        };
        Ok((machine, diagnostics))
    }

    fn poll_machine(machine: &mut Self::Machine) -> Option<MachineEvent> {
        CameraFactory::poll_machine(&mut machine.cameras)
    }
}

fn monospace_machine_event(epoch: u64, event: MonospaceEventKind) -> Option<MachineEvent> {
    match event {
        MonospaceEventKind::Pressure(_) => None,
        MonospaceEventKind::ButtonPressed => Some(MachineEvent::StopButton),
        MonospaceEventKind::UnknownEvent(payload) => {
            log::warn!("ignored unknown Monospace event on epoch {epoch}: {payload:?}");
            None
        }
        MonospaceEventKind::Disconnected => Some(MachineEvent::Lost(SetupBlocker::new(format!(
            "Monospace disconnected on connection epoch {epoch}"
        )))),
        MonospaceEventKind::Fault(fault) => Some(MachineEvent::Lost(SetupBlocker::new(format!(
            "Monospace connection epoch {epoch} is blocked: {}",
            monospace_fault_summary(&fault)
        )))),
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

    fn odrive_opener(&mut self) -> Box<dyn sans_core::FibreOpener> {
        Box::new(sans_core::UsbFibreOpener)
    }

    fn open(
        self,
        _profile: &PreparedMachineProfile,
    ) -> Result<(Self::Machine, Vec<SetupDiagnostic>), Vec<SetupBlocker>> {
        Err(vec![SetupBlocker::new(
            "Live Camera readiness requires Linux V4L2; diagnostics remain available.",
        )])
    }
}

fn main() -> eframe::Result {
    let arguments = Arguments::parse();
    init_logging(arguments.verbose);
    #[cfg(target_os = "linux")]
    let camera_factory = V4lCameraMachineFactory;
    #[cfg(not(target_os = "linux"))]
    let camera_factory = UnsupportedCameraFactory;
    let startup = bootstrap(
        arguments.config.as_deref(),
        BootstrapMachineFactory {
            cameras: camera_factory,
            monospace: None,
        },
    );
    match &startup {
        Err(error @ StartupError::TemplateCreated { .. }) => log::warn!("{error}"),
        Err(error) => log::error!("startup failed: {error}"),
        Ok(_) => {}
    }
    let startup = Startup::from_bootstrap(startup);
    let options = eframe::NativeOptions {
        viewport: window_request(),
        ..Default::default()
    };

    eframe::run_native(
        "Sans",
        options,
        Box::new(move |context| Ok(Box::new(SansApp::new(&context.egui_ctx, startup)))),
    )
}

#[cfg(test)]
mod tests {
    use sans_core::{MachineEvent, MonospaceEventKind, MonospaceFault, SetupBlocker};

    use super::{log_filter, monospace_fault_summary, monospace_machine_event};

    #[test]
    fn verbosity_raises_only_the_sans_crates() {
        assert_eq!(log_filter(0), "warn");
        assert_eq!(log_filter(1), "warn,sans=info,sans_core=info");
        assert_eq!(log_filter(2), "warn,sans=debug,sans_core=debug");
        assert_eq!(log_filter(5), "warn,sans=trace,sans_core=trace");
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
    fn typed_monospace_events_map_the_button_to_stop_and_failures_to_lost() {
        let lost = |summary: &str| Some(MachineEvent::Lost(SetupBlocker::new(summary)));
        assert_eq!(
            monospace_machine_event(8, MonospaceEventKind::Pressure(1_013.0)),
            None
        );
        assert_eq!(
            monospace_machine_event(8, MonospaceEventKind::ButtonPressed),
            Some(MachineEvent::StopButton)
        );
        assert_eq!(
            monospace_machine_event(8, MonospaceEventKind::UnknownEvent("BUTTON UP".into())),
            None
        );
        assert_eq!(
            monospace_machine_event(8, MonospaceEventKind::Disconnected),
            lost("Monospace disconnected on connection epoch 8")
        );
        assert_eq!(
            monospace_machine_event(9, MonospaceEventKind::Fault(MonospaceFault::ReplyTimeout)),
            lost(
                "Monospace connection epoch 9 is blocked: reply timeout poisoned response correlation"
            )
        );
    }
}
