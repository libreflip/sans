//! Drive the real Sans application headlessly, the way an operator and a camera do.
//!
//! A [`Session`] starts the real controller thread through the Machine factory
//! seam with scripted cameras, then runs the real application in an
//! egui_kittest harness at a chosen viewport and scale factor. Tests observe
//! only the accessibility tree, viewport commands, camera call counts, and
//! rendered pixels.

use std::fs;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::{By, NodeT, Queryable};
use egui_kittest::wgpu::WgpuTestRenderer;
use egui_kittest::Harness;
use sans::{SansApp, Startup};
use sans_core::bootstrap;
use tempfile::TempDir;

use super::cameras::{CaptureCalls, FixtureFactory, Gate, GateRelease, Step};
use super::fixtures::{portrait_full_frame, Fixture};

const VALID_PROFILE: &str = include_str!("../../../sans-core/tests/fixtures/valid-sans.toml");
const WAIT_LIMIT: Duration = Duration::from_secs(10);
/// Most frames [`Session::settle`] and the harness's own runs step.
const SETTLE_LIMIT: usize = 600;
/// Unchanged frames after which [`Session::settle`] treats the screen as still.
const STILL_FRAMES: usize = 5;

/// Accessibility label of the scrolling body region.
pub const BODY: &str = "Screen body";

/// A window inner size in points and the scale factor the desktop reports.
#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    pub name: &'static str,
    pub size: egui::Vec2,
    pub pixels_per_point: f32,
}

/// Display with no chrome.
pub const T0: Viewport = Viewport {
    name: "T0",
    size: egui::vec2(600.0, 1_024.0),
    pixels_per_point: 1.0,
};
/// Target display with a 36 pixel title bar and a 36 pixel menu bar.
pub const T1: Viewport = Viewport {
    name: "T1",
    size: egui::vec2(600.0, 952.0),
    pixels_per_point: 1.0,
};
/// Short window that forces scrolling.
pub const T2: Viewport = Viewport {
    name: "T2",
    size: egui::vec2(600.0, 640.0),
    pixels_per_point: 1.0,
};
/// T1's physical area at scale 1.5.
pub const T3: Viewport = Viewport {
    name: "T3",
    size: egui::vec2(400.0, 634.0),
    pixels_per_point: 1.5,
};
/// T1's physical area at scale 2.
pub const T4: Viewport = Viewport {
    name: "T4",
    size: egui::vec2(300.0, 476.0),
    pixels_per_point: 2.0,
};

/// Every viewport in the matrix.
pub const MATRIX: [Viewport; 5] = [T0, T1, T2, T3, T4];
/// The smallest window Sans accepts, `MINIMUM_INNER_SIZE` in the application.
pub const MINIMUM: Viewport = Viewport {
    name: "minimum 280x240",
    size: egui::vec2(280.0, 240.0),
    pixels_per_point: 1.0,
};
/// The matrix plus the minimum window.
pub const MATRIX_AND_MINIMUM: [Viewport; 6] = [T0, T1, T2, T3, T4, MINIMUM];
/// Viewports where a complete Capture pair overflows the body.
pub const SCROLLING: [Viewport; 3] = [T2, T3, T4];

/// The real application, its controller, and its scripted cameras.
pub struct Session {
    pub harness: Harness<'static, SansApp>,
    pub calls: CaptureCalls,
    pub viewport: Viewport,
    _profile: Option<TempDir>,
}

impl Session {
    /// Show the fatal startup screen with `message`.
    pub fn fatal(message: &str, viewport: Viewport) -> Self {
        Self::launch(
            Startup::Fatal(message.into()),
            None,
            CaptureCalls::default(),
            viewport,
            None,
        )
    }

    /// Start a controller whose Machine factory reports `reasons` as Setup blockers.
    pub fn setup_blocked(reasons: Vec<String>, viewport: Viewport) -> Self {
        let mut session = Self::controller(
            FixtureFactory::Blocked(reasons),
            CaptureCalls::default(),
            viewport,
            None,
        );
        session.wait_for_label("Setup blocked");
        session
    }

