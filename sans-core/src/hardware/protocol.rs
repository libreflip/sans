//! Line classifier for the `monospace` text protocol.
//!
//! Protocol reference: `monospace.md` §4-§6. One command per line,
//! `\n`-terminated (an optional preceding `\r` is tolerated on read).
//! Every command gets exactly one response line (`OK`, `OK <mbar>`, or
//! `ERR <reason>`) except for two kinds of unsolicited line: `PRESS <mbar>`
//! telemetry while `PRESS START` streaming is active, and `EVENT ...` lines
//! (currently only `EVENT BUTTON PRESSED`, §10) emitted on a debounced
//! button press regardless of streaming state. This module is pure/I/O-free
//! so it can be unit-tested without real hardware.

pub const MAX_COMMAND_BYTES: usize = 39;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum EventKind {
    ButtonPressed,
    Unknown(String),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum InvalidCommandLine {
    ControlByte,
    Lowercase,
    TooLong,
}

pub fn validate_command_line(line: &str) -> Result<(), InvalidCommandLine> {
    if line.contains(['\r', '\n', '\0']) {
        return Err(InvalidCommandLine::ControlByte);
    }
    if line.chars().any(char::is_lowercase) {
        return Err(InvalidCommandLine::Lowercase);
    }
    if line.len() > MAX_COMMAND_BYTES {
        return Err(InvalidCommandLine::TooLong);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub enum LineKind {
    /// Bare `OK`
    Ok,
    /// `OK <mbar>` — the reply to `PRESS?`
    OkPress(f32),
    /// `ERR <reason>`
    Err(String),
    /// Unsolicited `PRESS <mbar>` line during streaming
    Telemetry(f32),
    /// Unsolicited `EVENT ...` line — carries the text after `EVENT `
    /// (e.g. `BUTTON PRESSED`, §10). Like telemetry, never a command reply.
    Event(EventKind),
    /// Anything that doesn't match one of the above shapes
    Malformed(String),
}

pub fn classify_line(line: &str) -> LineKind {
    let line = line.trim_end_matches('\r');

    if let Some(rest) = line.strip_prefix("PRESS ") {
        return match parse_finite_pressure(rest) {
            Some(mbar) => LineKind::Telemetry(mbar),
            None => LineKind::Malformed(line.to_string()),
        };
    }

    if let Some(rest) = line.strip_prefix("EVENT ") {
        return LineKind::Event(if rest == "BUTTON PRESSED" {
            EventKind::ButtonPressed
        } else {
            EventKind::Unknown(rest.to_string())
        });
    }

    if let Some(rest) = line.strip_prefix("OK ") {
        return match parse_finite_pressure(rest) {
            Some(mbar) => LineKind::OkPress(mbar),
            None => LineKind::Malformed(line.to_string()),
        };
    }

    if line == "OK" {
        return LineKind::Ok;
    }

    if let Some(reason) = line.strip_prefix("ERR ") {
        return LineKind::Err(reason.to_string());
    }

    LineKind::Malformed(line.to_string())
}

fn parse_finite_pressure(text: &str) -> Option<f32> {
    text.parse::<f32>().ok().filter(|value| value.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_one_uppercase_command_without_control_bytes() {
        assert_eq!(validate_command_line("LED SET 1 20 255"), Ok(()));
        assert_eq!(validate_command_line("PRESS?"), Ok(()));
    }

    #[test]
    fn rejects_command_injection_and_lowercase_wire_data() {
        assert_eq!(
            validate_command_line("VACUUM ON\nBLOWER ON"),
            Err(InvalidCommandLine::ControlByte)
        );
        assert_eq!(
            validate_command_line("VACUUM ON\0BLOWER ON"),
            Err(InvalidCommandLine::ControlByte)
        );
        assert_eq!(
            validate_command_line("vacuum on"),
            Err(InvalidCommandLine::Lowercase)
        );
    }

    #[test]
    fn rejects_commands_that_exceed_the_firmware_buffer() {
        assert_eq!(validate_command_line(&"X".repeat(39)), Ok(()));
        assert_eq!(
            validate_command_line(&format!("{}VACUUM ON", "X".repeat(31))),
            Err(InvalidCommandLine::TooLong)
        );
        assert_eq!(
            validate_command_line(&"Ä".repeat(20)),
            Err(InvalidCommandLine::TooLong)
        );
    }

    #[test]
    fn classifies_ok() {
        assert_eq!(classify_line("OK"), LineKind::Ok);
        assert_eq!(classify_line("OK\r"), LineKind::Ok);
    }

    #[test]
    fn classifies_ok_press() {
        assert_eq!(classify_line("OK 1013.25"), LineKind::OkPress(1013.25));
    }

    #[test]
    fn classifies_telemetry() {
        assert_eq!(classify_line("PRESS 1013.25"), LineKind::Telemetry(1013.25));
    }

    #[test]
    fn classifies_event() {
        assert_eq!(
            classify_line("EVENT BUTTON PRESSED"),
            LineKind::Event(EventKind::ButtonPressed)
        );
        assert_eq!(
            classify_line("EVENT BUTTON PRESSED\r"),
            LineKind::Event(EventKind::ButtonPressed)
        );
        assert_eq!(
            classify_line("EVENT BUTTON RELEASED"),
            LineKind::Event(EventKind::Unknown("BUTTON RELEASED".to_string()))
        );
    }

    #[test]
    fn classifies_err() {
        assert_eq!(
            classify_line("ERR UNKNOWN_COMMAND"),
            LineKind::Err("UNKNOWN_COMMAND".to_string())
        );
    }

    #[test]
    fn classifies_malformed() {
        assert_eq!(
            classify_line("PRESS not_a_number"),
            LineKind::Malformed("PRESS not_a_number".to_string())
        );
        assert_eq!(classify_line(""), LineKind::Malformed("".to_string()));
        assert_eq!(
            classify_line("garbage"),
            LineKind::Malformed("garbage".to_string())
        );
        assert_eq!(
            classify_line("PRESS NaN"),
            LineKind::Malformed("PRESS NaN".to_string())
        );
        assert_eq!(
            classify_line("OK inf"),
            LineKind::Malformed("OK inf".to_string())
        );
    }
}
