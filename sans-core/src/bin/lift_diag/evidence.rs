//! Bounded, asynchronous CSV logging and transparent USB write observations.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use sans_core::{
    FibreConnection, FibreOpener, OpenRequest, PacketReader, PacketWriter, TelemetrySample,
};
use serde::Serialize;

#[derive(Serialize)]
struct Row {
    utc: String,
    elapsed_ms: f64,
    kind: String,
    epoch: Option<u64>,
    sample_age_ms: Option<f64>,
    cycle_ms: Option<f64>,
    position_mm: Option<f32>,
    velocity_mm_s: Option<f32>,
    iq_measured_a: Option<f32>,
    iq_setpoint_a: Option<f32>,
    detail: String,
}

/// No disk or terminal I/O runs on the policy or USB writer threads.
#[derive(Clone)]
pub(super) struct Evidence {
    tx: mpsc::SyncSender<Row>,
    failed: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    origin: Instant,
}

impl Evidence {
    pub(super) fn start(
        path: &Path,
        origin: Instant,
    ) -> io::Result<(Self, thread::JoinHandle<()>)> {
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        let (tx, rx) = mpsc::sync_channel::<Row>(8192);
        let failed = Arc::new(AtomicBool::new(false));
        let worker_failed = Arc::clone(&failed);
        let finished = Arc::new(AtomicBool::new(false));
        let worker_finished = Arc::clone(&finished);
        let worker = thread::Builder::new()
            .name("lift-diag-csv".into())
            .spawn(move || {
                if let Err(error) = write_rows(file, rx, worker_finished) {
                    worker_failed.store(true, Ordering::SeqCst);
                    eprintln!("evidence writer failed: {error}");
                }
            })?;
        Ok((
            Self {
                tx,
                failed,
                finished,
                origin,
            },
            worker,
        ))
    }

    fn row(&self, at: Instant, kind: &str, detail: String) -> Row {
        Row {
            utc: chrono::Utc::now().to_rfc3339(),
            elapsed_ms: at.saturating_duration_since(self.origin).as_secs_f64() * 1000.0,
            kind: kind.into(),
            epoch: None,
            sample_age_ms: None,
            cycle_ms: None,
            position_mm: None,
            velocity_mm_s: None,
            iq_measured_a: None,
            iq_setpoint_a: None,
            detail,
        }
    }

    fn send(&self, row: Row) {
        if self.finished.load(Ordering::SeqCst) {
            return;
        }
        if self.tx.try_send(row).is_err() {
            self.failed.store(true, Ordering::SeqCst);
        }
    }

    pub(super) fn record(&self, at: Instant, kind: &str, detail: impl Into<String>) {
        self.send(self.row(at, kind, detail.into()));
    }

    pub(super) fn sample(&self, at: Instant, sample: &TelemetrySample) {
        let mut row = self.row(at, "sample", String::new());
        row.epoch = Some(sample.epoch());
        row.sample_age_ms = Some(sample.age(at).as_secs_f64() * 1000.0);
        row.cycle_ms = Some(sample.cycle_duration().as_secs_f64() * 1000.0);
        if let Some(values) = sample.fresh_values(at) {
            row.position_mm = values.position_mm;
            row.velocity_mm_s = values.velocity_mm_s;
            row.iq_measured_a = Some(values.iq_measured_a);
            row.iq_setpoint_a = Some(values.iq_setpoint_a);
            row.detail = format!("{values:?}");
        } else {
            row.detail = "stale; values withheld".into();
        }
        self.send(row);
    }

    pub(super) fn failed(&self) -> bool {
        self.failed.load(Ordering::SeqCst)
    }

    pub(super) fn finish(&self) {
        self.finished.store(true, Ordering::SeqCst);
    }
}

fn write_rows(
    file: File,
    rx: mpsc::Receiver<Row>,
    finished: Arc<AtomicBool>,
) -> Result<(), csv::Error> {
    let mut csv = csv::Writer::from_writer(file);
    let mut last_flush = Instant::now();
    loop {
        if finished.load(Ordering::SeqCst) {
            while let Ok(row) = rx.try_recv() {
                csv.serialize(row)?;
            }
            break;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(row) => {
                // Samples and packets stay in CSV; operator messages are sparse.
                if !matches!(
                    row.kind.as_str(),
                    "profile"
                        | "sample"
                        | "usb_write"
                        | "usb_result"
                        | "usb_reply"
                        | "submit"
                        | "command_result"
                ) {
                    println!("[{:.1} ms] {} {}", row.elapsed_ms, row.kind, row.detail);
                }
                csv.serialize(row)?;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if last_flush.elapsed() >= Duration::from_millis(100) {
            csv.flush()?;
            last_flush = Instant::now();
        }
    }
    csv.flush()?;
    Ok(())
}

pub(super) struct ObservedOpener {
    pub(super) inner: Box<dyn FibreOpener>,
    pub(super) evidence: Evidence,
}

impl FibreOpener for ObservedOpener {
    fn open(&mut self, request: &OpenRequest) -> Result<FibreConnection, String> {
        let mut connection = self.inner.open(request)?;
        connection.reader = Box::new(ObservedReader {
            inner: connection.reader,
            evidence: self.evidence.clone(),
        });
        connection.writer = Box::new(ObservedWriter {
            inner: connection.writer,
            evidence: self.evidence.clone(),
        });
        Ok(connection)
    }
}

struct ObservedWriter {
    inner: Box<dyn PacketWriter>,
    evidence: Evidence,
}

impl PacketWriter for ObservedWriter {
    fn write_packet(&mut self, packet: &[u8], timeout: Duration) -> Result<(), String> {
        let at = Instant::now();
        let result = self.inner.write_packet(packet, timeout);
        let returned = Instant::now();
        let hex: String = packet.iter().map(|byte| format!("{byte:02x}")).collect();
        self.evidence.record(at, "usb_write", hex.clone());
        self.evidence.record(
            returned,
            "usb_result",
            format!("packet={hex} result={result:?}"),
        );
        result
    }

    fn shutdown(&mut self, bound: Duration) -> Result<(), String> {
        self.inner.shutdown(bound)
    }
}

struct ObservedReader {
    inner: Box<dyn PacketReader>,
    evidence: Evidence,
}

impl PacketReader for ObservedReader {
    fn read_packet(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, String> {
        let result = self.inner.read_packet(timeout);
        let at = Instant::now();
        if let Ok(Some(packet)) = &result {
            let hex: String = packet.iter().map(|byte| format!("{byte:02x}")).collect();
            self.evidence.record(at, "usb_reply", hex);
        }
        result
    }

    fn shutdown(&mut self, bound: Duration) -> Result<(), String> {
        self.inner.shutdown(bound)
    }
}
