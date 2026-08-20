# Sans — MVPrototype Implementation Instructions (Lean)

> Lean, implementer-facing build instructions for `sans`'s **MVPrototype
> scope only**. Self-contained: written to be handed to an implementer
> with no other file access, beyond the cross-referenced `../reference/`
> files for supplementary derivations (named where used).
>
> For rationale, alternatives considered, and the full feature set
> (MVPrototype **and** MVProduct together) see `sans-verbose.md` — not
> repeated here. For *why* this document only covers what it covers, and
> what's deferred, see `../mvprototype-scope.md` (`tasks.md` T36) — also
> not repeated here.
>
> Language: **Rust**. Single application, one job at a time — no message
> broker, no microservices split. Talks to the ESP32 FOC board via the
> G-code protocol in `../ligature.md`, and to the Arduino via the line
> protocol in `../monospace.md` (which also owns the BMP180 pressure
> sensor — no direct RPi-to-sensor I2C connection exists). UI language:
> English only; avoid hard-coding strings in a way that makes adding
> German later a rewrite.
>
> **Governing requirement, applies to every screen in §8:** the machine
> is operated by laypeople with no briefing. Every screen state must
> include an explicit, plain-language instruction of what to do next —
> never a bare preview image or raw data with no accompanying
> instruction.

---

## 1. FOC-board hardware client

Rust client speaking the G-code protocol (`../ligature.md`) over
USB-serial, direct connection to the RPi.

- `arm()` / `disarm()` — `M3`/`M5`.
- `home() -> pulloff_mm` — `G28`. Only ever called after the UI has
  obtained explicit user confirmation (§8 screen 1). Typically returns
  `-2.0`, not a small positive value — upward increases Z, ordinary
  positions are negative.
- `request_override()` (`M52`) / `move_relative(delta_mm)` (`G1 D<mm>`)
  — bring-up/commissioning primitives, not used by any normal
  MVPrototype operation below. Exposed for completeness since they're
  part of the same client; `move_relative()` only works immediately
  after `request_override()`.
- `move_to_top(target_mm)` — a plain `G0` to a small-magnitude negative
  Z near the top soft limit. Refused with `POSITION_UNTRUSTED` if
  position isn't trusted — surface that as a distinguishable error.
  Used by the recovery control (§8) and to park the box at job end,
  before `disarm()`.
- `move_fast(target_mm)` / `move_slow(target_mm)` — `G0`/`G1`, both
  absolute.
- `touchdown(press_percent) -> {settled_mm, press_percent}` — `G30
  [P<press_percent>]`. Contact detection is Hall-progress-based.
- `resume()` — `M24`. Must only be called after `capture_pair()` (§3)
  has actually succeeded, never just falling through in sequence.
- `abort_attempt()` — `M53`. The **routine** way to cancel an in-flight
  move (a failed pickup-check leg, §5). Stops motion only — stays armed
  and trusted, no fault. **Not for emergencies.**
- `stop()` — `M112`. **Genuine emergency stop only.** Disarms and
  latches a fault state; nothing moves again until `clear_fault()` + a
  fresh `arm()`.
- `clear_fault()` — `M999`. Clears the fault state only — does not
  itself re-arm or re-home. The caller must still call `arm()` (and
  `home()` again if trust was lost — check the resulting state) before
  any motion works.
- `status() -> {state, trust, position_mm, velocity_mm_s, current_a, press_percent, active_op, fault}`
  — `state` is the full 15-variant enum; reported trust is
  state-derived, not a separate `homed` boolean. `PROBING` (an active
  `touchdown()` contact search) is distinct from ordinary `MOVING`.
- Listens continuously for the unsolicited `status ...` heartbeat (rate
  via a `set_heartbeat_rate()` wrapper around `M155`) and for
  unsolicited `fault ...` messages — surface the latter to whatever is
  driving the current operation (§5) as equivalent to a Stop.

**Safety invariant callers must uphold:** the suction box must **never**
move downward while the vacuum motor is running, with exactly one
exception — the retreat/"wiggle" leg of the page-turn sequence (§5 step
9). Every other downward move must have vacuum and both blow units
switched off first. Enforced by caller discipline, not the board.

---

