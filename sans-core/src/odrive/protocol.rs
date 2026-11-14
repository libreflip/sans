//! Legacy native Fibre packet codec, as used by ODrive firmware `fw-v0.5.1`.
//!
//! Every constant here cites the file and symbol it came from in upstream ODrive
//! at revision `7831d795235e5ef8535e4b46621a0721b458ec8f` (tag `fw-v0.5.1`),
//! fetched again on 2026-11-10. Upstream is the baseline for the vendor-modified
//! MKS build, so these are the best available facts and not proof about the
//! installed image; #60 confirms them on the board.

/// CRC16 polynomial. `Firmware/fibre/cpp/include/fibre/protocol.hpp`
/// `CANONICAL_CRC16_POLYNOMIAL = 0x3d65`; `docs/protocol.md` "CRC16".
pub(crate) const CRC16_POLYNOMIAL: u16 = 0x3d65;
/// Fibre protocol version. `protocol.hpp` `PROTOCOL_VERSION = 1`. It is the
/// endpoint-0 trailer and the initial value of the descriptor CRC
/// (`docs/protocol.md` "Bytes N-2, N-1": "the initial value is set to the
/// protocol version"; "Initial value ... `0x0001` for the JSON CRC").
pub(crate) const PROTOCOL_VERSION: u16 = 1;

/// `ODrive.Axis.AxisState` value `Idle`, the second entry after `Undefined`
/// (`Firmware/odrive-interface.yaml`).
pub(crate) const AXIS_STATE_IDLE: i32 = 1;
/// `ODrive.Axis.AxisState` value `ClosedLoopControl`, ninth entry (`odrive-interface.yaml`).
pub(crate) const AXIS_STATE_CLOSED_LOOP_CONTROL: i32 = 8;
/// `ODrive.Axis.AxisState` value `Homing`, twelfth entry (`odrive-interface.yaml`).
pub(crate) const AXIS_STATE_HOMING: i32 = 11;
/// `ODrive.Controller.ControlMode` value `PositionControl`, fourth entry (`odrive-interface.yaml`).
pub(crate) const CONTROL_MODE_POSITION_CONTROL: i32 = 3;
/// `ODrive.Controller.InputMode` value `TrapTraj`, sixth entry (`odrive-interface.yaml`).
pub(crate) const INPUT_MODE_TRAP_TRAJ: i32 = 5;
/// `ODrive.Axis.Error` flag `MinEndstopPressed`, the thirteenth flag after the
/// `None` null flag, so bit 12 (`odrive-interface.yaml`). Upstream sets it on
/// every check while the enabled minimum endstop is pressed outside homing
/// (`Firmware/MotorControl/axis.cpp` `Axis::check_for_errors`).
pub(crate) const AXIS_ERROR_MIN_ENDSTOP_PRESSED: u32 = 1 << 12;

/// `ODrive.Axis.Error` flags by bit (`odrive-interface.yaml`, `fw-v0.5.1`). Flags
/// follow one another unless the YAML gives a `bit`, as it does for
/// `HomingWithoutEndstop` (17) and `CurrentLimitViolation` (12).
const AXIS_ERRORS: &[(u32, &str)] = &[
    (0, "InvalidState"),
    (1, "DcBusUnderVoltage"),
    (2, "DcBusOverVoltage"),
    (3, "CurrentMeasurementTimeout"),
    (4, "BrakeResistorDisarmed"),
    (5, "MotorDisarmed"),
    (6, "MotorFailed"),
    (7, "SensorlessEstimatorFailed"),
    (8, "EncoderFailed"),
    (9, "ControllerFailed"),
    (10, "PosCtrlDuringSensorless"),
    (11, "WatchdogTimerExpired"),
    (12, "MinEndstopPressed"),
    (13, "MaxEndstopPressed"),
    (14, "EstopRequested"),
    (17, "HomingWithoutEndstop"),
    (18, "OverTemp"),
];
/// `ODrive.Motor.Error` flags by bit.
const MOTOR_ERRORS: &[(u32, &str)] = &[
    (0, "PhaseResistanceOutOfRange"),
    (1, "PhaseInductanceOutOfRange"),
    (2, "AdcFailed"),
    (3, "DrvFault"),
    (4, "ControlDeadlineMissed"),
    (5, "NotImplementedMotorType"),
    (6, "BrakeCurrentOutOfRange"),
    (7, "ModulationMagnitude"),
    (8, "BrakeDeadtimeViolation"),
    (9, "UnexpectedTimerCallback"),
    (10, "CurrentSenseSaturation"),
    (12, "CurrentLimitViolation"),
    (13, "BrakeDutyCycleNan"),
    (14, "DcBusOverRegenCurrent"),
    (15, "DcBusOverCurrent"),
];
/// `ODrive.Encoder.Error` flags by bit.
const ENCODER_ERRORS: &[(u32, &str)] = &[
    (0, "UnstableGain"),
    (1, "CprPolepairsMismatch"),
    (2, "NoResponse"),
    (3, "UnsupportedEncoderMode"),
    (4, "IllegalHallState"),
    (5, "IndexNotFoundYet"),
    (6, "AbsSpiTimeout"),
    (7, "AbsSpiComFail"),
    (8, "AbsSpiNotReady"),
];
/// `ODrive.Controller.Error` flags by bit.
const CONTROLLER_ERRORS: &[(u32, &str)] = &[
    (0, "Overspeed"),
    (1, "InvalidInputMode"),
    (2, "UnstableGain"),
    (3, "InvalidMirrorAxis"),
    (4, "InvalidLoadEncoder"),
    (5, "InvalidEstimate"),
];

