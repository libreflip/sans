//! Endpoint-0 descriptor parsing, flattening, API fingerprint and capability check.
//!
//! The descriptor layout follows upstream ODrive `fw-v0.5.1`: a JSON root array
//! whose members are objects (`name`, `type: "object"`, `members`), properties
//! (`name`, `id`, `type`, `access`) and functions (`name`, `id`,
//! `type: "function"`, `inputs`, `outputs`). See
//! `Firmware/fibre/python/fibre/remote_object.py` at revision
//! `7831d795235e5ef8535e4b46621a0721b458ec8f`.

use std::fmt::Write as _;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::protocol::{descriptor_crc16, ValueType};

/// A downloaded endpoint descriptor that parsed as the upstream layout.
#[derive(Clone, Debug)]
pub(crate) struct Descriptor {
    length: usize,
    crc16: u16,
    flattened: Map<String, Value>,
    fingerprint: String,
}

impl Descriptor {
    /// Parse raw endpoint-0 bytes, flatten them and compute CRC and fingerprint.
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, String> {
        let root: Value = serde_json::from_slice(bytes)
            .map_err(|error| format!("descriptor is not valid JSON: {error}"))?;
        let Value::Array(members) = root else {
            return Err("descriptor root is not a JSON array".into());
        };
        let mut flattened = Map::new();
        flatten_members("", &members, &mut flattened)?;
        let mut canonical = String::new();
        write_canonical(&Value::Object(flattened.clone()), &mut canonical)?;
        let digest = Sha256::digest(canonical.as_bytes());
        let fingerprint = digest.iter().fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
        Ok(Self {
            length: bytes.len(),
            crc16: descriptor_crc16(bytes),
            flattened,
            fingerprint,
        })
    }

    /// Raw descriptor length in bytes.
    pub(crate) fn length(&self) -> usize {
        self.length
    }

    /// Descriptor CRC16, the trailer for every nonzero endpoint.
    pub(crate) fn crc16(&self) -> u16 {
        self.crc16
    }

    /// Map from dotted path to the descriptor entry, as the #57 capture stored it.
    pub(crate) fn flattened(&self) -> &Map<String, Value> {
        &self.flattened
    }

    /// Number of flattened endpoints, including endpoint 0.
    pub(crate) fn endpoint_count(&self) -> usize {
        self.flattened.len()
    }

