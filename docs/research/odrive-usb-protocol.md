# MKS ODrive Mini: ASCII versus legacy native USB

Researched 2026-11-10. Scope: Sans's Rust host and legacy firmware, not ODrive Pro or modern Fibre.

## Recommendation and evidence boundary

Prefer **legacy native Fibre over the vendor USB interface** for Sans, subject to verifying the installed factory build against the discovered device interface. Correlated responses, acknowledged writes and endpoint discovery justify the extra implementation work for motion control. ASCII over CDC is a reasonable lower-cost fallback for a small, verified property set, but its untagged replies and silent successful writes complicate recovery from timeouts.

Makerbase publishes Mini `0_5_1_20250326` BIN/HEX files and `ODriveMINI-fw-v0.5.1-20250326.rar` at repository revision `e15782976ae93d42b1f0648ceec96503141a343b`. That establishes the vendor's published version naming, **not** which image is installed or equivalence to upstream. The behavior below is verified against upstream `fw-v0.5.1`, commit `7831d795235e5ef8535e4b46621a0721b458ec8f`. The exact factory image installed remains unverified. No hardware was accessed. [Makerbase artifacts][mks], [upstream revision][upstream]

## Evidence for this machine

The operator confirmed on 2026-11-10 that the installed board is an **MKS ODrive Mini running the firmware it shipped with**. This identifies the product and firmware provenance, but not an exact build revision. No flashing is proposed.

The existing local commissioning backup at `/Users/hrmny/code/projects/libreflip/libreflip-odrive/config/odrive0-config.json` contains `config.enable_ascii_protocol_on_usb = true`, Hall mode `1`, CPR `24` and four pole pairs. These are saved configuration values inspected during this research, not fresh device readings or recommended tuning values. ASCII enablement does not disable the separate native vendor interface.

Makerbase's own Mini modification instructions explicitly say the board cannot directly use unmodified ODrive v3.6 code. They specify the v3.6-56V build target, setting up only axis 0, and encoder-source changes. Treat the installed firmware as the stock **vendor build**, not bit-identical upstream firmware. The published dated 0.5.1 artifacts are a compatibility baseline, not proof of the installed image. [Manufacturer modification instructions][mks-modifications]

The next compatibility check should be read-only: inspect USB descriptors and device identity, firmware/hardware fields, endpoint-0 schema and available telemetry/state properties. A generic or placeholder version string alone cannot identify the factory image; preserve a schema fingerprint and verify required capabilities. Do not arm, Home, calibrate, change settings or flash firmware as part of that check. Timing and mechanical behavior remain separate supervised work in #60.

## Protocol comparison

| Concern | ASCII | Legacy native Fibre |
|---|---|---|
| Transport | Line protocol through USB CDC, usually `/dev/ttyACM*` on Linux. | Packet protocol through USB bulk transfers; use vendor interface 2, OUT `0x03`, IN `0x83`. No UART stream wrapper on the normal native USB build. |
| Correlation | No request IDs or property names in ordinary value replies. Optional XOR checksum checks text integrity, not request identity. | Request sequence number echoed in response with bit 15 set. Supports a host pending-request map and rejecting late responses. Sequence numbers do not impose execution order or suppress duplicates. |
| Access | `r path`, `w path value` for string-convertible properties; dedicated motion, watchdog and save/erase/reboot commands. No arbitrary function-call command. | Endpoint 0 supplies the JSON hierarchy, IDs, types and access modes. Typed property reads/writes and exposed functions are available. Resolve the actual device's endpoints rather than copying IDs from another build. |
| Write response | Successful `w` and motion commands are silent; certain errors return text. | Set the endpoint ID's response bit even when expected payload length is zero to request an acknowledgement. |
| Telemetry | Poll `f axis` for position/velocity and `r` for state/errors. | Poll property endpoints. Neither inspected implementation offers general property-change subscriptions. |

Sources: [ASCII specification][ascii-doc], [ASCII parser][ascii-src], [native specification][protocol], [native dispatcher][dispatcher], [host object implementation][objects], [USB routing][usb]. Upstream's live plotter repeatedly calls a getter; its background thread is host polling, not a device subscription. Separate property reads are not an atomic telemetry snapshot. [Polling implementation][polling]

