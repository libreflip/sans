# Fake-backed touchscreen evidence

These screenshots render the real `sans` application's Setup screen using a fake Fibre device on macOS, with no live USB or camera access. They demonstrate presentation only.

- `fake-uncommissioned.png`: absent lift-axis commissioning table keeps setup blocked.
- `fake-verified.png`: verified adapter gate with the captured endstop error reproduced by the fake. Scanning remains disabled.

They are not supervised hardware evidence. The real USB transport, physical timing, motion and Stop behavior remain subject to #60 and the separate #59/#25 implementations.
