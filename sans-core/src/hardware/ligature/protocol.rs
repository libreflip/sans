//! Pure parser for Ligature's production response frames.

use std::collections::BTreeMap;
use std::fmt;

use thiserror::Error;

/// Public state names emitted by Ligature production firmware.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LigatureState {
    /// Transport and non-production diagnostics only.
    CommissioningOnly,
    /// PWM is off and no operation is active.
    Idle,
    /// PWM is enabled but Position trust is unavailable.
    Armed,
    /// One supervised relative move has been authorized.
    OverridePending,
    /// PWM is enabled and Position trust permits absolute motion.
    Ready,
    /// Sensor and phase alignment is active.
    Aligning,
    /// Endstop-based homing is active.
    Homing,
    /// Motion calibration is active.
    Calibrating,
    /// An absolute or supervised relative move is active.
    Moving,
    /// Contact-seeking downward motion is active.
    TouchingDown,
    /// Touchdown has detected contact and holds position.
    Holding,
    /// A hard fault is latched.
    Fault,
}

impl LigatureState {
    fn parse(value: &str) -> Result<Self, LigatureProtocolError> {
        match value {
            "COMMISSIONING_ONLY" => Ok(Self::CommissioningOnly),
            "IDLE" => Ok(Self::Idle),
            "ARMED" => Ok(Self::Armed),
            "OVERRIDE_PENDING" => Ok(Self::OverridePending),
            "READY" => Ok(Self::Ready),
            "ALIGNING" => Ok(Self::Aligning),
            "HOMING" => Ok(Self::Homing),
            "CALIBRATING" => Ok(Self::Calibrating),
            "MOVING" => Ok(Self::Moving),
            "TOUCHING_DOWN" => Ok(Self::TouchingDown),
            "HOLDING" => Ok(Self::Holding),
            "FAULT" => Ok(Self::Fault),
            _ => Err(LigatureProtocolError::UnknownState(value.into())),
        }
    }
}

/// Validated command identity carried by acceptance and terminal frames.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LigatureCommandToken(String);

impl LigatureCommandToken {
    /// Command text without parameters or a line terminator.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(super) fn from_static(value: &'static str) -> Self {
        Self(value.into())
    }

    fn parse(value: &str) -> Result<Self, LigatureProtocolError> {
        if value.is_empty()
            || value.len() > 4
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'?')
        {
            return Err(LigatureProtocolError::InvalidCommand(value.into()));
        }
        Ok(Self(value.into()))
    }
}

impl fmt::Display for LigatureCommandToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Whether Ligature permits its current position to be used for absolute motion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionTrust {
    /// Current position can be used for absolute motion.
    Trusted,
    /// Home is required before absolute motion.
    Untrusted,
}

impl PositionTrust {
    fn parse(value: &str) -> Result<Self, LigatureProtocolError> {
        match value {
            "1" => Ok(Self::Trusted),
            "0" => Ok(Self::Untrusted),
            _ => Err(LigatureProtocolError::InvalidField {
                field: "TRUST",
                value: value.into(),
            }),
        }
    }
}

/// Position reported by Ligature, separate from Position trust.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LigaturePosition {
    /// Position in millimetres in the homed frame.
    Known(f32),
    /// Firmware deliberately reported `?` because position is untrusted.
    Unknown,
}

impl LigaturePosition {
    fn parse(value: &str) -> Result<Self, LigatureProtocolError> {
        if value == "?" {
            return Ok(Self::Unknown);
        }
        parse_finite("Z", value).map(Self::Known)
    }
}

