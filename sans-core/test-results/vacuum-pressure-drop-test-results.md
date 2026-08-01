# Vacuum pressure-drop test results

Measured 2026-08-01 on real hardware via `hw_diag` (`sans` repo,
`feature/hw-diag` branch): `PRESS START` streaming (oversampling=2, ~49Hz,
~0.03-0.04 mbar sensor noise) while toggling `VACUUM ON`/`OFF`, logged to
CSV via `--log`. Sequence both times: 3s baseline → `VACUUM ON` → ~5s hold →
`VACUUM OFF` → ~3s recovery → `PRESS STOP`.

## Test 1 — open box, no book

Suction box empty, vacuum pulling against open air/whatever it normally
rests against (no page/book sealing it).

| Phase | Mean pressure | Stdev | n |
|---|---|---|---|
| Baseline (pump off) | 1016.076 mbar | ±0.028 | 23 |
| Pump on (steady-state) | 1015.349 mbar | ±0.037 | 115 |
| Recovered (pump off again) | 1016.122 mbar | ±0.043 | 46 |

**Drop: 0.727 mbar**

## Test 2 — book placed on the suction box

Same sequence, this time with a book/page actually sealing against the box
(real pickup-attempt condition).

| Phase | Mean pressure | Stdev | n |
|---|---|---|---|
| Baseline (pump off) | 1016.179 mbar | ±0.053 | 23 |
| Pump on (steady-state) | 1014.445 mbar | ±0.035 | 160 |
| Recovered (pump off again) | 1016.192 mbar | ±0.037 | 46 |

**Drop: 1.734 mbar**

## Comparison

| | No book | With book | Difference |
|---|---|---|---|
| Drop on `VACUUM ON` | 0.727 mbar | 1.734 mbar | **+1.007 mbar** |

With a book sealing the box, the pump pulls a noticeably deeper partial
vacuum (less air leaking past the seal) than with the box open to ambient
air — **more than double** the pressure drop, and the difference (~1.0 mbar)
is itself roughly 20-30x the sensor noise floor (~0.03-0.05 mbar stdev).
Both conditions recover cleanly back to baseline within ~2s of `VACUUM OFF`.

## Test 3 — manual multi-attempt session (no motor yet)

With no Z-axis motor built yet, ijon manually moved the suction box by hand
and made a whole run of page-turn/pickup attempts (informally, "mostly
successful, ~2 failures") while `PRESS START` streamed continuously across
the *entire* ~65s session — vacuum was switched on once at the start of the
attempts and off once at the end, not toggled per attempt. Full trace
(interactive): https://claude.ai/code/artifact/581413b3-94fa-4a19-a5f8-285092623028

A histogram of the pump-on portion of that trace (t=6-55s) showed **two
clearly separated clusters**, not one:

| Band | Range | Meaning |
|---|---|---|
| ~1016.1 mbar | pump off | before/after the session |
| ~1015.3-1015.45 mbar | pump on, no paper sealed | between/before a grip |
| ~1014.1-1014.3 mbar | pump on, paper sealed | box actually gripping a page |

Segmenting the trace by these thresholds found the signal alternating
cleanly between the "sealed" and "no-paper" bands roughly a dozen times each
over the session — consistent with repeated individual attempts, each with
its own grip/release moment, all landing in one of the two clearly-separated
bands. **The two specific reported failures were not identifiable from the
trace alone** — no timestamp/position marker was recorded per attempt, so
there's no ground truth to correlate against particular dips. This is an
accepted limitation for now, not a data problem: a future calibration pass
that logs the Z-axis position (once the pickup motor exists) alongside the
pressure stream would make this properly attributable per attempt.

## Interpretation

Across all three tests, the same qualitative picture holds and Test 3
sharpens it into a validated **3-state model** rather than a single
before/after drop:

1. **Pump off** — ~1016 mbar (ambient, whatever the room's baseline is)
2. **Pump on, nothing sealed** — ~1015.3-1015.45 mbar (Test 1's "no book"
   condition, and Test 3's "no-paper" band)
3. **Pump on, page sealed** — ~1014.1-1014.3 mbar (Test 2's "with book"
   result, and Test 3's "sealed" band — close agreement between an
   isolated single test and a real multi-attempt session)

This is the physical basis needed for an eventual pickup-success/-failure
threshold in the full application (`sans-serif.md`, not built yet) — states
2 and 3 are separated by roughly 1 mbar, itself ~20-30x the sensor noise
floor (~0.03-0.05 mbar stdev at oversampling=2), so a simple threshold
(e.g. somewhere around 1014.7-1014.8 mbar) should reliably separate
"gripping a page" from "not." Good enough to build against now; a later
calibration run with real Z-position data would make per-attempt
success/failure classification precise rather than inferred from clustering.

## Raw data

CSV logs (timestamp, mbar), alongside this file in the same folder:
- `vacuum-drop-test-no-book.csv` (Test 1)
- `vacuum-drop-test-with-book.csv` (Test 2)
- `vacuum-drop-test-manual-multi-attempt.csv` (Test 3)
