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

The `sans` tests include rendered-pixel proofs that the previews keep every edge of the crop and stay one-to-one at 100%. They render through wgpu and need an adapter: Metal on macOS, or Mesa's lavapipe Vulkan driver on Linux (`sudo apt-get install libvulkan1 mesa-vulkan-drivers`). Without an adapter each proof prints a `SKIPPED pixel proof` notice and passes. Set `SANS_REQUIRE_PIXEL_PROOF=1` to make a missing adapter fail instead, as CI does:

```sh
SANS_REQUIRE_PIXEL_PROOF=1 cargo test -p sans
```

Start the native application with the normal Machine-profile path:

```sh
cargo run -p sans
```

Sans opens maximized with its window decorations, so the desktop decides the usable size after its title bar and menu bar. It respects the desktop's scale factor. On a desktop it runs in a normal resizable window at any size down to 280x240 points. Every screen keeps its status line at the top and its actions, such as Capture pair and Exit, in a bar at the bottom. Everything between scrolls by finger or mouse drag, mouse wheel, or the scroll bar.

Use `cargo run -p sans -- --config /path/to/sans.toml` for an explicit development or deployment profile. The normal path is `$XDG_CONFIG_HOME/sans/sans.toml`, falling back to `~/.config/sans/sans.toml`.

On first start, Sans writes the parseable [sans.example.toml](sans.example.toml) template to the selected path and opens a fatal screen naming that path. Close Sans, fill every `<...>` placeholder, commission both zero page-width bounds, then restart. A relative `data_root` is resolved against the profile location and must be writable.

The profile is schema version 2. A version 1 profile is rejected at startup: remove `boards.ligature_path`, add the `[odrive]` section and set `schema_version = 2`. Move the old file aside and restart Sans to compare against a freshly generated template. There is no rewrite tool, because the new values do not exist in a version 1 file.

The active production workspace contains `sans-app/` and `sans-core/`. The old HTTP server, worker, remote CLI, shared-types crate, and processing stubs are not production workspace members.

## Run on a Raspberry Pi over SSH

`scripts/pi-run` builds Sans for a 64-bit Raspberry Pi, uploads it, and runs it in the foreground on the Pi's touchscreen:

```sh
scripts/pi-run pi@sans-pi.local
scripts/pi-run pi@sans-pi.local -- --config /home/pi/sans-test.toml
scripts/pi-run --binary ./sans --env WAYLAND_DISPLAY=wayland-1 sans-pi
```

The command builds with `cross build --package sans --release --locked --target aarch64-unknown-linux-gnu`, the same recipe as the CI `pi-binary` job, and honors `CARGO_TARGET_DIR`. Pass `--binary <local-path>` to skip the build and upload an existing aarch64 `sans`, such as the binary extracted from a CI `sans-aarch64-<sha>.tar.gz` artifact. Arguments after `--` reach Sans unchanged, and each `--env NAME=VALUE` is set on the Pi before the display is chosen.

The binary is streamed over SSH to `~/.local/bin/sans-dev`, so the production executable is never overwritten. The upload goes to a temporary file beside it; the byte count is checked, and the file is renamed into place only if it matches, so a broken upload leaves the previous `sans-dev` as it was. Sans then runs through `ssh -tt`, and the command exits with Sans's status. Both connections pass `-x`, so a `ForwardX11 yes` SSH setting cannot send the UI to your own display. Build, upload, and display failures stop the command with a nonzero status.

Sans selects the Machine profile on the Pi exactly as it does locally, from `$XDG_CONFIG_HOME/sans/sans.toml` or `~/.config/sans/sans.toml` there. The script never copies, edits, or uploads a profile, and it does not forward your local `XDG_CONFIG_HOME`.

Requirements:

