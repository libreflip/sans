# AGENTS.md

This file provides guidance to coding agents, including Claude Code (through the `CLAUDE.md` symlink), when working with code in this repository.

## Project Structure & Module Organization

This repository is a Rust workspace. `sans-app/` builds the native `sans` touchscreen executable; `sans-core/` owns Machine policy, configuration, camera, and hardware-protocol code. Standalone diagnostics live in `sans-core/src/bin/`. Legacy server, worker, CLI, shared-types, and processing directories (`sans-ctrl/`, `sans-server/`, `sans-worker/`, `sans-core/sans-processing/`, `sans-core/sans-types/`) are excluded from the production workspace; leave them untouched unless an issue asks for them. Integration tests belong in each active crate's `tests/` directory; hardware measurements and raw CSV evidence belong in `sans-core/test-results/`.

`docs/plan/sans.md` predates the ODrive migration: wherever it describes the Ligature ESP32 G-code board, the code and `README.md` win. The lift axis is now a stock MKS ODrive Mini, and the motion policy Ligature used to own lives in `sans-core/src/lift.rs`.

## Architecture

- **Controller thread.** `sans_core::bootstrap` spawns one controller thread (`sans-core/src/controller.rs`) that owns all authoritative Machine state and device adapters. The `egui` UI in `sans-app` sends `ControllerIntent`s and renders `ControllerSnapshot`s; it holds no machine policy. The intent set is closed and deliberately has no lift-axis arm, Home, move or clear-error intent.
- **Device seams.** `MachineFactory` and `ControllerMachine` are the controller's hardware boundary. `sans-app/src/main.rs` composes the production factory (V4L2 cameras on Linux, Monospace serial board, ODrive USB opener); tests substitute fakes from `<crate>/tests/support/`. V4L2 camera code is `#[cfg(target_os = "linux")]`, so camera capture code only compiles and runs on Linux.
- **Two boards.** The Monospace Arduino (`sans-core/src/hardware/`) speaks a line-oriented text protocol over serial and drives vacuum, flutter fan, turn blower, lights and the pressure sensor. The ODrive Mini (`sans-core/src/odrive/`) drives the lift axis over native Fibre USB.
- **Sans-I/O cores, thin thread shells.** `odrive::engine::LinkEngine` and `lift::LiftOperations` have no threads, no USB and no clock: callers feed them inputs plus the current time and receive packets, events and wake-up deadlines. `odrive::runner` wraps the engine in engine, reader and writer threads; `odrive::usb` is the `nusb` transport. Put new protocol or motion behaviour in the deterministic core and test it with explicit time; `sans-core/tests/support/fake_fibre.rs` serves a fake board from the captured descriptor in `sans-core/tests/fixtures/odrive/`.
- **Lift-axis write discipline.** `LinkEngine` itself makes the connection gate's writes: it commands the axis idle and writes the commissioned limits. After the gate, `LiftOperations` is the only code that submits `LinkCommand`s on its link; requesting a reconnect is allowed. Urgent commands (cancel, Stop, disarm) overtake queued ordinary ones. Every write the engine accepts from callers is a named typed command; it exposes no raw read or write. `LiftOperations` is not yet called by the application.
- **Machine profile.** `sans-core/src/config.rs` embeds `sans.example.toml` as the first-start template (`include_str!`), so a schema change edits both, and a breaking change bumps `SCHEMA_VERSION`. The `MAX_*` constants there are immutable software ceilings; profile values may only lower them.
- **App as library.** `sans-app/src/lib.rs` holds the application and screens so `egui_kittest` tests drive the same app headlessly; `main.rs` only parses arguments and bootstraps. `sans-app/src/screen.rs` documents the layout contract every screen follows: anything the operator depends on goes in the fixed action bar, with Stop first.

## Build, Test, and Development Commands