/// The four error words with their upstream flag names, for logs and the
/// Stop screen: `axis MotorDisarmed|ControllerFailed (0x220), motor none, ...`.
pub(crate) fn describe_error_words(axis: u32, motor: u32, encoder: u32, controller: u32) -> String {
    [
        ("axis", axis, AXIS_ERRORS),
        ("motor", motor, MOTOR_ERRORS),
        ("encoder", encoder, ENCODER_ERRORS),
        ("controller", controller, CONTROLLER_ERRORS),
    ]
    .iter()
    .map(|&(word, bits, names)| {
        if bits == 0 {
            return format!("{word} none");
        }
        let flags: Vec<String> = (0..32)
            .filter(|bit| bits & (1 << bit) != 0)
            .map(|bit| {
                names
                    .iter()
                    .find(|(named, _)| *named == bit)
                    .map_or(format!("bit{bit}"), |(_, name)| (*name).into())
            })
            .collect();
        format!("{word} {} (0x{bits:x})", flags.join("|"))
    })
    .collect::<Vec<_>>()
    .join(", ")
}

/// A property value encoding on the wire, little-endian.
///
/// Sizes follow the POD serialization in `docs/protocol.md` ("an int32
/// endpoint's input and output is a 4 byte little endian representation") and
/// the descriptor type names in the #57 capture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ValueType {
    /// `bool`, one byte.
    Bool,
    /// `uint8`, one byte.
    U8,
    /// `int32`, four bytes. Enums and error flags are reported as `int32`.
    I32,
    /// `uint64`, eight bytes.
    U64,
    /// `float`, four bytes IEEE 754.
    F32,
}

impl ValueType {
    /// The descriptor's `type` string for this encoding.
    pub(crate) fn descriptor_name(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::U8 => "uint8",
            Self::I32 => "int32",
            Self::U64 => "uint64",
            Self::F32 => "float",
        }
    }

    /// Encoded size in bytes; a reply of any other length is malformed.
    pub(crate) fn size(self) -> usize {
        match self {
            Self::Bool | Self::U8 => 1,
            Self::I32 | Self::F32 => 4,
            Self::U64 => 8,
        }
    }
}

/// Bit 15 of the endpoint ID requests a reply; bit 15 of a reply's sequence
/// number marks it as a reply. `docs/protocol.md` "Packet format";
/// `protocol.cpp` `expect_response = endpoint_id & 0x8000`, `seq_no | 0x8000`.
pub(crate) const REPLY_FLAG: u16 = 0x8000;
/// Endpoint 0 serves the JSON descriptor. `protocol.cpp` `endpoint0_handler`.
pub(crate) const DESCRIPTOR_ENDPOINT: u16 = 0;
/// Expected reply length of one descriptor chunk request.
/// `Firmware/fibre/python/fibre/protocol.py` `remote_endpoint_read_buffer`, `chunk_length = 512`.
pub(crate) const DESCRIPTOR_CHUNK_REQUEST_LEN: u16 = 512;
/// Bit 7 of every sequence number is forced on, as the legacy host does.
/// `protocol.py` `remote_endpoint_operation`: `seq_no |= 0x80`.
pub(crate) const SEQUENCE_FORCED_BIT: u16 = 0x0080;
/// Sequence numbers keep bit 15 clear. `protocol.py`: `& 0x7fff`; `docs/protocol.md` "MSB = 0".
pub(crate) const SEQUENCE_MASK: u16 = 0x7fff;