- 64-bit Raspberry Pi OS on a Pi 4 or newer, with the runtime libraries the CI binary links against (`libv4l` and the desktop's Wayland or X11 client libraries).
- An active desktop session on the Pi, owned by the SSH user. An existing `DISPLAY` or `WAYLAND_DISPLAY` is kept. Otherwise the script uses the first `wayland-*` socket in the remote `XDG_RUNTIME_DIR`, skipping lock files. If there is no socket, it stops and suggests `--env WAYLAND_DISPLAY=wayland-0` or `--env DISPLAY=:0`. A Wayland display name such as `wayland-0` resolves inside `XDG_RUNTIME_DIR`, so if the SSH session does not set it, also pass `--env XDG_RUNTIME_DIR=/run/user/<uid>`.
- Locally, `cross` with a running Docker engine, or `--binary`. The cross 0.2.5 images exist only for `linux/amd64`, so the script runs `cross` with `DOCKER_DEFAULT_PLATFORM=linux/amd64`. On Apple Silicon this runs the build under amd64 emulation (OrbStack or Docker Desktop with Rosetta), which is slower than a native build. Podman ignores that variable; on an arm64 host, use Docker or `--binary`.
- `<host>` is passed to `ssh` unchanged: either `user@hostname`, or an SSH config alias whose `HostName` and `User` come from your SSH configuration. Port, identity, and jump hosts also come from that configuration. Key authentication is recommended, because the command connects twice.
- A POSIX login shell on the Pi. The script never uses `sudo`, installs packages, or kills remote processes.

Use the command only during supervised hardware testing. Stop any other Sans instance on the Pi first, because both would open the same serial port and cameras. What happens to Sans on Ctrl-C or when the SSH connection drops has not been validated on hardware, so check that the Machine has reached a safe state before you walk away.

## Machine profile limits

The profile is a complete, versioned commissioning file; the scan workflow never edits it. Protocol baud rates and the 3840×2160 MJPEG camera profile stay fixed in code. Structural validation rejects unstable Camera identities, invalid crops or rotations, nonpositive or unordered fields, and commissioned actuation values above the resolved Issue #15 starting anchors. Those 50% Touchdown-press and seven Lift-percentage values are immutable software maxima; profile edits may only lower them. The separately decided Stop-terminal deadline has an immutable 500 ms maximum, and profiles may lower it. Saves are direct truncating complete-file writes and report errors; they make no atomic replacement or storage-synchronization claim.

The `[odrive]` section names the lift-axis board by USB serial and its commissioned API fingerprint. Its polling interval, telemetry age and request deadlines are provisional, unmeasured values inside immutable bounds in code. The optional `[odrive.lift_axis]` table holds the mechanism values; while it is absent the lift axis stays blocked and Sans substitutes no default. Commissioned limits are bounded by software policy caps of 4.8 A, 1 motor turn per second, and 1 motor turn per second squared for acceleration and deceleration. **These caps are arbitrary conservative host policy, not physical safety qualification.** They are not derived from the mechanism or measured; the 4.8 A figure only matches the current limit observed on the board on 2026-11-10. #60 reviews all three. The caps are in motor units, so a wrong `mm_per_turn` cannot raise motor speed or current. Production scanning stays disabled whatever the profile says.

## ODrive lift axis

Sans talks to the MKS ODrive Mini over its native Fibre USB vendor interface (interface 2, bulk `0x03`/`0x83`) and leaves the CDC serial interfaces to the kernel. On each connection it identifies the board by USB serial, checks the endpoint descriptor and API fingerprint against the profile, commands the axis idle, and writes and reads back the commissioned limits. It never saves, erases, reboots or flashes the board, and never changes its control or input mode. Reconnection happens only from the touchscreen's reconnect button. The lift-axis panel shows the discovered identity, descriptor CRC and API fingerprint for commissioning notes.

The Pi user running Sans needs access to the USB device. A udev rule such as the following, in `/etc/udev/rules.d/70-odrive.rules`, grants it to the `plugdev` group:

```text
SUBSYSTEM=="usb", ATTR{idVendor}=="1209", ATTR{idProduct}=="0d32", MODE="0660", GROUP="plugdev"
```

Automated tests run the link against a fake Fibre device served from the captured schema. They show host behaviour against upstream `fw-v0.5.1` semantics, not what the vendor build does on real hardware; the first supervised board run belongs to #60.

## Direct diagnostics

`sans-core` retains `hw_diag` and the Linux V4L2 `camcal` binary. Run them only during supervised hardware work while the `sans` application is stopped; they open devices directly, and `hw_diag` can energize relays.
