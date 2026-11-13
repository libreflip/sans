# Supervised lift commissioning

Issue #60 has two parts: the `lift_diag` runner and a supervised component
run on the actual lift. Automated fake results do not complete the physical
part. Production scanning stays disabled. Assembled-book acceptance is #31.

## Before connecting

Stop Sans, `hw_diag`, `odrivetool` and other device owners. Support the vertical
load before testing idle or Disarm. Keep machine power within reach. USB Stop
is a software command, not a hardware emergency stop. Do not run this unattended.

Use a copy of the normal Machine profile. Fill every placeholder in
`sans.example.toml`, including distinct stable camera identities and positive
page-width bounds. The runner validates the whole profile but never opens
cameras. Set the real Monospace path, ODrive USB serial, axis, measured
`mm_per_turn`, direction and conservative travel bounds. Millimetres start at
Home and increase downward. Include an explicit `[odrive.lift_axis]` table;
there are no substituted limits or tuning values.

Keep the existing ODrive calibration, Hall feedback, native homing, position
control and trapezoidal input configuration. Check native homing speed,
direction, endstop and calibration separately before using Home. The runner
neither tunes nor saves the board, and does not change those modes. The
previous report that native Home works is an operator observation; verify
Home through Sans and retain the result for this run.

For the first supported, unloaded trials, choose the smallest useful current,
speed and travel. An example candidate is 0.5 A, 1 mm/s, 1 mm/s² acceleration
and deceleration, 0.5 mm position tolerance, 0.2 mm/s stationary threshold,
10000 ms move timeout, 30000 ms Home timeout and 1000 ms cancel timeout.
These are starting candidates, not measured machine settings. If that current
cannot support the load, resolve that with the supervisor before motion.
Conversion, direction and travel must come from the mechanism, not this example.

For initial Touchdown trials, candidate host values are 100 ms startup grace,
200 ms dwell, 0.5 mm no-progress tolerance, 90% current saturation and 0.5 mm
hold depth. The existing 50% press setting makes press current half the
commissioned current. Measure whether it can both hold and lift the load.
Start with the template's 20 ms poll, 100 ms maximum age, 100 ms request and
50 ms urgent deadlines. Review these against recorded sample age and cycle
time. Do not relax the immutable caps to make a trial pass.

The immutable software ceilings are 4.8 A, 1 motor turn/s and 1 motor turn/s²
for acceleration and deceleration. Profile limits may only lower them.
They are host policy caps, not a physical qualification of this mechanism.
The Stop terminal budget is at most 500 ms; host dispatch targets 100 ms.

## Run and operate

Create the output directory first and use a new filename for every run:

```sh
mkdir -p sans-core/test-results/commissioning
cargo run -p sans-core --bin lift_diag --locked -- \
  --config /path/to/candidate-sans.toml \
  --log sans-core/test-results/commissioning/lift-YYYY-MM-DD-01.csv
```

Launching performs the production nonmoving gates: Monospace `ALL OFF` then
`PRESS STOP`, and ODrive identity/schema checks, idle and bounded limit setup.
Wait for `Verified` and fresh samples. Nothing arms, homes or moves automatically.
The CLI never switches vacuum, flutter fan or turn blower on. The readiness
gate's current-connection `ALL OFF` acknowledgement supplies vacuum-off evidence;
a board fault or disconnect revokes it and requests cross-board Stop.

| Command | Result |
| --- | --- |
| `home` | Native Home, watched by Sans; a clean completion grants Position trust. |
| `arm` | Enter closed loop; separate from Home and clearing faults. |
| `disarm` | Urgent idle request while no operation is active; loses Position trust. |
| `move 10` | Absolute 10 mm target within commissioned travel. |
| `touchdown` | Bounded descent and automatic press hold using the profile values. |
| `lift 5` | Absolute 5 mm target, required to be above the fresh current position. |
| `cancel` | Routine cancel of the active operation; may retain a live press hold. |
| `stop` or blank Enter | Cross-board Stop, including during motion, hold or collection. |
| `clear-fault` | Clear errors without arming. A latched Stop must first be Confirmed. |
| `status` or `help` | Policy status, fresh position/current/age and available commands. |
| `quit`, EOF, Ctrl-C or SIGTERM | Request cross-board Stop, wait for its verdict, then exit. |

