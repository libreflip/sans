//! Current-connection operation correlation for Ligature.

use thiserror::Error;

use super::{
    parse_ligature_line, LigatureCaptureSample, LigatureCommandToken, LigatureFault, LigatureLine,
    LigatureProtocolError, LigatureState, LigatureStatus, PositionTrust, ProtocolErrorTerminal,
    ProtocolTerminal,
};

/// Identity of one physical Ligature connection.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ConnectionEpoch(pub u64);

/// Host identity for one command lifecycle.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct OperationId(pub u64);

/// Whether a request uses the ordinary serialized writer or urgent writer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestPriority {
    /// Serialized production command path.
    Ordinary,
    /// Priority path reserved for routine cancellation and Stop.
    Urgent,
}

/// Typed commands needed to cross and control Ligature's readiness gate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LigatureCommand {
    /// Arm the commissioned motion board without moving it.
    Arm,
    /// Disable motor PWM.
    Disarm,
    /// Run the exclusive homing operation.
    Home,
    /// Run the exclusive supervised alignment operation.
    Align,
    /// Release a completed Touchdown hold.
    ReleaseHold,
    /// Clear a latched firmware fault without arming.
    ClearFault,
    /// Stop routine motion with `M53` while preserving the production fault gate.
    Cancel,
    /// Issue the software Stop command `M112`.
    Stop,
}

impl LigatureCommand {
    fn wire(self) -> &'static str {
        match self {
            Self::Arm => "M3",
            Self::Disarm => "M5",
            Self::Home => "G28",
            Self::Align => "M40",
            Self::ReleaseHold => "M24",
            Self::ClearFault => "M999",
            Self::Cancel => "M53",
            Self::Stop => "M112",
        }
    }

    fn priority(self) -> RequestPriority {
        match self {
            Self::Cancel | Self::Stop => RequestPriority::Urgent,
            _ => RequestPriority::Ordinary,
        }
    }

    fn requires_commissioning(self) -> bool {
        !matches!(
            self,
            Self::Align | Self::ClearFault | Self::Cancel | Self::Stop | Self::Disarm
        )
    }

    fn expects_acceptance(self) -> bool {
        matches!(self, Self::Home | Self::Align)
    }

    fn token(self) -> LigatureCommandToken {
        LigatureCommandToken::from_static(self.wire())
    }

    fn validate_done(self, terminal: &ProtocolTerminal) -> Result<(), LigatureProtocolError> {
        let required = match self {
            Self::Arm | Self::Disarm | Self::ClearFault => &["STATE", "TRUST"][..],
            Self::Home | Self::ReleaseHold => &["Z", "STATE", "TRUST"][..],
            Self::Align => &[
                "ZERO_ELECTRICAL",
                "SENSOR_DIRECTION",
                "VOLATILE",
                "STATE",
                "TRUST",
            ][..],
            Self::Cancel | Self::Stop => &["CANCELLED", "Z", "STATE", "TRUST"][..],
        };
        terminal.require_valid_fields(required)?;
        let state = terminal.state()?;
        let trust = terminal.position_trust()?;
        let valid_projection = match self {
            Self::Arm => matches!(
                (state, trust),
                (LigatureState::Armed, PositionTrust::Untrusted)
                    | (LigatureState::Ready, PositionTrust::Trusted)
            ),
            Self::Disarm => state == LigatureState::Idle,
            Self::Home | Self::ReleaseHold => {
                state == LigatureState::Ready && trust == PositionTrust::Trusted
            }
            Self::Align => {
                matches!(
                    state,
                    LigatureState::CommissioningOnly | LigatureState::Idle
                ) && trust == PositionTrust::Untrusted
            }
            Self::ClearFault => {
                state != LigatureState::Fault && state_and_trust_are_consistent(state, trust)
            }
            Self::Cancel => matches!(
                (state, trust),
                (LigatureState::CommissioningOnly, PositionTrust::Untrusted)
                    | (LigatureState::Idle, PositionTrust::Untrusted)
                    | (LigatureState::Armed, PositionTrust::Untrusted)
                    | (LigatureState::Ready, PositionTrust::Trusted)
            ),
            Self::Stop => state == LigatureState::Fault,
        };
        if !valid_projection {
            return Err(LigatureProtocolError::InvalidField {
                field: "terminal state",
                value: format!("STATE:{state:?} TRUST:{trust:?}"),
            });
        }

        if matches!(self, Self::Home | Self::ReleaseHold)
            && matches!(terminal.field("Z"), Some("KNOWN" | "?"))
        {
            return Err(LigatureProtocolError::InvalidField {
                field: "Z",
                value: terminal.field("Z").unwrap_or_default().into(),
            });
        }
        if matches!(self, Self::Cancel | Self::Stop) {
            let expected = match trust {
                PositionTrust::Trusted => "KNOWN",
                PositionTrust::Untrusted => "?",
            };
            if terminal.field("Z") != Some(expected) {
                return Err(LigatureProtocolError::InvalidField {
                    field: "Z",
                    value: terminal.field("Z").unwrap_or_default().into(),
                });
            }
        }
        Ok(())
    }
}

