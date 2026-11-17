# serif client (sans side)

> **Status:** specification, ready for implementation (2026-11-17). The
> wire contract (endpoints, payloads, status codes, lifecycle) is defined
> **only** in
> [`libreflip/serif` `docs/sans-serif-interface.md`](https://github.com/libreflip/serif/blob/master/docs/sans-serif-interface.md),
> called "the interface" below. This document says how sans implements its
> side. If the two disagree, the interface wins.

## 1. What sans does with serif

**Terminology.** sans's own language (`CONTEXT.md`) calls it a **Scan
session**. serif and the interface call the same thing a **job**. They are
one-to-one: the job ID *is* the Scan-session directory name. Inside sans
code, keep using "Scan session". Use "job" only in the `serif` module, where
it names the remote resource.

sans is always the client; serif never connects to sans. sans:

1. **Registers** each Scan session as a job on serif, under its own job ID.
2. **Uploads** pages, covers, `scan.jsonl` lines and `metadata.opf` in the
   background while scanning and afterwards. This runs at low priority and
   survives being offline and rebooting.
3. **Confirms** the finished book with a checksum-verified
   `capture-complete`, then shows the QR code.
4. **Deletes** old, confirmed books on its own when it needs the space.
5. **Fetches** rescan requests and uploads replacements (v2).

The scan itself never depends on serif. This keeps the sans#20 rule that the
core Scan-session flow has no network dependency. Only the separate upload
worker talks to the network.

## 2. Configuration

The optional `[serif]` table in the Machine profile, as in interface §7.1
(`url`, `portal_job_url_prefix`, `api_key_file`, `ca_file`,
`min_free_bytes`):

- Add it to `sans.example.toml`, commented out, and to `config.rs`
  (`deny_unknown_fields`), and bump `SCHEMA_VERSION` to 5. Version 4
  profiles stay valid by adding `schema_version = 5`; nothing else changes.
- Validation:
  - Both URLs must be `https://`.
  - `api_key_file` must exist, be owned by the running user and not be
    group- or world-readable.
  - `ca_file` must parse as PEM.
  - `min_free_bytes` must be positive.

  A failed validation disables uploading with a named reason on the idle
  screen; it is not a fatal start-up error. Scanning stays possible.
- The key is read from the file at start, never logged, and never shown on
  screen.

## 3. Components

| Component | Where | Notes |
|---|---|---|
| `serif::client` | `sans-core/src/serif/client.rs` | Blocking HTTPS client: `ureq` with `rustls`, TLS 1.3 only, roots taken **only** from `ca_file`, host name verified, redirects disabled. Typed request/response structs for every interface endpoint. No retries inside. |
| `serif::ledger` | `sans-core/src/serif/ledger.rs` | Per-job `upload.jsonl` (interface §8.2): append-and-flush only, the same discipline as `scan.jsonl`. Reads tolerate a truncated last line. |
| `serif::worker` | `sans-core/src/serif/worker.rs` | The background thread. It does the ordering, backoff, reconciliation (`GET jobs/{id}`) and capture-complete. It lowers its own priority at thread start (`setpriority` on its TID to 19, `ioprio_set` to `IOPRIO_CLASS_IDLE`). The policy is a deterministic core (inputs plus explicit time in, requests and wake-ups out), like `odrive::engine`, so it can be tested without a network. |
| `serif::retention` | `sans-core/src/serif/retention.rs` | The free-space policy (interface §8.5). |
| Status | `ControllerSnapshot` | The worker publishes an `UploadStatus` (queue counts, current job progress, last error class) that the controller forwards to the app. The app only renders it. |
| QR | `sans-app` | `qrcode` crate rendered into an `egui` texture. The URL is `portal_job_url_prefix + job_id`. |

The worker learns about new committed files from a channel. After each
durable commit the controller sends `Committed { job_id, item }`. At start,
the worker also scans `data_root`. It never reads a file the controller has
not committed.

## 4. Job ID and clock

- The job ID is the Scan-session directory name from
  `create_session_directory` (`YYYYMMDDTHHMMSSZ[-N]`) (sans#47, interface
  §4.1). It is never derived from anything else.
- Before accepting New Book / Start, the controller checks that the clock is
  synchronised with `libc::ntp_adjtime` (not `TIME_ERROR`, `STA_UNSYNC`
  clear). If it is not, the screen says "Clock not synchronised – check the
  network/NTP" and Start is refused. The Pi needs `systemd-timesyncd` or
  chrony pointed at the VM or the c-base NTP server. This is a deployment
  step to document in README.
- On `409 job_id_conflict`, the worker stops uploading that job and the
  idle screen names it. The maintainer's fix:
  1. Stop sans.
  2. Rename the folder to the next free `-N`.
  3. Append a `{"type":"job_id_reassigned","from":"…","to":"…"}` event.
  4. Delete the job's `upload.jsonl`.
  5. Start sans.

  This procedure is documented in README. It is not automated, because the
  conflict should never happen with a synchronised clock.

## 5. Order of work per job

1. `PUT jobs/{id}`.
2. `POST events`, from the ledger's last confirmed line.
3. `PUT pages/{n}`, ascending, committed pages only.
4. `PUT covers/front|back`.
5. `PUT metadata` (whenever the local OPF checksum differs from the last
   confirmed one).
6. After Finish Book, once 2–5 are fully confirmed: `POST capture-complete`
   with the full manifest. On `409 incomplete`, upload what it lists, then
   repeat.

Abort Book → `POST abort` (queued like everything else). Interrupted
sessions, with neither Finish nor Abort (sans#30), are uploaded as far as
they were committed. They never get capture-complete automatically and are
never deleted automatically.

## 6. Screens

- **Idle:** one upload status line (interface §8.3).
- **Book finished:** while capture-complete is pending, show the QR code
  with "Upload in progress". After `200`, show "Scan this code to view your
  scanned book". If the first capture-complete attempt fails, show sans#51's
  [Try again] / [Upload later]. [Upload later] goes back to Idle and leaves
  everything to the worker.
- **No popups** for background failures (sans#51).

## 7. Tests (definition of done for the sans tickets)

- The worker core runs against a fake serif (in-process HTTP server in
  `tests/support/`) that can drop connections, delay, answer every status
  code in interface §6.13, and lose responses after storing.
- Covered:
  - Offline → online.
  - Reboot mid-job (ledger replay plus reconciliation).
  - A lost response after the server stored the item (no duplicate).
  - `409 incomplete`.
  - `401`.
  - TLS failures: wrong CA, wrong host, TLS 1.2-only server.
- Retention against a temporary directory with a fake free-space source:
  oldest confirmed job deleted first, unconfirmed jobs never deleted, Start
  refused when nothing is deletable.
- QR: decode the rendered code and compare it with the URL (sans#52).
- One supervised run against a real serif test instance, as part of
  interface §10.

## 8. Tickets

| Ticket | Content |
|---|---|
| #77 | serif client foundation: profile `[serif]`, TLS 1.3 + key, `ping`, upload status |
| #78 | Background per-page upload worker and ledger (replaces #49's transport) |
| #79 | Free-space-driven local retention |
| #47 | Job ID + NTP gate |
| #48 | `scan.jsonl` / `metadata.opf` content |
| #51 | Operator-facing upload failure screen and count |
| #52 | QR code |
| #55, #56 | Rescan client (v2) |

Server side: libreflip/serif#38.