Use `cancel` or `stop` during an operation; `disarm` then reports Busy. A completed
Touchdown continues pressing until a successful lift-away or Stop. No extra
hold command is needed. After Stop, use `clear-fault`, wait for completion,
then separately `home` and `arm`. Nothing resumes an interrupted move.
An Unconfirmed Stop requires manual power intervention; clear-fault refuses it.
After fixing a lost connection, restart the diagnostic and repeat explicit recovery.
Forced process termination, host power loss and cable loss cannot run an exit
handler. Check the mechanism before leaving, even after a normal exit.

## Short physical procedure

1. Record the source commit, supervisor, date, load/support arrangement, board
   identity and available firmware provenance, Hall configuration, calibration,
   endstop behavior and measured conversion/direction. Firmware `0.0.0` alone
   does not identify a vendor build. Retain the CSV discovery/schema fingerprint
   and compatibility result alongside these observations.
2. With power intervention ready, check Home, separate Arm and Disarm. Use short
   moves within known travel. Check direction, distance and the stationary
   completion evidence. Do not deliberately overtravel.
3. Repeat modest moves under representative host load. Inspect polling intervals,
   cycle durations, sample ages, current and position steps. Record Hall resolution,
   latency and values selected for grace, dwell, no-progress and timeouts.
4. Use representative material for bounded Touchdown, hold and lift-away. Record
   false contacts, stiction, press stability and whether press current lifts the
   loaded box. A failed contact or lift-away trial keeps scanning blocked.
5. Exercise routine cancel, Stop during a move, Stop during Touchdown/hold and
   the physical button with the supervisor's agreed load/support arrangement.
   Check stopped and off evidence, then explicit clear-fault, Home and Arm.
   Retest after each fault; never infer physical behavior from the fake board.
6. Compare measured dispatch and terminal evidence against 100 ms and 500 ms.
   Record failures in #60 and relevant #59/#25/#20 follow-ups. Do not silently
   lengthen the contract. Review the physical evidence before any readiness change.

## Read the evidence

The CSV records the complete loaded profile, connection discovery and epochs,
command receipt/submission/results, operation outcomes and policy/Stop status.
`sample` rows contain position, velocity, both currents, sample age, cycle time
and raw state/error/calibration/endstop fields. Stale samples retain their age
but withhold values. Rows have UTC and monotonic milliseconds from process start.

`submit` means host queue submission. `usb_write` is the timestamp immediately
before the production transport write, with the packet as hexadecimal bytes;
`usb_result` records its return, including failure. The wrapper forwards the
production timeout and release behavior unchanged. Rows can arrive out of
timestamp order because USB and policy run on separate threads.
`usb_reply` records received packets, allowing sequence correlation, round-trip
measurements and reconstruction of the descriptor downloaded on this connection.
Use that descriptor's axis `requested_state` endpoint to identify the
idle write payload, little-endian `01000000`, and correlate sequence numbers
with replies/results. Do not count an unrelated telemetry request as Stop
dispatch. Successful write return does not itself prove stopped motion.

Compare `command_received` to that idle write and to `all_off_dispatch` for host
dispatch. `all_off_result` records current-connection acknowledgement and elapsed
time. The first status with Stationary and Confirmed records the fresh stopped/off
verdict. The sample's first-read timestamp is receipt minus `sample_age_ms`.
Cycle duration is a full sequential polling cycle, not a single USB round trip.
Transport write duration and command result latency are separate measurements.

Logging uses a bounded nonblocking queue and a separate writer. Queue overflow
or writer failure requests Stop and marks the run incomplete. Do not accept
partial evidence. Preserve the CSV and a short observation note under
`sans-core/test-results/`; do not mark unperformed trials passed.
