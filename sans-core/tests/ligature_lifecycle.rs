//! Pure tests for Ligature operation correlation, cancellation, and connection epochs.

use sans_core::{
    ConnectionEpoch, LigatureCommand, LigatureEvent, LigatureSession, LigatureSessionError,
    OperationId, RequestPriority,
};

const READY: &str = "state STATE:READY TRUST:1 Z:-2.000 VEL:0.000 IQ:0.000 \
                    PRESS:? ENDSTOP:0 PWM:ACTIVE ACTIVE:NONE FAULT:NONE \
                    RUNTIME_MODIFIED:0";
const FAULT: &str = "state STATE:FAULT TRUST:0 Z:? VEL:0.000 IQ:0.000 \
                    PRESS:? ENDSTOP:0 PWM:OFF ACTIVE:NONE FAULT:CURRENT_LIMIT \
                    RUNTIME_MODIFIED:0";
const IDLE: &str = "state STATE:IDLE TRUST:0 Z:? VEL:0.000 IQ:0.000 \
                   PRESS:? ENDSTOP:0 PWM:OFF ACTIVE:NONE FAULT:NONE \
                   RUNTIME_MODIFIED:0";
const UNCOMMISSIONED: &str = "state STATE:COMMISSIONING_ONLY TRUST:0 Z:? VEL:0.000 \
                             IQ:0.000 PRESS:? ENDSTOP:0 PWM:OFF ACTIVE:NONE \
                             FAULT:NONE RUNTIME_MODIFIED:0";
const HOMING: &str = "state STATE:HOMING TRUST:0 Z:? VEL:-1.000 IQ:0.400 \
                     PRESS:? ENDSTOP:0 PWM:ACTIVE ACTIVE:G28 FAULT:NONE \
                     RUNTIME_MODIFIED:0";
const ALIGNING: &str = "state STATE:ALIGNING TRUST:0 Z:? VEL:0.000 IQ:0.400 \
                       PRESS:? ENDSTOP:0 PWM:ACTIVE ACTIVE:M40 FAULT:NONE \
                       RUNTIME_MODIFIED:0";

#[test]
fn exclusive_operation_routes_status_between_acceptance_and_one_terminal() {
    let mut session = LigatureSession::from_query(READY).unwrap();
    let request = session.begin(LigatureCommand::Home).unwrap();
    assert_eq!(request.operation_id, OperationId(1));
    assert_eq!(request.epoch, ConnectionEpoch(1));
    assert_eq!(request.line, "G28");
    assert_eq!(request.priority, RequestPriority::Ordinary);

    let status = session
        .receive(
            ConnectionEpoch(1),
            "status STATE:HOMING TRUST:0 Z:? VEL:-5.000 IQ:0.500 PRESS:? ACTIVE:G28 FAULT:NONE",
        )
        .unwrap();
    assert!(matches!(status, LigatureEvent::Status(_)));
    assert_eq!(
        session.receive(ConnectionEpoch(1), "ok G28").unwrap(),
        LigatureEvent::Accepted(OperationId(1))
    );
    assert!(matches!(
        session
            .receive(ConnectionEpoch(1), "done G28 Z:-2.000 STATE:READY TRUST:1")
            .unwrap(),
        LigatureEvent::Completed {
            operation_id: OperationId(1),
            ..
        }
    ));

    assert!(matches!(
        session.receive(ConnectionEpoch(1), "done G28 Z:-2.000 STATE:READY TRUST:1"),
        Err(LigatureSessionError::UnmatchedTerminal { .. })
    ));
}

