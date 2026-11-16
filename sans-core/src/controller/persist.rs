//! Page records and the attempt log of a Scan session directory.
//!
//! Each Page record is one quality-95 baseline JPEG under its zero-padded
//! Sequence number. Each Capture attempt appends one line to
//! `pressure-log.jsonl`. Writes go straight to their final paths: no staging,
//! fsync or manifest.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::codecs::jpeg::JpegEncoder;
use image::ExtendedColorType;
use serde::Serialize;

use crate::camera::{CapturePair, CapturedFrame};

/// The attempt log's file name inside a Scan session directory.
pub(super) const ATTEMPT_LOG: &str = "pressure-log.jsonl";

/// The JPEG quality of every Page record.
const JPEG_QUALITY: u8 = 95;

/// How one Capture attempt ended.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AttemptResult {
    /// The page turned and the Capture cycle's Page records follow.
    Turned,
    /// Vacuum ran but no page sealed to the suction box.
    NeverSealed,
    /// The page sealed, then lost its seal before vacuum off.
    SealLost,
    /// The Pressure drop stayed below the Vacuum-working threshold.
    VacuumFault,
    /// The pressure stream gave no verdict in time.
    NoVerdict,
    /// A lift, board or Camera step failed.
    Failed,
    /// Stop ended the attempt.
    #[default]
    Stopped,
}

/// The bounded evidence of one Capture attempt: one JSONL line. A
/// measurement not obtained before the attempt ended is `null`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(super) struct AttemptRecord {
    pub(super) result: AttemptResult,
    pub(super) detail: Option<String>,
    pub(super) left_sequence: u32,
    pub(super) attempt: u32,
    /// When `VACUUM OFF` was acknowledged before the Pressure baseline.
    pub(super) vacuum_off_at: Option<String>,
    /// When `PRESS?` answered with the Pressure baseline.
    pub(super) baseline_at: Option<String>,
    pub(super) baseline_mbar: Option<f32>,
    pub(super) vacuum_working_threshold_mbar: f32,
    pub(super) pickup_threshold_mbar: f32,
    pub(super) touchdown_mm: Option<f32>,
    pub(super) press_current_a: Option<f32>,
    /// When the classifier started, stationary at the classification anchor.
    pub(super) classification_started_at: Option<String>,
    /// When three samples at or above the Pickup threshold acquired the page.
    pub(super) acquired_at: Option<String>,
    /// The lowest streamed pressure while classifying.
    pub(super) lowest_mbar: Option<f32>,
    /// The Pressure drop of the sample that completed the last verdict.
    pub(super) trigger_drop_mbar: Option<f32>,
    /// The page-turn leg that was running when the attempt failed.
    pub(super) failed_leg: Option<String>,
    /// Where a routine cancel stopped the suction box.
    pub(super) stop_position_mm: Option<f32>,
}

/// One save for the Machine thread: an attempt record and, after a turn,
/// the Capture cycle's two Page records.
#[derive(Clone, Debug)]
pub(super) struct SaveRequest {
    pub(super) directory: PathBuf,
    pub(super) record: AttemptRecord,
    /// The canonical pair to write under `record.left_sequence` and the next odd number.
    pub(super) pages: Option<Arc<CapturePair>>,
}

/// Write the pages, Left then Right, then append the record. A failed page
/// write is still logged, as a failed attempt, so every attempt is logged.
pub(super) fn save(request: &SaveRequest) -> io::Result<()> {
    let pages = match &request.pages {
        None => Ok(()),
        Some(pair) => {
            let left = request.record.left_sequence;
            write_page(&request.directory, left, &pair.left)
                .and_then(|()| write_page(&request.directory, left + 1, &pair.right))
        }
    };
    let mut record = request.record.clone();
    if let Err(error) = &pages {
        record.result = AttemptResult::Failed;
        record.detail = Some(format!("cannot save the Page records: {error}"));
    }
    append_attempt(&request.directory, &record)?;
    pages
}

