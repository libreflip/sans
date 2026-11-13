//! Headless fixture renders for pull requests that change touchscreen presentation.
//!
//! Set `SANS_SCREENSHOT_DIR` to write PNGs there. The run then fails when no
//! wgpu adapter can render them. These are renders of the
//! real application with fixture cameras, not target-screen evidence.

mod support;

use std::path::PathBuf;

use support::fixtures::{portrait_full_frame, portrait_smaller_crop};
use support::pixels::renderer;
use support::session::{Session, T1, T3};

#[test]
fn write_headless_fixture_renders() {
    let Some(directory) = std::env::var_os("SANS_SCREENSHOT_DIR").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&directory).unwrap();
    for viewport in [T1, T3] {
        for (fixture, pair) in [
            ("full-frame", portrait_full_frame()),
            ("smaller-crop", portrait_smaller_crop()),
        ] {
            // Asking for screenshots and getting none must not pass.
            let renderer = renderer("headless fixture renders")
                .expect("SANS_SCREENSHOT_DIR is set but no wgpu adapter can render");
            let mut session = Session::with_pair_rendered(pair, viewport, Some(renderer));
            for (position, scroll) in [("top", -100_000.0), ("end", 100_000.0)] {
                session.wheel(scroll);
                let path = directory.join(format!(
                    "headless-fixture-{}-{fixture}-{position}.png",
                    viewport.name
                ));
                session.render().save(&path).unwrap();
            }
        }
    }
    // A latched Stop. The fixture Machine has no Monospace, so it is Unconfirmed.
    let renderer = renderer("headless fixture renders")
        .expect("SANS_SCREENSHOT_DIR is set but no wgpu adapter can render");
    let mut session = Session::with_pair_rendered(portrait_full_frame(), T1, Some(renderer));
    session.tap_label("Stop");
    session.wait_for_label("UNCONFIRMED STOP: switch off machine power");
    session
        .render()
        .save(directory.join("headless-fixture-T1-unconfirmed-stop.png"))
        .unwrap();
}