/// Current public status projected by either a query or heartbeat frame.
#[derive(Clone, Debug, PartialEq)]
pub struct LigatureStatus {
    /// Public production state.
    pub state: LigatureState,
    /// Position trust reported separately from state.
    pub position_trust: PositionTrust,
    /// Known position or the explicit unknown marker.
    pub position: LigaturePosition,
    /// Measured linear velocity in millimetres per second.
    pub velocity_mm_s: f32,
    /// Measured q-axis current in amps.
    pub q_current_a: f32,
    /// Whether Touchdown press is set, or unavailable outside that state.
    pub press_is_set: Option<bool>,
    /// Active exclusive command token, if any.
    pub active: Option<LigatureCommandToken>,
    /// Latched firmware fault reason, if any.
    pub fault: Option<String>,
    /// Endstop input from a full query; absent from heartbeat frames.
    pub endstop_active: Option<bool>,
    /// PWM state from a full query; absent from heartbeat frames.
    pub pwm_active: Option<bool>,
    /// Whether runtime configuration differs from the commissioned image.
    pub runtime_modified: Option<bool>,
}

/// Fields carried by a `done` or `error` terminal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolTerminal {
    /// Command completed by this terminal.
    pub command: LigatureCommandToken,
    fields: BTreeMap<String, String>,
}

impl ProtocolTerminal {
    /// Return one firmware terminal field without discarding unrecognized diagnostics.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }

    pub(super) fn require_valid_fields(
        &self,
        required: &[&'static str],
    ) -> Result<(), LigatureProtocolError> {
        for field in required {
            let value = self
                .fields
                .get(*field)
                .ok_or(LigatureProtocolError::MissingField(field))?;
            validate_terminal_field(field, value)?;
        }
        Ok(())
    }

    pub(super) fn cancelled_command(
        &self,
    ) -> Result<Option<LigatureCommandToken>, LigatureProtocolError> {
        let value = self
            .fields
            .get("CANCELLED")
            .ok_or(LigatureProtocolError::MissingField("CANCELLED"))?;
        parse_optional_command("CANCELLED", value)
    }

    pub(super) fn state(&self) -> Result<LigatureState, LigatureProtocolError> {
        LigatureState::parse(
            self.fields
                .get("STATE")
                .ok_or(LigatureProtocolError::MissingField("STATE"))?,
        )
    }

    pub(super) fn position_trust(&self) -> Result<PositionTrust, LigatureProtocolError> {
        PositionTrust::parse(
            self.fields
                .get("TRUST")
                .ok_or(LigatureProtocolError::MissingField("TRUST"))?,
        )
    }
}

/// A command error terminal with its required firmware reason.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolErrorTerminal {
    /// Command completed by this error.
    pub command: LigatureCommandToken,
    /// Firmware reason code.
    pub reason: String,
    fields: BTreeMap<String, String>,
}

impl ProtocolErrorTerminal {
    /// Return one additional firmware error field.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }
}

/// An unsolicited hard-fault frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LigatureFault {
    /// Firmware hard-fault reason.
    pub reason: String,
    /// Exclusive command retired by the fault, if any.
    pub cancelled: Option<LigatureCommandToken>,
    /// Position trust remaining after the fault.
    pub position_trust: PositionTrust,
}

/// One routed control-rate diagnostic sample without exposing private firmware states.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LigatureCaptureSample {
    /// Zero-based sample index within the diagnostic capture.
    pub index: u16,
    /// Firmware monotonic timestamp in milliseconds.
    pub timestamp_ms: u64,
    /// Position in millimetres.
    pub position_mm: f32,
    /// Motor shaft angle in radians.
    pub shaft_angle_rad: f32,
    /// Motor shaft velocity in radians per second.
    pub velocity_rad_s: f32,
    /// Active control target in the firmware mode's units.
    pub target: f32,
    /// Measured q-axis current in amps.
    pub q_current_a: f32,
    /// Endstop input at sample time.
    pub endstop_active: bool,
}