USB classes and protocols are separate choices. Upstream exposes CDC interfaces 0/1 and a vendor group at interface 2. The actual USB dispatcher selects ASCII on CDC using `config.enable_ascii_protocol_on_usb`; the vendor path remains native. Therefore a tty's existence alone does not establish ASCII availability. The historical interface guide's claim that both endpoint pairs behave identically needs this source-code qualification. Both paths share one firmware USB worker and TX semaphore, so opening both does not provide an independent urgent channel. [Interface descriptors/docs][interfaces], [USB routing][usb]

## Rust integration cost and support

ASCII has the smallest initial cost. Sans already depends on `serialport` 4 in `sans-core/Cargo.toml`; the library provides blocking serial I/O and USB port enumeration. Add bounded line framing, parsing, deadlines and a single connection owner. The community `odrive-rs` 1.1.0 advertises ASCII commands/property editing, but this is not evidence of Mini compatibility. [serialport maintainers][serialport], [odrive-rs author README][rust-ascii]

Native requires USB discovery/interface claiming and permissions, packet framing, little-endian codecs, endpoint-0 download/JSON parsing, descriptor CRC16, sequence allocation, response dispatch, deadlines and reconnect handling. Its packet trailer is a protocol version for endpoint 0 and a descriptor CRC for other endpoints; it is not the stream-format packet CRC. Keep function argument/trigger/result operations together according to the discovered definition and legacy host behavior. [Native specification][protocol], [legacy host channel][channel], [host objects][objects]

`rusb` supplies libusb transport, not Fibre. The v0.5.1 guide provides Python support and explicitly no official other-language library; the inspected `odrive` Rust crate 0.2.0 supports CAN only. No inspected source establishes a supported, drop-in Rust legacy USB client. A narrow in-process client costs more than ASCII; a pinned legacy Python helper reuses discovery/codecs but adds IPC, packaging and process supervision. These are engineering estimates, not measured delivery times. [rusb maintainers][rusb], [legacy language support][interfaces], [odrive crate author docs][rust-can]

## Acknowledgements, timeouts and Stop

An ASCII OS write succeeding proves neither parsing nor acceptance. Follow important writes with property/state reads. Allow only one reply-producing request at a time, classify error lines, and treat a timed-out reply as connection ambiguity: draining currently buffered bytes alone cannot rule out a later stale reply. Reconnect/re-establish identity and state before resuming ordinary control. This is a Sans design requirement inferred from the untagged protocol. The parser's property-read buffer is only 10 bytes, another reason to verify required values and precision on the vendor image. [ASCII parser][ascii-src]

A native acknowledgement means the dispatcher reached endpoint handling, not that the requested motion completed. The dispatcher ignores the handler's boolean result before emitting its response, so an empty acknowledgement is not a universal semantic-success status either. Read back critical configuration and observe actual state/errors. A simultaneous property read/write conventionally returns the **old** value. [Dispatcher][dispatcher], [native specification][protocol]

Timeout means execution is unknown for either protocol. Native has no duplicate suppression; the legacy Python channel defaults to five attempts with a five-second resend timeout. Do not inherit that policy for urgent control or blindly retry non-idempotent functions. Use short, measured application deadlines, connection generations and sequence reuse rules that exclude stale responses. [Native specification][protocol], [host retry implementation][channel]

Give Stop priority over queued telemetry/motion, cancel unsent moves and prevent a blocked read/retry loop from holding the writer. Already submitted USB work cannot be assumed cancelled. Neither protocol provides a stop-priority opcode or bounded stop latency. An IDLE request is asynchronous; upstream disarms motor PWM in its idle loop, which does not prove the mechanism has stopped moving. Require fresh current-connection state and velocity evidence under the machine's commissioned stop policy. Without that evidence, report Unconfirmed Stop. Configure and verify the firmware watchdog as a communication-loss backstop; native property writes do not automatically have ASCII motion commands' watchdog-feeding behavior. USB failure still requires manual power intervention. These are design implications, not assembled-machine validation. [USB worker][usb], [axis state machine/watchdog][axis], [ASCII motion/watchdog commands][ascii-doc], [watchdog configuration][schema]