fn state_and_trust_are_consistent(state: LigatureState, trust: PositionTrust) -> bool {
    match state {
        LigatureState::Ready
        | LigatureState::Moving
        | LigatureState::TouchingDown
        | LigatureState::Holding => trust == PositionTrust::Trusted,
        LigatureState::CommissioningOnly
        | LigatureState::Armed
        | LigatureState::OverridePending
        | LigatureState::Aligning
        | LigatureState::Homing
        | LigatureState::Calibrating => trust == PositionTrust::Untrusted,
        LigatureState::Idle | LigatureState::Fault => true,
    }
}

/// A correlated command ready for the selected serial writer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LigatureRequest {
    /// Host-local identity for this lifecycle.
    pub operation_id: OperationId,
    /// Physical connection that owns this lifecycle.
    pub epoch: ConnectionEpoch,
    /// Typed identity used to correlate response frames.
    pub command: LigatureCommandToken,
    /// Validated firmware command without the line terminator.
    pub line: String,
    /// Writer path selected for the command.
    pub priority: RequestPriority,
    command_type: LigatureCommand,
}

#[derive(Clone, Debug)]
struct PendingRequest {
    request: LigatureRequest,
    expects_acceptance: bool,
    accepted: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalOwner {
    Active,
    Urgent(usize),
    RetiredUrgent(usize),
}

/// A reconnect result, including all work retired with the old link.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LigatureReconnect {
    /// Fresh connection identity installed by the query.
    pub epoch: ConnectionEpoch,
    /// Requests that can no longer complete after the connection changed.
    pub retired: Vec<OperationId>,
}

/// Routed lifecycle or unsolicited traffic from Ligature.
#[derive(Clone, Debug, PartialEq)]
pub enum LigatureEvent {
    /// Current public state or heartbeat data.
    Status(LigatureStatus),
    /// Immediate `ok` for an asynchronous exclusive operation.
    Accepted(OperationId),
    /// Successful terminal for an ordinary or urgent request.
    Completed {
        /// Request completed by this terminal.
        operation_id: OperationId,
        /// Parsed firmware terminal and its diagnostic fields.
        terminal: ProtocolTerminal,
    },
    /// Error terminal for an ordinary or urgent request.
    Failed {
        /// Request completed by this error.
        operation_id: OperationId,
        /// Parsed firmware error and its reason.
        terminal: ProtocolErrorTerminal,
    },
    /// Urgent command that retired the active ordinary operation.
    Cancelled {
        /// Ordinary operation retired by the urgent result.
        operation_id: OperationId,
        /// Urgent request that performed the cancellation.
        by: OperationId,
        /// Parsed urgent-command terminal.
        terminal: ProtocolTerminal,
    },
    /// Unsolicited firmware fault and all work it retired.
    HardFault {
        /// Parsed public hard-fault data.
        fault: LigatureFault,
        /// Current-epoch requests retired by the fault.
        retired: Vec<OperationId>,
    },
    /// Control-rate diagnostic sample routed around command correlation.
    Capture(LigatureCaptureSample),
    /// Response from an older physical connection, ignored by design.
    StaleIgnored(ConnectionEpoch),
    /// Late terminal for an urgent request already retired by a current-epoch fault.
    RetiredIgnored(OperationId),
}

