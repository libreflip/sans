# Sans

[![CI](https://github.com/libreflip/sans/actions/workflows/ci.yml/badge.svg)](https://github.com/libreflip/sans/actions/workflows/ci.yml)

Sans is the native touchscreen application for the Libreflip MVPrototype book scanner. One in-process controller thread owns authoritative Machine state and device adapters; the `egui` render loop sends typed intents and renders snapshots without owning machine policy.

## Run

Build and test the active workspace with:

```sh
cargo build --workspace
cargo test --workspace
```

The repository pins Rust 1.98.0 in `rust-toolchain.toml`.

Start the native application with the normal Machine-profile path:

```sh
cargo run -p sans
```

Use `cargo run -p sans -- --config /path/to/sans.toml` for an explicit development or deployment profile. The normal path is `$XDG_CONFIG_HOME/sans/sans.toml`, falling back to `~/.config/sans/sans.toml`.

On first start, Sans writes the parseable [sans.example.toml](sans.example.toml) template to the selected path and opens a fatal screen naming that path. Close Sans, fill every `<...>` placeholder, commission both zero page-width bounds, then restart. A relative `data_root` is resolved against the profile location and must be writable.

The active production workspace contains `sans-app/` and `sans-core/`. The old HTTP server, worker, remote CLI, shared-types crate, and processing stubs are not production workspace members.

## Machine profile limits

The profile is a complete, versioned commissioning file; the scan workflow never edits it. Protocol baud rates and the 3840×2160 MJPEG camera profile stay fixed in code. Structural validation rejects unstable Camera identities, invalid crops or rotations, nonpositive or unordered fields, and commissioned actuation values above the resolved Issue #15 starting anchors. Those 50% Touchdown-press and seven Lift-percentage values are immutable software maxima; profile edits may only lower them. The separately decided Stop-terminal deadline has an immutable 500 ms maximum, and profiles may lower it. Saves are direct truncating complete-file writes and report errors; they make no atomic replacement or storage-synchronization claim.

## Direct diagnostics

`sans-core` retains `hw_diag` and the Linux V4L2 `camcal` binary. Run them only during supervised hardware work while the `sans` application is stopped; they open devices directly, and `hw_diag` can energize relays.