/// Encode one request: sequence, endpoint with the reply bit, expected reply
/// length, payload and the descriptor-CRC trailer, all little-endian.
///
/// `endpoint` must be nonzero; endpoint 0 uses [`encode_descriptor_read`].
/// Every Sans request sets the reply bit, including zero-length writes.
pub(crate) fn encode_request(
    sequence: u16,
    endpoint: u16,
    expected_reply_len: u16,
    payload: &[u8],
    descriptor_crc: u16,
) -> Vec<u8> {
    debug_assert_ne!(endpoint, DESCRIPTOR_ENDPOINT);
    encode(
        sequence,
        endpoint,
        expected_reply_len,
        payload,
        descriptor_crc,
    )
}

/// Encode one descriptor chunk read at `offset`, with the protocol-version trailer.
pub(crate) fn encode_descriptor_read(sequence: u16, offset: u32) -> Vec<u8> {
    encode(
        sequence,
        DESCRIPTOR_ENDPOINT,
        DESCRIPTOR_CHUNK_REQUEST_LEN,
        &offset.to_le_bytes(),
        PROTOCOL_VERSION,
    )
}

fn encode(sequence: u16, endpoint: u16, expected: u16, payload: &[u8], trailer: u16) -> Vec<u8> {
    let mut packet = Vec::with_capacity(8 + payload.len());
    packet.extend_from_slice(&(sequence & SEQUENCE_MASK).to_le_bytes());
    packet.extend_from_slice(&(endpoint | REPLY_FLAG).to_le_bytes());
    packet.extend_from_slice(&expected.to_le_bytes());
    packet.extend_from_slice(payload);
    packet.extend_from_slice(&trailer.to_le_bytes());
    packet
}

/// Split a reply into its request sequence number and payload.
///
/// Returns `None` for packets too short to carry a sequence number or without
/// the reply bit.
pub(crate) fn decode_reply(packet: &[u8]) -> Option<(u16, &[u8])> {
    let header = u16::from_le_bytes([*packet.first()?, *packet.get(1)?]);
    (header & REPLY_FLAG != 0).then(|| (header & SEQUENCE_MASK, &packet[2..]))
}

/// A decoded property value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum WireValue {
    Bool(bool),
    U8(u8),
    I32(i32),
    U64(u64),
    F32(f32),
}

impl WireValue {
    /// Little-endian encoding of a value to write.
    pub(crate) fn encode(self) -> Vec<u8> {
        match self {
            Self::Bool(value) => vec![u8::from(value)],
            Self::U8(value) => vec![value],
            Self::I32(value) => value.to_le_bytes().to_vec(),
            Self::U64(value) => value.to_le_bytes().to_vec(),
            Self::F32(value) => value.to_le_bytes().to_vec(),
        }
    }

    /// Decode a reply payload; any length other than the type size is malformed.
    pub(crate) fn decode(value_type: ValueType, bytes: &[u8]) -> Option<Self> {
        if bytes.len() != value_type.size() {
            return None;
        }
        Some(match value_type {
            ValueType::Bool => match bytes[0] {
                0 => Self::Bool(false),
                1 => Self::Bool(true),
                _ => return None,
            },
            ValueType::U8 => Self::U8(bytes[0]),
            ValueType::I32 => Self::I32(i32::from_le_bytes(bytes.try_into().ok()?)),
            ValueType::U64 => Self::U64(u64::from_le_bytes(bytes.try_into().ok()?)),
            ValueType::F32 => Self::F32(f32::from_le_bytes(bytes.try_into().ok()?)),
        })
    }
}

