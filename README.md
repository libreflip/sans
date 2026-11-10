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

## Run on a Raspberry Pi over SSH

`scripts/pi-run` builds Sans for a 64-bit Raspberry Pi, uploads it, and runs it in the foreground on the Pi's touchscreen:

```sh
scripts/pi-run pi@sans-pi.local
scripts/pi-run pi@sans-pi.local -- --config /home/pi/sans-test.toml
scripts/pi-run --binary ./sans --env WAYLAND_DISPLAY=wayland-1 sans-pi
```

The command builds with `cross build --package sans --release --locked --target aarch64-unknown-linux-gnu`, the same recipe as the CI `pi-binary` job, and honors `CARGO_TARGET_DIR`. Pass `--binary <local-path>` to skip the build and upload an existing aarch64 `sans`, such as the binary extracted from a CI `sans-aarch64-<sha>.tar.gz` artifact. Arguments after `--` reach Sans unchanged, and each `--env NAME=VALUE` is set on the Pi before the display is chosen.

The binary is streamed over SSH to `~/.local/bin/sans-dev`, so the production executable is never overwritten. The upload goes to a temporary file beside it; the byte count is checked, and the file is renamed into place only if it matches, so a broken upload leaves the previous `sans-dev` as it was. Sans then runs through `ssh -tt`, and the command exits with Sans's status. Build, upload, and display failures stop the command with a nonzero status.

Sans selects the Machine profile on the Pi exactly as it does locally, from `$XDG_CONFIG_HOME/sans/sans.toml` or `~/.config/sans/sans.toml` there. The script never copies, edits, or uploads a profile, and it does not forward your local `XDG_CONFIG_HOME`.

Requirements:

- 64-bit Raspberry Pi OS on a Pi 4 or newer, with the runtime libraries the CI binary links against (`libv4l` and the desktop's Wayland or X11 client libraries).
- An active desktop session on the Pi, owned by the SSH user. An existing `DISPLAY` or `WAYLAND_DISPLAY` is kept. Otherwise the script uses the first `wayland-*` socket in the remote `XDG_RUNTIME_DIR`, skipping lock files. If there is no socket, it stops and suggests `--env WAYLAND_DISPLAY=wayland-0` or `--env DISPLAY=:0`.
- Locally, `cross` with a running Docker or Podman engine, or `--binary`.
- Host, user, port, identity, and jump hosts come from your SSH configuration. Key authentication is recommended, because the command connects twice.
- A POSIX login shell on the Pi. The script never uses `sudo`, installs packages, or kills remote processes.

Use the command only during supervised hardware testing. Stop any other Sans instance on the Pi first, because both would open the same serial port and cameras. What happens to Sans on Ctrl-C or when the SSH connection drops has not been validated on hardware, so check that the Machine has reached a safe state before you walk away.

## Machine profile limits

The profile is a complete, versioned commissioning file; the scan workflow never edits it. Protocol baud rates and the 3840×2160 MJPEG camera profile stay fixed in code. Structural validation rejects unstable Camera identities, invalid crops or rotations, nonpositive or unordered fields, and commissioned actuation values above the resolved Issue #15 starting anchors. Those 50% Touchdown-press and seven Lift-percentage values are immutable software maxima; profile edits may only lower them. The separately decided Stop-terminal deadline has an immutable 500 ms maximum, and profiles may lower it. Saves are direct truncating complete-file writes and report errors; they make no atomic replacement or storage-synchronization claim.

## Direct diagnostics

`sans-core` retains `hw_diag` and the Linux V4L2 `camcal` binary. Run them only during supervised hardware work while the `sans` application is stopped; they open devices directly, and `hw_diag` can energize relays.
