# Repository Guidelines

## Project Structure & Module Organization

This repository is a Rust workspace. `sans-core/` contains shared camera, configuration, and hardware-protocol code; its standalone diagnostics live in `sans-core/src/bin/`. Shared data types are in `sans-core/sans-types/`. `sans-server/` provides the Actix web service and keeps templates and CSS under `static/`. `sans-worker/` hosts background processing, while `sans-ctrl/` builds the `sansctl` development CLI. Integration tests belong in each crate's `tests/` directory; hardware measurements and raw CSV evidence belong in `sans-core/test-results/`. `sans-processing/` exists but is not currently a workspace member.

## Build, Test, and Development Commands

- `cargo build --workspace` builds every active workspace crate.
- `cargo test --workspace` runs unit and integration tests.
- `cargo fmt --all -- --check` checks formatting without rewriting files.
- `cargo clippy --workspace --all-targets -- -D warnings` treats lint findings as failures.
- `cargo run -p sans-server` starts the local HTTP service on `127.0.0.1:8080`.
- `cross build --target armv7-unknown-linux-gnueabihf` mirrors the Raspberry Pi target used by CI and `Cross.toml`.

Run `cargo run -p sans-core --bin hw_diag -- --port /dev/ttyACM0` only during supervised hardware testing. It opens a real serial device and can energize vacuum, fan, blower, and light relays.

## Coding Style & Naming Conventions

Use standard `rustfmt` output and four-space indentation. Name modules, functions, and files with `snake_case`; types and traits with `UpperCamelCase`; constants with `SCREAMING_SNAKE_CASE`. Keep public APIs documented with `///` comments and module-level intent in `//!` comments. Prefer typed protocol methods in `sans-core::hardware` over duplicating raw serial commands.

## Testing Guidelines

Place focused unit tests beside code in `#[cfg(test)]` modules and cross-module tests under `<crate>/tests/`. Use descriptive `snake_case` test names such as `classifies_malformed`. There is no configured coverage threshold; add regression tests for protocol parsing and other deterministic behavior. Record physical observations separately from automated test results.

## Hardware Design Principles

Treat Sans as a hackerspace machine: favor direct, simple, inspectable controls over commercial-style access control. Preserve safety through immutable electrical and motion ceilings, explicit motion commands, zero-PWM stop behavior, applicable soft limits, and supervised physical testing. Keep automated, component-level, and assembled-machine evidence distinct.

## Commit & Pull Request Guidelines

Use Conventional Commits for every Jujutsu change description and Git commit message, for example `feat(scope): add ...` or `docs: update ...`. Keep each change focused and inspect it with `jj status` and `jj diff`. Pull requests should explain behavior and risk, link the relevant issue, list commands run, and call out hardware validation separately. Include screenshots for changes under `sans-server/static/` and attach representative logs or CSVs when hardware behavior changes.

## Agent skills

### Issue tracker

Issues and specs are tracked in this repository's GitHub Issues. See `docs/agents/issue-tracker.md`.

### Triage labels

Use the canonical triage labels `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, and `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

This repository uses a single-context domain-doc layout. See `docs/agents/domain.md`.