/// MSB-first CRC16 with no reflection and no final XOR.
///
/// Mirrors `calc_crc` in `Firmware/fibre/python/fibre/protocol.py`.
pub(crate) fn crc16(initial: u16, bytes: &[u8]) -> u16 {
    let mut remainder = initial;
    for &byte in bytes {
        remainder ^= u16::from(byte) << 8;
        for _ in 0..8 {
            remainder = if remainder & 0x8000 != 0 {
                (remainder << 1) ^ CRC16_POLYNOMIAL
            } else {
                remainder << 1
            };
        }
    }
    remainder
}

/// CRC16 over the raw endpoint-0 descriptor bytes: the trailer of every request
/// to a nonzero endpoint (`Firmware/fibre/cpp/protocol.cpp`
/// `BidirectionalPacketBasedChannel::process_packet`, `fibre::json_crc_`).
pub(crate) fn descriptor_crc16(descriptor: &[u8]) -> u16 {
    crc16(PROTOCOL_VERSION, descriptor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_words_are_named_by_upstream_flag() {
        assert_eq!(
            describe_error_words(0x220, 0x10, 0, 0x1 | 1 << 31),
            "axis MotorDisarmed|ControllerFailed (0x220), motor ControlDeadlineMissed (0x10), \
             encoder none, controller Overspeed|bit31 (0x80000001)"
        );
        // Explicit YAML bits: the axis skips 15 and 16, the motor skips 11.
        assert_eq!(
            describe_error_words(1 << 15 | 1 << 17 | 1 << 18, 1 << 11 | 1 << 12, 0, 0),
            "axis bit15|HomingWithoutEndstop|OverTemp (0x68000), \
             motor bit11|CurrentLimitViolation (0x1800), encoder none, controller none"
        );
    }

    const RECONSTRUCTED_DESCRIPTOR: &[u8] =
        include_bytes!("../../tests/fixtures/odrive/descriptor-RECONSTRUCTED-2026-11-10.json");

    #[test]
    fn crc16_matches_the_protocol_description_examples() {
        // docs/protocol.md, "CRC16": initial value 0x1337 examples.
        assert_eq!(crc16(0x1337, &[0x01, 0x02, 0x03, 0x04]), 0x672E);
        assert_eq!(crc16(0x1337, &[0x05, 0x04, 0x03, 0x02, 0x01]), 0xE251);
    }

    #[test]
    fn descriptor_crc_of_the_reconstructed_descriptor_is_the_reported_trailer() {
        assert_eq!(descriptor_crc16(RECONSTRUCTED_DESCRIPTOR), 39744);
    }

    /// Worked packets built from the "Packet format" section of the fetched
    /// `docs/protocol.md`, with the #57 capture's IDs and trailer 39744 (0x9B40).
    #[test]
    fn worked_packets_encode_byte_for_byte() {
        assert_eq!(
            encode_descriptor_read(0x0080, 0),
            [0x80, 0x00, 0x00, 0x80, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00]
        );
        assert_eq!(
            encode_request(0x0081, 1, 4, &[], 39744),
            [0x81, 0x00, 0x01, 0x80, 0x04, 0x00, 0x40, 0x9B]
        );
        assert_eq!(
            decode_reply(&[0x81, 0x80, 0x00, 0x00, 0xC0, 0x41]),
            Some((0x0081, &[0x00, 0x00, 0xC0, 0x41][..]))
        );
        assert_eq!(
            WireValue::decode(ValueType::F32, &[0x00, 0x00, 0xC0, 0x41]),
            Some(WireValue::F32(24.0))
        );
        assert_eq!(
            encode_request(0x0082, 74, 0, &WireValue::I32(1).encode(), 39744),
            [0x82, 0x00, 0x4A, 0x80, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x40, 0x9B]
        );
        assert_eq!(decode_reply(&[0x82, 0x80]), Some((0x0082, &[][..])));
    }

    #[test]
    fn requests_are_not_replies_and_wrong_sizes_are_malformed() {
        assert_eq!(decode_reply(&[0x82, 0x00, 0x4A, 0x80]), None);
        assert_eq!(decode_reply(&[0x82]), None);
        assert_eq!(WireValue::decode(ValueType::F32, &[0, 0, 0xC0]), None);
        assert_eq!(WireValue::decode(ValueType::Bool, &[2]), None);
        assert_eq!(
            WireValue::decode(ValueType::U64, &54310774846261_u64.to_le_bytes()),
            Some(WireValue::U64(0x3165_3671_3335))
        );
    }
}