/// A lifecycle violation that poisons the current transport correlation.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LigatureSessionError {
    /// The wire frame itself was malformed or unsupported.
    #[error(transparent)]
    Protocol(#[from] LigatureProtocolError),
    /// A second request attempted to use an occupied command lifecycle.
    #[error("Ligature already has an outstanding ordinary operation")]
    Busy,
    /// Production motion was requested from an uncommissioned board.
    #[error("Ligature is connected for diagnostics but is not commissioned")]
    CommissioningOnly,
    #[error(
        "Ligature response belongs to future epoch {received:?}, current epoch is {current:?}"
    )]
    /// A reader supplied a response tagged with an epoch not opened yet.
    FutureEpoch {
        /// Epoch attached to the response.
        received: ConnectionEpoch,
        /// Session epoch currently accepting responses.
        current: ConnectionEpoch,
    },
    /// No pending request owns an acceptance frame.
    #[error("unmatched Ligature acceptance for {command}")]
    UnmatchedAcceptance {
        /// Firmware command token in the unmatched frame.
        command: String,
    },
    /// No pending request owns a terminal frame.
    #[error("unmatched Ligature terminal for {command}")]
    UnmatchedTerminal {
        /// Firmware command token in the unmatched frame.
        command: String,
    },
    /// A frame conflicts with the pending command or its expected phase.
    #[error("contradictory Ligature terminal: expected {expected}, received {received}")]
    ContradictoryTerminal {
        /// Lifecycle evidence required at this point.
        expected: String,
        /// Conflicting evidence supplied by the firmware.
        received: String,
    },
    /// Opening or reconnect did not yield the required `state` frame.
    #[error("Ligature query did not return a state frame")]
    QueryDidNotReturnState,
}

/// One connection epoch and its active operation lifecycles.
pub struct LigatureSession {
    epoch: ConnectionEpoch,
    next_operation_id: u64,
    status: LigatureStatus,
    commissioned: Option<bool>,
    active: Option<PendingRequest>,
    orphaned_active: Option<LigatureCommandToken>,
    urgent: Vec<PendingRequest>,
    retired_urgent: Vec<PendingRequest>,
}

impl LigatureSession {
    /// Start a session from the required parseable `?` response.
    pub fn from_query(line: &str) -> Result<Self, LigatureSessionError> {
        Self::from_query_with_commissioning(line, None)
    }

    pub(super) fn from_query_with_commissioning(
        line: &str,
        commissioned_configuration: Option<bool>,
    ) -> Result<Self, LigatureSessionError> {
        let LigatureLine::State(status) = parse_ligature_line(line)? else {
            return Err(LigatureSessionError::QueryDidNotReturnState);
        };
        let commissioned = commissioned_configuration
            .map(Some)
            .unwrap_or_else(|| commissioning_from_state(status.state));
        let orphaned_active = status.active.clone();
        Ok(Self {
            epoch: ConnectionEpoch(1),
            next_operation_id: 1,
            status,
            commissioned,
            active: None,
            orphaned_active,
            urgent: Vec::new(),
            retired_urgent: Vec::new(),
        })
    }

    /// A successfully queried session is connected even when uncommissioned.
    pub fn is_connected(&self) -> bool {
        true
    }

    /// Whether production motion may pass the commissioning gate.
    pub fn scan_enabled(&self) -> bool {
        self.commissioned == Some(true)
            && !matches!(
                self.status.state,
                LigatureState::CommissioningOnly | LigatureState::Fault
            )
    }

    /// Whether the current lifecycle permits stationary Camera acquisition.
    pub fn capture_ready(&self) -> bool {
        self.scan_enabled()
            && self.active.is_none()
            && self.orphaned_active.is_none()
            && self.urgent.is_empty()
            && matches!(
                self.status.state,
                LigatureState::Idle
                    | LigatureState::Armed
                    | LigatureState::OverridePending
                    | LigatureState::Ready
                    | LigatureState::Holding
            )
    }

    /// Current public status from the query or latest heartbeat.
    pub fn status(&self) -> &LigatureStatus {
        &self.status
    }