## 2. Arduino hardware client

Rust client speaking the line protocol (`../monospace.md`) over a
second, independent USB-serial connection.

- `set_vacuum(bool)`, `set_fan(bool)`, `set_blower(bool)`,
  `set_light(bool)` — boolean-parameter style, not paired on/off
  functions. No `light_auto()` — "auto" light behavior (§8 step 2) is
  host-side sequencing of plain `set_light()` calls.
- `all_off()` — atomically de-energizes vacuum, fan, and blower; light
  untouched.
- `press_once() -> mbar` — single-shot, blocking, averaged. Used
  wherever only one reading is needed (per-attempt baseline, §5 step 1).
- `open(path, baud, boot_delay, on_telemetry)` registers the telemetry
  callback **once**, at connection-open time, for the connection's whole
  lifetime. `start_press_stream()`/`stop_press_stream()` just toggle
  whether the board is actively emitting `PRESS <mbar>` lines; whatever
  arrives while active reaches `on_telemetry` directly, interleaved with
  ordinary command/response traffic (the `PRESS `-prefix framing
  disambiguates it). Used during the pickup-success check (§5 step 9) —
  must always be stopped again afterward, success or failure.
- `set_led(r: u8, g: u8, b: u8)` — sets the status button's RGB ring to
  a raw color, immediate. No blink primitive — blinking (§9) is this
  client's caller calling `set_led()` repeatedly on its own timer.
- `on_button_press` callback, registered the same way as
  `on_telemetry` — once, at `open()` time. Fires once per button press;
  no release event, no query.

---

## 3. Camera capture & preview

- **`capture_pair() -> (raw_left, raw_right)`** — triggers both UVC
  cameras via v4l2 for a synchronized shot. Applies rotation here
  (fixed, hardware-measured constant per camera, may differ left vs.
  right) — callers never see un-rotated images. If a camera fails to
  return a frame: retry once, then surface a hard failure to the caller.
- **`make_preview(raw_image, crop_rect, target_size) -> preview_image`**
  — crop-then-scale. `crop_rect` is always expressed in raw-image pixel
  space (post-rotation) — one consistent convention for every caller.

---

## 4. Page-width entry

MVPrototype-specific — replaces the automated Setup-Calibration
controller (`sans-verbose.md` §4) for this scope (`architecture.md`
AD-013). Runs once per book, before Auto-Scan starts.

- New screen: "Measure the page width with a ruler and enter it in
  millimeters" → on-screen numeric keypad → `page_width_mm: f64`.
- Apply basic range validation (reject zero/negative at minimum; exact
  bounds not specified further here).
- No calibration move, no captured images, no page-number-crop tap
  interaction — all of that is MVProduct (`mvprototype-scope.md`
  §5b/§6), moot for MVPrototype since there's no OCR to feed a crop to.
- Store `page_width_mm` for the session; consumed identically by §5 and
  §7 below regardless of how it was obtained — do not write code in
  either that assumes the value is always operator-typed (e.g. skipping
  validation entirely), since MVProduct's automated measurement will
  feed the same slot later without new plumbing.

---

## 5. Auto-scan flip-cycle controller

Work happens in **page slots**, each spanning one or more pickup
**attempts**. A failed pickup retries locally within the same slot; it
never discards the slot's photo or restarts from the top.

**Safety invariant (binding on every step):** same as §1 above — never
move downward with vacuum running, except the retreat/wiggle leg (step
9 below).

### 5.1 Single pickup attempt

Every call is identical — this operation doesn't know or care whether
it's attempt 1, 2, or 3 for the current slot; that bookkeeping is
entirely the loop driver's (§5.2) job. Input: `page_width_mm` (§4).

1. `press_once()` → `p_baseline`. Read fresh on every attempt, before
   any pneumatics for this attempt engage.
2. `touchdown()` — a real closed-loop descend-until-contact every
   attempt, including retries; never a move to a remembered position.
3. If the light switch is in Auto mode: `set_light(true)`.
4. `capture_pair()` — both cameras, box stationary at the
   touchdown/compressed position. Taken on **every** attempt. Hold
   `img_left`, `img_right`.