#[test]
fn urgent_cancel_retires_active_operation_without_host_queueing() {
    let mut session = LigatureSession::from_query(READY).unwrap();
    session.begin(LigatureCommand::Home).unwrap();
    assert_eq!(
        session.begin(LigatureCommand::Home),
        Err(LigatureSessionError::Busy)
    );

    let cancel = session.begin(LigatureCommand::Cancel).unwrap();
    assert_eq!(cancel.operation_id, OperationId(2));
    assert_eq!(cancel.line, "M53");
    assert_eq!(cancel.priority, RequestPriority::Urgent);
    assert!(matches!(
        session.receive(
            ConnectionEpoch(1),
            "done M53 CANCELLED:G28 Z:garbage STATE:PRIVATE TRUST:9"
        ),
        Err(LigatureSessionError::Protocol(
            sans_core::LigatureProtocolError::InvalidField { .. }
        ))
    ));
    assert!(matches!(
        session
            .receive(
                ConnectionEpoch(1),
                "done M53 CANCELLED:G28 Z:KNOWN STATE:READY TRUST:1"
            )
            .unwrap(),
        LigatureEvent::Cancelled {
            operation_id: OperationId(1),
            by: OperationId(2),
            ..
        }
    ));

    assert_eq!(
        session.begin(LigatureCommand::Home).unwrap().operation_id,
        OperationId(3)
    );
}

#[test]
fn hard_fault_and_reconnect_retire_work_without_reusing_old_terminals() {
    let mut session = LigatureSession::from_query(READY).unwrap();
    session.begin(LigatureCommand::Home).unwrap();
    assert!(matches!(
        session
            .receive(
                ConnectionEpoch(1),
                "fault ENDSTOP_UNEXPECTED CANCELLED:G28 STATE:FAULT TRUST:0 Z:?"
            )
            .unwrap(),
        LigatureEvent::HardFault {
            ref retired,
            ..
        } if retired == &vec![OperationId(1)]
    ));

    session.begin(LigatureCommand::Stop).unwrap();
    let reconnect = session.reconnect(READY).unwrap();
    assert_eq!(reconnect.epoch, ConnectionEpoch(2));
    assert_eq!(reconnect.retired, vec![OperationId(2)]);
    assert_eq!(
        session
            .receive(
                ConnectionEpoch(1),
                "done M112 CANCELLED:NONE Z:? STATE:FAULT TRUST:0"
            )
            .unwrap(),
        LigatureEvent::StaleIgnored(ConnectionEpoch(1))
    );

    let current = session.begin(LigatureCommand::Home).unwrap();
    assert_eq!(current.epoch, ConnectionEpoch(2));
    assert!(matches!(
        session.receive(ConnectionEpoch(2), "done G0 Z:-2.000 STATE:READY TRUST:1"),
        Err(LigatureSessionError::ContradictoryTerminal { .. })
    ));
}

#[test]
fn late_urgent_terminal_after_hard_fault_is_ignored_once() {
    let mut session = LigatureSession::from_query(READY).unwrap();
    session.begin(LigatureCommand::Home).unwrap();
    let stop = session.begin(LigatureCommand::Stop).unwrap();

    assert!(matches!(
        session
            .receive(
                ConnectionEpoch(1),
                "fault ENDSTOP_UNEXPECTED CANCELLED:G28 STATE:FAULT TRUST:0 Z:?"
            )
            .unwrap(),
        LigatureEvent::HardFault {
            ref retired,
            ..
        } if retired == &vec![OperationId(1), stop.operation_id]
    ));
    assert_eq!(
        session.begin(LigatureCommand::Stop),
        Err(LigatureSessionError::Busy)
    );
    assert_eq!(
        session
            .receive(
                ConnectionEpoch(1),
                "done M112 CANCELLED:NONE Z:? STATE:FAULT TRUST:0"
            )
            .unwrap(),
        LigatureEvent::RetiredIgnored(stop.operation_id)
    );
    assert!(matches!(
        session.receive(
            ConnectionEpoch(1),
            "done M112 CANCELLED:NONE Z:? STATE:FAULT TRUST:0"
        ),
        Err(LigatureSessionError::UnmatchedTerminal { .. })
    ));
    assert!(session.begin(LigatureCommand::Stop).is_ok());
}

#[test]
fn orphan_stop_accepts_none_when_the_queried_operation_finished_first() {
    let mut session = LigatureSession::from_query(HOMING).unwrap();
    let stop = session.begin(LigatureCommand::Stop).unwrap();

    assert!(matches!(
        session
            .receive(
                ConnectionEpoch(1),
                "done M112 CANCELLED:NONE Z:? STATE:FAULT TRUST:0"
            )
            .unwrap(),
        LigatureEvent::Completed { operation_id, .. } if operation_id == stop.operation_id
    ));
}

