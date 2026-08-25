//! Supervised single-Camera capture diagnostic.

#[cfg(target_os = "linux")]
use std::error::Error;
#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use clap::{Parser, ValueEnum};
#[cfg(target_os = "linux")]
use image::codecs::jpeg::JpegEncoder;
#[cfg(target_os = "linux")]
use image::ExtendedColorType;
#[cfg(target_os = "linux")]
use sans_core::{capture_configured_camera, prepare_machine_profile, CameraRole};

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, ValueEnum)]
enum RoleArgument {
    Left,
    Right,
}

#[cfg(target_os = "linux")]
impl From<RoleArgument> for CameraRole {
    fn from(role: RoleArgument) -> Self {
        match role {
            RoleArgument::Left => Self::Left,
            RoleArgument::Right => Self::Right,
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Parser)]
#[command(about = "Capture one configured Camera role for supervised calibration")]
struct Arguments {
    /// Complete Machine profile containing the stable Camera identity and geometry.
    #[arg(long)]
    config: PathBuf,
    /// Stable Camera role to capture.
    #[arg(long, value_enum)]
    role: RoleArgument,
    /// Explicit JPEG destination for the diagnostic frame.
    #[arg(long)]
    output: PathBuf,
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn Error>> {
    let arguments = Arguments::parse();
    let profile = prepare_machine_profile(Some(&arguments.config))?;
    let frame = capture_configured_camera(&profile, arguments.role.into())?;
    let output = File::create(&arguments.output)?;
    JpegEncoder::new_with_quality(output, 95).encode(
        frame.rgb8(),
        frame.width,
        frame.height,
        ExtendedColorType::Rgb8,
    )?;
    println!(
        "Captured {} Camera frame {}x{} to {}",
        frame.role,
        frame.width,
        frame.height,
        arguments.output.display()
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("camcal requires Linux V4L2 camera support");
}