5. `resume()` — required before any of the following steps.
6. `set_fan(true)`.
7. If the light switch is in Auto mode: `set_light(false)`.
8. `set_vacuum(true)`.
9. Sequence this attempt's remaining upward motion as several ordinary
   `move_slow()`/`move_fast()` calls, one per leg: (a) up to the
   pickup-check waypoint, (b) if that check passes, down by the retreat
   distance (the wiggle — the one downward leg §5.1 permits with vacuum
   engaged), (c) one or more further legs up to the final target.

   `start_press_stream()` is opened right before leg (a) and consumed
   **continuously across every leg of this step**, success or failure.
   `stop_press_stream()` is called once this step ends, in every case,
   before the next slot's `touchdown()`. Two distinct readings:
   - **`p_leak`** — some drop below `p_baseline` from switching the
     vacuum pump on, even with **no** page held (leaks through the
     suction box). Not a fixed constant — needs empirical
     characterization on real hardware.
   - **Success** — a real vacuum builds (page sealing the cups), the
     reading sits **below** (more negative than) `p_leak`. The exact
     threshold needs empirical tuning (§6's `threshold_mbar_used`
     logging exists for this) — checking only "any drop from
     `p_baseline`" isn't sufficient, `p_leak` alone would satisfy that.

   **Two distinct failure conditions**, either one triggers the same
   abort:
   1. The reading never drops past the success threshold within an
      early window (page never picked up).
   2. The reading **was** past threshold, then **suddenly jumps back**
      toward `p_baseline` at any point during the ascent — page lost
      its seal partway through. Can happen during **any** leg, which is
      why the check must run across all of them, not just once early
      on.

   **On success** (reading stays past threshold, no sudden return): do
   nothing — let the current leg's move call keep running toward its
   target, uninterrupted.

   **On either failure:** call `abort_attempt()` (`M53` — **not**
   `stop()`) to abort whichever leg is in flight immediately. Then
   `set_fan(false)`, `set_vacuum(false)`, then a fresh `touchdown()` for
   the retry. Return `{pickup_result: "failure", images: {img_left,
   img_right}, ambient_mbar: p_baseline, differential_mbar: p_baseline -
   p_check}`. **Stop here on failure** — no further legs are sent, step
   10 doesn't run.

   **Relay timing:**
   - Once leg (b)'s (the wiggle's) `done` response arrives:
     `set_fan(false)`.
   - Once whichever climb leg targets ~80% of page width returns
     `done`: `set_blower(true)`.
   - `set_vacuum(false)` — timing not fixed: natural page separation
     likely happens near 100% of page width on the way up; exact anchor
     (dedicated leg boundary vs. step 9's final leg returning) is an
     open tuning point.
10. Return `{pickup_result: "success", images: {img_left, img_right},
    ambient_mbar: p_baseline, differential_mbar: p_baseline -
    p_deepest, touchdown_position, touchdown_press_percent}`
    (`p_deepest` = lowest reading observed during the continuous
    monitoring above). **The box is held stationary** at wherever step
    9's last leg stopped (102–105% of page width, or the fallback) — it
    does not reverse or descend on its own. The *next* slot's
    `touchdown()` call is what brings it back down; poll `status()`
    while that call is in flight to catch the box passing back down
    through ~90% of page width and call `set_blower(false)` there.

### 5.2 Loop driver (control flow, recording, Stop handling)

For each page slot (identified by the `sequence_number` it will occupy
once stored, §7):

1. Assign the slot's `sequence_number` now, at the start of attempt 1.
   Left camera → even number, right camera → odd number, continuous
   across the whole book (never reassigned, never reused except by
   retries of the same slot).
2. Call the single-attempt operation (§5.1), attempt 1. Keep this
   attempt's `images` result in hand regardless of outcome — this is
   the photo that ultimately gets stored under this slot's
   `sequence_number`, no matter which attempt's pickup actually
   succeeds.
3. Emit this attempt's `ambient_mbar`/`differential_mbar` for live
   display (§6) and logging (§6) — every attempt logs its pressure
   values, not just the first. (No page-number recognition here —
   OCR is MVProduct, `mvprototype-scope.md` §4.)
4. **On success:** persist attempt 1's images (held since step 2) under
   this slot's `sequence_number` (§7). Reset the pickup-failure counter.
   Advance to the next slot.
5. **On failure:** increment the failure counter; at 3, trigger the
   failure dialog (§8). Discard this attempt's images unless it was
   attempt 1 (whose images stay held). If under the limit, retry (back
   to step 2, same `sequence_number`).

**Stop handling:** must interrupt promptly, including mid-attempt —
requires the FOC client's `stop()` (genuine emergency only, not
`abort_attempt()`) and the Arduino client's `all_off()`. An unsolicited
hard-stop fault from the FOC board is treated exactly like a
user-pressed Stop. The physical Start/Stop/E-Stop button (§9) is a
third trigger for this same path, whenever §9's indicator logic
considers the machine to be in its "active" (amber) state. **On Stop or
an unsolicited fault:** halt, leave the machine in the stopped state for
the recovery controls (§8) — the FOC board is latched in its fault
state, so recovery requires the UI to actually call `clear_fault()`
then `arm()` again before any move (including `move_to_top()`) will
work. A slot that was mid-attempt when stopped has **no** stored image —
resuming after a Stop starts that slot over from a fresh attempt 1.