    /// Start a controller with scripted cameras and wait for the Machine ready screen.
    pub fn cameras(left: Vec<Step>, right: Vec<Step>, viewport: Viewport) -> Self {
        Self::cameras_with_renderer(left, right, viewport, None)
    }

    /// Like [`Session::cameras`], rendering through `renderer`.
    pub fn cameras_with_renderer(
        left: Vec<Step>,
        right: Vec<Step>,
        viewport: Viewport,
        renderer: Option<WgpuTestRenderer>,
    ) -> Self {
        let (factory, calls) = FixtureFactory::cameras(left, right);
        let mut session = Self::controller(factory, calls, viewport, renderer);
        session.wait_for_label("Machine ready");
        session
    }

    /// Start with cameras returning `pair`, capture it, and wait for the four views.
    pub fn with_pair(pair: (Fixture, Fixture), viewport: Viewport) -> Self {
        Self::with_pair_rendered(pair, viewport, None)
    }

    /// Show a first pair, with the Left Camera's next capture held at a gate.
    ///
    /// The session is left on the ready preview. Tapping Capture pair starts a
    /// capture that stays in Capturing until the returned release is opened
    /// or dropped.
    pub fn with_held_next_capture(viewport: Viewport) -> (Self, GateRelease) {
        let (left, right) = portrait_full_frame();
        let (gate, release) = Gate::closed();
        let mut session = Self::cameras(
            vec![
                Step::Frame(left.frame.clone()),
                Step::Gated(left.frame, gate),
            ],
            vec![Step::Frame(right.frame)],
            viewport,
        );
        session.capture_pair();
        (session, release)
    }

    /// Like [`Session::with_pair`], rendering through `renderer`.
    pub fn with_pair_rendered(
        (left, right): (Fixture, Fixture),
        viewport: Viewport,
        renderer: Option<WgpuTestRenderer>,
    ) -> Self {
        let mut session = Self::cameras_with_renderer(
            vec![Step::Frame(left.frame)],
            vec![Step::Frame(right.frame)],
            viewport,
            renderer,
        );
        session.capture_pair();
        session
    }

    fn controller(
        factory: FixtureFactory,
        calls: CaptureCalls,
        viewport: Viewport,
        renderer: Option<WgpuTestRenderer>,
    ) -> Self {
        let profile = TempDir::new().unwrap();
        let profile_path = profile.path().join("sans.toml");
        let data_root = profile.path().join("scan-data");
        fs::write(
            &profile_path,
            VALID_PROFILE.replace(
                "data_root = \"scan-data\"",
                &format!("data_root = {data_root:?}"),
            ),
        )
        .unwrap();
        let handle = bootstrap(Some(&profile_path), factory).unwrap();
        Self::launch(
            Startup::Controller(handle),
            Some(profile),
            calls,
            viewport,
            renderer,
        )
    }

    fn launch(
        startup: Startup,
        profile: Option<TempDir>,
        calls: CaptureCalls,
        viewport: Viewport,
        renderer: Option<WgpuTestRenderer>,
    ) -> Self {
        let mut builder = Harness::builder()
            .with_size(viewport.size)
            .with_pixels_per_point(viewport.pixels_per_point)
            // Frames shorter than the application's 16 ms snapshot polling, so a
            // poll is a future repaint and an idle screen lets the run settle.
            .with_step_dt(0.01)
            .with_max_steps(SETTLE_LIMIT as u64);
        if let Some(renderer) = renderer {
            builder = builder.renderer(renderer);
        }
        let harness = builder.build_eframe(|context| SansApp::new(&context.egui_ctx, startup));
        Self {
            harness,
            calls,
            viewport,
            _profile: profile,
        }
    }

