//! Interactive diagnostic CLI for a `monospace`-protocol board
//! (`monospace.md` §9).
//!
//! Not a one-shot-per-invocation CLI: opening the serial port resets the
//! Arduino (its DTR auto-reset circuit), so re-invoking per command would
//! reset the board mid-sequence. Instead this opens the connection once
//! and stays running as one persistent interactive connection (§9.1).

use chrono::Local;
use clap::Parser;
use csv::Writer;
use sans_core::{MonospaceClient, MonospaceEventKind};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Interactive diagnostic CLI for the monospace relay/BMP180 board.
#[derive(Parser)]
struct Args {
    /// Serial port the board is connected to, e.g. /dev/ttyACM0
    #[arg(long)]
    port: String,

    /// Optional CSV file to log PRESS START streamed readings to
    #[arg(long)]
    log: Option<String>,
}

const BOOT_DELAY: Duration = Duration::from_millis(2500);
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
fn main() {
    let args = Args::parse();

    let csv_writer: Option<Arc<Mutex<Writer<std::fs::File>>>> = args.log.as_ref().map(|path| {
        let file = std::fs::File::create(path).expect("failed to create --log file");
        Arc::new(Mutex::new(Writer::from_writer(file)))
    });

    let connection = MonospaceClient::connect(&args.port, BOOT_DELAY, REPLY_TIMEOUT)
        .expect("failed Monospace readiness gate");
    let mut client = connection.client;
    let events = connection.events;
    std::thread::spawn(move || {
        for event in events {
            let timestamp = Local::now().format("%Y-%m-%dT%H:%M:%S%.3f");
            match event.kind {
                MonospaceEventKind::Pressure(mbar) => {
                    println!("[{timestamp}] PRESS {mbar:.2}");
                    if let Some(writer) = &csv_writer {
                        let mut writer = writer.lock().unwrap();
                        let _ = writer.write_record([timestamp.to_string(), format!("{mbar:.2}")]);
                        let _ = writer.flush();
                    }
                }
                MonospaceEventKind::ButtonPressed => {
                    println!("[{timestamp}] EVENT BUTTON PRESSED");
                }
                MonospaceEventKind::UnknownEvent(payload) => {
                    println!("[{timestamp}] UNKNOWN EVENT {payload}");
                }
                MonospaceEventKind::Disconnected => {
                    eprintln!("[{timestamp}] DISCONNECTED");
                }
                MonospaceEventKind::Fault(fault) => {
                    eprintln!("[{timestamp}] FAULT {fault:?}");
                }
            }
        }
    });

    println!(
        "Connected to {}. Type a command (Ctrl-D to exit).",
        args.port
    );

    let stdin = io::stdin();
    loop {
        print!("> ");
        io::stdout().flush().ok();

        let mut line = String::new();
        if stdin.read_line(&mut line).unwrap_or(0) == 0 {
            break; // Ctrl-D
        }
        // Case-insensitive input is a local convenience only, not a wire
        // protocol change (monospace.md §4 requires uppercase on the wire).
        let cmd = line.trim().to_uppercase();
        if cmd.is_empty() {
            continue;
        }

        // Convenience `led <r> <g> <b>` command (monospace.md §10.4): a thin
        // alias for the wire's `LED SET <r> <g> <b>`, going through the typed
        // client. The raw `LED SET ...` form is deliberately left to fall
        // through to send_raw below, so its ERR BAD_ARGS path stays testable.
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if parts.len() == 4 && parts[0] == "LED" && parts[1] != "SET" {
            match (
                parts[1].parse::<u8>(),
                parts[2].parse::<u8>(),
                parts[3].parse::<u8>(),
            ) {
                (Ok(r), Ok(g), Ok(b)) => match client.set_led(r, g, b) {
                    Ok(()) => println!("OK"),
                    Err(e) => println!("error: {e:?}"),
                },
                _ => println!("error: led expects three 0-255 values, e.g. `led 255 0 0`"),
            }
            continue;
        }

        let result = match cmd.as_str() {
            "VACUUM ON" => client.set_vacuum(true).map(|_| "OK".to_string()),
            "VACUUM OFF" => client.set_vacuum(false).map(|_| "OK".to_string()),
            "FAN ON" => client.set_fan(true).map(|_| "OK".to_string()),
            "FAN OFF" => client.set_fan(false).map(|_| "OK".to_string()),
            "BLOWER ON" => client.set_blower(true).map(|_| "OK".to_string()),
            "BLOWER OFF" => client.set_blower(false).map(|_| "OK".to_string()),
            "LIGHT ON" => client.set_light(true).map(|_| "OK".to_string()),
            "LIGHT OFF" => client.set_light(false).map(|_| "OK".to_string()),
            "ALL OFF" => client.all_off().map(|_| "OK".to_string()),
            "PRESS?" => client.press_once().map(|mbar| format!("OK {mbar:.2}")),
            "PRESS START" => client.start_press_stream().map(|_| "OK".to_string()),
            "PRESS STOP" => client.stop_press_stream().map(|_| "OK".to_string()),
            // Anything unrecognized falls through to the raw wire protocol
            // directly, so typos/lowercase/garbage can still deliberately
            // exercise the firmware's ERR UNKNOWN_COMMAND path.
            other => client.send_raw(other),
        };

        match result {
            Ok(reply) => println!("{reply}"),
            Err(e) => println!("error: {e:?}"),
        }
    }
}
