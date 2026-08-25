//! Pure conformance tests for Ligature's implemented production response frames.

use sans_core::{
    parse_ligature_line, LigatureLine, LigaturePosition, LigatureState, PositionTrust,
};

#[test]
fn parses_every_public_state_separately_from_position_trust() {
    let cases = [
        ("COMMISSIONING_ONLY", LigatureState::CommissioningOnly),
        ("IDLE", LigatureState::Idle),
        ("ARMED", LigatureState::Armed),
        ("OVERRIDE_PENDING", LigatureState::OverridePending),
        ("READY", LigatureState::Ready),
        ("ALIGNING", LigatureState::Aligning),
        ("HOMING", LigatureState::Homing),
        ("CALIBRATING", LigatureState::Calibrating),
        ("MOVING", LigatureState::Moving),
        ("TOUCHING_DOWN", LigatureState::TouchingDown),
        ("HOLDING", LigatureState::Holding),
        ("FAULT", LigatureState::Fault),
    ];

    for (wire_state, expected) in cases {
        let line = format!(
            "state STATE:{wire_state} TRUST:0 Z:? VEL:0.000 IQ:0.000 PRESS:? \
             ENDSTOP:0 PWM:OFF ACTIVE:NONE FAULT:NONE RUNTIME_MODIFIED:0"
        );
        let LigatureLine::State(status) = parse_ligature_line(&line).unwrap() else {
            panic!("expected state frame for {wire_state}");
        };

        assert_eq!(status.state, expected);
        assert_eq!(status.position_trust, PositionTrust::Untrusted);
        assert_eq!(status.position, LigaturePosition::Unknown);
    }
}

#[test]
fn parses_status_acceptance_terminals_and_hard_faults() {
    let LigatureLine::Status(status) = parse_ligature_line(
        "status STATE:MOVING TRUST:1 Z:-12.500 VEL:-3.250 IQ:0.400 \
         PRESS:? ACTIVE:G1 FAULT:NONE",
    )
    .unwrap() else {
        panic!("expected status frame");
    };
    assert_eq!(status.state, LigatureState::Moving);
    assert_eq!(status.position_trust, PositionTrust::Trusted);
    assert_eq!(status.position, LigaturePosition::Known(-12.5));
    assert_eq!(
        status.active.as_ref().map(|token| token.as_str()),
        Some("G1")
    );

    let LigatureLine::Accepted { command } = parse_ligature_line("ok G28").unwrap() else {
        panic!("expected acceptance");
    };
    assert_eq!(command.as_str(), "G28");

    let LigatureLine::Done(done) =
        parse_ligature_line("done M53 CANCELLED:G1 Z:KNOWN STATE:READY TRUST:1").unwrap()
    else {
        panic!("expected done terminal");
    };
    assert_eq!(done.command.as_str(), "M53");
    assert_eq!(done.field("CANCELLED"), Some("G1"));

    let LigatureLine::Error(error) = parse_ligature_line("error G28 REASON:HOMING_FAILED").unwrap()
    else {
        panic!("expected error terminal");
    };
    assert_eq!(error.command.as_str(), "G28");
    assert_eq!(error.reason, "HOMING_FAILED");

    let LigatureLine::Fault(fault) =
        parse_ligature_line("fault ENDSTOP_UNEXPECTED CANCELLED:G1 STATE:FAULT TRUST:0 Z:?")
            .unwrap()
    else {
        panic!("expected hard fault");
    };
    assert_eq!(fault.reason, "ENDSTOP_UNEXPECTED");
    assert_eq!(
        fault.cancelled.as_ref().map(|token| token.as_str()),
        Some("G1")
    );
    assert_eq!(fault.position_trust, PositionTrust::Untrusted);
}

#[test]
fn rejects_incomplete_or_unknown_frames() {
    assert!(parse_ligature_line("state STATE:READY TRUST:1").is_err());
    assert!(parse_ligature_line(
        "status STATE:PRIVATE TRUST:1 Z:0 VEL:0 IQ:0 PRESS:? ACTIVE:NONE FAULT:NONE"
    )
    .is_err());
    assert!(parse_ligature_line("done").is_err());
    assert!(parse_ligature_line("ok G28 EXTRA").is_err());
    assert!(parse_ligature_line(
        "fault ENDSTOP_UNEXPECTED CANCELLED:G1 STATE:FAULT TRUST:0 Z:KNOWN"
    )
    .is_err());
    assert!(parse_ligature_line("wat G28").is_err());
}

#[test]
fn parses_capture_diagnostics_without_treating_them_as_terminals() {
    let LigatureLine::Capture(sample) = parse_ligature_line(
        "capture I:2 T_MS:910 STATE_ID:10 Z:-10.000 ANGLE:-12.566371 \
         VEL_RAD_S:-1.000 TARGET:-15.000000 IQ:0.400 ENDSTOP:0",
    )
    .unwrap() else {
        panic!("expected capture sample");
    };

    assert_eq!(sample.index, 2);
    assert_eq!(sample.timestamp_ms, 910);
    assert_eq!(sample.position_mm, -10.0);
    assert!(!sample.endstop_active);
}