    /// Begin one command without adding a host-side queue.
    pub fn begin(
        &mut self,
        command: LigatureCommand,
    ) -> Result<LigatureRequest, LigatureSessionError> {
        if command.requires_commissioning() && !self.scan_enabled() {
            return Err(LigatureSessionError::CommissioningOnly);
        }
        let priority = command.priority();
        match priority {
            RequestPriority::Ordinary
                if self.active.is_some() || self.orphaned_active.is_some() =>
            {
                return Err(LigatureSessionError::Busy)
            }
            RequestPriority::Urgent
                if self
                    .urgent
                    .iter()
                    .chain(self.retired_urgent.iter())
                    .any(|pending| pending.request.command == command.token()) =>
            {
                return Err(LigatureSessionError::Busy)
            }
            _ => {}
        }
        let request = LigatureRequest {
            operation_id: OperationId(self.next_operation_id),
            epoch: self.epoch,
            command: command.token(),
            line: command.wire().into(),
            priority,
            command_type: command,
        };
        self.next_operation_id += 1;
        let pending = PendingRequest {
            request: request.clone(),
            expects_acceptance: command.expects_acceptance(),
            accepted: false,
        };
        match priority {
            RequestPriority::Ordinary => {
                self.active = Some(pending);
            }
            RequestPriority::Urgent => {
                self.urgent.push(pending);
            }
        }
        Ok(request)
    }

    /// Retire the old epoch and install the new connection's query response.
    pub fn reconnect(&mut self, query: &str) -> Result<LigatureReconnect, LigatureSessionError> {
        self.reconnect_with_commissioning(query, None)
    }

    pub(super) fn reconnect_with_commissioning(
        &mut self,
        query: &str,
        commissioned_configuration: Option<bool>,
    ) -> Result<LigatureReconnect, LigatureSessionError> {
        let LigatureLine::State(status) = parse_ligature_line(query)? else {
            return Err(LigatureSessionError::QueryDidNotReturnState);
        };
        let mut retired = Vec::new();
        if let Some(active) = self.active.take() {
            retired.push(active.request.operation_id);
        }
        retired.extend(
            self.urgent
                .drain(..)
                .map(|urgent| urgent.request.operation_id),
        );
        self.retired_urgent.clear();
        self.epoch = ConnectionEpoch(self.epoch.0 + 1);
        self.update_commissioning(status.state, commissioned_configuration);
        self.orphaned_active = status.active.clone();
        self.status = status;
        Ok(LigatureReconnect {
            epoch: self.epoch,
            retired,
        })
    }

    pub(super) fn has_orphaned_active(&self) -> bool {
        self.orphaned_active.is_some()
    }

    /// Route a line tagged by the reader's connection epoch.
    pub fn receive(
        &mut self,
        epoch: ConnectionEpoch,
        line: &str,
    ) -> Result<LigatureEvent, LigatureSessionError> {
        if epoch < self.epoch {
            return Ok(LigatureEvent::StaleIgnored(epoch));
        }
        if epoch > self.epoch {
            return Err(LigatureSessionError::FutureEpoch {
                received: epoch,
                current: self.epoch,
            });
        }
        match parse_ligature_line(line)? {
            LigatureLine::State(status) | LigatureLine::Status(status) => {
                self.update_commissioning(status.state, None);
                self.status = status.clone();
                Ok(LigatureEvent::Status(status))
            }
            LigatureLine::Boot(_) => Err(LigatureProtocolError::InvalidField {
                field: "frame",
                value: "boot after readiness query".into(),
            }
            .into()),
            LigatureLine::Accepted { command } => self.accept(command),
            LigatureLine::Done(terminal) => self.complete_done(terminal),
            LigatureLine::Error(terminal) => self.complete_error(terminal),
            LigatureLine::Fault(fault) => self.hard_fault(fault),
            LigatureLine::Capture(sample) => Ok(LigatureEvent::Capture(sample)),
        }
    }

    fn accept(
        &mut self,
        command: LigatureCommandToken,
    ) -> Result<LigatureEvent, LigatureSessionError> {
        let pending = self.pending_for_command_mut(&command).ok_or_else(|| {
            LigatureSessionError::UnmatchedAcceptance {
                command: command.to_string(),
            }
        })?;
        if !pending.expects_acceptance {
            return Err(LigatureSessionError::ContradictoryTerminal {
                expected: format!("immediate terminal for {command}"),
                received: format!("ok {command}"),
            });
        }
        if pending.accepted {
            return Err(LigatureSessionError::ContradictoryTerminal {
                expected: format!("one acceptance for {command}"),
                received: format!("second acceptance for {command}"),
            });
        }
        pending.accepted = true;
        Ok(LigatureEvent::Accepted(pending.request.operation_id))
    }