    /// SHA-256 over the canonical flattened form: keys sorted at every level,
    /// compact separators, ASCII with `\uXXXX` escapes. This reproduces the #57
    /// capture's `json.dumps(sort_keys=True, separators=(",", ":"))`.
    pub(crate) fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

/// Every endpoint Sans uses. IDs are always resolved from the live descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Endpoint {
    SerialNumber,
    HwVersionMajor,
    HwVersionMinor,
    HwVersionVariant,
    FwVersionMajor,
    FwVersionMinor,
    FwVersionRevision,
    FwVersionUnreleased,
    VbusVoltage,
    MotorArmedState,
    CurrentState,
    AxisError,
    MotorError,
    EncoderError,
    ControllerError,
    PosEstimate,
    VelEstimate,
    IqMeasured,
    IqSetpoint,
    TrajectoryDone,
    IsHomed,
    MinEndstopState,
    MotorIsCalibrated,
    EncoderIsReady,
    ControlMode,
    InputMode,
    RequestedState,
    InputPos,
    CurrentLimit,
    ControllerVelLimit,
    TrapVelLimit,
    TrapAccelLimit,
    TrapDecelLimit,
    ClearErrors,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scope {
    Root,
    Axis,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Property(ValueType, Access),
    Function,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Access {
    Read,
    Write,
    ReadWrite,
}

impl Access {
    fn description(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::ReadWrite => "read and write",
        }
    }

    fn satisfied_by(self, granted: &str) -> bool {
        let read = granted.contains('r');
        let write = granted.contains('w');
        match self {
            Self::Read => read,
            Self::Write => write,
            Self::ReadWrite => read && write,
        }
    }
}

use Access::{Read, ReadWrite, Write};
use ValueType::{Bool, F32, I32, U64, U8};

/// The capability table: every endpoint Sans depends on, in check order.
const CAPABILITIES: [(Endpoint, Scope, &str, Kind); 34] = [
    (
        Endpoint::SerialNumber,
        Scope::Root,
        "serial_number",
        Kind::Property(U64, Read),
    ),
    (
        Endpoint::HwVersionMajor,
        Scope::Root,
        "hw_version_major",
        Kind::Property(U8, Read),
    ),
    (
        Endpoint::HwVersionMinor,
        Scope::Root,
        "hw_version_minor",
        Kind::Property(U8, Read),
    ),
    (
        Endpoint::HwVersionVariant,
        Scope::Root,
        "hw_version_variant",
        Kind::Property(U8, Read),
    ),
    (
        Endpoint::FwVersionMajor,
        Scope::Root,
        "fw_version_major",
        Kind::Property(U8, Read),
    ),
    (
        Endpoint::FwVersionMinor,
        Scope::Root,
        "fw_version_minor",
        Kind::Property(U8, Read),
    ),
    (
        Endpoint::FwVersionRevision,
        Scope::Root,
        "fw_version_revision",
        Kind::Property(U8, Read),
    ),
    (
        Endpoint::FwVersionUnreleased,
        Scope::Root,
        "fw_version_unreleased",
        Kind::Property(U8, Read),
    ),
    (
        Endpoint::VbusVoltage,
        Scope::Root,
        "vbus_voltage",
        Kind::Property(F32, Read),
    ),
    (
        Endpoint::MotorArmedState,
        Scope::Axis,
        "motor.armed_state",
        Kind::Property(I32, Read),
    ),
    (
        Endpoint::CurrentState,
        Scope::Axis,
        "current_state",
        Kind::Property(I32, Read),
    ),
    (
        Endpoint::AxisError,
        Scope::Axis,
        "error",
        Kind::Property(I32, Read),
    ),
    (
        Endpoint::MotorError,
        Scope::Axis,
        "motor.error",
        Kind::Property(I32, Read),
    ),
    (
        Endpoint::EncoderError,
        Scope::Axis,
        "encoder.error",
        Kind::Property(I32, Read),
    ),
    (
        Endpoint::ControllerError,
        Scope::Axis,
        "controller.error",
        Kind::Property(I32, Read),
    ),
    (
        Endpoint::PosEstimate,
        Scope::Axis,
        "encoder.pos_estimate",
        Kind::Property(F32, Read),
    ),
    (
        Endpoint::VelEstimate,
        Scope::Axis,
        "encoder.vel_estimate",
        Kind::Property(F32, Read),
    ),
    (
        Endpoint::IqMeasured,
        Scope::Axis,
        "motor.current_control.Iq_measured",
        Kind::Property(F32, Read),
    ),
    (
        Endpoint::IqSetpoint,
        Scope::Axis,
        "motor.current_control.Iq_setpoint",
        Kind::Property(F32, Read),
    ),
    (
        Endpoint::TrajectoryDone,
        Scope::Axis,
        "controller.trajectory_done",
        Kind::Property(Bool, Read),
    ),
    (
        Endpoint::IsHomed,
        Scope::Axis,
        "is_homed",
        Kind::Property(Bool, Read),
    ),
    (
        Endpoint::MinEndstopState,
        Scope::Axis,
        "min_endstop.endstop_state",
        Kind::Property(Bool, Read),
    ),
    (
        Endpoint::MotorIsCalibrated,
        Scope::Axis,
        "motor.is_calibrated",
        Kind::Property(Bool, Read),
    ),
    (
        Endpoint::EncoderIsReady,
        Scope::Axis,
        "encoder.is_ready",
        Kind::Property(Bool, Read),
    ),
    (
        Endpoint::ControlMode,
        Scope::Axis,
        "controller.config.control_mode",
        Kind::Property(I32, Read),
    ),
    (
        Endpoint::InputMode,
        Scope::Axis,
        "controller.config.input_mode",
        Kind::Property(I32, Read),
    ),
    (
        Endpoint::RequestedState,
        Scope::Axis,
        "requested_state",
        Kind::Property(I32, Write),
    ),
    (
        Endpoint::InputPos,
        Scope::Axis,
        "controller.input_pos",
        Kind::Property(F32, ReadWrite),
    ),
    (
        Endpoint::CurrentLimit,
        Scope::Axis,
        "motor.config.current_lim",
        Kind::Property(F32, ReadWrite),
    ),
    (
        Endpoint::ControllerVelLimit,
        Scope::Axis,
        "controller.config.vel_limit",
        Kind::Property(F32, ReadWrite),
    ),
    (
        Endpoint::TrapVelLimit,
        Scope::Axis,
        "trap_traj.config.vel_limit",
        Kind::Property(F32, ReadWrite),
    ),
    (
        Endpoint::TrapAccelLimit,
        Scope::Axis,
        "trap_traj.config.accel_limit",
        Kind::Property(F32, ReadWrite),
    ),
    (
        Endpoint::TrapDecelLimit,
        Scope::Axis,
        "trap_traj.config.decel_limit",
        Kind::Property(F32, ReadWrite),
    ),
    (
        Endpoint::ClearErrors,
        Scope::Axis,
        "clear_errors",
        Kind::Function,
    ),
];

/// Endpoint IDs resolved from one live descriptor for one axis.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedEndpoints {
    ids: [u16; CAPABILITIES.len()],
}