/// One classified Ligature response line.
#[derive(Clone, Debug, PartialEq)]
pub enum LigatureLine {
    /// Full response to the `?` query.
    State(LigatureStatus),
    /// Unsolicited public-status heartbeat.
    Status(LigatureStatus),
    /// Immediate acceptance of an asynchronous exclusive operation.
    Accepted {
        /// Accepted firmware command token.
        command: LigatureCommandToken,
    },
    /// Successful command terminal.
    Done(ProtocolTerminal),
    /// Command error terminal.
    Error(ProtocolErrorTerminal),
    /// Unsolicited hard fault.
    Fault(LigatureFault),
    /// Control-rate diagnostic sample.
    Capture(LigatureCaptureSample),
}

/// Why a Ligature response could not be classified safely.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LigatureProtocolError {
    /// No response content was supplied.
    #[error("empty Ligature response")]
    Empty,
    /// The first word does not name a production response kind.
    #[error("unknown Ligature response kind: {0}")]
    UnknownKind(String),
    /// A required field is absent.
    #[error("missing Ligature field {0}")]
    MissingField(&'static str),
    /// A named field has a malformed or unsupported value.
    #[error("invalid Ligature field {field}: {value}")]
    InvalidField {
        /// Protocol field name or frame context.
        field: &'static str,
        /// Rejected wire value.
        value: String,
    },
    /// A state is not one of the twelve public production states.
    #[error("unknown public Ligature state: {0}")]
    UnknownState(String),
    /// A command token violates production framing rules.
    #[error("invalid Ligature command token: {0}")]
    InvalidCommand(String),
}

/// Parse one newline-stripped production response.
pub fn parse_ligature_line(line: &str) -> Result<LigatureLine, LigatureProtocolError> {
    let mut words = line.trim_end_matches('\r').split_whitespace();
    let kind = words.next().ok_or(LigatureProtocolError::Empty)?;
    match kind {
        "state" => parse_status(words, true).map(LigatureLine::State),
        "status" => parse_status(words, false).map(LigatureLine::Status),
        "ok" => parse_acceptance(words),
        "done" => parse_done(words).map(LigatureLine::Done),
        "error" => parse_error(words).map(LigatureLine::Error),
        "fault" => parse_fault(words).map(LigatureLine::Fault),
        "capture" => parse_capture(words).map(LigatureLine::Capture),
        other => Err(LigatureProtocolError::UnknownKind(other.into())),
    }
}

fn parse_acceptance<'a>(
    mut words: impl Iterator<Item = &'a str>,
) -> Result<LigatureLine, LigatureProtocolError> {
    let command = parse_command(words.next())?;
    if let Some(extra) = words.next() {
        return Err(LigatureProtocolError::InvalidField {
            field: "acceptance",
            value: extra.into(),
        });
    }
    Ok(LigatureLine::Accepted { command })
}

fn parse_capture<'a>(
    words: impl Iterator<Item = &'a str>,
) -> Result<LigatureCaptureSample, LigatureProtocolError> {
    let fields = parse_fields(words)?;
    let required = |name| {
        fields
            .get(name)
            .map(String::as_str)
            .ok_or(LigatureProtocolError::MissingField(name))
    };
    let index = parse_integer("I", required("I")?)?;
    let timestamp_ms = parse_integer("T_MS", required("T_MS")?)?;
    let _: u16 = parse_integer("STATE_ID", required("STATE_ID")?)?;
    Ok(LigatureCaptureSample {
        index,
        timestamp_ms,
        position_mm: parse_finite("Z", required("Z")?)?,
        shaft_angle_rad: parse_finite("ANGLE", required("ANGLE")?)?,
        velocity_rad_s: parse_finite("VEL_RAD_S", required("VEL_RAD_S")?)?,
        target: parse_finite("TARGET", required("TARGET")?)?,
        q_current_a: parse_finite("IQ", required("IQ")?)?,
        endstop_active: parse_bool("ENDSTOP", required("ENDSTOP")?)?,
    })
}

