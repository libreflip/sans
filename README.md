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

Sans logs to stderr. Errors and warnings, such as a failed Capture pair, a refused Monospace command or an unconfirmed Stop, are always logged. Pass `-v` after Cargo's `--` (`cargo run -p sans -- -v`) for operator operations, Stop changes and lift-axis link changes, `-vv` for every Monospace command and reply and every ODrive command and queued write, and `-vvv` for ODrive reads as well. `RUST_LOG` adds directives on top of that filter, for example `RUST_LOG=sans_core::hardware=debug`; a directive replaces only the one for the module it names, so other modules keep their warnings.

On first start, Sans writes the parseable [sans.example.toml](sans.example.toml) template to the selected path and opens a fatal screen naming that path. Close Sans, fill every `<...>` placeholder, commission both zero page-width bounds, then restart. A relative `data_root` is resolved against the profile location and must be writable.

The profile is schema version 3. A version 2 profile is rejected at startup with the fields to change: remove `current_limit_a`, `velocity_limit_mm_s`, `acceleration_limit_mm_s2` and `deceleration_limit_mm_s2` from `[odrive.lift_axis]`, replace `motion.touchdown_press_percent` with `motion.touchdown_press_current_a` in amps, and set `schema_version = 3`. A version 1 profile is rejected too: remove `boards.ligature_path`, add the `[odrive]` section, then apply the version 2 changes. Move the old file aside and restart Sans to compare against a freshly generated template. There is no rewrite tool, because the new values do not exist in a version 1 file.

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

The profile is a complete, versioned commissioning file; the scan workflow never edits it. Protocol baud rates and the 3840×2160 MJPEG camera profile stay fixed in code. Structural validation rejects unstable Camera identities, invalid crops or rotations, nonpositive or unordered fields, and commissioned actuation values above the resolved Issue #15 starting anchors. Those seven Lift-percentage values are immutable software maxima; profile edits may only lower them. The Touchdown press current has no software maximum: it is bounded by the ODrive's own current limit. The separately decided Stop-terminal deadline has an immutable 500 ms maximum, and profiles may lower it. Saves are direct truncating complete-file writes and report errors; they make no atomic replacement or storage-synchronization claim.

The `[odrive]` section names the lift-axis board by USB serial. Its polling interval, telemetry age and request deadlines are provisional, unmeasured values inside immutable bounds in code. The optional `[odrive.lift_axis]` table holds the mechanism values; while it is absent the lift axis stays blocked and Sans substitutes no default. For now the profile holds no velocity, acceleration, deceleration or current limits, and Sans does not overwrite those ODrive settings: the limits stored on the board apply, so commission them there. **Sans adds no ODrive motion or current limit of its own;** the commissioned travel bounds still apply. The only exception is Touchdown, which lowers the current limit to `motion.touchdown_press_current_a` (amps, refused when above the board's own limit) and restores the board's limit before the next motion. With the table present and the cameras, Monospace and lift-axis link live, the touchscreen offers Home, which moves the axis only after the operator consents. Supervised commissioning (#60) has not validated that motion yet.

Lift-axis millimetres are 0 at the Home reference and increase downward; commission `direction` so this holds. The table also carries ten required tuning values for host lift-axis operations: position tolerance, stationary velocity, move, Home and cancel timeouts, and the Touchdown grace, dwell, no-progress tolerance, saturation percent and hold depth. Each must be positive and finite, the saturation percent at most 100, and the hold depth at most a 5 mm software policy cap. That cap is arbitrary conservative host policy like the others, and every tuning value is provisional until #60 sets measured ones. A table missing any of them fails to parse with the field named. These values drive the core `LiftOperations` module, which the application calls for Home, Stop and Clear Stop.

## ODrive lift axis

Sans talks to the MKS ODrive Mini over its native Fibre USB vendor interface (interface 2, bulk `0x03`/`0x83`) and leaves the CDC serial interfaces to the kernel. On each connection it checks the board's USB serial and `serial_number` against the profile, checks that every endpoint Sans uses exists with the right type and access, and commands the axis idle. It also reads the board's own current limit and trajectory deceleration. It writes no velocity, acceleration or current limits, so the limits stored on the ODrive apply; a Touchdown lowers the current limit to its press current and the next motion restores the board's own limit as read at connection. The cancel braking estimate uses the board's deceleration. Other endpoints may differ between firmware builds without blocking the link. Sans never saves, erases, reboots or flashes the board, and never changes its control or input mode. The Setup screens show the lift-axis status, the discovered identity and descriptor CRC, with a reconnect action in the action bar; the capture preview keeps its full body for the camera views.

Reconnection happens only from the touchscreen's reconnect button. A new connection opens only after the previous one has released the device: each USB half cancels its pending transfers and waits for `nusb` to reap them, and a background reaper keeps trying without reporting release early. Each attempt waits at most the connection gate deadline for that release. The USB open itself is not interrupted; on Linux it only reads sysfs and cached descriptors and claims the interface, and an attempt that does not finish within the gate deadline is reported as blocked.

An urgent command overtakes ordinary commands that have not left the host, so a queued move cannot follow a disarm. A command packet never leaves after its deadline, and an arm, Home or move write never leaves after a link fault, after a disarm is accepted, or once the sample it was accepted on is stale; such commands are retired as not transmitted. A new disarm is always possible while the device is open. Non-finite telemetry faults the link. Ignored replies are counted on the panel and logged to standard error, and a shown sample's age keeps updating, so a silent link never looks fresh.

The Pi user running Sans needs access to the USB device. A udev rule such as the following, in `/etc/udev/rules.d/70-odrive.rules`, grants it to the `plugdev` group:

```text
SUBSYSTEM=="usb", ATTR{idVendor}=="1209", ATTR{idProduct}=="0d32", MODE="0660", GROUP="plugdev"
```

Automated tests run the link against a fake Fibre device served from the captured schema. They show host behaviour against upstream `fw-v0.5.1` semantics, not what the vendor build does on real hardware; the first supervised board run belongs to #60.

## Direct diagnostics

`sans-core` retains `hw_diag` and the Linux V4L2 `camcal` binary. Run them only during supervised hardware work while the `sans` application is stopped; they open devices directly, and `hw_diag` can energize relays.
