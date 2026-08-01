//! Line classifier for the `monospace` text protocol.
//!
//! Protocol reference: `monospace.md` §4-§6. One command per line,
//! `\n`-terminated (an optional preceding `\r` is tolerated on read).
//! Every command gets exactly one response line (`OK`, `OK <mbar>`, or
//! `ERR <reason>`) except while `PRESS START` streaming is active, during
//! which unsolicited `PRESS <mbar>` lines are also emitted. This module is
//! pure/I/O-free so it can be unit-tested without real hardware.

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
    /// Anything that doesn't match one of the above shapes
    Malformed(String),
}

pub fn classify_line(line: &str) -> LineKind {
    let line = line.trim_end_matches('\r');

    if let Some(rest) = line.strip_prefix("PRESS ") {
        return match rest.parse::<f32>() {
            Ok(mbar) => LineKind::Telemetry(mbar),
            Err(_) => LineKind::Malformed(line.to_string()),
        };
    }

    if let Some(rest) = line.strip_prefix("OK ") {
        return match rest.parse::<f32>() {
            Ok(mbar) => LineKind::OkPress(mbar),
            Err(_) => LineKind::Malformed(line.to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
