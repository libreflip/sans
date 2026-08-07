//! Interactive diagnostic CLI for a `monospace`-protocol board
//! (`monospace.md` §9).
//!
//! Not a one-shot-per-invocation CLI: opening the serial port resets the
//! Arduino (its DTR auto-reset circuit), so re-invoking per command would
//! reset the board mid-sequence. Instead this opens the connection once
//! and stays running as a single persistent interactive session (§9.1).

use chrono::Local;
use clap::Parser;
use csv::Writer;
use sans_core::HwClient;
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
const BAUD: u32 = 115200;

fn main() {
    let args = Args::parse();

    let csv_writer: Option<Arc<Mutex<Writer<std::fs::File>>>> = args.log.as_ref().map(|path| {
        let file = std::fs::File::create(path).expect("failed to create --log file");
        Arc::new(Mutex::new(Writer::from_writer(file)))
    });

    let csv_for_telemetry = csv_writer.clone();
    let mut client = HwClient::open(
        &args.port,
        BAUD,
        BOOT_DELAY,
        move |mbar| {
            let ts = Local::now().format("%Y-%m-%dT%H:%M:%S%.3f");
            println!("[{ts}] PRESS {mbar:.2}");
            if let Some(w) = &csv_for_telemetry {
                let mut w = w.lock().unwrap();
                let _ = w.write_record([ts.to_string(), format!("{mbar:.2}")]);
                let _ = w.flush();
            }
        },
        // Unsolicited board events (currently only `BUTTON PRESSED`, §10),
        // timestamped like the PRESS stream. Not logged to --log: that file
        // is the pressure-stream CSV (§9.1), events would corrupt its shape.
        |event| {
            let ts = Local::now().format("%Y-%m-%dT%H:%M:%S%.3f");
            println!("[{ts}] EVENT {event}");
        },
    )
    .expect("failed to open serial connection");

    // Reconnect-recovery pattern (monospace.md §4): always start from a
    // known state before showing a prompt, in case the board was already
    // left in some state from a previous session.
    if let Err(e) = client.all_off() {
        eprintln!("warning: initial ALL OFF failed: {e:?}");
    }

    println!("Connected to {}. Type a command (Ctrl-D to exit).", args.port);

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