---

## 6. Live diagnostics & pressure log

- **Live display (during Auto-Scan):** current ambient pressure
  (pre-vacuum baseline, reused from §5.1 step 1) and the differential
  values of the last three flip cycles (N-2, N-1, current). Shown
  directly in the main scan screen (§8), not a separate debug screen.
- **`pressure-log.jsonl`** — one entry per pickup **attempt** (every
  attempt, not just the one whose photo gets stored), written into the
  per-session directory (§7). Fields: `ambient_mbar`, `differential_mbar`,
  `pickup_result`, `threshold_mbar_used`, `touchdown_position`,
  `touchdown_press_percent`. MVPrototype: local file only, not uploaded
  — upload is MVProduct (`mvprototype-scope.md` §10).

---

## 7. Local image storage

MVPrototype slice of the eventual Job/Archive manager
(`mvprototype-scope.md` §10) — local storage only, no `metadata.json`,
no formal job ID, no upload, no completeness check, no upload-failure
handling. All of that is MVProduct.

- **Per-session directory:** created when a scan session starts,
  timestamp-named — `<data-root>/<YYYYMMDD-HHMMSS>/`. Placeholder
  scheme, confirmed by ijon (2026-08-18, `mvprototype-scope.md` §5) as
  acceptable and low-stakes to change later, since nothing yet reads
  this path convention beyond this application itself.
- **File naming:** filenames are the continuous `sequence_number` from
  §5.2 (e.g. `0024.jpg`), left camera even, right camera odd — **must be
  implemented exactly this way now**, not approximated (e.g. not
  capture-order-based, not per-camera-separate counters). This is the
  one load-bearing requirement in this whole document
  (`mvprototype-scope.md` §4(c)): get it wrong here and MVProduct's
  Job/Archive manager needs a rename pass across every already-scanned
  book to add itself on top.
- **Per-page bookkeeping:** implement as a struct per page slot
  (`sequence_number`, file path, ...) from the start — not a bare file
  listing (`Vec<PathBuf>` or equivalent) — so `recognized_page_number:
  Option<String>` and a per-book `page_number_crop` can be added later
  as fields, not a restructuring (`mvprototype-scope.md` §4(a)).
- Writes `pressure-log.jsonl` (§6) into the same session directory.

---

## 8. Touchscreen UI flow

1. **Boot/home.** "Machine ready? Box will move up" → explicit confirm
   required before homing runs (never automatic on power-on). Only
   after homing succeeds does the next screen appear. Homing failure
   needs its own error path (see error screens below).
2. **Page-width entry** (§4).
3. **Auto-Scan start.** "Ready to scan?" [Start]. The physical status
   button is green and solid on this screen (§9) — a press here is an
   equivalent alternate trigger for the same [Start] action.
4. **Auto-Scan live screen.** Full-page preview + zoom preview (§3's
   `make_preview`, unscaled 100% crop) + page counter + live pressure
   diagnostics (§6) — [Stop] always visible and reachable.
5. **Scan-completion confirmation.** "Finished scanning this book?"
   [Yes, done] / [Keep scanning].