#[test]
fn hard_fault_with_none_retires_an_immediate_command() {
    let mut session = LigatureSession::from_query(READY).unwrap();
    let arm = session.begin(LigatureCommand::Arm).unwrap();

    assert!(matches!(
        session
            .receive(
                ConnectionEpoch(1),
                "fault CURRENT_LIMIT CANCELLED:NONE STATE:FAULT TRUST:0 Z:?"
            )
            .unwrap(),
        LigatureEvent::HardFault { ref retired, .. } if retired == &vec![arm.operation_id]
    ));
}

#[test]
fn stop_is_not_blocked_by_an_outstanding_routine_cancel() {
    let mut session = LigatureSession::from_query(READY).unwrap();
    session.begin(LigatureCommand::Home).unwrap();
    let cancel = session.begin(LigatureCommand::Cancel).unwrap();
    let stop = session.begin(LigatureCommand::Stop).unwrap();

    assert_eq!(cancel.priority, RequestPriority::Urgent);
    assert_eq!(stop.priority, RequestPriority::Urgent);
    assert!(matches!(
        session.receive(
            ConnectionEpoch(1),
            "done M112 CANCELLED:G28 Z:KNOWN STATE:READY TRUST:1"
        ),
        Err(LigatureSessionError::Protocol(_))
    ));
    assert!(matches!(
        session
            .receive(
                ConnectionEpoch(1),
                "done M53 CANCELLED:G28 Z:KNOWN STATE:READY TRUST:1"
            )
            .unwrap(),
        LigatureEvent::Cancelled { .. }
    ));
    assert!(matches!(
        session
            .receive(
                ConnectionEpoch(1),
                "done M112 CANCELLED:NONE Z:KNOWN STATE:FAULT TRUST:1"
            )
            .unwrap(),
        LigatureEvent::Completed { operation_id, .. } if operation_id == stop.operation_id
    ));
}

#[test]
fn commissioning_only_is_connected_but_rejects_scan_motion() {
    let mut session = LigatureSession::from_query(UNCOMMISSIONED).unwrap();

    assert!(session.is_connected());
    assert!(!session.scan_enabled());
    assert_eq!(
        session.begin(LigatureCommand::Home),
        Err(LigatureSessionError::CommissioningOnly)
    );
}

#[test]
fn fault_state_does_not_claim_that_the_board_is_commissioned() {
    let mut session = LigatureSession::from_query(FAULT).unwrap();

    assert!(session.is_connected());
    assert!(!session.scan_enabled());
    assert_eq!(
        session.begin(LigatureCommand::Home),
        Err(LigatureSessionError::CommissioningOnly)
    );
}

#[test]
fn ambiguous_alignment_state_does_not_establish_commissioning() {
    let mut session = LigatureSession::from_query(ALIGNING).unwrap();

    assert!(!session.scan_enabled());
    assert_eq!(
        session.begin(LigatureCommand::Home),
        Err(LigatureSessionError::CommissioningOnly)
    );
}

#[test]
fn reconnect_in_fault_state_remains_fail_closed() {
    let mut session = LigatureSession::from_query(READY).unwrap();

    session.reconnect(FAULT).unwrap();

    assert!(!session.scan_enabled());
    session.receive(ConnectionEpoch(2), IDLE).unwrap();
    assert!(session.scan_enabled());
}

#[test]
fn uncommissioned_marker_survives_fault_clear_to_idle() {
    let mut session = LigatureSession::from_query(UNCOMMISSIONED).unwrap();

    session.reconnect(FAULT).unwrap();
    let clear = session.begin(LigatureCommand::ClearFault).unwrap();
    assert!(matches!(
        session
            .receive(ConnectionEpoch(2), "done M999 STATE:IDLE TRUST:0")
            .unwrap(),
        LigatureEvent::Completed { operation_id, .. } if operation_id == clear.operation_id
    ));
    session.receive(ConnectionEpoch(2), IDLE).unwrap();

    assert!(!session.scan_enabled());
    assert_eq!(
        session.begin(LigatureCommand::Home),
        Err(LigatureSessionError::CommissioningOnly)
    );
}

