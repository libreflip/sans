# Sans — Raspberry Pi Application Specification (Verbose)

> **Verbose companion.** This is the full-reasoning version of the
> `sans` spec — every correction, alternative considered, and open
> question, in the order they were resolved. For an implementer who
> just needs to build the current MVPrototype scope, start with
> `sans.md` instead (lean, implementer-facing, MVPrototype-only) and
> come back here only for the "why" behind a given instruction.
> `../mvprototype-scope.md` is where the MVPrototype/MVProduct boundary
> itself is decided and justified (`tasks.md` T36) — this file's content
> covers the full feature set on both sides of that boundary, since it
> predates the split.
>
> Renamed from `sans-serif.md` 2026-08-18 (`tasks.md` T37,
> `architecture.md` AD-012's `sans`/`serif` naming — this file was
> always about `sans`, the RPi app, never about `serif`, which is the
> separate VM/server, see `serif-verbose.md`). Section numbering is
> unchanged from `sans-serif.md`, so earlier architecture decisions
> (`architecture.md` AD-001 through AD-012) that cite it by its old name
> and section numbers still resolve correctly here. The old file itself
> is now a deprecated stub at `../sans-serif.md`, kept for history only.
>
> Target-state spec for the application running on the Raspberry Pi:
> hardware orchestration, image pipeline, calibration, the auto-scan
> loop, metadata acquisition, archive management, and the touchscreen UI.
> Self-contained: written to be handed to an implementer with no other
> file access, beyond the cross-referenced `reference/` files for
> supplementary derivations (explicitly named where used) — those live
> at `../reference/` relative to this file.
>
> Language: **Rust**. Runs as a single application, one job at a time —
> no message broker, no microservices split. Talks to the ESP32 FOC
> board via the G-code protocol in `ligature.md`, and to the Arduino
> via the line protocol in `monospace.md` — which also owns the BMP180
> pressure sensor (physically wired there, not to the RPi; see §1.2 for
> its two read modes). No direct RPi-to-sensor I2C connection exists.
>
> UI language: English only. No localization infrastructure yet, but
> avoid hard-coding strings in a way that makes adding German later a
> rewrite.
>
> **UI toolkit: `egui`/`eframe` (Rust) — a native GUI in this process,
> not a browser or HTTP-served UI. Missing from this file until
> 2026-08-20 (a gap in the original `sans-serif.md` that carried through
> T37 unnoticed — added retroactively once it caused an implementing
> agent to reuse browser-UI code instead).** Decided
> `project-management/decisions.md`, 2026-08-10 — the touchscreen UI is
> a **complete rebuild on `egui`**, evaluated against Slint and Iced as
> the other browserless/Rust-capable candidates, chosen for fitting the
> already-Rust `sans-core` base (no language boundary). Explicitly
> **not** reused: the old `serif` repository's Ember.js frontend (mostly
> unmodified 2017-era Ember-CLI boilerplate plus a merged 7-year-old
> community PR with a working 4-step tutorial — no scan overview, no
> backend wiring, no live preview beyond that,
> `docs/software/code-review.md`'s `serif` finding). That old `serif`
> repo predates and is unrelated to the current meaning of `serif`
> (`architecture.md` AD-012 later reused the name for the VM/server) —
> the dead Ember code has nothing to do with either. Also not a starting
> point: `sans-server`'s `actix-web`/Handlebars stub
> (`reference/sans-code-reusability-review.md` — one static route,
> hardcoded template, no functional API, never a real UI). §8 below
> describes screens in framework-neutral prose (a carryover from before
> this note existed) — read every one of them as `egui` widget/state
> descriptions, not as HTML pages or HTTP routes.
>
> **Provenance note:** some historical decision/review paths cited below
> are not present in this checkout. The requirements reproduced in this
> file are authoritative for `sans`; those citations explain their origin
> and are not required reading for implementation. Verify every claim
> about an existing repository path against the current checkout.

---

## 0. Governing requirement

**The machine is operated by laypeople with no briefing.** Every screen
state handed to the UI must include an explicit, plain-language
instruction of what to do next — never a bare preview image or raw data
with no accompanying instruction. This applies to every screen listed
in §8.

---

## Existing codebase — what to build on

**This is not a from-scratch application.** It extends the existing
`sans` repository (`sans-core` crate, Rust — `github.com/libreflip/sans`)
— decided `architecture.md` AD-001 (2026-08-09): `alexandria` fully
discarded as a runtime base and `sans-core` kept to extend. Verify the
current repo state before writing new
code against any of the facts below — they're accurate as of the cited
reviews/decisions, not guaranteed current.

- **Reuse and extend, already real:**
  - **`sans-core::hardware::HwClient`** (`sans-core/src/hardware/mod.rs`)
    — §1.2's Arduino client. As of 2026-08-02
    (`architecture.md` AD-007, merged `sans` PR #6, commit `beab768`)
    this already implements the granular protocol described in §1.2
    (`set_vacuum`/`set_fan`/`set_blower`/`set_light`, `press_once`,
    streaming, `set_led`, `on_button_press`) and is **hardware-tested**:
    `PRESS START` streaming reached ~49Hz on real hardware (oversampling
    2, target was ≥5Hz), relay-to-actuator mapping verified by ijon.
    Extend/verify this client — don't rewrite it from scratch.
  - **`sans-core/src/bin/hw_diag.rs`** — an existing interactive
    diagnostic CLI built on `HwClient` (`press`, `press-stream` with
    optional CSV logging, `vacuum`/`fan`/`blower`/`light on|off`,
    `all-off` subcommands, `monospace.md` §9.1). Not part of this
    application's screens (§8) — a separate binary in the same crate.
    Useful as a reference for the client's shape and for manual hardware
    checks during development, not something §8's UI calls into.
  - **`sans-core::config`** (`SansConfig`/`ConfigBackend`) — a TOML
    configuration boundary via `serde`. The actual struct fields are
    shaped for the old single-Arduino architecture (`cameras`,
    `http_port`, `img_worker`, a single `hw_port`) — no ESP32 port, no
    calibration parameters
    (`page_width_mm` etc.), no job/sequence data. Its current `save()`
    opens without truncating, so a shorter rewrite can leave stale
    trailing bytes. Reuse the data-model *pattern*, not the write
    implementation verbatim; extend the struct and make persistence
    safe as part of §7's storage work.
- **Needs real modification, not verbatim reuse:**
  - **`sans-core::camera`** (`camera.rs` + `camera/vl_cam.rs`) — §2's
    camera capture. `capture_image()` via `rscam`/V4L2 is real and
    working (hardcoded 3840×2160 MJPG), but per
    `reference/sans-code-reusability-review.md`: no pair/simultaneity
    handling (it's a single-camera method, not a synchronized-pair
    operation like `capture_pair()`), writes JPEGs straight to a
    hardcoded path instead of returning bytes to the caller, no rotation
    handling, and `auto_config` is `unimplemented!()`. All four need
    real changes to match §2's shape. **Whether this code path actually
    works against the real camera hardware is itself unconfirmed**
    (`architecture.md` AD-001, 2026-08-09 decision) — the reference
    images available were reportedly captured with `alexandria`, not
    `sans`. `alexandria`'s camera/image-processing logic (discarded
    entirely as a runtime base, see below) may be read for reference if
    real-hardware issues surface here — read for how the problem was
    solved, not copied as code.
