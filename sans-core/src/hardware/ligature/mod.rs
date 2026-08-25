//! Typed host model for Ligature's production serial protocol.
#![warn(missing_docs)]

mod client;
mod lifecycle;
mod protocol;

pub use client::{LigatureClient, LigatureTransportError, LigatureWire, SerialLigatureWire};

pub use lifecycle::{
    ConnectionEpoch, LigatureCommand, LigatureEvent, LigatureReconnect, LigatureRequest,
    LigatureSession, LigatureSessionError, OperationId, RequestPriority,
};

pub use protocol::{
    parse_ligature_line, LigatureCaptureSample, LigatureFault, LigatureLine, LigaturePosition,
    LigatureProtocolError, LigatureState, LigatureStatus, PositionTrust, ProtocolErrorTerminal,
    ProtocolTerminal,
};

/// Fixed baud rate implemented by Ligature's production firmware.
pub const LIGATURE_BAUD: u32 = 500_000;
