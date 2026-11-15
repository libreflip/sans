# Lift commissioning, run 16

Supervised run on 2026-11-15, 23:02–23:04 UTC. The operator reports that
interruption worked correctly. This records component evidence for #60;
the assembled-book workflow remains #31.

## Setup

- MKS ODrive Mini, USB serial `316536713335`, hardware `3.6.56`, firmware
  reports `0.0.0`, unreleased. Native Fibre discovery passed with no blockers;
  descriptor length 38157, CRC 39744, 535 endpoints. Exact firmware build is
  unknown. Earlier Hall setup is in the existing identity record; this run's
  samples reported calibrated motor and ready encoder, and Home completed.
- Profile schema 4, normal direction, 5 mm/motor turn, travel −228 to 0 mm.
  Position tolerance 0.5 mm; stationary threshold 1 mm/s. Touchdown grace and
  dwell both 1000 ms, no-progress distance 1 mm, saturation threshold 10%,
  post-contact hold depth 1 mm.
- Discovered board current limit 1.1 A; controller speed limit 10 motor turns/s
  = 50 mm/s, trajectory speed limit 200 turns/s, deceleration 100 turns/s².
  Torque constant 0.05 Nm/A, motor direction −1, velocity gain 0.012;
  firmware torque-mode velocity limiter enabled.
- All three Touchdowns explicitly requested 1.1 A and 50 mm/s. The profile's
  unused default press current was 2 A. The tested setting is 1.1 A.
- Source reviewed at `cb73eaec9f2d791efc8598935416c29ed6fc5722`.
  The CSV did not contain the runner build's commit ID.

## Observed results

Home, Arm, absolute moves to −200 mm and return to 0 completed. Three
Touchdowns completed with torque hold:

| Contact position | Observed hold before next action | Next action |
| --- | --- | --- |
| −224.790 mm | 7.33 s | Physical-button Stop |
| −224.584 mm | 3.94 s | Lift to −200 mm, completed |
| −224.997 mm | 6.34 s | Lift to −200 mm, completed |

All three holds reported zero velocity and no position change at the encoder's
resolution. Setpoint current stayed at 1.1 A. No ODrive error bits, overspeed,
travel exhaustion or unexpected Stop appeared in these three attempts.

Physical-button Stop interrupted two moves and the first completed torque
hold. Position trust remained valid. Each Clear Stop completed without motion;
a subsequent move while idle was refused until an explicit Arm. The final
Quit also produced a Confirmed Stop. The capture contains no standalone
`cancel`, terminal `stop`, or explicit `disarm` command; those are not claimed
as separately measured physical tests here.

| Button event at elapsed s | Idle USB write attempt, ms | ALL OFF dispatch, ms | ALL OFF acknowledgement after dispatch, ms | Confirmed Stop, ms |
| --- | --- | --- | --- | --- |
| 14.231 | 0.157 | 0.065 | 3.123 | 73.556 |
| 33.006 | 0.139 | 0.060 | 3.827 | 93.522 |
| 73.082 | 0.165 | 0.057 | 5.184 | 159.020 |

Times start when the host received the button event, except the acknowledgement
column. Idle writes were decoded from endpoint 74, requested state 1. All
observed host dispatches met 100 ms and Confirmed Stops met 500 ms.

## Telemetry

7356 fresh samples over about 148 seconds: median interval
20.10 ms, about 49.8 Hz;
maximum interval 27.24 ms. Median sample age
5.86 ms, maximum 13.05 ms, below the configured
100 ms freshness threshold. Median poll cycle 5.82 ms.
Across 119116 matched USB requests/replies, median round trip was
0.287 ms and maximum 3.518 ms. No stale samples or
incomplete logging were reported. This was the diagnostic-runner workload;
no camera-load measurement or independent Hall-resolution measurement is claimed.

The 35.9 MB source CSV was analysed locally and removed at the operator's
request. Only this summary is retained. Source SHA-256:
`dfd7ee7442b831b960ef21480926361066d194cd8818cd891e289fa6ca2189f2`.