- **Greenfield, no existing code:** the FOC-board client (§1.1) — the
  board and its G-code protocol (`ligature.md`) didn't exist before P1
  (2026-07-04); build it fresh. Following `sans-core::hardware`'s
  established pattern (open serial port, dedicated thread, channel-based
  command/response) keeps it consistent with the Arduino client above —
  not a hard requirement, just the local convention.
- **Not present and not part of this application:** there is currently no
  **`sans-core/src/bin/foc_diag.rs`** in this checkout. If a separate
  diagnostic binary for `ligature` bring-up/commissioning is added later,
  it follows the `hw_diag`/`camcal` same-crate convention but remains
  outside the touchscreen application and the Sans MVPrototype scope.
- **Do not build on these — confirmed dead ends by direct code reading,
  not assumption** (`reference/sans-code-reusability-review.md`,
  `architecture.md` AD-001/AD-007):
  - **`alexandria`** — fully discarded as a runtime base. Node.js/
    Moleculer/NATS microservices architecture, real operational overhead
    (broker process, service discovery) that doesn't fit a single-
    device/single-job model. Camera logic may still be read for
    reference (above); nothing else from it.
  - **The old `serif` repository's Ember.js frontend and
    `sans-server`'s `actix-web`/Handlebars stub** — see this file's
    header ("UI toolkit") for the full reasoning; neither is a
    touchscreen-UI starting point.
  - **`sans-worker`** (3-line stub, `println!("Hello, world!")`),
    **`sans-ctrl`** (CLI argument parsing for a `sans-server` daemon,
    zero subcommands implemented — talks to a server process that does
    nothing, not a hardware-access tool despite the name),
    **`sans-types`** (empty), **`sans-processing`** (empty trait/module
    skeleton) — all confirmed stubs with no behavior to build on.

