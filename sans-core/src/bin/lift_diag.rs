//! Supervised lift commissioning. Never run alongside Sans or another device owner.

#[path = "lift_diag/evidence.rs"]
mod evidence;
#[path = "lift_diag/policy.rs"]
mod policy;

use std::io::{self, BufRead};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clap::Parser;
use evidence::{Evidence, ObservedOpener};
use policy::{Command, Diagnostic, HELP};
use sans_core::{
    prepare_machine_profile, spawn_link, LinkEvent, LinkHandle, MonospaceClient,
    MonospaceEventKind, StopCause, StopVerdict, UsbFibreOpener,
};

const CURRENT_PRINT_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Parser)]
#[command(about = "Supervised ODrive lift diagnostic. Stop other device owners first.")]
struct Args {
    /// Complete, validated candidate Machine profile. Cameras are not opened.
    #[arg(long)]
    config: PathBuf,
    /// New CSV evidence file; existing files are never overwritten.
    #[arg(long)]
    log: PathBuf,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let prepared = prepare_machine_profile(Some(&args.config))?;
    let profile = prepared.profile();
    let config = sans_core::LinkConfig::from_profile(profile).map_err(|e| e.join("; "))?;
    if profile.odrive.lift_axis.is_none() {
        return Err("commissioning needs explicit candidate [odrive.lift_axis] values".into());
    }
    let origin = Instant::now();
    let (log, writer) = Evidence::start(&args.log, origin)?;
    log.record(
        origin,
        "profile",
        format!(
            "path={}\n{}",
            args.config.display(),
            toml::to_string_pretty(profile)?
        ),
    );
    log.record(
        Instant::now(),
        "session",
        format!(
            "pid={} native Fibre USB; scanning disabled; {HELP}",
            std::process::id()
        ),
    );