[mks]: https://github.com/makerbase-motor/MKS-ODrive/tree/e15782976ae93d42b1f0648ceec96503141a343b/Firmware/MKS%20ODrive%20MINI
[upstream]: https://github.com/odriverobotics/ODrive/tree/7831d795235e5ef8535e4b46621a0721b458ec8f
[ascii-doc]: https://github.com/odriverobotics/ODrive/blob/fw-v0.5.1/docs/ascii-protocol.md
[ascii-src]: https://github.com/odriverobotics/ODrive/blob/fw-v0.5.1/Firmware/communication/ascii_protocol.cpp
[protocol]: https://github.com/odriverobotics/ODrive/blob/fw-v0.5.1/docs/protocol.md
[dispatcher]: https://github.com/odriverobotics/ODrive/blob/fw-v0.5.1/Firmware/fibre/cpp/protocol.cpp
[objects]: https://github.com/odriverobotics/ODrive/blob/fw-v0.5.1/Firmware/fibre/python/fibre/remote_object.py
[channel]: https://github.com/odriverobotics/ODrive/blob/fw-v0.5.1/Firmware/fibre/python/fibre/protocol.py
[usb]: https://github.com/odriverobotics/ODrive/blob/fw-v0.5.1/Firmware/communication/interface_usb.cpp
[interfaces]: https://github.com/odriverobotics/ODrive/blob/fw-v0.5.1/docs/interfaces.md
[polling]: https://github.com/odriverobotics/ODrive/blob/fw-v0.5.1/tools/odrive/utils.py
[serialport]: https://github.com/serialport/serialport-rs
[rust-ascii]: https://docs.rs/crate/odrive-rs/1.1.0/source/README.md
[rusb]: https://github.com/a1ien/rusb
[rust-can]: https://docs.rs/odrive/0.2.0/odrive/
[axis]: https://github.com/odriverobotics/ODrive/blob/fw-v0.5.1/Firmware/MotorControl/axis.cpp
[schema]: https://github.com/odriverobotics/ODrive/blob/fw-v0.5.1/Firmware/odrive-interface.yaml

[mks-modifications]: https://github.com/makerbase-motor/MKS-ODrive/blob/e15782976ae93d42b1f0648ceec96503141a343b/Firmware/MKS%20ODrive%20MINI/ODriveMINI%20code%20modification%20instructions.docx

## Read-only board check, 2026-11-10

Native USB discovery and selected property GETs succeeded against the connected board. USB identity is VID `1209`, PID `0d32`, serial `316536713335`. Reported hardware is `3.6`, variant `56`; firmware fields are `0.0.0`, unreleased `1`. The factory image revision remains unknown. Do not equate these placeholder fields with vendor artifact version 0.5.1.

The discovered schema has 535 flattened endpoints and descriptor CRC 39744. Canonical flattened-schema SHA-256 is `a36d84437bd2bf5925c0119a1df53d88de8ebe1aa8df0a338d2f6dd56fd5e52b`. This fingerprints the discovered API, not the firmware binary. Position, velocity, measured/setpoint current, trajectory completion, Home status and ASCII enablement all returned readable values. The schema also exposes requested-state, per-axis clear-errors and watchdog-feed operations; these were discovered but never invoked or written.

The axis reported current state `1`, error `4096` / `0x1000`, encoder ready, velocity and measured current zero, trajectory done, `is_homed=false` and minimum-endstop state true. These are instantaneous observations. No error was cleared, and no motion, calibration, firmware change or configuration write occurred. Exposing an operation does not validate its physical behavior. Native compatibility is now demonstrated for discovery and the selected reads; this does not resolve Touchdown performance or Stop semantics.

Evidence: [identity/status results](../../sans-core/test-results/odrive-mini-identity-2026-11-10.json) and [discovered flattened endpoint schema](../../sans-core/test-results/odrive-mini-endpoints-2026-11-10.json).
