# Stop action fixture renders

These screenshots render the real Sans application with scripted fixture cameras through wgpu on Metal. They are automated evidence, not photographs of the target touchscreen or supervised physical evidence.

- `headless-fixture-T1-unconfirmed-stop.png`: 600x952 points at scale 1 after tapping Stop. The fixture Machine has no Monospace or lift axis, so the Stop is Unconfirmed.
- `headless-fixture-T3-smaller-crop-end.png`: 400x634 points at scale 1.5, scrolled to the bottom. Stop shares the first action row, so the previews keep their height.

Regenerate with `SANS_REQUIRE_PIXEL_PROOF=1 SANS_SCREENSHOT_DIR=<directory> cargo test -p sans --locked --test screenshots` on a host with a wgpu renderer.

No live hardware actions were performed for this change.