    fn complete_done(
        &mut self,
        terminal: ProtocolTerminal,
    ) -> Result<LigatureEvent, LigatureSessionError> {
        match self.terminal_owner(&terminal.command)? {
            TerminalOwner::Urgent(index) => self.complete_urgent(index, terminal),
            TerminalOwner::RetiredUrgent(index) => {
                self.retired_urgent[index]
                    .request
                    .command_type
                    .validate_done(&terminal)?;
                let operation_id = self.retired_urgent.remove(index).request.operation_id;
                Ok(LigatureEvent::RetiredIgnored(operation_id))
            }
            TerminalOwner::Active => {
                let active = self.active.as_ref().expect("terminal owner checked");
                active.request.command_type.validate_done(&terminal)?;
                if active.expects_acceptance && !active.accepted {
                    return Err(LigatureSessionError::ContradictoryTerminal {
                        expected: format!("ok {}", active.request.line),
                        received: format!("done {}", terminal.command),
                    });
                }
                let active = self.active.take().expect("terminal owner checked");
                Ok(LigatureEvent::Completed {
                    operation_id: active.request.operation_id,
                    terminal,
                })
            }
        }
    }

    fn complete_error(
        &mut self,
        terminal: ProtocolErrorTerminal,
    ) -> Result<LigatureEvent, LigatureSessionError> {
        match self.terminal_owner(&terminal.command)? {
            TerminalOwner::Urgent(index) => {
                let operation_id = self.urgent.remove(index).request.operation_id;
                Ok(LigatureEvent::Failed {
                    operation_id,
                    terminal,
                })
            }
            TerminalOwner::RetiredUrgent(index) => {
                let operation_id = self.retired_urgent.remove(index).request.operation_id;
                Ok(LigatureEvent::RetiredIgnored(operation_id))
            }
            TerminalOwner::Active => {
                let active = self.active.take().expect("terminal owner checked");
                Ok(LigatureEvent::Failed {
                    operation_id: active.request.operation_id,
                    terminal,
                })
            }
        }
    }

    fn complete_urgent(
        &mut self,
        urgent_index: usize,
        terminal: ProtocolTerminal,
    ) -> Result<LigatureEvent, LigatureSessionError> {
        let urgent_command = self.urgent[urgent_index].request.command_type;
        urgent_command.validate_done(&terminal)?;
        let cancelled = terminal.cancelled_command()?;
        let expected = self
            .active
            .as_ref()
            .map(|active| &active.request.command)
            .or(self.orphaned_active.as_ref());
        let allow_completed_orphan = self.active.is_none() && self.orphaned_active.is_some();
        validate_cancelled(cancelled.as_ref(), expected, allow_completed_orphan)?;
        let by = self.urgent.remove(urgent_index).request.operation_id;
        if let Some(active) = self.active.take() {
            return Ok(LigatureEvent::Cancelled {
                operation_id: active.request.operation_id,
                by,
                terminal,
            });
        }
        self.orphaned_active = None;
        Ok(LigatureEvent::Completed {
            operation_id: by,
            terminal,
        })
    }

    fn hard_fault(&mut self, fault: LigatureFault) -> Result<LigatureEvent, LigatureSessionError> {
        let expected = self
            .active
            .as_ref()
            .filter(|active| active.expects_acceptance)
            .map(|active| &active.request.command)
            .or(self.orphaned_active.as_ref());
        let allow_completed_orphan = self
            .active
            .as_ref()
            .is_none_or(|active| !active.expects_acceptance)
            && self.orphaned_active.is_some();
        validate_cancelled(fault.cancelled.as_ref(), expected, allow_completed_orphan)?;
        let mut retired = Vec::new();
        if let Some(active) = self.active.take() {
            retired.push(active.request.operation_id);
        }
        self.orphaned_active = None;
        let retired_urgent = std::mem::take(&mut self.urgent);
        retired.extend(
            retired_urgent
                .iter()
                .map(|urgent| urgent.request.operation_id),
        );
        self.retired_urgent.extend(retired_urgent);
        Ok(LigatureEvent::HardFault { fault, retired })
    }

    fn retired_urgent_index(&self, command: &LigatureCommandToken) -> Option<usize> {
        self.retired_urgent
            .iter()
            .position(|pending| pending.request.command == *command)
    }