#[test]
fn clear_fault_must_leave_the_fault_state() {
    let mut session = LigatureSession::from_query(READY).unwrap();
    session
        .receive(
            ConnectionEpoch(1),
            "fault CURRENT_LIMIT CANCELLED:NONE STATE:FAULT TRUST:0 Z:?",
        )
        .unwrap();
    let clear = session.begin(LigatureCommand::ClearFault).unwrap();

    assert!(matches!(
        session.receive(ConnectionEpoch(1), "done M999 STATE:FAULT TRUST:0"),
        Err(LigatureSessionError::Protocol(_))
    ));
    assert_eq!(clear.operation_id, OperationId(1));
}

#[test]
fn capture_readiness_requires_a_stationary_lifecycle() {
    let mut session = LigatureSession::from_query(READY).unwrap();
    assert!(session.capture_ready());

    session.begin(LigatureCommand::Home).unwrap();
    assert!(!session.capture_ready());

    let moving = LigatureSession::from_query(HOMING).unwrap();
    assert!(!moving.capture_ready());
}

#[test]
fn capture_traffic_remains_routable_during_an_operation() {
    let mut session = LigatureSession::from_query(READY).unwrap();
    session.begin(LigatureCommand::Home).unwrap();

    assert!(matches!(
        session
            .receive(
                ConnectionEpoch(1),
                "capture I:0 T_MS:20 STATE_ID:6 Z:-3.000 ANGLE:-3.769911 \
                 VEL_RAD_S:-1.000 TARGET:-5.000000 IQ:0.300 ENDSTOP:0"
            )
            .unwrap(),
        LigatureEvent::Capture(_)
    ));
}

#[test]
fn rejected_cancel_does_not_complete_the_active_operation() {
    let mut session = LigatureSession::from_query(READY).unwrap();
    session.begin(LigatureCommand::Home).unwrap();
    session.receive(ConnectionEpoch(1), "ok G28").unwrap();
    session.begin(LigatureCommand::Cancel).unwrap();

    assert!(matches!(
        session
            .receive(ConnectionEpoch(1), "error M53 REASON:FAULTED")
            .unwrap(),
        LigatureEvent::Failed {
            operation_id: OperationId(2),
            ..
        }
    ));
    assert!(matches!(
        session
            .receive(ConnectionEpoch(1), "done G28 Z:-2.000 STATE:READY TRUST:1")
            .unwrap(),
        LigatureEvent::Completed {
            operation_id: OperationId(1),
            ..
        }
    ));
}

#[test]
fn immediate_ordinary_command_serializes_without_an_acceptance_frame() {
    let mut session = LigatureSession::from_query(READY).unwrap();
    let arm = session.begin(LigatureCommand::Arm).unwrap();

    assert!(matches!(
        session
            .receive(ConnectionEpoch(1), "done M3 STATE:READY TRUST:1")
            .unwrap(),
        LigatureEvent::Completed { operation_id, .. } if operation_id == arm.operation_id
    ));
}

#[test]
fn terminal_missing_required_operation_fields_is_malformed() {
    let mut session = LigatureSession::from_query(READY).unwrap();
    session.begin(LigatureCommand::Home).unwrap();
    session.receive(ConnectionEpoch(1), "ok G28").unwrap();

    assert!(matches!(
        session.receive(ConnectionEpoch(1), "done G28"),
        Err(LigatureSessionError::Protocol(
            sans_core::LigatureProtocolError::MissingField("Z")
        ))
    ));

    for terminal in [
        "done G28 Z:garbage STATE:READY TRUST:1",
        "done G28 Z:-2.000 STATE:PRIVATE TRUST:1",
        "done G28 Z:-2.000 STATE:READY TRUST:9",
        "done G28 Z:? STATE:FAULT TRUST:1",
    ] {
        assert!(matches!(
            session.receive(ConnectionEpoch(1), terminal),
            Err(LigatureSessionError::Protocol(_))
        ));
    }
    assert!(matches!(
        session
            .receive(ConnectionEpoch(1), "done G28 Z:-2.000 STATE:READY TRUST:1")
            .unwrap(),
        LigatureEvent::Completed { .. }
    ));
}
