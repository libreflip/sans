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

Profiles now use schema version 3. To convert a version 2 lift profile, flip
`direction` and set `new_min = -old_max`, `new_max = -old_min`. Leave positive
speed, current and distance magnitudes unchanged. With no lift section, only
the schema number needs changing.

The code currently caps current at 4.8 A, speed at 1 motor turn/s, and
acceleration/deceleration at 1 motor turn/s². These are provisional software
choices, not measured machine limits. Start with settings suitable for your
mechanism; the profile can lower these caps.

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
| `touchdown` | Lower to contact and keep pressing at the profile's press setting. |
| `lift -5` | Lift to absolute position -5 mm, which must be above the current position. |
| `cancel` | Cancel the current operation; a press hold may remain active. |
| `stop` or blank Enter | Stop the lift and switch the air actuators off. |
| `clear-fault` | Clear a Confirmed Stop without arming or resuming motion. |
| `status` or `help` | Show state, position, current, sample age and commands. |
| `quit`, EOF or Ctrl-C | Request Stop and wait for its verdict before exiting. |

## Try it

1. Run `home`, then `arm`, waiting for each to finish. Try a short move and
   check direction and distance before moving farther.
2. Try `touchdown` on some scrap material, then `lift` away. Check that contact
   detection and press hold work, and that the press current can lift the load.
3. Try `cancel`, `stop` and the physical button during motion. After a Confirmed
   Stop, recover with separate `clear-fault`, `home` and `arm` commands. If Stop
   is Unconfirmed, switch off power and check the machine before continuing.

Keep the CSV and a short note of what worked or failed. It already records the
profile, telemetry, commands and Stop results. The 100 ms dispatch and 500 ms
stopped/off targets still need checking on the machine. Physical testing for
#60 remains pending, and production scanning stays disabled.
