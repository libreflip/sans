//! Native portrait touchscreen entry point for Sans.

use std::path::PathBuf;

use clap::Parser;
use sans::{window_request, SansApp, Startup};
use sans_core::bootstrap;
#[cfg(target_os = "linux")]
use sans_core::V4lCameraMachineFactory;
#[cfg(not(target_os = "linux"))]
use sans_core::{
    CameraCaptureError, CameraRole, CapturePair, CapturePairError, ControllerMachine,
    MachineFactory, PreparedMachineProfile, SetupBlocker,
};

#[derive(Debug, Parser)]
#[command(about = "Run the native Sans touchscreen application")]
struct Arguments {
    /// Use an explicit Machine profile instead of the normal user config path.
    #[arg(long)]
    config: Option<PathBuf>,
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

    fn open(self, _profile: &PreparedMachineProfile) -> Result<Self::Machine, Vec<SetupBlocker>> {
        Err(vec![SetupBlocker::new(
            "Live Camera readiness requires Linux V4L2; diagnostics remain available.",
        )])
    }
}

fn main() -> eframe::Result {
    let arguments = Arguments::parse();
    #[cfg(target_os = "linux")]
    let startup = bootstrap(arguments.config.as_deref(), V4lCameraMachineFactory);
    #[cfg(not(target_os = "linux"))]
    let startup = bootstrap(arguments.config.as_deref(), UnsupportedCameraFactory);
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