impl ResolvedEndpoints {
    /// The live endpoint ID for a capability.
    pub(crate) fn id(&self, endpoint: Endpoint) -> u16 {
        let index = CAPABILITIES
            .iter()
            .position(|(candidate, ..)| *candidate == endpoint)
            .expect("every Endpoint variant has a capability row");
        self.ids[index]
    }
}

/// The wire encoding of a property capability.
pub(crate) fn value_type(endpoint: Endpoint) -> ValueType {
    CAPABILITIES
        .iter()
        .find_map(|(candidate, _, _, kind)| match kind {
            Kind::Property(value_type, _) if *candidate == endpoint => Some(*value_type),
            _ => None,
        })
        .expect("value_type is only asked for property capabilities")
}

/// Walk the flattened live descriptor and collect every capability problem.
///
/// Extra endpoints are ignored. Paths are relative to `axis<N>` unless the
/// capability is on the root object.
pub(crate) fn check_capabilities(
    flattened: &Map<String, Value>,
    axis: u8,
) -> Result<ResolvedEndpoints, Vec<String>> {
    let mut problems = Vec::new();
    let mut ids = [0_u16; CAPABILITIES.len()];
    for (index, (_, scope, relative, kind)) in CAPABILITIES.iter().enumerate() {
        let path = match scope {
            Scope::Root => (*relative).to_owned(),
            Scope::Axis => format!("axis{axis}.{relative}"),
        };
        let Some(entry) = flattened.get(&path) else {
            problems.push(format!("missing endpoint {path}"));
            continue;
        };
        let found_type = entry
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("<none>");
        let (needed_type, access) = match kind {
            Kind::Property(value_type, access) => (value_type.descriptor_name(), Some(*access)),
            Kind::Function => ("function", None),
        };
        if found_type != needed_type {
            problems.push(format!(
                "endpoint {path} has type {found_type}, Sans needs {needed_type}"
            ));
            continue;
        }
        if let Some(access) = access {
            // `remote_object.py` `RemoteProperty`: `json_data.get("access", "r")`.
            let granted = entry.get("access").and_then(Value::as_str).unwrap_or("r");
            if !access.satisfied_by(granted) {
                problems.push(format!(
                    "endpoint {path} has access {granted}, Sans needs {}",
                    access.description()
                ));
                continue;
            }
        } else {
            // `remote_object.py` `RemoteFunction`: `arguments` (deprecated) or
            // `inputs`, and `outputs`; an absent list is empty.
            let no_arguments = |key: &str| match entry.get(key) {
                None => true,
                Some(list) => list.as_array().is_some_and(Vec::is_empty),
            };
            if !(no_arguments("arguments") && no_arguments("inputs") && no_arguments("outputs")) {
                problems.push(format!(
                    "function {path} must have no inputs and no outputs"
                ));
                continue;
            }
        }
        match entry
            .get("id")
            .and_then(Value::as_u64)
            .and_then(|id| u16::try_from(id).ok())
            .filter(|id| *id != 0 && *id < 0x8000)
        {
            Some(id) => ids[index] = id,
            None => problems.push(format!("endpoint {path} has no usable ID")),
        }
    }
    if problems.is_empty() {
        Ok(ResolvedEndpoints { ids })
    } else {
        Err(problems)
    }
}

fn flatten_members(
    prefix: &str,
    members: &[Value],
    flattened: &mut Map<String, Value>,
) -> Result<(), String> {
    for member in members {
        let Value::Object(entry) = member else {
            return Err(format!(
                "descriptor member under {prefix:?} is not an object"
            ));
        };
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("descriptor member under {prefix:?} has no name"))?;
        let path = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}.{name}")
        };
        if entry.get("type").and_then(Value::as_str) == Some("object") {
            let children = entry
                .get("members")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("descriptor object {path} has no members"))?;
            flatten_members(&path, children, flattened)?;
        } else if flattened.insert(path.clone(), member.clone()).is_some() {
            return Err(format!("descriptor path {path} appears twice"));
        }
    }
    Ok(())
}

