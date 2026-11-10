# Camera preview fixture renders

These screenshots render the real Sans application with scripted fixture cameras through wgpu on Metal. They are automated evidence, not photographs of the target touchscreen or supervised physical evidence.

- `headless-fixture-T1-full-frame-top.png`: 600x952 points at scale 1, with representative desktop and window chrome removed from the available height. All four previews fit.
- `headless-fixture-T3-smaller-crop-end.png`: 400x634 points at scale 1.5, scrolled to the bottom, with a commissioned crop smaller than the frame.

Regenerate with `SANS_REQUIRE_PIXEL_PROOF=1 SANS_SCREENSHOT_DIR=<directory> cargo test -p sans --locked --test screenshots` on a host with a wgpu renderer.

Target desktop work-area sizing, touchscreen dragging, and OpenGL sampling on the Raspberry Pi remain pending supervised checks. No live hardware actions were performed for this change.
