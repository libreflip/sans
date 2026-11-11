//! Compact lift-axis status for a screen body, and its reconnect action.
//!
//! The only action is a reconnect, which belongs in the action bar. Nothing here
//! arms, homes, moves or clears errors, and scanning stays disabled whatever it
//! shows.

use eframe::egui;
use sans_core::{
    AxisState, ControllerClosed, ControllerHandle, ControllerIntent, LiftAxisStatus, LinkPhase,
    ScanReadiness,
};

use crate::screen;

/// The reconnect action for a screen's action bar.
pub(crate) fn reconnect_action(
    ui: &mut egui::Ui,
    handle: &ControllerHandle,
) -> Result<(), ControllerClosed> {
    if screen::action_button(ui, "Reconnect lift axis").clicked() {
        handle.send(ControllerIntent::ReconnectLiftAxis)?;
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(16));
    }
    Ok(())
}

/// Read-only lift-axis status for a screen body.
pub(crate) fn show_lift_axis(
    ui: &mut egui::Ui,
    status: &LiftAxisStatus,
    scan_readiness: &ScanReadiness,
) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.strong("Lift axis");
            let (text, colour) = phase_label(status.phase);
            ui.colored_label(colour, text);
            ui.label(format!("epoch {}", status.epoch));
            if status.unmatched_replies > 0 {
                ui.label(format!("{} ignored replies", status.unmatched_replies));
            }
        });
        if status.phase != LinkPhase::Verified {
            for blocker in &status.blockers {
                ui.label(format!("• {blocker}"));
            }
        }
        if let Some(discovery) = &status.discovery {
            let text = |value: Option<String>| value.unwrap_or_else(|| "—".into());
            ui.label(format!(
                "USB {}  board serial {}  hw {}  fw {}",
                text(discovery.usb_serial.clone()),
                text(discovery.board_serial.map(|serial| format!("{serial:X}"))),
                text(
                    discovery
                        .hardware_version
                        .map(|[major, minor, variant]| format!("{major}.{minor}-{variant}"))
                ),
                text(discovery.firmware_version.map(
                    |[major, minor, revision, unreleased]| format!(
                        "{major}.{minor}.{revision}{}",
                        if unreleased != 0 { " (unreleased)" } else { "" }
                    )
                )),
            ));
            ui.label(format!(
                "{} endpoints  descriptor CRC {}  {} bytes",
                text(discovery.endpoint_count.map(|count| count.to_string())),
                text(discovery.descriptor_crc.map(|crc| crc.to_string())),
                text(discovery.descriptor_length.map(|length| length.to_string())),
            ));
        }
        if let Some(telemetry) = &status.telemetry {
            let values = &telemetry.values;
            let state = match values.axis_state {
                AxisState::Idle => "idle".to_string(),
                AxisState::ClosedLoopControl => "closed-loop".to_string(),
                AxisState::Homing => "homing".to_string(),
                AxisState::Unknown(raw) => format!("unknown ({raw})"),
            };
            let millimetres = |value: Option<f32>, unit: &str| {
                value.map_or_else(
                    || "uncommissioned".into(),
                    |value| format!("{value:.2} {unit}"),
                )
            };
            ui.label(format!(
                "{state}  position {}  velocity {}  Iq {:.2} A",
                millimetres(values.position_mm, "mm"),
                millimetres(values.velocity_mm_s, "mm/s"),
                values.iq_measured_a,
            ));
            ui.label(format!(
                "errors axis 0x{:X} motor 0x{:X} encoder 0x{:X} controller 0x{:X}  endstop {}",
                values.axis_error,
                values.motor_error,
                values.encoder_error,
                values.controller_error,
                if values.min_endstop_pressed {
                    "pressed"
                } else {
                    "clear"
                },
            ));
            let freshness = format!(
                "sample {} ms old, cycle {} ms{}",
                telemetry.age_ms,
                telemetry.cycle_ms,
                if telemetry.fresh { "" } else { " — stale" }
            );
            if telemetry.fresh {
                ui.label(freshness);
            } else {
                ui.colored_label(egui::Color32::YELLOW, freshness);
            }
        }
        let ScanReadiness::Disabled { reason } = scan_readiness;
        ui.small(format!("Scanning disabled: {reason}"));
    });
}

fn phase_label(phase: LinkPhase) -> (&'static str, egui::Color32) {
    match phase {
        LinkPhase::Disconnected => ("disconnected", egui::Color32::LIGHT_RED),
        LinkPhase::Connecting => ("connecting", egui::Color32::LIGHT_BLUE),
        LinkPhase::Blocked => ("blocked", egui::Color32::YELLOW),
        LinkPhase::Verified => ("verified link", egui::Color32::LIGHT_GREEN),
        LinkPhase::Faulted => ("link fault", egui::Color32::LIGHT_RED),
    }
}
