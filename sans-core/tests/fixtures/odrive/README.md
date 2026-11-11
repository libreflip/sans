# ODrive test fixtures

## `descriptor-RECONSTRUCTED-2026-11-10.json`

**This is a reconstruction, not a captured byte stream.** No raw endpoint-0 bytes were saved during the read-only board check on 2026-11-10 (#57). That check stored only the flattened schema, [`odrive-mini-endpoints-2026-11-10.json`](../../../test-results/odrive-mini-endpoints-2026-11-10.json).

The file was rebuilt offline from that flattened capture by `descriptor-reconstruction.py`, which reads only the captured file. It uses the upstream Fibre JSON layout: a root array, members in ID order, objects as name, type and members, properties as name, ID, type and access, and functions with inputs and outputs.

- Length: 38157 bytes.
- SHA-256: `6035e5f8f15addcbea8309597e590143e4a2b23af1f4255bd4652ea7ef1e7625`.
- CRC16 (polynomial `0x3D65`, initial value `1`, MSB first, no final XOR): `39744`, the trailer value the board reported.

A 16-bit check can collide, so the matching CRC makes byte identity very likely but does not prove it. #60 should save the raw descriptor on first supervised contact and replace this file.

The fake Fibre device in `tests/support` serves this file. Tests that pass against it show host behaviour against upstream semantics, not what the vendor build does.