    /// Step frames until `condition` holds, failing after a bounded wait.
    pub fn wait_until(&mut self, description: &str, mut condition: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + WAIT_LIMIT;
        loop {
            self.harness.step();
            if condition(self) {
                self.settle();
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description} at {}",
                self.viewport.name
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Step until the screen stops changing.
    ///
    /// A spinner asks for a new frame forever, so waiting for repaints to stop
    /// would spend the whole step limit after every action on a busy screen.
    /// The screen counts as settled once the accessibility layout has stayed
    /// the same for several frames with no wheel input left to apply, which
    /// still lets scroll animations finish. A screen that never settles within
    /// [`SETTLE_LIMIT`] frames fails the test.
    pub fn settle(&mut self) {
        let mut previous = self.labelled_rects();
        let mut still_frames = 0;
        for _ in 0..SETTLE_LIMIT {
            self.harness.step();
            let current = self.labelled_rects();
            // Wheel input is released over several frames. A turn past either
            // end leaves input pending while the layout stays clamped.
            let wheel_pending = self.harness.ctx.input(|input| input.is_scrolling());
            if current == previous && !wheel_pending {
                still_frames += 1;
                if still_frames == STILL_FRAMES {
                    return;
                }
            } else {
                still_frames = 0;
                previous = current;
            }
        }
        panic!(
            "the screen did not settle within {SETTLE_LIMIT} frames at {}",
            self.viewport.name
        );
    }

    /// Step frames until a node labelled `label` exists.
    pub fn wait_for_label(&mut self, label: &str) {
        self.wait_until(label, |session| session.has(label));
    }

    /// Tap Capture pair and wait until the new pair is shown and Cameras are ready.
    pub fn capture_pair(&mut self) {
        self.tap_and_wait_for_capture("Capture pair");
    }

    /// Tap `action` and wait until a capture it started has finished and Cameras are ready.
    ///
    /// The controller publishes Capturing before it calls either camera. Both
    /// call counts are read before a frame steps, so once they have advanced,
    /// that frame drains at least the Capturing snapshot and a Ready status it
    /// shows belongs to the new capture, not to the one before the tap.
    pub fn tap_and_wait_for_capture(&mut self, action: &str) {
        let (left, right) = (self.calls.left(), self.calls.right());
        self.tap_label(action);
        let deadline = Instant::now() + WAIT_LIMIT;
        loop {
            let cameras_called = self.calls.left() > left && self.calls.right() > right;
            self.harness.step();
            if cameras_called && self.has("Cameras ready") && self.has("Left Full-page preview") {
                self.settle();
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {action} to capture at {}",
                self.viewport.name
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Whether a node labelled exactly `label` exists.
    pub fn has(&self, label: &str) -> bool {
        self.harness.query_by_label(label).is_some()
    }

    /// The rectangle, in points, of the one node labelled exactly `label`.
    pub fn rect(&self, label: &str) -> egui::Rect {
        self.harness.get_by_label(label).rect()
    }

    /// The rectangle of the first node whose label contains `text`.
    pub fn rect_containing(&self, text: &str) -> egui::Rect {
        self.harness
            .query_all(By::new().label_contains(text))
            .next()
            .unwrap_or_else(|| panic!("no node contains {text:?}"))
            .rect()
    }

    /// Every node's label and rectangle, for nodes that have both.
    pub fn labelled_rects(&self) -> Vec<(String, egui::Rect)> {
        self.harness
            .query_all(By::new())
            .filter_map(|node| {
                let label = node
                    .accesskit_node()
                    .label()
                    .or_else(|| node.accesskit_node().value())?;
                node.accesskit_node().bounding_box()?;
                Some((label, node.rect()))
            })
            .collect()
    }

    /// Every node rectangle in the accessibility tree.
    pub fn all_rects(&self) -> Vec<egui::Rect> {
        self.harness
            .query_all(By::new())
            .filter(|node| node.accesskit_node().bounding_box().is_some())
            .map(|node| node.rect())
            .collect()
    }

    /// The body viewport the operator sees between header and action bar.
    pub fn body(&self) -> egui::Rect {
        self.rect(BODY)
    }

    /// Whether the node labelled `label` lies fully inside the body viewport.
    pub fn in_body(&self, label: &str) -> bool {
        self.harness
            .query_by_label(label)
            .is_some_and(|node| self.contains(self.body(), node.rect()))
    }

    /// Whether `outer` contains all of `inner`, within half a physical pixel.
    ///
    /// egui rounds widget rectangles to the physical pixel grid while panel
    /// edges can fall on half pixels.
    pub fn contains(&self, outer: egui::Rect, inner: egui::Rect) -> bool {
        outer
            .expand(0.5 / self.viewport.pixels_per_point + 0.001)
            .contains_rect(inner)
    }

    /// The whole window in points.
    pub fn window(&self) -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, self.viewport.size)
    }

    /// Press and release the primary pointer at `position`.
    pub fn tap(&mut self, position: egui::Pos2) {
        self.harness.event(egui::Event::PointerMoved(position));
        for pressed in [true, false] {
            self.harness.event(egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        self.harness.step();
        self.settle();
    }

    /// Tap the centre of the node labelled `label`.
    pub fn tap_label(&mut self, label: &str) {
        let position = self.rect(label).center();
        self.tap(position);
    }

    /// Turn the mouse wheel over the body by `points`. Positive scrolls toward the end.
    pub fn wheel(&mut self, points: f32) {
        self.harness
            .event(egui::Event::PointerMoved(self.body().center()));
        self.harness.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -points),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        self.harness.step();
        self.settle();
    }

    /// Drag the primary pointer from `from` to `to`, optionally with matching touch events.
    pub fn drag(&mut self, from: egui::Pos2, to: egui::Pos2, touch: bool) {
        self.drag_start(from, touch);
        self.drag_move(from, to, touch);
        self.drag_end(to, touch);
        self.settle();
    }

    /// Press at `at`, leaving the drag active.
    pub fn drag_start(&mut self, at: egui::Pos2, touch: bool) {
        if touch {
            self.touch(egui::TouchPhase::Start, at);
        }
        self.harness.event(egui::Event::PointerMoved(at));
        self.harness.drag_at(at);
        self.harness.step();
    }

    /// Move an active drag from `from` to `to` in several frames.
    pub fn drag_move(&mut self, from: egui::Pos2, to: egui::Pos2, touch: bool) {
        for step in 1..=8 {
            let at = from + (to - from) * (step as f32 / 8.0);
            if touch {
                self.touch(egui::TouchPhase::Move, at);
            }
            self.harness.event(egui::Event::PointerMoved(at));
            self.harness.step();
        }
    }

    /// Release an active drag at `at`.
    pub fn drag_end(&mut self, at: egui::Pos2, touch: bool) {
        if touch {
            self.touch(egui::TouchPhase::End, at);
        }
        self.harness.drop_at(at);
        self.harness.step();
    }

    fn touch(&mut self, phase: egui::TouchPhase, pos: egui::Pos2) {
        self.harness.event(egui::Event::Touch {
            device_id: egui::TouchDeviceId(1),
            id: egui::TouchId(1),
            phase,
            pos,
            force: None,
        });
    }

    /// Scroll from the top of the body to its end by wheel, calling `visit` at every step.
    pub fn wheel_through(&mut self, step: f32, mut visit: impl FnMut(&mut Self)) {
        self.wheel(-100_000.0);
        visit(self);
        for _ in 0..200 {
            let before = self.labelled_rects();
            self.wheel(step);
            visit(self);
            if self.labelled_rects() == before {
                return;
            }
        }
        panic!("the body never reached its end at {}", self.viewport.name);
    }

    /// Whether the application asked the window to close during the last frame.
    pub fn close_requested(&self) -> bool {
        self.harness
            .output()
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|output| output.commands.contains(&egui::ViewportCommand::Close))
    }
}