fn parse_status<'a>(
    words: impl Iterator<Item = &'a str>,
    is_query: bool,
) -> Result<LigatureStatus, LigatureProtocolError> {
    let fields = parse_fields(words)?;
    let required = |name| {
        fields
            .get(name)
            .map(String::as_str)
            .ok_or(LigatureProtocolError::MissingField(name))
    };
    let state = LigatureState::parse(required("STATE")?)?;
    let position_trust = PositionTrust::parse(required("TRUST")?)?;
    let position = LigaturePosition::parse(required("Z")?)?;
    let velocity_mm_s = parse_finite("VEL", required("VEL")?)?;
    let q_current_a = parse_finite("IQ", required("IQ")?)?;
    let press_is_set = match required("PRESS")? {
        "SET" => Some(true),
        "?" => None,
        value => {
            return Err(LigatureProtocolError::InvalidField {
                field: "PRESS",
                value: value.into(),
            })
        }
    };
    let active = parse_optional_command("ACTIVE", required("ACTIVE")?)?;
    let fault = parse_optional_token("FAULT", required("FAULT")?)?;

    let endstop_active = parse_optional_bool(&fields, "ENDSTOP")?;
    let pwm_active = match fields.get("PWM").map(String::as_str) {
        Some("ACTIVE") => Some(true),
        Some("OFF") => Some(false),
        Some(value) => {
            return Err(LigatureProtocolError::InvalidField {
                field: "PWM",
                value: value.into(),
            })
        }
        None => None,
    };
    let runtime_modified = parse_optional_bool(&fields, "RUNTIME_MODIFIED")?;
    if is_query {
        for name in ["ENDSTOP", "PWM", "RUNTIME_MODIFIED"] {
            if !fields.contains_key(name) {
                return Err(LigatureProtocolError::MissingField(name));
            }
        }
    }

    Ok(LigatureStatus {
        state,
        position_trust,
        position,
        velocity_mm_s,
        q_current_a,
        press_is_set,
        active,
        fault,
        endstop_active,
        pwm_active,
        runtime_modified,
    })
}

fn parse_done<'a>(
    mut words: impl Iterator<Item = &'a str>,
) -> Result<ProtocolTerminal, LigatureProtocolError> {
    let command = parse_command(words.next())?;
    let fields = parse_fields(words)?;
    Ok(ProtocolTerminal { command, fields })
}

fn parse_error<'a>(
    mut words: impl Iterator<Item = &'a str>,
) -> Result<ProtocolErrorTerminal, LigatureProtocolError> {
    let command = parse_command(words.next())?;
    let fields = parse_fields(words)?;
    let reason = fields
        .get("REASON")
        .cloned()
        .ok_or(LigatureProtocolError::MissingField("REASON"))?;
    Ok(ProtocolErrorTerminal {
        command,
        reason,
        fields,
    })
}

fn parse_fault<'a>(
    mut words: impl Iterator<Item = &'a str>,
) -> Result<LigatureFault, LigatureProtocolError> {
    let reason = words
        .next()
        .ok_or(LigatureProtocolError::MissingField("fault reason"))?
        .to_owned();
    let fields = parse_fields(words)?;
    let cancelled = parse_optional_command(
        "CANCELLED",
        fields
            .get("CANCELLED")
            .ok_or(LigatureProtocolError::MissingField("CANCELLED"))?,
    )?;
    let position_trust = PositionTrust::parse(
        fields
            .get("TRUST")
            .ok_or(LigatureProtocolError::MissingField("TRUST"))?,
    )?;
    if fields.get("STATE").map(String::as_str) != Some("FAULT") {
        return Err(LigatureProtocolError::InvalidField {
            field: "STATE",
            value: fields.get("STATE").cloned().unwrap_or_default(),
        });
    }
    let position = fields
        .get("Z")
        .ok_or(LigatureProtocolError::MissingField("Z"))?;
    let expected_position = match position_trust {
        PositionTrust::Trusted => "KNOWN",
        PositionTrust::Untrusted => "?",
    };
    if position != expected_position {
        return Err(LigatureProtocolError::InvalidField {
            field: "Z",
            value: position.clone(),
        });
    }
    Ok(LigatureFault {
        reason,
        cancelled,
        position_trust,
    })
}