    // Both production gates are nonmoving: Monospace confirms ALL OFF and
    // PRESS STOP; ODrive idles and checks compatibility and the board's limits.
    let connection = MonospaceClient::connect(
        &profile.boards.monospace_path,
        Duration::from_millis(2500),
        Duration::from_millis(profile.timeouts.device_open_ms),
        Duration::from_millis(profile.timeouts.command_ms),
    )?;
    let mut board = connection.client;
    let board_events = connection.events;
    log.record(
        Instant::now(),
        "monospace_ready",
        format!(
            "epoch={} ALL OFF and PRESS STOP acknowledged",
            board.epoch().get()
        ),
    );
    let (events, inbox) = mpsc::channel::<LinkEvent>();
    let (handle, runner) = spawn_link(
        config,
        Box::new(ObservedOpener {
            inner: Box::new(UsbFibreOpener),
            evidence: log.clone(),
        }),
        events,
    )?;
    let (urgent_tx, urgent) = mpsc::channel();
    let signals = urgent_tx.clone();
    ctrlc::set_handler(move || {
        let _ = signals.send((Instant::now(), Command::Quit));
    })?;
    let (ordinary_tx, ordinary) = mpsc::sync_channel(8);
    let input_log = log.clone();
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            let at = Instant::now();
            let line = match line {
                Ok(line) => line,
                Err(_) => break,
            };
            // A blank line is a quick Stop command.
            let command = if line.trim().is_empty() {
                Ok(Command::Stop)
            } else {
                line.parse()
            };
            match command {
                Ok(command) if command.urgent() => {
                    if urgent_tx.send((at, command)).is_err() {
                        return;
                    }
                }
                Ok(command) => {
                    if ordinary_tx.try_send((at, command)).is_err() {
                        input_log.record(at, "rejected", "input queue full; command discarded");
                    }
                }
                Err(reason) => input_log.record(at, "rejected", reason),
            }
        }
        let _ = urgent_tx.send((Instant::now(), Command::Quit));
    });

    let mut diagnostic = Diagnostic::new(profile);
    let mut closing = None;
    let mut failed = false;
    let mut board_lost = false;
    let mut shown = diagnostic.status(Instant::now());
    let mut last_current_print = None;
    loop {
        let mut link = link_port(&handle, &log);
        let mut relays = |within| {
            let at = Instant::now();
            log.record(
                at,
                "all_off_dispatch",
                format!("epoch={} budget={within:?}", board.epoch().get()),
            );
            let result = board
                .all_off_within(within)
                .map_err(|error| error.to_string());
            log.record(
                Instant::now(),
                "all_off_result",
                format!(
                    "epoch={} elapsed_ms={:.3} result={result:?}",
                    board.epoch().get(),
                    at.elapsed().as_secs_f64() * 1000.0
                ),
            );
            result
        };

        if log.failed() && !failed {
            failed = true;
            eprintln!("Evidence incomplete. Stopping; do not use this run for acceptance.");
            closing =
                Some(Instant::now() + Duration::from_millis(profile.timeouts.stop_terminal_ms));
            diagnostic.halt(
                Instant::now(),
                StopCause::Uncertain("evidence logging failed or overflowed".into()),
                &mut link,
                &mut relays,
            );
        }
        // Never drain an unbounded queue before checking urgent input again.
        if let Ok((received, command)) = urgent.try_recv().or_else(|_| ordinary.try_recv()) {
            log.record(received, "command_received", format!("{command:?}"));
            if closing.is_some()
                && !matches!(command, Command::Stop | Command::Quit | Command::Status)
            {
                log.record(Instant::now(), "rejected", "shutdown in progress");
            } else {
                let result = diagnostic.command(Instant::now(), command, &mut link, &mut relays);
                log.record(
                    Instant::now(),
                    "command_result",
                    format!("{command:?} {result:?}"),
                );
                if let Err(error) = result {
                    log.record(Instant::now(), "rejected", error);
                }
                if command == Command::Status {
                    log.record(
                        Instant::now(),
                        "status",
                        format!(
                            "{:?}; {}; {HELP}",
                            diagnostic.status(Instant::now()),
                            diagnostic.sample_summary(Instant::now())
                        ),
                    );
                }
                if command == Command::Quit && closing.is_none() {
                    closing = Some(
                        Instant::now() + Duration::from_millis(profile.timeouts.stop_terminal_ms),
                    );
                }
            }
            settle(&mut diagnostic, &log, &mut relays);
        }
        for _ in 0..16 {
            let Ok(event) = board_events.try_recv() else {
                break;
            };
            log.record(Instant::now(), "monospace_event", format!("{event:?}"));
            match event.kind {
                MonospaceEventKind::ButtonPressed => diagnostic.halt(
                    Instant::now(),
                    StopCause::Requested("physical button".into()),
                    &mut link,
                    &mut relays,
                ),
                MonospaceEventKind::Fault(_) | MonospaceEventKind::Disconnected if !board_lost => {
                    board_lost = true;
                    diagnostic.board_lost(Instant::now(), &mut link, &mut relays);
                }
                _ => {}
            }
            settle(&mut diagnostic, &log, &mut relays);
        }
        // A single event per turn bounds input latency even during discovery.
        let wait = diagnostic
            .next_wakeup()
            .map_or(Duration::from_millis(5), |deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(5))
            });
        match inbox.recv_timeout(wait) {
            Ok(event) => {
                let at = Instant::now();
                match &event {
                    LinkEvent::Telemetry(sample) => log.sample(at, sample),
                    LinkEvent::Status(status) => log.record(at, "link", format!("{status:?}")),
                    LinkEvent::CommandResult { .. } => {
                        log.record(at, "command_result", format!("{event:?}"))
                    }
                }
                diagnostic.link_event(at, &event, &mut link);
                if let LinkEvent::Telemetry(sample) = &event {
                    let armed = sample.epoch() == diagnostic.status(Instant::now()).0.epoch
                        && sample
                            .fresh_values(at)
                            .is_some_and(|values| values.axis_state.is_armed());
                    if !armed {
                        last_current_print = None;
                    } else if last_current_print.is_none_or(|last| {
                        at.saturating_duration_since(last) >= CURRENT_PRINT_INTERVAL
                    }) {
                        log.record(at, "current", diagnostic.sample_summary(at));
                        last_current_print = Some(at);
                    }
                }
                settle(&mut diagnostic, &log, &mut relays);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) if closing.is_none() => {
                failed = true;
                closing =
                    Some(Instant::now() + Duration::from_millis(profile.timeouts.stop_terminal_ms));
                diagnostic.halt(
                    Instant::now(),
                    StopCause::LinkLost("ODrive event stream closed".into()),
                    &mut link,
                    &mut relays,
                );
            }
            _ => {}
        }
        diagnostic.poll(Instant::now(), &mut link);
        settle(&mut diagnostic, &log, &mut relays);
        let status = diagnostic.status(Instant::now());
        if status != shown {
            log.record(Instant::now(), "status", format!("{status:?}"));
            shown = status;
        }
        if let Some(deadline) = closing {
            let terminal = shown
                .1
                .as_ref()
                .is_some_and(|stop| stop.verdict != StopVerdict::Confirming);
            if terminal || Instant::now() >= deadline {
                if !shown
                    .1
                    .as_ref()
                    .is_some_and(|stop| stop.verdict == StopVerdict::Confirmed)
                {
                    failed = true;
                    eprintln!("UNCONFIRMED STOP: switch off machine power before leaving.");
                }
                break;
            }
        }
    }
    log.record(
        Instant::now(),
        "end",
        format!(
            "incomplete={} {:?}",
            failed || log.failed(),
            diagnostic.status(Instant::now())
        ),
    );
    drop(runner);
    log.finish();
    failed |= writer.join().is_err() || log.failed();
    if failed {
        Err("diagnostic ended with incomplete evidence or Unconfirmed Stop".into())
    } else {
        Ok(())
    }
}

fn link_port<'a>(
    handle: &'a LinkHandle,
    log: &'a Evidence,
) -> impl FnMut(sans_core::LinkCommand, sans_core::Priority) -> Option<sans_core::Ticket> + 'a {
    move |command, priority| {
        let ticket = handle.submit(command, priority).ok();
        log.record(
            Instant::now(),
            "submit",
            format!("{command:?} {priority:?} ticket={ticket:?}"),
        );
        ticket
    }
}

fn settle(diagnostic: &mut Diagnostic, log: &Evidence, relays: &mut sans_core::Relays<'_>) {
    for event in diagnostic.settle(Instant::now(), relays) {
        log.record(Instant::now(), "outcome", format!("{event:?}"));
    }
}