**One more constraint, easy to violate by accident later:** the core
scan loop (§1–§7) must work fully without network/VM connectivity — no
blocking on reachability checks anywhere in it. Only the metadata-
enrichment flow (§8.1 steps 3–5, §8.3 — MVProduct,
`mvprototype-scope.md` #11c) is allowed to assume internet access
(`architecture.md` AD-004, 2026-07-26: "MVP geht von vorhandener
Internetverbindung aus, keine Fallback-Logik nötig... Gilt nur für
diesen optionalen Anreicherungs-Flow, nicht für den Kern-Scan-Prozess
(HAL/Auto-Scan), der weiterhin offline funktionieren muss").

---

## 1. Hardware client layer

### 1.1 FOC-board client

**Greenfield — no existing RPi-side code for this board (see "Existing
codebase" above); follow `sans-core::hardware`'s serial-thread-channel
pattern for consistency, not as a hard requirement.**

**Substantially rewritten (`architecture.md` AD-011, 2026-08-16) to
match `ligature.md`'s current protocol (§4/§5/§8/§9/§9a/§12.1) — this
section previously described an earlier, less precise version of the
same board. Two changes affect nearly every value this client
exchanges, not just the specific bullets they're mentioned under:**

1. **Coordinate sign flip:** `ligature.md` §6.0 — upward increases Z,
   ordinary positions are **negative**. Every position this client
   sends or receives, and every place in this document that computes a
   target Z from a percentage of page width, must use this convention.
   Not exhaustively re-derived number-by-number in this document; treat
   any concrete Z example elsewhere here as needing a sign check.
2. **State/trust model:** `ligature.md` §5.1's state variants encode
   position trust directly (`*Untrusted`/`*Trusted`/`Ready`/etc.) — there
   is no separate `homed` boolean to track alongside a coarse state.

Rust client speaking the G-code protocol defined in `ligature.md`
over USB-serial, direct connection to the RPi. Exposes to the rest of
this application:

- `arm()` / `disarm()` (`M3`/`M5`, `ligature.md` §5.3)
- `home() -> pulloff_mm` (`G28`, §6.1) — only ever called after the UI
  has obtained explicit user confirmation (§8, screen 1). Typically
  returns `-2.0`, **not** a small positive value.
- `request_override()` (`M52`, §9a) — **new.** Authorizes exactly the
  next accepted `move_relative()` call to bypass the homing requirement
  and soft-limit checks. Needed for the Stage-2 bring-up sequence
  (direction-check before the axis is trustworthy yet) — not part of
  normal Auto-Scan operation. Consumed on acceptance, not on send;
  re-issuable if the following move is rejected.
- `move_relative(delta_mm)` (`G1 D<mm>`, §9a) — **new**, the *only*
  relative-move primitive this protocol has, and only usable
  immediately after `request_override()`. Ordinary operation never uses
  this — see `move_fast()`/`move_slow()` below for everything else.
- `move_to_top(target_mm)` (a plain `G0` to a small-magnitude negative
  Z near the top soft limit — no dedicated board-side command, see
  `ligature.md` §10) — refused upstream with `POSITION_UNTRUSTED`
  (§4.2's taxonomy — supersedes the earlier generic "not homed" framing)
  if position isn't trusted; this client surfaces that as a
  distinguishable error, not a generic failure. Used both by the
  touchscreen recovery control (§8) and to park the box at job end,
  before `disarm()`.
- `move_fast(target_mm)` / `move_slow(target_mm)` (`G0`/`G1` — both
  always absolute; the *only* relative form is `move_relative()` above,
  and only under an active override)
- **Removed (AD-010, 2026-08-16): `complete_page_turn()`.** There is no
  composite board-side page-turn command (`ligature.md` §9 retired
  `G73`) — it gave the board process-sequencing responsibility (when to
  wiggle, when the turn is "done") that belongs here, on the host, per
  hrmny's correctly-flagged objection that the driver had become
  smarter than intended. **Replacement:** this client exposes only
  `move_fast()`/`move_slow()` (above) — the caller (§5.2) sequences the
  page-turn itself as a series of ordinary calls to those, one per leg
  (pickup-check waypoint, retreat/"wiggle", then one or more legs up to
  the final target broken at whatever waypoints its own relay timing
  needs). Each call's return *is* the synchronization point for driving
  the Arduino relays — no position-crossing event or `status()`-polling
  race needed for that anymore. Every leg can still be cancelled
  mid-flight by calling **`abort_attempt()`, not `stop()`**
  (`ligature.md` §9) — the caller does this when its own concurrent
  pickup-success check fails; the board has no concept of pickup
  success/failure itself, it's purely a motion command. A cancelled leg
  does **not** include the following descent — the *next* `touchdown()`
  call handles that, same as before.
- `touchdown(press_percent) -> {settled_mm, press_percent}` (`G30
  [P<press_percent>]`, §8) — **corrected: reports settled position and
  the commanded normalized press level (0–100%), not a
  `{stop_position_mm, compression}` pair.** There is no calibrated
  force/compression value anymore (§12.1, `M42` removed) — `compression`
  is gone from this return type, full stop, not renamed. Contact
  detection is now Hall-progress-based, not velocity-sag-based (§8.1) —
  irrelevant to this client's interface, but relevant if tuning
  detection parameters via the diagnostic tool.
- `resume()` (`M24`) — must only be called after `capture_pair()` (§2)
  has actually succeeded, never just falling through in sequence
- `abort_attempt()` (`M53`, `ligature.md` §5.3/§9) — the **routine** way
  to cancel an in-flight move (specifically one of step 9's page-turn
  legs on a failed pickup check, above). Stops motion only — stays armed
  and trusted, no fault, no re-arming needed; the caller sends a fresh
  `touchdown()` afterward for the retry. **Not for genuine emergencies**
  — see `stop()` below.
- `stop()` (`M112`) — **genuine emergency stop only** (Stop-button
  presses, unsolicited faults, §5.3) — never for routine pickup-failure
  handling, that's `abort_attempt()` above. Disarms and puts the board
  into a latched fault state (`FaultUntrusted`/`FaultTrusted`, §5.1);
  nothing moves again until `clear_fault()` + a fresh `arm()`.
- `clear_fault()` (`M999`, `ligature.md` §5.4) — clears the fault state
  `stop()` latches. Does **not** itself re-arm or re-home; the caller
  must still call `arm()` (and `home()` again if trust was lost — check
  the resulting state, don't assume) before any motion works. Two
  separate calls, deliberately — recovering from a genuine stop should
  never be a single reflexive action.
- `status() -> {state, trust, position_mm, velocity_mm_s, current_a, press_percent, active_op, fault}`
  (`?`, `ligature.md` §5.1/§12.2) — **corrected return shape:** `state`
  is the full 15-variant enum (§5.1), reported trust is state-derived,
  not a separate `homed` boolean; `current_a` is measured q-axis current
  in amperes (`IQ`), **not** a `torque`/force value (that conversion no
  longer exists, §12.1); the earlier `mode` field (drive mode, §11) is
  **dropped** — §11 itself is flagged unconfirmed/possibly-superseded in
  `ligature.md`, don't build a client dependency on it. `PROBING` (a
  `touchdown()`'s active contact-search) remains distinct from ordinary
  `MOVING` — callers that care about *why* the box is moving should
  check for this, not treat all motion alike.
- Also listens continuously for the unsolicited `status ...` heartbeat
  (`ligature.md` §4.4/§12.2, rate configurable via a
  `set_heartbeat_rate()` wrapper around `M155`) alongside the `fault
  ...` messages below. **Corrected (2026-08-16):** the heartbeat no
  longer auto-accelerates during `PROBING` — this client (or its caller)
  must request whatever rate it needs itself. Used by the diagnostic
  tool (`foc_diag`, `ligature.md` §16) and for tuning `touchdown()`'s
  detection parameters; also requested during step 9's page-turn
  sequence (§5.2) for diagnostic/tuning visibility, even though the
  relay timing itself no longer depends on it (AD-010).
- Calibration triggers (`M40`/`M41`, §12.1 — **`M42` removed
  2026-08-16**, force calibration no longer exists) — exposed for a
  dedicated calibration entry point, not part of the normal scan flow
  (§8 has no screen for this yet; needs one). Both produce volatile RAM
  candidates only (§12.1) — this client does **not** assume they persist
  across a board reset; promoting a candidate into the board's compiled
  configuration is a manual, out-of-band operator action (edit source,
  rebuild, reflash), not something this client does.
- Listens continuously for unsolicited `fault ...` messages and
  surfaces them to whatever is driving the current operation
  (§5's Auto-Scan loop, in particular) as equivalent to a Stop.

**Safety invariant this client's callers must uphold:** the suction box
must never move downward while the vacuum motor is running, with
exactly one exception — the retreat/"wiggle" leg of the page-turn
sequence (§5.2, formerly `complete_page_turn()`'s embedded retreat step).
Every other downward move must have vacuum and both blow units switched
off first. This is currently enforced by caller discipline (§5), not by
the board itself.

### 1.2 Arduino client

Rust client speaking the line protocol defined in `monospace.md` over
USB-serial (a second, independent serial connection from the FOC
board's). The existing `HwClient` command surface is the starting point,
but its current `open()` waits for boot without sending `ALL OFF`; after
every open or reconnect, Sans must successfully establish that safe state
before accepting any other board command. **Corrected 2026-08-02** to
match the actual merged implementation (`sans` repo,
`sans-core/src/hardware/mod.rs`,
`HwClient`) — an earlier version of this section described a
speculative API (paired `x_on()`/`x_off()` functions, `light_auto()`,
a stream call returning a `PressureStream` object) that was never
checked against what actually got built. Exposes:

- `set_vacuum(bool)`, `set_fan(bool)`, `set_blower(bool)`,
  `set_light(bool)` — boolean-parameter style, not paired on/off
  functions. **No `light_auto()`** — the firmware never implements
  `LIGHT AUTO` (excluded from MVP, `monospace.md` §5); "auto" light
  behavior described in §5.2 below is host-side sequencing of plain
  `set_light(true)`/`set_light(false)` calls, not a firmware mode.
- `all_off()` (`ALL OFF`) — atomically de-energizes vacuum, fan, and
  blower; light untouched.
- `press_once() -> mbar` (`PRESS?`, `monospace.md` §5/§6) —
  single-shot, blocking, averaged for accuracy. Used wherever only one
  reading is needed (calibration baseline, §4 step 5; per-attempt
  baseline, §5.2 step 1).
- **Streaming is callback-based, not a returned stream object:**
  `open(path, baud, boot_delay, on_telemetry, on_button_press)` registers
  both callbacks **once**, at connection-open time, for the connection's
  whole lifetime — there is no separate "start streaming, get a handle
  back" call. `start_press_stream()`/`stop_press_stream()`
  (`PRESS START`/`PRESS STOP`) just toggle whether the *board* is
  actively emitting `PRESS <mbar>` lines; whatever arrives while active
  reaches `on_telemetry` directly, interleaved with ordinary
  command/response traffic on the same connection
  (`monospace.md` §4's `PRESS `-prefix framing is what makes the
  interleaving unambiguous — implemented in `protocol::classify_line`,
  not something callers need to reimplement). Used only during the
  pickup-success check across step 9's page-turn legs (§5.2) and during
  the calibration ascent (§4 steps 7–9) — must
  always be stopped again afterward (success or failure), even though
  the callback itself stays registered for the connection's lifetime.

- **Added 2026-08-08 (`monospace.md` §10, `architecture.md` AD-009):
  `set_led(r: u8, g: u8, b: u8)`** (`LED SET <r> <g> <b>`) — sets the
  status button's RGB ring to a raw color, takes effect immediately.
  There is no `set_led_blink()` or similar — blinking is this client's
  caller (§11) calling `set_led()` repeatedly on its own timer; the
  Arduino has no concept of a blink mode (`monospace.md` §10.3).
- **Added 2026-08-08: button presses arrive via a second callback,**
  `on_button_press`, registered the same way as `on_telemetry` — once,
  at `open()` time, for the connection's whole lifetime. Fires once per
  `EVENT BUTTON PRESSED` line (`monospace.md` §10.2); there is no
  release event and no query. `protocol::classify_line` (already
  distinguishing `PRESS ` telemetry from ordinary responses) gets a
  second prefix to recognize, `EVENT `, routing to this callback instead
  of `on_telemetry` — same interleaving mechanism, one more prefix.

**Correlation note (residual from AD-001's original reasoning,
`architecture.md`):** pressure now arrives over this Arduino connection
instead of an RPi-local I2C read, so the pickup-success check (§5.2 step
9) and the FOC board's `status()` polling (§1.1, used for relay timing
in the same window) run over two independent serial links with their
own latency, not perfectly synchronous. This is a deliberate trade-off
against the original single-process/I2C design (AD-001), not a fact to
lose track of — nothing in the current design needs sub-tens-of-
milliseconds alignment between the two (the pressure check reacts to
its own stream independently of position), but if that ever changes,
this is where the assumption would break.

---

## 2. Camera capture & preview service

**Starting point: `sans-core::camera` — needs real modification, not
verbatim reuse (see "Existing codebase" above).**

- **`capture_pair() -> (raw_left, raw_right)`** — triggers both UVC
  cameras via v4l2 for a synchronized shot. **Applies rotation here**
  (fixed, hardware-measured constant per camera, may differ left vs.
  right if mirror-mounted) — callers never see un-rotated images. If a
  camera fails to return a frame (disconnected, timeout, corrupt read):
  retry once, then surface a hard failure to the caller. This runs
  unattended across potentially hundreds of shots per book; a silent
  bad/missing image is worse than a loud failure.
- **`make_preview(raw_image, crop_rect, target_size) -> preview_image`**
  — crop-then-scale, used identically by the calibration preview, the
  live full-page preview, and the live zoom preview (§8). `crop_rect` is
  always expressed in raw-image pixel space (post-rotation) — one
  consistent convention for every caller, so tap-coordinate mapping
  (§4) never has to guess which space it's in.

---

## 3. Page-number recognition service

- **`recognize_page_number(raw_image, crop_rect) -> Option<String>`** —
  crops the given rectangle and runs local `tesseract` on it. **Must
  not** restrict to a digit-only whitelist — front-matter pages may use
  roman numerals (i, ii, iii...), which would otherwise all read as
  `None`. Trims whitespace; empty result → `None`. No further
  validation or gap-tolerance logic here — that's the whole-book
  completeness check, owned by the Job/Archive Manager (§7).
- Stateless, synchronous, single-page in/out. Used per-attempt by the
  Auto-Scan loop (§5) and, for re-scans (§9), by the same operation
  again.

---

## 4. Setup-calibration controller

Runs once per book, before Auto-Scan starts. Produces
`{page_width_mm, page_number_crop: {left, right}}`, handed to the
Job/Archive Manager (§7) and to the Auto-Scan controller (§5) for this
session. The two images captured here are **ephemeral** — used to build
the tap-preview, then discarded; they get no `sequence_number` and never
reach the archive.

**Sequence:**

1. UI prompts the user to open the book to a page with visible page
   numbers and place it in the cradle; waits for a "ready" signal.
2. `touchdown()` (no press-value override needed beyond the default) —
   lowers onto the page.
3. `capture_pair()` — the calibration shot, box stationary. Hold
   `calib_img_left`, `calib_img_right`.
4. `resume()` — required before any further motion (the board is
   holding).
5. `press_once()` → `p_baseline`, before vacuum engages.
6. `start_press_stream()`, `set_vacuum(true)`, `set_fan(true)`.
7. **Detection #1 (pickup-success drop)** — the same check every normal
   page-turn does (§5.2 step 9): shortly after vacuum engages, the
   pressure stream should show a clear drop vs. `p_baseline`. No drop =
   pickup failure, same error condition as a normal flip-cycle failure
   (exact retry/abort handling for this calibration context is an open
   point, not specified further here).
8. Once pickup is confirmed: ascend via a **distinct monitored-ascent
   move** (not `move_to_top()` — a plain vertical move, watching the
   still-open pressure stream as it goes, with no page-separation
   wiggle and no return trip; this is a one-way measurement, not a flip
   cycle).
9. **Detection #2 (page-separation rise, calibration-only)** — watch the
   pressure stream for a sudden rise back toward baseline once the
   page's trailing edge clears the suction box. Exact detection
   algorithm needs real hardware data to finalize; a starting
   placeholder is "value crosses back above a threshold, having been
   steady below it since step 7." A small compensating offset
   (~velocity × sensor lag) may need to be added to the raw detected
   position — needs empirical determination.
10. Record the box's position (mm, plus any lag offset) at detection
    #2's point as `page_width_mm`.
11. `stop_press_stream()`, `set_vacuum(false)`, `set_fan(false)`. Box
    ends at the top; the user manually pages the book back to its
    actual first page before Auto-Scan starts (this calibration page
    was arbitrary, not necessarily page 1).
12. Return `{page_width_mm, calib_img_left, calib_img_right}`.

**Tap-interaction (coordinate mapping):**

1. `make_preview()` on both calibration images, using the same fixed
   crop-then-scale rectangle as the live full-page preview.
2. UI prompts "tap the page number on the left page" → reports
   `tap_point_left` in **preview-display pixel space**.
3. Convert to raw-image space using the same `crop_rect` from step 1:
   ```
   raw_x = crop_rect.x + tap_point.x * (crop_rect.width / preview_display_width)
   raw_y = crop_rect.y + tap_point.y * (crop_rect.height / preview_display_height)
   ```
4. Center a default-size rectangle (fixed constant, needs empirical
   tuning on the real machine — large enough to comfortably contain the
   digits regardless of tap precision) on `(raw_x, raw_y)`.
5. Re-tapping the same side before confirming **replaces** the
   rectangle, doesn't add a second one. Resizing is not built.
6. Repeat for the right side.
7. Return `{page_number_crop: {left: rect_left, right: rect_right}}`.

Full derivation and the open tuning points: `reference/process-assumptions-audit.md`.

---

## 5. Auto-scan flip-cycle controller

Work happens in **page slots**, each spanning one or more pickup
**attempts**. A failed pickup retries locally within the same slot; it
never discards the slot's photo or restarts from the top.

### 5.1 Safety invariant (binding on every step)

The suction box must **never** move downward while the vacuum motor is
running, with exactly **one** exception: the retreat/"wiggle" leg (step
9 below). Every other downward move must
have vacuum and both blow units switched off *first*.

### 5.2 Single pickup attempt

Every call is identical — this operation doesn't know or care whether
it's attempt 1, 2, or 3 for the current slot; that bookkeeping is
entirely the loop driver's (§5.3) job. Inputs: `page_width_mm`.

1. `press_once()` → `p_baseline`. Read fresh on every
   attempt, before any pneumatics for this attempt engage.
2. `touchdown()` — a real closed-loop descend-until-contact every
   attempt, including retries; never a move to a remembered position
   (book thickness/binding varies page to page).
3. If the light switch is in Auto mode: `set_light(true)`.
4. `capture_pair()` — both cameras, box stationary at the
   touchdown/compressed position. Taken on **every** attempt, not just
   the first — every attempt's photo gets OCR'd (step 3, cross-checked
   against attempt 1's reading) even though only attempt 1's photo ends
   up archived. Hold `img_left`, `img_right`.
5. `resume()` — required before any of the following steps; the board
   is holding after touchdown and refuses to move otherwise.
6. `set_fan(true)`.
7. If the light switch is in Auto mode: `set_light(false)`.
8. `set_vacuum(true)`.
9. **Rewritten (AD-010, `architecture.md`, 2026-08-16) — `complete_page_turn()`/`G73`
   retired.** The host now sequences this attempt's remaining upward
   motion itself as several ordinary `move_slow()`/`move_fast()` calls
   (§1.1), one per leg: (a) up to the pickup-check waypoint, (b) if that
   check passes, down by the retreat distance (the wiggle — the one
   downward leg §5.1 permits with vacuum engaged), (c) one or more
   further legs up to the final target, split at whatever waypoints the
   relay timing below needs. Each leg is an ordinary, individually
   awaited call — there is no single long-running call to poll during
   anymore.

   `start_press_stream()` is opened right before leg (a) and consumed
   **continuously across every leg of this step**, success or failure —
   this doesn't depend on how the motion is split into legs, since the
   Arduino pressure stream (`monospace.md`) runs on its own connection,
   independent of whatever the FOC board is doing. `stop_press_stream()`
   is called once this step ends, in every case, before the next slot's
   `touchdown()`. Expected physical behavior, two distinct readings to
   tell apart:
     - Switching the vacuum pump on causes **some** drop below
       `p_baseline` even with **no** page actually held — air still
       flows (leaks) through the suction cups when nothing is sealing
       them, so the pump can't build much vacuum. Call this shallow
       reading `p_leak` (not a fixed constant — needs empirical
       characterization on real hardware, but expected to exist).
     - A **successful** pickup (page sealing the suction cups) lets the
       pump build a real vacuum — the reading should sit **below**
       (more negative / further from ambient than) `p_leak`, not just
       below `p_baseline`. The threshold distinguishing "shallow,
       pump-on-but-nothing-held" from "deep, actually sealed" is the
       one that needs empirical tuning (§6's `threshold_mbar_used`
       logging exists for exactly this) — checking only "any drop from
       `p_baseline`" isn't sufficient, since `p_leak` alone would
       already satisfy that.
     - **Two distinct failure conditions**, either one triggers the
       same abort below:
       1. The reading never drops past the success threshold at all
          within an early window (page was never picked up) — exact
          window/trigger not decided.
       2. The reading **was** past the success threshold (pickup looked
          fine) and then **suddenly jumps back up toward `p_baseline`**
          at any point during the ascent — the page was picked up
          initially but lost its seal partway through the lift. This
          can happen during **any** leg of step 9, not just the first,
          which is exactly why the check has to keep running across all
          of them, not just once early on.
     - **On success (reading stays past the threshold, no sudden
       return):** do nothing — let the current leg's `move_slow()`/
       `move_fast()` call keep running toward its target, uninterrupted.
     - **On either failure condition:** call `abort_attempt()`
       (`M53` — **not** `stop()`/`M112`, this is a routine, expected
       outcome, not an emergency, see §1.1) to abort whichever leg is
       currently in flight, immediately, wherever the box happens to be
       — the same board behavior as before, just now applied to a plain
       `G1` leg instead of `G73`. Then `set_fan(false)`,
       `set_vacuum(false)`, then a fresh `touchdown()` (`G30`) for the
       retry — it descends correctly from wherever the abort left the
       box, no matter where that was. Return
       `{pickup_result: "failure", images: {img_left, img_right},
       ambient_mbar: p_baseline, differential_mbar: p_baseline - p_check}`
       (`p_check` being whichever reading triggered the failure) —
       images are returned even on failure (needed for the
       cross-check, and in case this was attempt 1). **Stop here on
       failure** — no further legs are sent, and step 10 doesn't run.
   - **Relay timing — restructured (AD-010, 2026-08-16):** previously
     this polled `status()` during the single in-flight `G73` call to
     catch position crossings. **Now each leg boundary already is the
     synchronization point** — the relays are driven directly off each
     leg's `done` response, not off polled telemetry:
     - After leg (a) (pickup-check waypoint) passes its check and before
       leg (b) (the wiggle) is sent: no relay change here specifically —
       carried over unchanged from the previous design's timing, which
       fired `set_fan(false)` "right after the embedded retreat," i.e.
       once leg (b) itself completes.
     - Once leg (b)'s (the wiggle's) `done` response arrives:
       `set_fan(false)`. (This is a change of anchor from "~60–70% of
       page width" to "the wiggle leg has completed" — needs validating
       that these coincide closely enough on real hardware; not assumed
       equivalent without checking.)
     - Once whichever climb leg targets ~80% of page width returns
       `done`: `set_blower(true)`. (Same move-then-engage timing as
       before; still needs validating against real page-turn behavior —
       the blower may need to fire earlier/concurrently, which would
       mean splitting that leg further rather than engaging on its
       `done`.)
     - `set_vacuum(false)` — timing still not fixed: natural page
       separation likely happens near 100% of page width on the way up,
       but whether this should tie to a dedicated leg boundary there or
       simply happen once step 9's final leg returns (box now held at
       the top of this attempt's motion) isn't decided — unchanged open
       question from the previous design, just restated in terms of legs
       instead of a poll.

   **Why the pressure check still doesn't need a single checkpoint
   read, even with discrete legs:** failure condition 2 above (page
   lifted successfully at first, then lost partway through the ascent)
   can only be caught by watching continuously across every leg — a
   single stationary check-then-decide move (read once, only send the
   next leg if it looks good) would catch condition 1 fine, but would
   have no way to notice a mid-lift loss during a leg already in
   flight. This is why `start_press_stream()`/`stop_press_stream()`
   spans the *entire* step 9, not just its individual legs — the
   granularity of the motion commands and the granularity of the
   pressure monitoring are deliberately independent of each other. A
   moving box may add some airflow/vibration noise to the
   differential-pressure reading compared to a stationary read — worth
   validating on real hardware, since it affects how tight the success
   threshold can be set — but the continuous approach isn't optional
   here, it's what the failure mode requires.
10. Return `{pickup_result: "success", images: {img_left, img_right},
    ambient_mbar: p_baseline, differential_mbar: p_baseline - p_deepest,
    touchdown_position, touchdown_press_percent}` (last two from step 2
    — **corrected 2026-08-16: `touchdown_press_percent`, not
    `touchdown_compression`**, since `touchdown()` no longer returns a
    calibrated compression value, `ligature.md` §12.1/AD-011).
    `p_deepest` is the lowest (furthest-below-`p_baseline`) reading
    observed during the continuous monitoring above — a single
    representative value for logging (§6), not any one specific
    instant.
    **The box is held stationary** at wherever step 9's last leg
    stopped (110–120% of page width, or the fallback) when this
    returns — it does not reverse or descend on its own. The *next*
    slot's `touchdown()` call is what brings it back down; `touchdown()`
    itself stays a single board-driven closed-loop call (§1.1, unlike
    step 9, `G30` was not part of AD-010's change), so **this is one of
    the few places that still needs `status()`/`M155` polling during an
    in-flight call**, not leg boundaries — poll while that call is in
    flight to catch the box passing back down through ~90% of page width
    and call `set_blower(false)` at that point. The blower's on-time
    spans the end of *this* attempt's step 9 and the start of the *next*
    slot's `touchdown()`, not just this attempt in isolation.

### 5.3 Loop driver (control flow, recording, Stop handling)

For each page slot (identified by the `sequence_number` it will occupy
once archived):

1. Assign the slot's `sequence_number` now, at the start of attempt 1.
   Left camera → even number, right camera → odd number, continuous
   across the whole book (never reassigned, never reused except by
   retries of the same slot).
2. Call the single-attempt operation (§5.2), attempt 1. Keep this
   attempt's `images` result in hand regardless of outcome — this is
   the photo that ultimately gets archived under this slot's
   `sequence_number`, no matter which attempt's pickup actually
   succeeds.
3. Run page-number recognition (§3) on this attempt's images. Emit this
   attempt's `ambient_mbar`/`differential_mbar` for live display (§8)
   and logging (§6) — every attempt logs its pressure values, not just
   the first.
4. **On success:** persist attempt 1's images (held since step 2) under
   this slot's `sequence_number`, with attempt 1's recognized page
   number (not necessarily this attempt's, if they differ — attempt 1's
   OCR reading describes the archived photo). Reset the pickup-failure
   counter. Advance to the next slot.
5. **On failure:** a mismatch between this attempt's recognized number
   and attempt 1's is expected OCR noise, not a real page change (a
   failed pickup-success check aborts step 9's in-flight leg early,
   well before the page could have separated and turned) — no special
   handling needed
   beyond keeping attempt 1's reading as authoritative. Increment the
   failure counter; at 3, trigger the failure dialog (§8) — the box
   moves to the real move-to-top position only as part of that abort,
   not as part of per-attempt failure handling (which stays at a fresh
   touchdown). Discard this attempt's images unless it was attempt 1
   (whose images stay held). If under the limit, retry (back to step 2,
   same `sequence_number`).

**Stop handling:** must interrupt promptly, including mid-attempt —
requires the FOC client's `stop()` (`M112`, genuine emergency only, not
`abort_attempt()`/`M53`) and the Arduino client's all-actuators-off
operation. An unsolicited hard-stop fault from the FOC board (§1.1) is
treated exactly like a user-pressed Stop. **Added 2026-08-08:** the
physical Start/Stop/E-Stop button (§11) is a third trigger for this same
path, alongside the touchscreen [Stop] and an unsolicited FOC fault —
whenever §11's indicator logic considers the machine to be in its
"active" (amber) state, an `on_button_press` event routes here exactly
like a touchscreen Stop tap, no separate handling. **On Stop or an
unsolicited fault specifically** (see open question below re: 3-failure abort):
halt, leave the machine in the stopped state for the recovery controls
(§8) — the FOC board is now latched in its fault state
(`ligature.md` §3.8), so **recovery requires the UI to actually call
`clear_fault()` (`M999`) then `arm()` (`M3`) again before any move
(including `move_to_top()`) will work**, not just present the recovery
screen and let the next command through. A slot that was mid-attempt
when stopped has **no** archived image — resuming after a Stop starts
that slot over from a fresh attempt 1, never from wherever the
interrupted attempt was.

**Open question, flagged 2026-08-02, not resolved here:** this
paragraph groups "Stop, fault, or a 3-failure abort" as leading to the
same halted state. A 3-failure abort (step above) isn't itself a board-
level fault — every individual failed attempt already used the routine
`abort_attempt()`/`M53` path, which leaves the board `Armed`, not
faulted. Grouping it with genuine Stop/fault here may be the same kind
of category-blur this session already found and fixed at the protocol
level (`ligature.md`'s old `M112`) — or it may be an intentional UX
choice (present the same recovery screen either way, for consistency,
even though the underlying board state differs). Not changed here
pending ijon's call.

---

## 6. Live diagnostics & pressure log

- **Live display (during Auto-Scan):** current ambient pressure
  (pre-vacuum baseline, reused from step 1 of §5.2, no new measurement
  point) and the differential values of the last three flip cycles
  (N-2, N-1, current). Shown directly in the main scan screen (§8), not
  a separate debug screen.
- **`pressure-log.jsonl`** — one entry per pickup **attempt** (matching
  §5.3 step 3 — every attempt is logged, not just the one whose photo
  gets archived), separate per-job file, uploaded alongside the
  archive. Fields:
  `ambient_mbar`, `differential_mbar`, `pickup_result`,
  `threshold_mbar_used`, `touchdown_position`, `touchdown_press_percent`
  (corrected 2026-08-16, was `touchdown_compression`).
  Purpose: deriving real pickup-success/-failure thresholds empirically
  across many books, and correlating stop position/press level/
  differential pressure for debugging — kept separate from
  `metadata.json` (§7) since it has a different consumer and lifecycle
  (cross-book analysis, not per-book archival data).

---

## 7. Job/archive manager

- **Creates the job ID** at job start — assigned once, immutable for
  the job's lifetime, never derived from editable fields (title, ISBN).
- **Assembles `metadata.json`** as data arrives from calibration (§4),
  Auto-Scan (§5), and metadata capture (§8's flow) — not necessarily
  all at once; Auto-Scan emits per-page, not batched. Schema:
  - `page_width_mm`, `page_number_crop: {left, right}`
  - `pages[]`: one entry per archived page, each with
    `sequence_number`, `file`, `recognized_page_number` (string or
    `null`), `status`
  - full schema and rescan-related fields: `reference/rescan-flow.md`
- **File naming:** filenames are the continuous `sequence_number`
  (e.g. `0024.jpg`), left captures even, right captures odd — never
  derived from upload order or a timestamp, never reassigned. Enables
  a future single-file replace (re-scan) without renumbering anything
  else in the archive.
- **Writes `pressure-log.jsonl`** (§6).
- **Uploads the finished archive** to the Z2 storage-machine share on
  job completion.
- **Whole-book completeness/gap check** — comparing every page slot's
  `recognized_page_number` to flag a likely-missing page, with
  tolerance for roman/arabic switches and unnumbered plates — is owned
  here (natural fit: this is the component that already assembles the
  full `pages[]` array, and the check needs to run RPi-side). Detection
  algorithm not yet specified.
- **Upload failure handling** — not yet specified beyond: at minimum,
  an error message on the touchscreen with a manual retry option (§8).
- **Explicitly not responsible for:** deciding *what* the metadata
  values are (only persists what other components hand it), any image
  processing, any UI.

---

## 8. Touchscreen UI shell

**Naming note:** this section header used to read "(serif)" — stale,
predates `architecture.md` AD-012. This UI is part of `sans` (there is
no separate `sans-ui` application, AD-012); `serif` is the unrelated
VM/server, see `serif-verbose.md`.

**Toolkit reminder (see this file's header):** `egui`/`eframe`, native,
in-process. Everything below is an `egui` screen/widget description —
not a webpage, not a route. Not `sans-server`'s `actix-web` stub, not
the old `serif` repo's Ember.js code.

Pure presentation + input capture + navigation — no process logic of
its own. Every screen below carries an explicit, plain-language
instruction (§0).

### 8.1 Happy-path screen flow

1. **Boot/home.** "Machine ready? Box will move up" → explicit confirm
   required before homing runs (never automatic on power-on). Only
   after homing succeeds does the start screen appear. Homing failure
   needs its own error path (§8.2).
2. **Cover capture.** Box moves to top. "Place the closed book, front
   cover up, on the right side" → confirm → capture (right camera).
   "Now the back cover, on the left side" → confirm → capture (left
   camera). Both photos always taken, unconditionally — thumbnail
   source for the web portal regardless of what happens next.
3. **ISBN check.** "Does the book have an ISBN barcode? Check the
   cover, spine, or first/last pages" → three branches: barcode found
   (cover case reuses step 2's photos directly, no reposition; interior
   case: open to that page, fresh shot, decode via a barcode-decoding
   library — distinct from the tesseract text-OCR in §3) / printed but
   no barcode (on-screen numeric keyboard) / none found (skip to step
   5).
4. **Metadata confirmation.** Show the found/guessed candidate:
   "Is this correct?" [Confirm] / [Enter manually].
5. **Manual metadata form** (fallback only). On-screen keyboard,
   clearly labeled fields, then search-after-typing (not live
   autocomplete) against the same book-database lookup used in step 3,
   showing candidates for the user to confirm or dismiss.
6. **Calibration.** "Open the book to a page with a visible page
   number, in the middle, and place it in the cradle" → [Ready] →
   preview shown → "Tap the page number on the left" → tap → "...now
   the right" → tap → confirm.
7. **Auto-Scan start.** "Ready to scan?" [Start]. **Added 2026-08-08:**
   the physical status button is green and solid on this screen (§11)
   — a press here is an equivalent alternate trigger for the same
   [Start] action, not a separate path.
8. **Auto-Scan live screen.** Full-page preview + zoom preview (§2's
   `make_preview`, unscaled 100% crop) + page counter + live pressure
   diagnostics (§6) — [Stop] always visible and reachable.
9. **Scan-completion confirmation.** "Finished scanning this book?"
   [Yes, done] / [Keep scanning].
10. **Upload/finalization.** Progress indicator, then a success
    confirmation (failure case is §8.2's).

### 8.2 Error, recovery, and interruption screens

1. **Pickup-retry visibility.** A failed pickup attempt retries
   automatically (§5.3) — show a brief visible status during a retry
   ("Retrying this page...") rather than silence, so a layperson
   doesn't assume the machine is stuck.
2. **3-failure choice dialog.** Plain-language explanation ("This page
   couldn't be picked up. Please check it's lying flat.") followed by
   three explicit choices: **[Try again]** (resets the failure counter,
   3 further attempts on the same slot), **[Turn page manually]** (user
   physically turns the page, confirms via button, Auto-Scan resumes;
   attempt 1's already-held photo for this slot gets archived as-is,
   since it's a valid photo of the page as it was before the manual
   turn), **[Stop job]** (ends Auto-Scan, leads into the Stop/recovery
   screen below).
3. **Stop/recovery screen.** Shown after any Stop (user-initiated or
   "Stop job" above). Explains the state in plain language, not a bare
   button row: **[Move to top]** (primary, with a short explanation —
   moves the box up so the book can be checked/freed) and
   **[Move down]** (secondary, jog control). A way back into the flow
   once ready — resume, never silent auto-continue.
4. **Upload failure.** Error message + [Try again], never a silent
   failure or dead end.

### 8.3 Metadata-flow database lookup (shared sub-capability)

One lookup capability, two query modes — exact ISBN lookup and
title/author search — against Open Library (no key) with Google Books
as secondary (key required). Returns structured metadata (title,
author, publisher, year, cover URL if available) or no-match. Built
once, shared by the ISBN path, the cover-LLM fallback path, and the
manual-form verification step (§8.1 steps 3–5) — not reimplemented per
caller.

**Cover-LLM fallback:** when the ISBN path produces no result, send the
step-2 cover photos to an image-recognizing LLM (local AI hardware) to
extract a candidate title/author, then run that candidate through the
title/author search mode. Always surfaced to the user for confirmation
(§8.1 step 4) — never auto-accepted, since it's a guess, unlike a
direct ISBN hit.

---

## 9. Rescan support (data-model groundwork only — flow itself not built yet)

The current MVP data model must not block adding, later, a flow where a
user marks a defective page in the web portal and rescans it at the
machine. Concretely, already required by §7's schema above:

- Stable per-page file identity via `sequence_number` (§7) — a future
  rescan overwrites one file, renumbers nothing.
- `recognized_page_number` persisted per page (§7), not just used
  transiently.
- Calibration parameters (`page_width_mm`, `page_number_crop`)
  persisted per book (§7), not kept only as runtime state.

Full flow design (on-device job list, rescan-prompt loop, web-portal
gallery, archive replace-on-upload): `reference/rescan-flow.md` — none
of it is part of this application's MVP scope yet.

---

## 10. Cover-photo post-processing (not MVP scope)

The two cover photos from §8.1 step 2 are stored raw for MVP. A later
version needs to rectify them (corner detection, perspective correction,
crop, color-correct) — different requirements from normal-page
post-processing since the book is closed/rigid with no fixed reference
rectangle. Not part of this application; see
`reference/cover-photo-postprocessing.md`. This is VM-side (Z2)
post-processing work, out of scope for this RPi application entirely.

---

## 11. Physical status button — indicator & input

> Added 2026-08-08. Full decision background, alternatives considered,
> and open questions for ijon: `architecture.md` AD-009. Physical part
> and wiring: `docs/hardware/electronics.md` §3.3. Arduino-side
> protocol this section drives: `monospace.md` §10.

A single momentary pushbutton with an RGB LED ring, wired to the
Arduino (§1.2), doubling as Start, Stop, and emergency stop depending
on which of these two things is true right now: is anything moving
(vacuum/fan/blower/motion), and is the machine sitting at the "ready to
scan" gate (§8.1 step 7). This component owns exactly two
responsibilities: (a) continuously deciding what color/pattern the LED
should show, given the rest of this application's current state, and
(b) interpreting each `on_button_press` event (§1.2) in light of that
same state. It holds no state of its own beyond "what did I last tell
the LED" — the actual machine state it reads comes from the FOC client
(§1.1), the Auto-Scan loop driver (§5.3), and the UI's current screen
(§8).

### 11.1 Color/pattern → machine state

| Machine state | Color | Pattern | A button press does |
|---|---|---|---|
| Standby — §8.1, outside any automatically-commanded motion | Blue | Solid | New job (§11.2) |
| Ready to scan — §8.1 screen 7 | Green | Solid | Start (§8.1 step 7 — same as tapping [Start]) |
| Automatic motion — **any** host-commanded move, not just Auto-Scan: homing at job start (§8.1 step 1), calibration's touchdown/ascent (§4), Auto-Scan itself (§8.1 screen 8), `move_to_top()` (job-end park, recovery screen §8.2.3), jog moves (§8.2.3 [Move down]) | Amber | Solid | Stop (§5.3 — same as tapping [Stop]: `stop()` + `all_off()`) |
| Stopped, expected — user-initiated Stop, or the 3-failure abort (§8.2 screen 2/3) | Red | Slow blink (~1 Hz) | Nothing (§11.2 — recovery is touchscreen-only by design, §5.3) |
| Stopped, unsolicited fault — an unrequested `fault ...` from the FOC board (§1.1) | Red | Fast blink (~4–5 Hz) | Nothing, same as above |

Rationale for this exact mapping, the amber choice, and the two red
patterns: `architecture.md` AD-009 — not repeated here. **Blink timing
is this component's own responsibility**, implemented as a simple
repeating `set_led()` / `set_led(0,0,0)` alternation on a timer; the
Arduino has no blink concept of its own (`monospace.md` §10.3).

**Resolved (ijon, 2026-08-08, `architecture.md` AD-009):** Amber covers
*every* automatically-commanded move, not only Auto-Scan/calibration as
originally scoped — see the table row above for the full list.
Practically: derive Amber from the FOC client's (§1.1) `status().state`
being `Moving` or `Probing`, **plus** treating the whole Auto-Scan
screen (§8.1 step 8) as Amber even during its brief non-moving pauses
between individual moves (vacuum-engage before ascent, etc.) — deriving
Amber purely from `state` there would otherwise flicker
Amber→Blue→Amber within a single pickup attempt, which §5.1's existing
safety framing (vacuum and motion are treated as one hazard window)
argues against. **Still open:** exact blink frequencies (defaults
above, not tuned).

### 11.2 Button-press interpretation

Evaluated in this order, every time `on_button_press` fires:

1. **If the machine is in the Amber (automatic-motion) state:** always
   Stop, regardless of anything else — this is the "E-Stop" half of the
   button's job, and it takes priority over every other interpretation.
2. **Else if the machine is in the Green (ready-to-scan) state:** Start.
3. **Else if the machine is in the Blue (standby) state: New job.**
   **Resolved (ijon, 2026-08-08):** triggers the same entry point the
   touchscreen uses to begin a book (§8.1 steps 1–2 — homing if not
   already homed, otherwise straight into cover capture). **Working
   assumption, not separately confirmed with ijon — flag/correct if
   wrong:** this only applies at genuine idle (before the first job of
   a session, or after a previous job's upload/finalization, §8.1 step
   10, has completed) — Blue also covers the in-between screens of an
   *already-started* job (cover capture, ISBN check, metadata,
   calibration setup, §8.1 steps 2–6), where a press stays inert rather
   than being read as "new job": those screens already have their own
   specific touchscreen confirmations, and starting a second, unrelated
   job while the current one is mid-setup has no safe, unambiguous
   meaning.
4. **Red:** no defined action, unchanged — recovery from a genuine stop
   must stay a touchscreen-only, two-step action (`clear_fault()` then
   `arm()`), never a single reflexive button press (§5.3).

### 11.3 Implementation note

This is presentation/routing logic layered on top of already-specified
components (§1.1 FOC client status, §1.2 Arduino client, §5.3 loop
driver, §8 UI screen state) — it doesn't own or duplicate any of their
state, only reads it to decide what to show and how to route a press.
A natural home is a small task that polls/subscribes to "what screen/
state are we in" at some short fixed interval (e.g. every 100–200ms,
fast enough that the LED transition feels immediate to a user standing
at the machine, slow enough to be irrelevant next to the serial link's
own latency) and calls `set_led()` only when the target color/pattern
actually changes, not on every tick.
