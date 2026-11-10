//! The action-bar contract later screens rely on.
//!
//! The scan screen and its Stop action do not exist yet, so a stand-in Stop
//! sits in the action bar above a body several viewports tall.
//!
//! Stop is tapped after wheel scrolls and while the body coasts after a
//! fling. During an active drag the test only checks that Stop does not move.
//! egui forwards one touch as the single pointer, so a second finger cannot
//! tap Stop while the first is still dragging. That case is not covered here
//! and belongs with the physical Stop work in #25 and #26.

use eframe::egui;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

use super::{action_button, show, Header};

/// Viewport sizes in points and scale factors, matching the application tests.
/// The last entry is the minimum window the application accepts.
const MATRIX: [([f32; 2], f32); 6] = [
    ([600.0, 1_024.0], 1.0),
    ([600.0, 952.0], 1.0),
    ([600.0, 640.0], 1.0),
    ([400.0, 634.0], 1.5),
    ([300.0, 476.0], 2.0),
    ([280.0, 240.0], 1.0),
];

#[derive(Default)]
struct StandIn {
    stops: usize,
}

fn stand_in_screen(size: [f32; 2], pixels_per_point: f32) -> Harness<'static, StandIn> {
    let instructions = (3.0 * size[1] / 14.0) as usize;
    Harness::builder()
        .with_size(size)
        .with_pixels_per_point(pixels_per_point)
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(600)
        .build_ui_state(
            move |ui, state: &mut StandIn| {
                show(
                    ui,
                    Header::new("Scanning", egui::Color32::LIGHT_GREEN),
                    |ui| {
                        if action_button(ui, "Stop").clicked() {
                            state.stops += 1;
                        }
                        action_button(ui, "Exit");
                    },
                    |ui, _| {
                        for line in 0..instructions {
                            ui.label(format!("Instruction {line}"));
                        }
                    },
                );
            },
            StandIn::default(),
        )
}

fn rect(harness: &Harness<'_, StandIn>, label: &str) -> egui::Rect {
    harness.get_by_label(label).rect()
}

fn tap(harness: &mut Harness<'_, StandIn>, label: &str) {
    let at = rect(harness, label).center();
    harness.event(egui::Event::PointerMoved(at));
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    harness.step();
}

fn wheel(harness: &mut Harness<'_, StandIn>, points: f32) {
    let at = rect(harness, "Screen body").center();
    harness.event(egui::Event::PointerMoved(at));
    harness.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -points),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    harness.run();
}

/// Assert Stop is inside the window, where it started, and pressable exactly once.
fn assert_stop_ready(
    harness: &mut Harness<'_, StandIn>,
    window: egui::Rect,
    expected: egui::Rect,
    moment: &str,
) {
    let stop = rect(harness, "Stop");
    assert!(window.contains_rect(stop), "Stop left the window {moment}");
    assert_eq!(stop, expected, "Stop moved {moment}");
    let before = harness.state().stops;
    tap(harness, "Stop");
    assert_eq!(harness.state().stops, before + 1, "Stop tap {moment}");
}

#[test]
fn stop_stays_put_and_pressable_while_the_body_scrolls() {
    for (size, pixels_per_point) in MATRIX {
        let mut harness = stand_in_screen(size, pixels_per_point);
        harness.run();
        let window = egui::Rect::from_min_size(egui::Pos2::ZERO, size.into());
        let expected = rect(&harness, "Stop");
        let at = format!("at {size:?} x{pixels_per_point}");

        for scroll in [0.0, 37.3, 211.6, 100_000.0] {
            wheel(&mut harness, -100_000.0);
            wheel(&mut harness, scroll);
            assert_stop_ready(
                &mut harness,
                window,
                expected,
                &format!("after scrolling {scroll} {at}"),
            );
        }

        wheel(&mut harness, -100_000.0);
        let body = rect(&harness, "Screen body");
        let (from, to) = (
            body.center_bottom() - egui::vec2(0.0, 10.0),
            body.center_top(),
        );
        harness.event(egui::Event::PointerMoved(from));
        harness.drag_at(from);
        harness.step();
        for step in 1..=4 {
            harness.event(egui::Event::PointerMoved(
                from + (to - from) * (step as f32 / 4.0),
            ));
            harness.step();
            let stop = rect(&harness, "Stop");
            assert_eq!(stop, expected, "Stop moved during an active drag {at}");
            assert!(
                window.contains_rect(stop),
                "Stop left the window during a drag {at}"
            );
        }
        harness.drop_at(to);
        harness.step();
        let coasting_from = rect(&harness, "Instruction 0");
        harness.step();
        assert_ne!(
            rect(&harness, "Instruction 0"),
            coasting_from,
            "the body should still be coasting after the fling {at}"
        );
        assert_stop_ready(
            &mut harness,
            window,
            expected,
            &format!("while coasting {at}"),
        );
    }
}

/// The target display's area at scale factor 2, the smallest viewport Sans is laid out for.
#[test]
fn the_body_keeps_a_third_of_the_target_area_at_scale_2_with_two_actions() {
    let (size, pixels_per_point) = MATRIX[4];
    let mut harness = stand_in_screen(size, pixels_per_point);
    harness.run();

    let body = rect(&harness, "Screen body");

    assert!(
        body.height() >= size[1] / 3.0,
        "body is {} of {} points",
        body.height(),
        size[1]
    );
}

/// The minimum window only keeps the window manager inside the work area.
/// There the body must still show and scroll a few lines of text.
#[test]
fn the_body_still_shows_three_lines_at_the_minimum_window_with_two_actions() {
    let (size, pixels_per_point) = MATRIX[5];
    let mut harness = stand_in_screen(size, pixels_per_point);
    harness.run();

    let body = rect(&harness, "Screen body");
    let line = rect(&harness, "Instruction 0").height();

    assert!(
        body.height() >= 3.0 * line,
        "body is {} points for {line} point lines",
        body.height()
    );
}