- `cargo build --workspace` builds every active workspace crate.
- `cargo test --workspace` runs unit and integration tests. CI adds `--locked` and also runs `cross test` for aarch64.
- `cargo test -p sans-core --test odrive_link` runs one integration-test file; append a test-name filter to run a single test.
- `cargo fmt --all -- --check` checks formatting without rewriting files.
- `cargo clippy --workspace --all-targets -- -D warnings` treats lint findings as failures.
- `SANS_SCREENSHOT_DIR=<dir> cargo test -p sans --test screenshots` writes headless fixture renders as PNGs, for example for PR screenshots.
- `cargo run -p sans` starts the native application with the normal user Machine profile; use `-- --config <path>` for an explicit profile. Add `-v`, `-vv` or `-vvv` to log operator operations, then board commands, then ODrive reads to stderr; errors and warnings always log.
- `cross build --target aarch64-unknown-linux-gnu` mirrors the 64-bit Raspberry Pi 4+ target used by CI.
- `scripts/pi-run [--binary <path>] [--env NAME=VALUE]... <host> [-- <sans-arg>...]` builds the release binary with `cross` unless `--binary` supplies one, uploads it to `~/.local/bin/sans-dev` on the Pi, and runs it there over SSH. `sans-app/tests/pi_run.rs` tests it with fake `cross` and `ssh` on `PATH`. Agents must not run it against a real Pi: like `hw_diag`, it drives real hardware and needs a supervised session.

Run `cargo run -p sans-core --bin hw_diag -- --port /dev/ttyACM0` only during supervised hardware testing while the `sans` application is stopped. It opens a real serial device and can energize vacuum, flutter fan, turn blower, and light relays.

## Coding Style & Naming Conventions

Use standard `rustfmt` output and four-space indentation. Name modules, functions, and files with `snake_case`; types and traits with `UpperCamelCase`; constants with `SCREAMING_SNAKE_CASE`. Keep public APIs documented with `///` comments and module-level intent in `//!` comments. Prefer typed protocol methods in `sans-core::hardware` over duplicating raw serial commands.

## Testing Guidelines

Place focused unit tests beside code in `#[cfg(test)]` modules and cross-module tests under `<crate>/tests/`. Use descriptive `snake_case` test names such as `classifies_malformed`. There is no configured coverage threshold; add regression tests for protocol parsing and other deterministic behavior. Record physical observations separately from automated test results. The `sans` pixel proofs need a wgpu renderer (Metal on macOS, lavapipe on Linux). They skip with a printed notice when no adapter exists; set `SANS_REQUIRE_PIXEL_PROOF=1` wherever a skip must count as a failure. Headless fixture renders are automated evidence, not target-screen evidence.

## Hardware Design Principles

Treat Sans as a hackerspace machine: favor direct, simple, inspectable controls over commercial-style access control. Preserve safety through immutable electrical and motion ceilings, explicit motion commands, zero-PWM stop behavior, applicable soft limits, and supervised physical testing. Keep automated, component-level, and assembled-machine evidence distinct.

Sans leaves the ODrive factory firmware as it is: it calls no save, erase, reboot or DFU function, and writes neither control mode, input mode nor any stored configuration except the connection gate's limits. Fake-Fibre tests demonstrate host behaviour against upstream `fw-v0.5.1` semantics, not vendor-board behaviour; supervised board validation is tracked in #60.

## Commit & Pull Request Guidelines

Use Conventional Commits for every Jujutsu change description and Git commit message, for example `feat(scope): add ...` or `docs: update ...`. Keep each change focused and inspect it with `jj status` and `jj diff`. Pull requests should explain behavior and risk, link the relevant issue, list commands run, and call out hardware validation separately. Include screenshots for touchscreen presentation changes and attach representative logs or CSVs when hardware behavior changes.

## Agent skills

### Issue tracker

Issues and specs are tracked in this repository's GitHub Issues. See `docs/agents/issue-tracker.md`.

### Triage labels

Use the canonical triage labels `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, and `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

This repository uses a single-context domain-doc layout. See `docs/agents/domain.md`.