fn write_canonical(value: &Value, out: &mut String) -> Result<(), String> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        Value::Number(number) => {
            if !(number.is_i64() || number.is_u64()) {
                return Err(format!("descriptor contains non-integer number {number}"));
            }
            let _ = write!(out, "{number}");
        }
        Value::String(text) => write_canonical_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical_string(key, out);
                out.push(':');
                write_canonical(&map[key], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// Python `json.dumps(ensure_ascii=True)` string escaping.
fn write_canonical_string(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(character),
            _ => {
                let mut units = [0_u16; 2];
                for unit in character.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECONSTRUCTED_DESCRIPTOR: &[u8] =
        include_bytes!("../../tests/fixtures/odrive/descriptor-RECONSTRUCTED-2026-11-10.json");
    const CAPTURED_ENDPOINTS: &str =
        include_str!("../../test-results/odrive-mini-endpoints-2026-11-10.json");

    #[test]
    fn flattening_the_reconstructed_descriptor_reproduces_the_captured_endpoint_file() {
        let descriptor = Descriptor::parse(RECONSTRUCTED_DESCRIPTOR).unwrap();
        let captured: serde_json::Value = serde_json::from_str(CAPTURED_ENDPOINTS).unwrap();

        assert_eq!(
            serde_json::Value::Object(descriptor.flattened().clone()),
            captured
        );
        assert_eq!(descriptor.endpoint_count(), 535);
        assert_eq!(
            descriptor.fingerprint(),
            "a36d84437bd2bf5925c0119a1df53d88de8ebe1aa8df0a338d2f6dd56fd5e52b"
        );
    }

    fn captured() -> Map<String, Value> {
        match serde_json::from_str(CAPTURED_ENDPOINTS).unwrap() {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    #[test]
    fn capability_check_passes_on_the_capture_and_resolves_the_captured_ids() {
        let resolved = check_capabilities(&captured(), 0).unwrap();

        for (endpoint, captured_id) in [
            (Endpoint::SerialNumber, 4),
            (Endpoint::VbusVoltage, 1),
            (Endpoint::FwVersionUnreleased, 11),
            (Endpoint::CurrentState, 73),
            (Endpoint::RequestedState, 74),
            (Endpoint::AxisError, 71),
            (Endpoint::MotorError, 132),
            (Endpoint::MotorArmedState, 133),
            (Endpoint::EncoderError, 236),
            (Endpoint::ControllerError, 193),
            (Endpoint::PosEstimate, 243),
            (Endpoint::VelEstimate, 249),
            (Endpoint::IqMeasured, 150),
            (Endpoint::IqSetpoint, 149),
            (Endpoint::TrajectoryDone, 200),
            (Endpoint::IsHomed, 77),
            (Endpoint::MinEndstopState, 284),
            (Endpoint::MotorIsCalibrated, 134),
            (Endpoint::EncoderIsReady, 237),
            (Endpoint::ControlMode, 208),
            (Endpoint::InputMode, 209),
            (Endpoint::InputPos, 194),
            (Endpoint::CurrentLimit, 180),
            (Endpoint::ControllerVelLimit, 213),
            (Endpoint::TrapVelLimit, 281),
            (Endpoint::TrapAccelLimit, 282),
            (Endpoint::TrapDecelLimit, 283),
            (Endpoint::ClearErrors, 299),
        ] {
            assert_eq!(resolved.id(endpoint), captured_id, "{endpoint:?}");
        }
        let axis1 = check_capabilities(&captured(), 1).unwrap();
        assert_eq!(axis1.id(Endpoint::RequestedState), 303);
        assert_eq!(axis1.id(Endpoint::SerialNumber), 4);
    }

    #[test]
    fn capability_check_names_each_problem_and_lists_them_all() {
        let mut missing = captured();
        missing.remove("axis0.motor.config.current_lim");
        assert_eq!(
            check_capabilities(&missing, 0).unwrap_err(),
            vec!["missing endpoint axis0.motor.config.current_lim".to_string()]
        );

        let mut retyped = captured();
        retyped["axis0.controller.input_pos"]["type"] = "int32".into();
        assert_eq!(
            check_capabilities(&retyped, 0).unwrap_err(),
            vec![
                "endpoint axis0.controller.input_pos has type int32, Sans needs float".to_string()
            ]
        );

        let mut read_only = captured();
        read_only["axis0.requested_state"]["access"] = "r".into();
        assert_eq!(
            check_capabilities(&read_only, 0).unwrap_err(),
            vec!["endpoint axis0.requested_state has access r, Sans needs write".to_string()]
        );

        let mut several = captured();
        several.remove("serial_number");
        several["axis0.trap_traj.config.decel_limit"]["access"] = "r".into();
        several["axis0.clear_errors"]["type"] = "float".into();
        several["axis0.encoder.is_ready"]["type"] = "uint8".into();
        assert_eq!(
            check_capabilities(&several, 0).unwrap_err(),
            vec![
                "missing endpoint serial_number".to_string(),
                "endpoint axis0.encoder.is_ready has type uint8, Sans needs bool".to_string(),
                "endpoint axis0.trap_traj.config.decel_limit has access r, Sans needs read and write"
                    .to_string(),
                "endpoint axis0.clear_errors has type float, Sans needs function".to_string(),
            ]
        );
    }
}