6. **End.** Images sit locally on disk (§7). No upload/finalization
   screen — upload is MVProduct.

**Error/recovery screens:**
1. **Homing-failure path** (referenced at step 1 above) — needed, not
   further specified here.
2. **Pickup-retry visibility.** A failed pickup attempt retries
   automatically (§5.2) — show a brief visible status during a retry
   ("Retrying this page...") rather than silence.
3. **3-failure choice dialog.** Plain-language explanation ("This page
   couldn't be picked up. Please check it's lying flat.") followed by
   three explicit choices: **[Try again]** (resets the failure counter,
   3 further attempts on the same slot), **[Turn page manually]** (user
   physically turns the page, confirms via button, Auto-Scan resumes;
   attempt 1's already-held photo for this slot gets stored as-is),
   **[Stop job]** (ends Auto-Scan, leads into the Stop/recovery screen
   below).
4. **Stop/recovery screen.** Shown after any Stop. Explains the state in
   plain language: **[Move to top]** (primary, with a short explanation)
   and **[Move down]** (secondary, jog control). A way back into the
   flow once ready — resume, never silent auto-continue.

**Not part of MVPrototype:** cover capture, ISBN/metadata flow,
calibration tap interaction, upload/finalization screen, upload-failure
screen — all MVProduct (`mvprototype-scope.md`).

---

## 9. Physical status button — indicator & input

A single momentary pushbutton with an RGB LED ring, wired to the
Arduino (§2). Owns exactly two responsibilities: (a) continuously
deciding what color/pattern the LED should show, given the rest of this
application's current state, and (b) interpreting each `on_button_press`
event in light of that same state. Holds no state of its own beyond
"what did I last tell the LED."

| Machine state | Color | Pattern | A button press does |
|---|---|---|---|
| Standby — outside any automatically-commanded motion | Blue | Solid | New job — routes to §8 screen 1 (boot/home) |
| Ready to scan — §8 screen 3 | Green | Solid | Start (§8 step 3 — same as tapping [Start]) |
| Automatic motion — **any** host-commanded move: homing, page-width entry has none, Auto-Scan itself, `move_to_top()`, jog moves | Amber | Solid | Stop (§5.2 — same as tapping [Stop]) |
| Stopped, expected — user-initiated Stop, or the 3-failure abort | Red | Slow blink (~1 Hz) | Nothing — recovery is touchscreen-only by design (§5.2) |
| Stopped, unsolicited fault | Red | Fast blink (~4–5 Hz) | Nothing, same as above |

Derive Amber from the FOC client's `status().state` being `Moving` or
`Probing`, **plus** treating the whole Auto-Scan screen (§8 step 4) as
Amber even during its brief non-moving pauses between individual moves
— deriving Amber purely from `state` there would flicker
Amber→Blue→Amber within a single pickup attempt.

**Button-press interpretation, evaluated in this order every time
`on_button_press` fires:**
1. **Amber:** always Stop, regardless of anything else — takes priority
   over every other interpretation.
2. **Green:** Start.
3. **Blue:** New job — routes to §8 screen 1. **Only applies at genuine
   idle** (before the first job of a session, or after a previous job's
   completion, §8 step 6) — stays inert during an already-started job's
   in-between screens (those have their own touchscreen confirmations).
   **This routing target must be a single named reference** ("go to the
   first screen of the job flow"), not hardcoded to screen 1 specifically
   — MVProduct inserts cover-capture and the job-setup/metadata flow
   before this point (`mvprototype-scope.md` §4(e)), and that insertion
   must not require changing this component.
4. **Red:** no defined action — recovery from a genuine stop must stay a
   touchscreen-only, two-step action (`clear_fault()` then `arm()`),
   never a single reflexive button press.

**Implementation note:** presentation/routing logic layered on top of
already-specified components (§1 FOC client status, §2 Arduino client,
§5.2 loop driver, §8 UI screen state) — doesn't own or duplicate any of
their state. A natural home is a small task that polls/subscribes to
"what screen/state are we in" at a short fixed interval (e.g. every
100–200ms) and calls `set_led()` only when the target color/pattern
actually changes, not on every tick.