/// The final path of the Page record with `sequence`: four digits at least.
pub(super) fn page_path(directory: &Path, sequence: u32) -> PathBuf {
    directory.join(format!("{sequence:04}.jpg"))
}

/// Encode the full post-rotation frame once and write it to its final name.
fn write_page(directory: &Path, sequence: u32, frame: &CapturedFrame) -> io::Result<()> {
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY)
        .encode(
            frame.rgb8(),
            frame.width,
            frame.height,
            ExtendedColorType::Rgb8,
        )
        .map_err(io::Error::other)?;
    std::fs::write(page_path(directory, sequence), jpeg)
}

fn append_attempt(directory: &Path, record: &AttemptRecord) -> io::Result<()> {
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(ATTEMPT_LOG))?
        .write_all(&line)
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::{CameraRole, CropGeometry};

    fn pair() -> Arc<CapturePair> {
        let frame = |role, value| {
            CapturedFrame::from_rgb8(
                role,
                4,
                6,
                vec![value; 4 * 6 * 3],
                SystemTime::UNIX_EPOCH,
                CropGeometry {
                    x: 0,
                    y: 0,
                    width: 4,
                    height: 6,
                },
            )
            .unwrap()
        };
        Arc::new(CapturePair {
            left: frame(CameraRole::Left, 40),
            right: frame(CameraRole::Right, 200),
        })
    }

    fn request(directory: &Path, left_sequence: u32, pages: bool) -> SaveRequest {
        SaveRequest {
            directory: directory.to_path_buf(),
            record: AttemptRecord {
                result: if pages {
                    AttemptResult::Turned
                } else {
                    AttemptResult::NeverSealed
                },
                left_sequence,
                attempt: 1,
                baseline_mbar: Some(1000.0),
                ..AttemptRecord::default()
            },
            pages: pages.then(pair),
        }
    }

    #[test]
    fn writes_even_left_and_odd_right_baseline_jpegs_and_one_line_per_attempt() {
        let dir = tempfile::tempdir().unwrap();

        save(&request(dir.path(), 0, true)).unwrap();
        save(&request(dir.path(), 2, false)).unwrap();
        save(&request(dir.path(), 2, true)).unwrap();

        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["0000.jpg", "0001.jpg", "0002.jpg", "0003.jpg", ATTEMPT_LOG]
        );
        for (sequence, expected) in [(0, 40), (1, 200)] {
            let bytes = std::fs::read(page_path(dir.path(), sequence)).unwrap();
            // Baseline DCT (SOF0), never progressive (SOF2).
            assert!(bytes.windows(2).any(|w| w == [0xFF, 0xC0]));
            assert!(!bytes.windows(2).any(|w| w == [0xFF, 0xC2]));
            let image = image::load_from_memory(&bytes).unwrap().to_rgb8();
            assert_eq!(image.dimensions(), (4, 6), "full post-rotation frame");
            let value = i16::from(image.get_pixel(1, 1)[0]);
            assert!((value - expected).abs() <= 2, "{sequence}: {value}");
        }
        let log = std::fs::read_to_string(dir.path().join(ATTEMPT_LOG)).unwrap();
        let lines: Vec<serde_json::Value> = log
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["result"], "turned");
        assert_eq!(lines[1]["result"], "never_sealed");
        assert_eq!(lines[1]["left_sequence"], 2);
        assert_eq!(lines[1]["touchdown_mm"], serde_json::Value::Null);
    }

    #[test]
    fn a_failed_page_write_fails_the_save_but_still_logs_the_attempt() {
        let dir = tempfile::tempdir().unwrap();
        // A directory at the Right page's final path makes its write fail.
        std::fs::create_dir(page_path(dir.path(), 1)).unwrap();

        assert!(save(&request(dir.path(), 0, true)).is_err());
        let log = std::fs::read_to_string(dir.path().join(ATTEMPT_LOG)).unwrap();
        let lines: Vec<serde_json::Value> = log
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["result"], "failed", "never logged as turned");
    }
}