fn parse_fields<'a>(
    words: impl Iterator<Item = &'a str>,
) -> Result<BTreeMap<String, String>, LigatureProtocolError> {
    let mut fields = BTreeMap::new();
    for word in words {
        let Some((name, value)) = word.split_once(':') else {
            return Err(LigatureProtocolError::InvalidField {
                field: "frame",
                value: word.into(),
            });
        };
        if name.is_empty() || value.is_empty() || fields.insert(name.into(), value.into()).is_some()
        {
            return Err(LigatureProtocolError::InvalidField {
                field: "frame",
                value: word.into(),
            });
        }
    }
    Ok(fields)
}

fn parse_command(value: Option<&str>) -> Result<LigatureCommandToken, LigatureProtocolError> {
    let value = value.ok_or(LigatureProtocolError::MissingField("command"))?;
    LigatureCommandToken::parse(value)
}

fn parse_optional_command(
    field: &'static str,
    value: &str,
) -> Result<Option<LigatureCommandToken>, LigatureProtocolError> {
    if value == "NONE" {
        return Ok(None);
    }
    LigatureCommandToken::parse(value)
        .map(Some)
        .map_err(|_| LigatureProtocolError::InvalidField {
            field,
            value: value.into(),
        })
}

fn parse_optional_token(
    field: &'static str,
    value: &str,
) -> Result<Option<String>, LigatureProtocolError> {
    if value == "NONE" {
        return Ok(None);
    }
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(LigatureProtocolError::InvalidField {
            field,
            value: value.into(),
        });
    }
    Ok(Some(value.into()))
}

fn parse_optional_bool(
    fields: &BTreeMap<String, String>,
    name: &'static str,
) -> Result<Option<bool>, LigatureProtocolError> {
    match fields.get(name).map(String::as_str) {
        Some("1") => Ok(Some(true)),
        Some("0") => Ok(Some(false)),
        Some(value) => Err(LigatureProtocolError::InvalidField {
            field: name,
            value: value.into(),
        }),
        None => Ok(None),
    }
}

fn validate_terminal_field(field: &'static str, value: &str) -> Result<(), LigatureProtocolError> {
    match field {
        "STATE" => LigatureState::parse(value).map(|_| ()),
        "TRUST" => PositionTrust::parse(value).map(|_| ()),
        "Z" => {
            if matches!(value, "KNOWN" | "?") {
                Ok(())
            } else {
                parse_finite(field, value).map(|_| ())
            }
        }
        "CANCELLED" => parse_optional_command(field, value).map(|_| ()),
        "ZERO_ELECTRICAL" => parse_finite(field, value).map(|_| ()),
        "SENSOR_DIRECTION" if matches!(value, "CW" | "CCW") => Ok(()),
        "SENSOR_DIRECTION" => Err(LigatureProtocolError::InvalidField {
            field,
            value: value.into(),
        }),
        "VOLATILE" if value == "1" => Ok(()),
        "VOLATILE" => Err(LigatureProtocolError::InvalidField {
            field,
            value: value.into(),
        }),
        _ => Ok(()),
    }
}

fn parse_finite(field: &'static str, value: &str) -> Result<f32, LigatureProtocolError> {
    value
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .ok_or_else(|| LigatureProtocolError::InvalidField {
            field,
            value: value.into(),
        })
}

fn parse_integer<T>(field: &'static str, value: &str) -> Result<T, LigatureProtocolError>
where
    T: std::str::FromStr,
{
    value
        .parse()
        .map_err(|_| LigatureProtocolError::InvalidField {
            field,
            value: value.into(),
        })
}

fn parse_bool(field: &'static str, value: &str) -> Result<bool, LigatureProtocolError> {
    match value {
        "1" => Ok(true),
        "0" => Ok(false),
        _ => Err(LigatureProtocolError::InvalidField {
            field,
            value: value.into(),
        }),
    }
}
