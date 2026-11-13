//! Native portrait touchscreen entry point for Sans.

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use sans::{window_request, SansApp, Startup};
#[cfg(target_os = "linux")]
use sans_core::V4lCameraMachineFactory;
use sans_core::{
    bootstrap, CapturePair, CapturePairError, ControllerMachine, MachineEvent, MachineFactory,
    MonospaceClient, MonospaceConnection, MonospaceEventKind, MonospaceFault,
    PreparedMachineProfile, SetupBlocker, SetupDiagnostic,
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

    fn all_off(&mut self, within: Duration) -> Result<(), String> {
        self.monospace
            .client
            .all_off_within(within)
            .map_err(|error| error.to_string())
    }
}

impl<CameraFactory: MachineFactory> MachineFactory for BootstrapMachineFactory<CameraFactory> {
    type Machine = BootstrapMachine<CameraFactory::Machine>;

    fn odrive_opener(&mut self) -> Box<dyn sans_core::FibreOpener> {
        self.cameras.odrive_opener()
    }

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

    fn poll_machine(machine: &mut Self::Machine) -> Option<MachineEvent> {
        if let Some(event) = CameraFactory::poll_machine(&mut machine.cameras) {
            return Some(event);
        }
        while let Ok(event) = machine.monospace.events.try_recv() {
            if let Some(event) = monospace_machine_event(event.epoch.get(), event.kind) {
                return Some(event);
            }
        }
        None
    }
}

fn monospace_machine_event(epoch: u64, event: MonospaceEventKind) -> Option<MachineEvent> {
    match event {
        MonospaceEventKind::Pressure(_) => None,
        MonospaceEventKind::ButtonPressed => Some(MachineEvent::StopButton),
        MonospaceEventKind::UnknownEvent(payload) => {
            eprintln!("ignored unknown Monospace event on epoch {epoch}: {payload:?}");
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

    use super::{monospace_fault_summary, monospace_machine_event};

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
