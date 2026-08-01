# `sans-core`

Core library for the Libreflip stack, plus small standalone diagnostic
binaries (`src/bin/`) that don't belong in the main `sans-server`/
`sans-worker` runtime.

## `hw_diag` — testing the `monospace` board

`hw_diag` is a diagnostic CLI for manually exercising the `monospace`
Arduino firmware (relay board + BMP180 pressure sensor) over its serial
text protocol (`monospace.md` §4-§6). This is how to test the whole
firmware ↔ `hw_diag` chain end to end, and how to check which physical
actuator each relay command actually drives (see
[libreflip/monospace#4](https://github.com/libreflip/monospace/issues/4)).

### Safety first

Sending `VACUUM ON`, `FAN ON`, or `BLOWER ON` energizes a real actuator on
the machine immediately. Don't run these against a connected board unless
you're present and watching/listening for what happens. `LIGHT ON`/`OFF` is
lower-stakes but still a real relay. There is no harm in `PRESS?`/`PRESS
START`/`PRESS STOP` — those only read the sensor.

### 0. Prerequisite: firmware already flashed

`hw_diag` talks to whatever is currently flashed onto the Arduino. If you
haven't flashed the new firmware yet (`monospace`'s `feature/text-protocol`
branch / PR [#2](https://github.com/libreflip/monospace/pull/2)):

```sh
cd ~/dev/monospace
git checkout feature/text-protocol   # or master, once merged
arduino-cli board list               # confirm which port is the Arduino —
                                      # a second serial device (the ESP32 FOC
                                      # board) is often plugged in too
arduino-cli compile --fqbn arduino:avr:uno bookscanner_control
arduino-cli upload -p /dev/ttyACM0 --fqbn arduino:avr:uno bookscanner_control
```

You can sanity-check the board responds at all before touching `hw_diag`,
using `arduino-cli monitor -p /dev/ttyACM0 -c baudrate=115200` and typing a
command (e.g. `ALL OFF`) by hand — this is the "does it even respond" check,
independent of any Rust code.

### 1. Build `hw_diag`

```sh
cd ~/dev/sans
git checkout feature/hw-diag   # or master, once merged
cargo build --release -p sans-core --bin hw_diag
```

### 2. Run it

```sh
./target/release/hw_diag --port /dev/ttyACM0
# or, to also log streamed pressure readings to a file:
./target/release/hw_diag --port /dev/ttyACM0 --log pressure-test.csv
```

`--port` is always required — it is never auto-detected, specifically
because a second serial device (the ESP32 FOC board) is commonly attached to
the same Pi at the same time, and guessing wrong would send a relay command
to the wrong board.

**This opens the serial connection once and keeps it open for the whole
session** — it is not a one-shot-per-command tool. Opening the port resets
the Arduino (its USB auto-reset), so `hw_diag` waits out that reset's boot
delay, then automatically sends `ALL OFF` (so you always start from a known
state) before showing a `>` prompt. Leave it running and type commands one
at a time; exit with Ctrl-D when done.

### 3. Commands

Type any of these at the prompt (lowercase is fine — `hw_diag` uppercases
input before sending; the wire protocol itself is case-sensitive uppercase
only, per spec):

| Command | Effect |
|---|---|
| `VACUUM ON` / `VACUUM OFF` | Energizes/de-energizes the vacuum pump relay |
| `FAN ON` / `FAN OFF` | Energizes/de-energizes the page-separation fan relay |
| `BLOWER ON` / `BLOWER OFF` | Energizes/de-energizes the turn-blower relay |
| `LIGHT ON` / `LIGHT OFF` | Energizes/de-energizes the light relay |
| `ALL OFF` | Atomically turns vacuum, fan, and blower off (light untouched) |
| `PRESS?` | One averaged pressure reading, prints `OK <mbar>` |
| `PRESS START` | Begins continuous pressure streaming — prints timestamped `PRESS <mbar>` lines as they arrive, interleaved with anything else you type |
| `PRESS STOP` | Stops streaming |

Anything else is sent to the board as-is and will come back as `ERR
UNKNOWN_COMMAND` — useful for deliberately testing that path.

### 4. Checking the relay→actuator mapping

To work out which relay drives which physical actuator (see
[libreflip/monospace#4](https://github.com/libreflip/monospace/issues/4)):
send `VACUUM ON`, listen/watch for which pump or fan actually activates,
then `VACUUM OFF` before moving to the next one. Repeat for `FAN` and
`BLOWER`. Note down what you observe for each — if something doesn't match
`monospace.md` §2's table (`VAC_PUMP`=D4, `PES_PUMP`=D5 "blower", `FAN`=D7),
or an actuator doesn't respond at all, that's exactly what the linked issue
is tracking.

### 5. Streaming + CSV logging

`PRESS START` streams as fast as the firmware can sample (measured ~49Hz on
real hardware at oversampling=2 — comfortably above the ≥5Hz target and
close to the ≤50Hz ideal, with noticeably less per-sample sensor noise than
the faster-but-noisier oversampling=0). If `--log` was given at
startup, every streamed reading is also appended to that file as
`timestamp,mbar` — this is the data you'd use to derive real pickup-success/
-failure pressure thresholds later. The on-screen printout and the CSV file
get the same readings; the CSV is just for keeping more than terminal
scrollback.