    fn terminal_owner(
        &self,
        command: &LigatureCommandToken,
    ) -> Result<TerminalOwner, LigatureSessionError> {
        if let Some(index) = self
            .urgent
            .iter()
            .position(|pending| pending.request.command == *command)
        {
            return Ok(TerminalOwner::Urgent(index));
        }
        if let Some(index) = self.retired_urgent_index(command) {
            return Ok(TerminalOwner::RetiredUrgent(index));
        }
        let Some(active) = self.active.as_ref() else {
            return Err(LigatureSessionError::UnmatchedTerminal {
                command: command.to_string(),
            });
        };
        if active.request.command != *command {
            return Err(LigatureSessionError::ContradictoryTerminal {
                expected: active.request.command.to_string(),
                received: command.to_string(),
            });
        }
        Ok(TerminalOwner::Active)
    }

    fn update_commissioning(
        &mut self,
        state: LigatureState,
        commissioned_configuration: Option<bool>,
    ) {
        if let Some(commissioned) = commissioned_configuration {
            self.commissioned = Some(commissioned);
        } else if self.commissioned.is_none() {
            self.commissioned = commissioning_from_state(state);
        }
    }

    fn pending_for_command_mut(
        &mut self,
        command: &LigatureCommandToken,
    ) -> Option<&mut PendingRequest> {
        if let Some(pending) = self
            .urgent
            .iter_mut()
            .find(|pending| pending.request.command == *command)
        {
            return Some(pending);
        }
        if self
            .active
            .as_ref()
            .is_some_and(|pending| pending.request.command == *command)
        {
            return self.active.as_mut();
        }
        None
    }
}

fn validate_cancelled(
    received: Option<&LigatureCommandToken>,
    expected: Option<&LigatureCommandToken>,
    allow_completed_orphan: bool,
) -> Result<(), LigatureSessionError> {
    if received == expected || (allow_completed_orphan && received.is_none()) {
        return Ok(());
    }
    Err(LigatureSessionError::ContradictoryTerminal {
        expected: format!(
            "CANCELLED:{}",
            expected.map_or("NONE", LigatureCommandToken::as_str)
        ),
        received: format!(
            "CANCELLED:{}",
            received.map_or("NONE", LigatureCommandToken::as_str)
        ),
    })
}

fn commissioning_from_state(state: LigatureState) -> Option<bool> {
    match state {
        LigatureState::CommissioningOnly => Some(false),
        LigatureState::Aligning | LigatureState::Fault => None,
        _ => Some(true),
    }
}

#[cfg(test)]
mod tests {
    use super::{ConnectionEpoch, LigatureCommand, LigatureSession, LigatureSessionError};

    const FAULT: &str = "state STATE:FAULT TRUST:0 Z:? VEL:0 IQ:0 PRESS:? ENDSTOP:0 \
                         PWM:OFF ACTIVE:NONE FAULT:CURRENT_LIMIT RUNTIME_MODIFIED:0";
    const IDLE: &str = "state STATE:IDLE TRUST:0 Z:? VEL:0 IQ:0 PRESS:? ENDSTOP:0 \
                        PWM:OFF ACTIVE:NONE FAULT:NONE RUNTIME_MODIFIED:0";
    const COMMISSIONING_ONLY: &str = "state STATE:COMMISSIONING_ONLY TRUST:0 Z:? VEL:0 \
                                      IQ:0 PRESS:? ENDSTOP:0 PWM:OFF ACTIVE:NONE \
                                      FAULT:NONE RUNTIME_MODIFIED:0";

    #[test]
    fn explicit_commissioning_evidence_survives_fault_recovery() {
        let mut session =
            LigatureSession::from_query_with_commissioning(FAULT, Some(true)).unwrap();
        assert!(!session.scan_enabled());

        session.begin(LigatureCommand::ClearFault).unwrap();
        session
            .receive(ConnectionEpoch(1), "done M999 STATE:IDLE TRUST:0")
            .unwrap();
        session.receive(ConnectionEpoch(1), IDLE).unwrap();

        assert!(session.scan_enabled());
        assert!(session.begin(LigatureCommand::Home).is_ok());
    }

    #[test]
    fn commissioned_configuration_still_cannot_scan_in_commissioning_only() {
        let mut session =
            LigatureSession::from_query_with_commissioning(COMMISSIONING_ONLY, Some(true)).unwrap();

        assert!(!session.scan_enabled());
        assert_eq!(
            session.begin(LigatureCommand::Home),
            Err(LigatureSessionError::CommissioningOnly)
        );
    }
}
