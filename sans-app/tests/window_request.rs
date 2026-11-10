//! The startup window request, checked as a plain value without opening a window.

use eframe::egui;

/// The 600x1024 touchscreen in physical pixels.
const DISPLAY: egui::Vec2 = egui::vec2(600.0, 1_024.0);
/// A 36 pixel title bar plus a 36 pixel desktop menu bar.
const CHROME_HEIGHT: f32 = 72.0;
/// A 10 pixel window border on each side.
const CHROME_WIDTH: f32 = 20.0;

#[test]
fn minimum_inner_size_fits_the_display_with_chrome_at_every_scale_factor() {
    let minimum = sans::window_request()
        .min_inner_size
        .expect("the request sets a minimum inner size");

    for pixels_per_point in [1.0, 1.5, 2.0] {
        let physical = minimum * pixels_per_point + egui::vec2(CHROME_WIDTH, CHROME_HEIGHT);
        assert!(
            physical.x <= DISPLAY.x && physical.y <= DISPLAY.y,
            "minimum {minimum:?} at scale {pixels_per_point} needs {physical:?} physical pixels"
        );
    }
}

#[test]
fn requests_a_maximized_decorated_window_that_is_not_fullscreen() {
    let request = sans::window_request();

    assert_eq!(request.maximized, Some(true));
    assert_eq!(request.decorations, Some(true));
    assert_ne!(request.fullscreen, Some(true));
}
