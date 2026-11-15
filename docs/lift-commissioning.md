# Try the lift

`lift_diag` lets you test lift motion and Touchdown without the scan UI.
Run it with someone at the machine, other device-control tools stopped, and
the power switch within reach. Support the lift before disarming it; USB Stop
cannot hold an unpowered vertical load.

## Setup

Use an ODrive with calibration and native homing already working. Copy
[sans.example.toml](../sans.example.toml), fill its required fields, and add
the `[odrive.lift_axis]` section with your mechanism's conversion, direction,
travel bounds and motion settings. Home is 0 mm; downward travel is negative.
Set `direction` so descending reads negative. Camera identities and page-width
bounds still need valid values, though this tool does not open cameras.

The bought 20-tooth T2.5 pulley gives 20 × 2.5 = 50 mm per pulley revolution.
With the 10:1 reduction, use `mm_per_turn = 5.0` for that pulley.
Keep this value positive; `direction` handles the sign.

Profiles now use schema version 4. For positive-down bounds, flip `direction`
and set `new_min = -old_max`, `new_max = -old_min`. Keep already negative-down
bounds unchanged. Version 2 profiles also need the old current/speed limit
fields removed and `motion.touchdown_press_percent` replaced by
`motion.touchdown_press_current_a` in amps; compare with the current template.

Set current, speed and acceleration limits on the ODrive. The tool uses those
board limits. Touchdown uses current/torque mode with the firmware velocity
limiter; its current and optional speed cannot exceed the board's limits. Omitted
speed uses the lower controller/trajectory limit. The firmware may reduce actual
current to enforce that speed, including while stationary.
Contact hold keeps the torque and monitors downward drift against
`touchdown_hold_depth_mm`. Cancel and lift-away restore position control.
Touchdown checks measured travel rather than predicting a stopping point;
the book is intended to stop the descent.
Its descent boundary is `travel_minimum_mm` directly. Hold depth limits drift
after contact and never extends beyond that boundary.

```sh
mkdir -p sans-core/test-results/commissioning
cargo run -p sans-core --bin lift_diag --locked -- \
  --config /path/to/sans.toml \
  --log sans-core/test-results/commissioning/run-01.csv
```

To build, upload and run it on the Pi over SSH:

```sh
scripts/pi-run --bin lift_diag pi@sans-pi.local -- \
  --config /home/pi/sans.toml --log /home/pi/lift-01.csv
```

Those profile and CSV paths are on the Pi; the CSV stays there.

Use a new CSV filename each time. Startup leaves the axis idle and the air
actuators off. Wait for `Verified`, then use `status` to check fresh telemetry.

## Commands

| Command | What it does |
| --- | --- |
| `home` | Run Home and establish Position trust. |
| `arm` | Enable the motor, as a separate action. |
| `disarm` | Idle the motor when no operation is active; loses Position trust. |
| `move -10` | Move down to absolute position -10 mm. |
| `touchdown 1.0 5` | Press at 1 A in torque mode with a 5 mm/s firmware velocity limit. Speed is optional. |
| `lift -5` | Lift to absolute position -5 mm, which must be above the current position. |
| `cancel` | Cancel the current operation; a press hold may remain active. |
| `stop` or blank Enter | Stop the lift and switch the air actuators off. |
| `clear-fault` | Clear a Confirmed Stop without arming or resuming motion. |
| `status` or `help` | Show state, position, current, sample age and commands. |
| `quit`, EOF or Ctrl-C | Request Stop and wait for its verdict before exiting. |

## Try it

1. Run `home`, then `arm`, waiting for each to finish. Try a short move and
   check direction and distance before moving farther.
2. Try `touchdown <amps> [mm/s]` on scrap material, then `lift` away. Check that contact
   detection and press hold work, and that the press current can lift the load.
3. Try `cancel`, `stop` and the physical button during motion. After a Confirmed
   Stop, recover with separate `clear-fault`, `home` and `arm` commands. If Stop
   is Unconfirmed, switch off power and check the machine before continuing.

A crash can leave temporary limits in board RAM. Check the board limits before
restarting a run. Torque mode has no trajectory acceleration ramp; check its
entry, stop distance and overspeed behavior on the machine.

Keep the CSV and a short note of what worked or failed. It already records the
profile, telemetry, commands and Stop results. The 100 ms dispatch and 500 ms
stopped/off targets still need checking on the machine. Physical testing for
#60 remains pending, and production scanning stays disabled.

While armed, the terminal shows measured and setpoint current, position and
sample age about five times a second. The CSV keeps every telemetry sample.
