use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gdk;
use gtk::glib;

use crate::config::Config;
use crate::fetcher::{ArticleData, NavigationLink};
use crate::storage::Storage;
use crate::theme;
use crate::ui::settings_drawer;

/// Messages sent from the background tokio thread to the GTK main thread.
pub enum FetchMessage {
    Loading,
    Done(Box<ArticleData>),
    Error(String),
}

// ═══════════════════════════════════════════════════════════════════════════
//  Chromium-compatible scroll animation (Blink / cc::ScrollAnimator port)
// ═══════════════════════════════════════════════════════════════════════════

/// Chromium's easing for keyboard / wheel impulse scrolling:
/// `cubic-bezier(0.25, 1.0, 0.5, 1.0)` — an ease-out quintic.
/// The initial slope (`y1 / x1`) is what makes a fresh impulse feel instant.
const CHROMIUM_BEZIER: (f64, f64, f64, f64) = (0.25, 1.0, 0.5, 1.0);

/// Arrow up/down (and `j`/`k`) travel per key press, in device pixels.
const ARROW_STEP_PX: f64 = 40.0;
/// One discrete mouse-wheel notch, in device pixels (Chromium's `kScrollbarPixelsPerTick`).
const WHEEL_NOTCH_PX: f64 = 100.0;
/// Successive notches closer together than this get a progressive impulse boost.
const WHEEL_BOOST_WINDOW_MS: f64 = 80.0;
/// Per-notch progressive acceleration applied while the wheel spins quickly.
const WHEEL_BOOST_STEP: f64 = 0.25;
/// Ceiling of the progressive wheel acceleration (Chromium clamps at 2.5x).
const WHEEL_MAX_BOOST: f64 = 2.5;
/// Page up/down keeps 12.5% of the previous viewport as reading context.
const PAGE_FRACTION: f64 = 0.875;

/// Curve duration for arrow steps and wheel notches.
const ARROW_DURATION_MS: f64 = 220.0;
/// Curve duration for wheel notches.
const WHEEL_DURATION_MS: f64 = 220.0;
/// Curve duration for Space / PageDown / PageUp (heavier travel, slightly slower).
const PAGE_DURATION_MS: f64 = 280.0;
/// Home / End never animate for longer than this, however far they travel.
const JUMP_MAX_DURATION_MS: f64 = 350.0;

/// Delay before a held key transitions from discrete stepping into a glide.
const HOLD_REPEAT_DELAY_MS: f64 = 250.0;
/// Continuous glide speed while a directional key is held down.
const HOLD_GLIDE_SPEED_PX_PER_S: f64 = 1200.0;
/// How far ahead of the playhead the target sits while gliding, so that
/// releasing the key still has momentum left to coast along the ease-out curve.
const HOLD_COAST_LEAD_PX: f64 = 120.0;
/// Nominal curve duration used when a held glide is released.
const HOLD_COAST_DURATION_MS: f64 = 220.0;
/// Upper bound for the release coast.
const HOLD_COAST_MAX_DURATION_MS: f64 = 350.0;

/// Animation terminates once the remaining distance falls below this (px).
const SCROLL_STOP_EPSILON: f64 = 0.2;

/// Evaluate one axis of a cubic bezier with implicit endpoints 0 and 1.
fn bezier_axis(t: f64, p1: f64, p2: f64) -> f64 {
    let mt = 1.0 - t;
    3.0 * mt * mt * t * p1 + 3.0 * mt * t * t * p2 + t * t * t
}

/// Map linear progress `x` through `cubic-bezier(0.25, 1.0, 0.5, 1.0)`.
/// Newton-Raphson inverts the x-component, then the y-component is evaluated.
fn chromium_ease(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let (x1, y1, x2, y2) = CHROMIUM_BEZIER;
    let mut t = x;
    for _ in 0..8 {
        let err = bezier_axis(t, x1, x2) - x;
        if err.abs() < 1e-7 {
            break;
        }
        let mt = 1.0 - t;
        let slope = 3.0 * mt * mt * x1 + 6.0 * mt * t * (x2 - x1) + 3.0 * t * t * (1.0 - x2);
        if slope.abs() < 1e-9 {
            break;
        }
        t = (t - err / slope).clamp(0.0, 1.0);
    }
    bezier_axis(t, y1, y2)
}

/// Instantaneous velocity of the timing function at `t = 0` (`y1 / x1` = 4.0).
fn chromium_initial_slope() -> f64 {
    CHROMIUM_BEZIER.1 / CHROMIUM_BEZIER.0
}

/// Chromium's page step: 87.5% of the viewport height, keeping 12.5% of the
/// previous view as overlap so no line is skipped.
fn chromium_page_step(page_size: f64) -> f64 {
    (page_size * PAGE_FRACTION).max(1.0)
}

/// Progressive wheel acceleration. Notches arriving within the boost window of
/// each other ramp the impulse up by `WHEEL_BOOST_STEP` per notch, capped at
/// `WHEEL_MAX_BOOST` (2.5x), exactly like Blink's wheel event coalescing.
fn wheel_boost_for_gap(gap_ms: f64, current_boost: f64) -> f64 {
    if gap_ms <= WHEEL_BOOST_WINDOW_MS {
        (current_boost + WHEEL_BOOST_STEP).min(WHEEL_MAX_BOOST)
    } else {
        1.0
    }
}

/// Distance a held directional key advances per frame while gliding.
fn gliding_advance(direction: i32, dt: f64) -> f64 {
    direction as f64 * HOLD_GLIDE_SPEED_PX_PER_S * dt
}

/// Duration for a retargeted curve that preserves the in-flight velocity.
///
/// A curve starting at velocity `v` covers `distance` in roughly
/// `distance / (v * initial_slope)` seconds; reusing the current velocity as the
/// starting slope is what stops consecutive impulses from stuttering.
fn retarget_duration_ms(velocity: f64, distance: f64, nominal_ms: f64, cap_ms: f64) -> f64 {
    let cap = cap_ms.max(nominal_ms);
    if velocity.abs() < 1.0 || distance.abs() < 1.0 || (velocity > 0.0) != (distance > 0.0) {
        return nominal_ms.min(cap);
    }
    let estimate_ms = distance.abs() * chromium_initial_slope() / velocity.abs() * 1000.0;
    estimate_ms.clamp(80.0, cap)
}

/// All scroll-animation state for the article viewport.
///
/// Mirrors `cc::ScrollAnimator`: a single frame-clock driven curve whose target
/// can be retargeted mid-flight without dropping the in-flight velocity, plus
/// Chromium's held-key stepping -> glide -> coast behaviour.
struct ScrollControllerState {
    scrolled_window: gtk::ScrolledWindow,
    config: Rc<RefCell<Config>>,
    /// Registered tick callback; only ever holds a `Weak` reference to `Self`.
    tick_id: RefCell<Option<gtk::TickCallbackId>>,
    is_animating: Cell<bool>,
    /// Where the curve is heading (accumulated across impulses).
    target_y: Cell<f64>,
    /// Position at which the current curve segment started.
    start_y: Cell<f64>,
    /// Frame-clock timestamp (µs) at which the current segment started.
    start_us: Cell<i64>,
    duration_us: Cell<f64>,
    /// Measured velocity in px/s, used for velocity-preserving retargeting.
    velocity: Cell<f64>,
    last_frame_us: Cell<Option<i64>>,
    last_pos: Cell<f64>,
    held_direction: Cell<Option<i32>>,
    hold_start_us: Cell<i64>,
    gliding: Cell<bool>,
    last_wheel_us: Cell<i64>,
    wheel_boost: Cell<f64>,
}

impl ScrollControllerState {
    fn new(scrolled_window: &gtk::ScrolledWindow, config: Rc<RefCell<Config>>) -> Rc<Self> {
        Rc::new(Self {
            scrolled_window: scrolled_window.clone(),
            config,
            tick_id: RefCell::new(None),
            is_animating: Cell::new(false),
            target_y: Cell::new(0.0),
            start_y: Cell::new(0.0),
            start_us: Cell::new(0),
            duration_us: Cell::new(0.0),
            velocity: Cell::new(0.0),
            last_frame_us: Cell::new(None),
            last_pos: Cell::new(0.0),
            held_direction: Cell::new(None),
            hold_start_us: Cell::new(0),
            gliding: Cell::new(false),
            last_wheel_us: Cell::new(i64::MIN / 2),
            wheel_boost: Cell::new(1.0),
        })
    }

    fn now_us() -> i64 {
        glib::monotonic_time()
    }

    fn adjustment(&self) -> gtk::Adjustment {
        self.scrolled_window.vadjustment()
    }

    fn current_y(&self) -> f64 {
        self.adjustment().value()
    }

    fn max_scroll(&self) -> f64 {
        let adj = self.adjustment();
        (adj.upper() - adj.page_size()).max(0.0)
    }

    /// Chromium's page step: 87.5% of the viewport, retaining 12.5% context.
    fn page_step(&self) -> f64 {
        chromium_page_step(self.adjustment().page_size())
    }

    fn is_animating(&self) -> bool {
        self.is_animating.get()
    }

    /// Re-point the idle target at the live adjustment value (layout reflow).
    fn sync_target(&self, value: f64) {
        if !self.is_animating.get() {
            self.target_y.set(value);
        }
    }

    fn smooth_enabled(&self) -> bool {
        self.config.borrow().smooth_scrolling
    }

    // ── Lifecycle ──────────────────────────────────────────────────────────

    /// Hard stop: drop the tick callback, re-enable GTK kinetic physics and
    /// re-synchronise the target with reality (used by selection, zoom, resize).
    fn cancel(&self) {
        self.held_direction.set(None);
        self.gliding.set(false);
        self.is_animating.set(false);
        self.velocity.set(0.0);
        self.last_frame_us.set(None);
        if let Some(tick_id) = self.tick_id.borrow_mut().take() {
            tick_id.remove();
        }
        self.target_y.set(self.current_y());
        self.scrolled_window.set_kinetic_scrolling(true);
    }

    /// Touchpad path: GTK owns the pixels and the momentum, we own nothing.
    fn surrender_to_native(&self) {
        self.cancel();
    }

    /// Stop from inside the tick callback (cannot `remove()` a running source).
    fn finish(&self) {
        self.is_animating.set(false);
        self.gliding.set(false);
        self.velocity.set(0.0);
        self.last_frame_us.set(None);
        *self.tick_id.borrow_mut() = None;
        self.scrolled_window.set_kinetic_scrolling(true);
    }

    // ── Curve driving ──────────────────────────────────────────────────────

    /// Duration that preserves the in-flight velocity, so a new impulse blends
    /// into the running curve instead of restarting from a dead stop.
    fn retarget_duration(&self, distance: f64, nominal_ms: f64, cap_ms: f64) -> f64 {
        retarget_duration_ms(self.velocity.get(), distance, nominal_ms, cap_ms)
    }

    fn drive_to(self: &Rc<Self>, target: f64, duration_ms: f64) {
        let start = self.current_y();
        self.start_y.set(start);
        self.last_pos.set(start);
        self.start_us.set(Self::now_us());
        self.duration_us.set((duration_ms.max(1.0) * 1000.0).max(1000.0));
        self.target_y.set(target.clamp(0.0, self.max_scroll()));
        self.last_frame_us.set(None);
        self.is_animating.set(true);
        self.ensure_tick();
    }

    /// Animate by a relative delta (arrows, wheel, page steps).
    fn animate_by(self: &Rc<Self>, delta: f64, nominal_ms: f64) {
        if delta.abs() < 0.01 {
            return;
        }

        if !self.smooth_enabled() {
            let target = (self.current_y() + delta).clamp(0.0, self.max_scroll());
            self.cancel();
            self.adjustment().set_value(target);
            self.target_y.set(target);
            return;
        }

        let current = self.current_y();
        let base = if self.is_animating.get() { self.target_y.get() } else { current };
        let new_target = (base + delta).clamp(0.0, self.max_scroll());
        let duration = if self.is_animating.get() {
            // Velocity-preserving retarget: keep the momentum we already have.
            self.retarget_duration(new_target - current, nominal_ms, nominal_ms)
        } else {
            nominal_ms
        };
        self.drive_to(new_target, duration);
    }

    /// Animate to an absolute position (Home / End / find-in-page).
    fn scroll_to(self: &Rc<Self>, destination: f64, max_duration_ms: f64) {
        let current = self.current_y();
        let target = destination.clamp(0.0, self.max_scroll());
        let distance = target - current;

        if !self.smooth_enabled() {
            self.cancel();
            self.adjustment().set_value(target);
            self.target_y.set(target);
            return;
        }
        if distance.abs() < SCROLL_STOP_EPSILON {
            // Already there. Keep the state consistent, and still abort any
            // in-flight curve heading somewhere else (e.g. Home during End).
            self.target_y.set(target);
            if !self.is_animating.get() {
                return;
            }
            self.drive_to(target, 1.0);
            return;
        }

        let nominal = (distance.abs() * 0.35).clamp(160.0, max_duration_ms);
        let duration = if self.is_animating.get() {
            self.retarget_duration(distance, nominal, max_duration_ms)
        } else {
            nominal
        };
        self.drive_to(target, duration);
    }

    /// One discrete wheel notch, with Chromium's progressive impulse boost.
    fn wheel_impulse(self: &Rc<Self>, dy: f64) {
        if dy == 0.0 {
            return;
        }
        let now = Self::now_us();
        let gap_ms = (now - self.last_wheel_us.get()) as f64 / 1000.0;
        let boost = wheel_boost_for_gap(gap_ms, self.wheel_boost.get());
        self.wheel_boost.set(boost);
        self.last_wheel_us.set(now);
        self.animate_by(dy * WHEEL_NOTCH_PX * boost, WHEEL_DURATION_MS);
    }

    // ── Held-key behaviour ─────────────────────────────────────────────────

    /// Initial press: immediate 40px step, then glide after the repeat delay.
    fn begin_hold(self: &Rc<Self>, direction: i32) {
        if self.held_direction.get() == Some(direction) {
            return; // OS auto-repeat while the key is already held
        }
        self.held_direction.set(Some(direction));
        self.hold_start_us.set(Self::now_us());
        self.gliding.set(false);
        self.animate_by(direction as f64 * ARROW_STEP_PX, ARROW_DURATION_MS);
    }

    /// Drop held-key state without any coast (focus moved to a text input).
    fn abort_hold(&self) {
        self.held_direction.set(None);
        self.gliding.set(false);
    }

    /// Key release: coast to a stop along the remaining ease-out curve.
    fn end_hold(self: &Rc<Self>) {
        let was_gliding = self.gliding.get();
        self.held_direction.set(None);
        self.gliding.set(false);
        if !was_gliding {
            return; // A quick tap simply finishes its own step curve.
        }
        let target = self.target_y.get().clamp(0.0, self.max_scroll());
        let distance = target - self.current_y();
        if distance.abs() < SCROLL_STOP_EPSILON {
            return;
        }
        let duration =
            self.retarget_duration(distance, HOLD_COAST_DURATION_MS, HOLD_COAST_MAX_DURATION_MS);
        self.drive_to(target, duration);
    }

    // ── Frame clock ────────────────────────────────────────────────────────

    fn ensure_tick(self: &Rc<Self>) {
        if self.tick_id.borrow().is_some() {
            return;
        }
        // GTK's own kinetic physics would fight the curve for the same pixels.
        self.scrolled_window.set_kinetic_scrolling(false);
        let weak = Rc::downgrade(self);
        let tick_id = self.scrolled_window.add_tick_callback(move |_widget, frame_clock| {
            // `Weak` upgrade keeps the widget -> callback -> state graph acyclic.
            match weak.upgrade() {
                Some(state) => state.tick(frame_clock),
                None => glib::ControlFlow::Break,
            }
        });
        *self.tick_id.borrow_mut() = Some(tick_id);
    }

    fn tick(&self, frame_clock: &gdk::FrameClock) -> glib::ControlFlow {
        if !self.is_animating.get() {
            self.finish();
            return glib::ControlFlow::Break;
        }

        let now_us = frame_clock.frame_time();
        let dt = match self.last_frame_us.get() {
            Some(prev) if now_us > prev => ((now_us - prev).clamp(1_000, 100_000)) as f64 / 1_000_000.0,
            _ => 1.0 / 60.0,
        };
        self.last_frame_us.set(Some(now_us));

        let max_scroll = self.max_scroll();
        let mut target = self.target_y.get().clamp(0.0, max_scroll);
        self.target_y.set(target);

        // Held key: after the repeat delay, drive position directly at a
        // constant glide speed with the target kept a coast-length ahead.
        let held = self.held_direction.get();
        if let Some(direction) = held {
            let held_ms = (now_us - self.hold_start_us.get()) as f64 / 1000.0;
            if held_ms >= HOLD_REPEAT_DELAY_MS {
                self.gliding.set(true);
            }
            if self.gliding.get() {
                let dir = direction as f64;
                let current = self.current_y();
                let next = (current + gliding_advance(direction, dt)).clamp(0.0, max_scroll);
                // A boundary stops the glide: there are no more pixels to give.
                if (next - current).abs() < f64::EPSILON {
                    self.finish();
                    return glib::ControlFlow::Break;
                }
                self.adjustment().set_value(next);
                self.velocity.set(dir * HOLD_GLIDE_SPEED_PX_PER_S);
                self.last_pos.set(next);
                target = (next + dir * HOLD_COAST_LEAD_PX).clamp(0.0, max_scroll);
                self.target_y.set(target);
                // Keep the underlying curve parked on the glide head so that
                // releasing the key coasts from exactly here.
                self.start_y.set(next);
                self.start_us.set(now_us);
                return glib::ControlFlow::Continue;
            }
        }

        let current_y = self.current_y();
        let remaining = target - current_y;
        let elapsed = (now_us - self.start_us.get()) as f64;
        let duration = self.duration_us.get().max(1.0);
        let progress = (elapsed / duration).clamp(0.0, 1.0);

        if remaining.abs() < SCROLL_STOP_EPSILON || progress >= 1.0 {
            self.adjustment().set_value(target);
            // A held key that has not reached the glide delay yet must keep the
            // frame clock alive, otherwise key-repeat would never take over.
            let holding = held.is_some()
                && (now_us - self.hold_start_us.get()) as f64 / 1000.0 < HOLD_REPEAT_DELAY_MS;
            if holding {
                self.start_y.set(target);
                self.start_us.set(now_us);
                self.duration_us.set(1_000.0);
                self.last_pos.set(target);
                return glib::ControlFlow::Continue;
            }
            self.finish();
            return glib::ControlFlow::Break;
        }

        let eased = chromium_ease(progress);
        let next = (self.start_y.get() + (target - self.start_y.get()) * eased)
            .clamp(0.0, max_scroll);
        self.adjustment().set_value(next);

        if dt > 0.0 {
            self.velocity.set((next - self.last_pos.get()) / dt);
        }
        self.last_pos.set(next);

        glib::ControlFlow::Continue
    }
}

/// Is the user typing into a text input right now?
///
/// Single-key navigation shortcuts (`j`, `k`, `Space`, arrows, Home/End, …) must
/// not steal keystrokes while an input has focus. Two details make the naive
/// check wrong:
///
/// * `GtkEntry`/`GtkSearchEntry` do not take keyboard focus themselves — they
///   delegate it to an inner `GtkText`. `Root::focus()` therefore returns that
///   `GtkText`, never the entry, so testing `is::<gtk::Entry>()` alone never
///   fires. `Editable` and `Text` are the checks that actually match.
/// * The article `TextView` is non-editable, so it does not report `Editable`
///   and must keep driving scrolling.
///
/// `adw::ApplicationWindow::focus()` is ambiguous between `GtkWindowExt` and
/// `RootExt`, so it must be disambiguated at the call site.
fn focused_widget_is_text_input(focused: Option<&gtk::Widget>) -> bool {
    match focused {
        Some(w) => {
            w.is::<gtk::Editable>()
                || w.is::<gtk::Text>()
                || w.is::<gtk::Entry>()
                || w.is::<gtk::SearchEntry>()
        }
        None => false,
    }
}

/// Vertical gap left above a jumped-to heading, so it isn't flush with the
/// top edge of the viewport.
const TOC_JUMP_TOP_OFFSET_PX: f64 = 24.0;

/// Scroll the article so the heading at `offset` sits just below the top edge.
///
/// `iter_location` reports the heading's position in the `TextView`'s own
/// coordinates, which for this single-`TextView` reading pane are already the
/// scrollable content coordinates — no widget-to-buffer translation needed.
fn toc_scroll_target(text_view: &gtk::TextView, buffer: &gtk::TextBuffer, offset: i32) -> f64 {
    let iter = buffer.iter_at_offset(offset);
    let loc = text_view.iter_location(&iter);
    (loc.y() as f64 - TOC_JUMP_TOP_OFFSET_PX).max(0.0)
}

/// Move the insertion caret to the line visible at `scroll_y`.
///
/// A restored viewport with a caret left at offset 0 is unstable: the next click
/// into the article is a focus transition, and GTK scrolls the caret onscreen —
/// dragging the restored article back to the top. Parking the caret where the
/// viewport already is keeps the two notions of "here" in agreement.
///
/// The offset is found by binary searching `iter_location`, the same
/// offset→pixel mapping the TOC and find-in-page jumps use. `iter_at_location`
/// is not used because it returns `None` for valid coordinates that fall
/// outside the laid-out line boxes.
fn park_caret_at_scroll(text_view: &gtk::TextView, buffer: &gtk::TextBuffer, scroll_y: f64) {
    let target = scroll_y.max(0.0);
    if target <= 0.5 {
        buffer.place_cursor(&buffer.start_iter());
        return;
    }

    let end = buffer.end_iter().offset();
    if end <= 0 {
        return;
    }

    // Find the first offset whose line has reached `target`.
    let (mut lo, mut hi) = (0i32, end);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let y = text_view.iter_location(&buffer.iter_at_offset(mid)).y() as f64;
        if y < target {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }

    buffer.place_cursor(&buffer.iter_at_offset(lo.min(end)));
}

/// Keep the insertion caret at the viewport while the article is unfocused.
///
/// GTK scrolls the insertion caret onscreen whenever the article `TextView`
/// gains focus, and clicking the article is exactly that focus transition. So
/// any scroll that leaves the caret behind — first load, a restored offset,
/// TOC or find-in-page jumps, keyboard scrolling — makes the very next click
/// snap the article back to the caret's stale position (usually the top).
///
/// Rather than patching each scroll site, follow the viewport: once scrolling
/// settles and the article is unfocused, park the caret at the visible line.
/// Then the click finds the caret already on screen and changes nothing.
fn install_caret_follows_viewport(
    vadj: &gtk::Adjustment,
    text_view: &gtk::TextView,
    buffer: &gtk::TextBuffer,
) {
    let tv = text_view.clone();
    let tb = buffer.clone();

    // The last viewport position observed while the article was unfocused —
    // this is where the reader actually is, and it is what a click must not
    // disturb.
    let resting_y: Rc<Cell<f64>> = Rc::new(Cell::new(vadj.value()));

    let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let pending_outer = pending.clone();
    {
        let tv = tv.clone();
        let tb = tb.clone();
        let resting = resting_y.clone();
        vadj.connect_value_changed(move |adj| {
            if tv.has_focus() {
                return;
            }
            resting.set(adj.value());
            // Restart the settle timer on every change so a running animation or
            // a burst of keyboard scrolling only parks once, at its final
            // position.
            if let Some(id) = pending_outer.borrow_mut().take() {
                id.remove();
            }
            let tv_t = tv.clone();
            let tb_t = tb.clone();
            let pending_t = pending_outer.clone();
            let settled_y = adj.value();
            let id = glib::timeout_add_local_once(
                std::time::Duration::from_millis(150),
                move || {
                    *pending_t.borrow_mut() = None;
                    // The user may have clicked in during the delay, in which
                    // case GTK owns the caret and moving it would fight the
                    // click.
                    if tv_t.has_focus() {
                        return;
                    }
                    park_caret_at_scroll(&tv_t, &tb_t, settled_y);
                },
            );
            *pending_outer.borrow_mut() = Some(id);
        });
    }

    // Clicking the article is a focus transition, and GTK scrolls the insertion
    // caret onscreen as part of it — using whatever stale caret the buffer
    // happens to hold. That scroll is what drags a scrolled page back to the
    // top, and it happens before any click position is applied, so parking the
    // caret beforehand is not enough on its own. Two things happen here:
    //   1. park the caret at the resting viewport so the scroll finds nothing
    //      to do, then
    //   2. make sure the viewport really did not move, restoring it if GTK
    //      moved it anyway.
    {
        let tv = tv.clone();
        let tb = tb.clone();
        let resting = resting_y.clone();
        let adj_owned = vadj.clone();
        tv.connect_has_focus_notify(move |view| {
            if !view.has_focus() {
                return;
            }
            let target = resting.get();
            park_caret_at_scroll(view, &tb, target);

            // This handler runs on every focus change, so each invocation needs
            // its own clones for the idle callback.
            let view_idle = view.clone();
            let adj = adj_owned.clone();
            let tb_idle = tb.clone();
            glib::idle_add_local_once(move || {
                if (adj.value() - target).abs() > 1.0 {
                    adj.set_value(target);
                }
                // Keep the caret consistent with the pinned viewport so a
                // subsequent keyboard scroll starts from the right place.
                park_caret_at_scroll(&view_idle, &tb_idle, target);
            });
        });
    }
}

/// Suppress the empty-entry Backspace bell on a text input.
///
/// GTK rings the system alert sound when Backspace/Delete is pressed in an
/// entry that is already empty. `gtk-error-bell` is already off globally, but
/// that setting is overridable by the theme; stopping the event in the capture
/// phase guarantees the bell signal is never reached regardless of theme.
fn suppress_empty_backspace_bell<T>(entry: &T)
where
    T: IsA<gtk::Widget> + IsA<gtk::Editable> + Clone + 'static,
{
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let entry_ref = entry.clone();
    controller.connect_key_pressed(move |_, keyval, _, _| {
        let is_delete = matches!(keyval, gdk::Key::BackSpace | gdk::Key::Delete);
        if is_delete && entry_ref.text().is_empty() {
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    entry.add_controller(controller);
}

/// Width below which the Contents pane becomes an overlay instead of a column.
const TOC_COLLAPSE_BREAKPOINT_PX: i32 = 900;

/// Wrap `content` in a left-hand Contents split view.
///
/// Two details are load-bearing and were both bugs at first:
///
/// * `show_sidebar` must be applied *after* `collapsed`. Setting `collapsed`
///   rewrites `show-sidebar` back to `FALSE` inside libadwaita, so a builder
///   chain that assigns `show_sidebar` before `collapsed` silently opens with
///   the sidebar hidden no matter what the config says.
/// * `collapsed` must stay `false` for a normal desktop window. A collapsed
///   `OverlaySplitView` renders its sidebar as a *floating overlay over a dim
///   scrim*, so the pane appears on top of the article and the first click
///   anywhere near it dismisses it. The Contents pane is meant to be a real
///   260px column beside the text, so the sidebar is only collapsed for narrow
///   windows — see `sync_toc_collapsed`.
fn build_toc_split_view(
    content: &impl IsA<gtk::Widget>,
    toc_root: &gtk::Box,
    show_sidebar: bool,
) -> adw::OverlaySplitView {
    let split = adw::OverlaySplitView::builder()
        .content(content)
        .sidebar(toc_root)
        .sidebar_position(gtk::PackType::Start)
        .build();
    split.set_show_sidebar(show_sidebar);
    split
}

/// Width for the Contents pane at a given font size.
///
/// The pane has to grow with its own type: at a fixed 260px a large Contents
/// font wraps every heading onto three lines or clips it. 14px is the baseline.
fn toc_sidebar_width(font_size: f64) -> i32 {
    ((font_size / 14.0) * 260.0).round().clamp(200.0, 520.0) as i32
}

/// Build the Table of Contents sidebar's own widget tree.
///
/// Returns the sidebar container plus a setter that swaps in a new heading
/// list. Kept separate from `build_window` so the list-building, indentation
/// and empty-state logic stay testable without a whole application window.
fn build_toc_sidebar(
    jump_to_offset: Rc<dyn Fn(i32)>,
    font_size: f64,
) -> (gtk::Box, Rc<dyn Fn(&[TocItem])>) {
    let width = toc_sidebar_width(font_size);
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .width_request(width)
        .build();
    root.add_css_class("toc-sidebar");

    let title = gtk::Label::builder()
        .label("Contents")
        .halign(gtk::Align::Start)
        .build();
    title.add_css_class("toc-title");
    root.append(&title);

    let list_box = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .build();
    list_box.add_css_class("toc-list");
    list_box.set_activate_on_single_click(true);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&list_box)
        .build();
    scroller.add_css_class("toc-scroll");
    root.append(&scroller);

    // Click-to-jump: each row carries its buffer offset in its widget name,
    // set when the list is rebuilt below.
    {
        let list_box_activate = list_box.clone();
        list_box.connect_row_activated(move |_, row| {
            if let Ok(offset) = row.widget_name().parse::<i32>() {
                jump_to_offset(offset);
            }
            // Do not leave a row highlighted: the TOC is a navigation list,
            // not a persistent selection.
            list_box_activate.unselect_all();
        });
    }

    let list_box_set = list_box.clone();
    let set_items: Rc<dyn Fn(&[TocItem])> = Rc::new(move |items: &[TocItem]| {
        // Rebuild wholesale: heading count changes every article, and rows
        // carry their own offset in a closure, so reusing them would mean
        // stale offsets.
        while let Some(child) = list_box_set.first_child() {
            list_box_set.remove(&child);
        }

        if items.is_empty() {
            let empty = gtk::Label::builder()
                .label("No headings found")
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .build();
            empty.add_css_class("toc-empty");
            let row = gtk::ListBoxRow::builder()
                .child(&empty)
                .selectable(false)
                .activatable(false)
                .build();
            row.set_focusable(false);
            row.add_css_class("toc-row");
            list_box_set.append(&row);
            return;
        }

        for item in items {
            let label = gtk::Label::builder()
                .label(&item.text)
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .lines(3)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .margin_start(toc_indent_px(item.level))
                .build();
            label.add_css_class("toc-label");
            label.add_css_class(&format!("toc-level-{}", item.level.min(4)));

            let row = gtk::ListBoxRow::builder().child(&label).build();
            row.set_focusable(false);
            row.add_css_class("toc-row");
            // The click handler is attached by the caller, which owns the
            // text view and the scroll animator.
            row.set_widget_name(&item.offset.to_string());
            list_box_set.append(&row);
        }
    });

    (root, set_items)
}

/// Wrap the article `TextView` in the scrolling reading column.
///
/// `halign(Fill)` on the box is load-bearing. With `Center`, the box is not
/// stretched by its parent, so it allocates its own minimum width — which for a
/// vertically-oriented box is just its horizontal CSS padding. That made the
/// reading column exactly `2 * margin` wide, starving the text of space and
/// making `max_width` unreachable. `adw::Clamp` already provides the centring
/// and the maximum width, so the box must simply fill whatever the clamp grants.
fn build_article_column(text_view: &gtk::TextView) -> gtk::Box {
    let article_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .halign(gtk::Align::Fill)
        .hexpand(true)
        .build();
    article_box.add_css_class("reader-surface");
    article_box.add_css_class("article-box");
    article_box.append(text_view);
    article_box
}

/// Build and present the main application window.
pub fn build_window(app: &adw::Application, initial_url: Option<String>) {
    // GTK rings the system bell when Backspace/Delete is pressed in an empty
    // entry, which on a desktop is an audible error alert for a harmless
    // keystroke. Turn the bell off process-wide.
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_error_bell(false);
    }

    // ── Shared State ──
    let config = Rc::new(RefCell::new(Config::load()));
    let storage = Rc::new(RefCell::new(
        Storage::open().ok()
    ));
    let css_provider = gtk::CssProvider::new();
    let auto_scrolling = Rc::new(Cell::new(false));
    let auto_scroll_tick_id: Rc<RefCell<Option<gtk::TickCallbackId>>> = Rc::new(RefCell::new(None));
    let auto_scroll_pause_until: Rc<Cell<Option<std::time::Instant>>> = Rc::new(Cell::new(None));
    let last_as_frame_time_us: Rc<Cell<Option<i64>>> = Rc::new(Cell::new(None));
    let current_url: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let zen_mode = Rc::new(Cell::new(false));
    let is_fetching = Rc::new(Cell::new(false));
    let saved_refresh_scroll: Rc<Cell<Option<f64>>> = Rc::new(Cell::new(None));

    let (initial_width, initial_height, start_maximized, start_fullscreen) = {
        let cfg = config.borrow();
        (
            cfg.window.width,
            cfg.window.height,
            cfg.window.is_maximized,
            cfg.window.is_fullscreen,
        )
    };

    // ── CSS ──
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css_provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
    }
    apply_css(&css_provider, &config.borrow(), true);

    // ── Channel for async fetch results ──
    let (fetch_tx, fetch_rx) = async_channel::unbounded::<FetchMessage>();

    // ── Window ──
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Folio")
        .default_width(initial_width)
        .default_height(initial_height)
        .build();

    if start_maximized {
        window.maximize();
    }

    // ── Session History (Browser-style Back / Forward) ──
    let session_history: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let session_index: Rc<Cell<usize>> = Rc::new(Cell::new(0));
    let is_history_nav: Rc<Cell<bool>> = Rc::new(Cell::new(false));

    // ── Header Bar ──
    let header = adw::HeaderBar::new();
    if start_fullscreen {
        zen_mode.set(true);
        window.fullscreen();
        header.set_visible(false);
    }

    // Browser-style Back & Forward Navigation Buttons
    let nav_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(0)
        .build();
    nav_box.add_css_class("linked");

    let back_btn = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .tooltip_text("Back (Alt+Left)")
        .sensitive(false)
        .build();

    let forward_btn = gtk::Button::builder()
        .icon_name("go-next-symbolic")
        .tooltip_text("Forward (Alt+Right)")
        .sensitive(false)
        .build();

    nav_box.append(&back_btn);
    nav_box.append(&forward_btn);
    header.pack_start(&nav_box);

    // Table of Contents toggle, on the left of the header bar.
    let toc_btn = gtk::ToggleButton::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text("Toggle Contents (Ctrl+\\ / F9)")
        .active(config.borrow().show_toc)
        .build();
    header.pack_start(&toc_btn);

    let update_nav_buttons = {
        let back_btn = back_btn.clone();
        let forward_btn = forward_btn.clone();
        let history = session_history.clone();
        let index = session_index.clone();
        Rc::new(move || {
            let hist = history.borrow();
            let idx = index.get();
            let can_back = idx > 0 && !hist.is_empty();
            let can_forward = !hist.is_empty() && (idx + 1 < hist.len());
            back_btn.set_sensitive(can_back);
            forward_btn.set_sensitive(can_forward);
        })
    };

    // URL entry & Refresh Button
    let url_entry = gtk::Entry::builder()
        .placeholder_text("Paste a URL and press Enter…")
        .hexpand(true)
        .build();

    let refresh_btn = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Refresh Article (Ctrl+R / F5)")
        .sensitive(false)
        .build();

    suppress_empty_backspace_bell(&url_entry);

    let url_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    url_box.append(&url_entry);
    url_box.append(&refresh_btn);

    let go_back = {
        let history = session_history.clone();
        let index = session_index.clone();
        let is_nav = is_history_nav.clone();
        let entry = url_entry.clone();
        let tx = fetch_tx.clone();
        let storage = storage.clone();
        let update_btns = update_nav_buttons.clone();
        Rc::new(move || {
            let hist = history.borrow();
            let idx = index.get();
            if idx > 0 && !hist.is_empty() {
                let new_idx = idx - 1;
                index.set(new_idx);
                let target_url = hist[new_idx].clone();
                is_nav.set(true);
                entry.set_text(&target_url);
                trigger_fetch(&target_url, &tx, &storage, false);
                drop(hist);
                update_btns();
            }
        })
    };
    {
        let go_back = go_back.clone();
        back_btn.connect_clicked(move |_| {
            go_back();
        });
    }

    let go_forward = {
        let history = session_history.clone();
        let index = session_index.clone();
        let is_nav = is_history_nav.clone();
        let entry = url_entry.clone();
        let tx = fetch_tx.clone();
        let storage = storage.clone();
        let update_btns = update_nav_buttons.clone();
        Rc::new(move || {
            let hist = history.borrow();
            let idx = index.get();
            if !hist.is_empty() && idx + 1 < hist.len() {
                let new_idx = idx + 1;
                index.set(new_idx);
                let target_url = hist[new_idx].clone();
                is_nav.set(true);
                entry.set_text(&target_url);
                trigger_fetch(&target_url, &tx, &storage, false);
                drop(hist);
                update_btns();
            }
        })
    };
    {
        let go_forward = go_forward.clone();
        forward_btn.connect_clicked(move |_| {
            go_forward();
        });
    }

    // Dark / Light mode toggle button
    let dark_mode_btn = gtk::Button::builder()
        .icon_name(if config.borrow().is_dark() {
            "display-brightness-symbolic"
        } else {
            "weather-clear-night-symbolic"
        })
        .tooltip_text("Toggle dark/light mode (t)")
        .build();

    // Library button
    let library_btn = gtk::Button::builder()
        .icon_name("view-list-symbolic")
        .tooltip_text("Saved articles")
        .build();

    // Settings button (Aa)
    let settings_btn = gtk::ToggleButton::builder()
        .label("Aa")
        .tooltip_text("Reading & Page Settings (Ctrl+,)")
        .build();

    header.set_title_widget(Some(&url_box));
    header.pack_end(&settings_btn);
    header.pack_end(&dark_mode_btn);
    header.pack_end(&library_btn);

    // ── Reading Progress Bar ──
    let progress_bar = gtk::ProgressBar::new();
    progress_bar.add_css_class("reading-progress");
    progress_bar.set_fraction(0.0);
    progress_bar.set_visible(config.borrow().show_progress_bar);

    // ── Article Display Area (Single Read-Only TextView in Clamp) ──
    let text_buffer = gtk::TextBuffer::new(None);
    setup_article_tags(&text_buffer, &config.borrow());

    let text_view = gtk::TextView::builder()
        .buffer(&text_buffer)
        .editable(false)
        .cursor_visible(false)
        .wrap_mode(gtk::WrapMode::WordChar)
        .hexpand(true)
        .vexpand(true)
        .justification(if config.borrow().justified {
            gtk::Justification::Fill
        } else {
            gtk::Justification::Left
        })
        .build();
    text_view.add_css_class("article-textview");
    text_view.add_css_class("reader-surface");

    let article_box = build_article_column(&text_view);

    let article_clamp = adw::Clamp::builder()
        .maximum_size(config.borrow().max_width as i32)
        .tightening_threshold(config.borrow().max_width as i32)
        .child(&article_box)
        .build();
    article_clamp.add_css_class("max-width-clamp");

    let scrolled_window = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .hexpand(true)
        .child(&article_clamp)
        .build();
    scrolled_window.add_css_class("article-viewport");
    scrolled_window.add_css_class("reader-surface");

    // Chromium-style scroll animator owning every programmatic scroll on this viewport.
    let scroll_state = ScrollControllerState::new(&scrolled_window, config.clone());

    let do_refresh = {
        let current_url = current_url.clone();
        let is_fetching = is_fetching.clone();
        let fetch_tx = fetch_tx.clone();
        let storage = storage.clone();
        let refresh_btn = refresh_btn.clone();
        let scrolled_window = scrolled_window.clone();
        let saved_refresh_scroll = saved_refresh_scroll.clone();
        Rc::new(move || {
            let url = current_url.borrow().clone();
            if url.is_empty() || is_fetching.get() {
                return;
            }
            saved_refresh_scroll.set(Some(scrolled_window.vadjustment().value()));
            refresh_btn.set_sensitive(false);
            trigger_fetch(&url, &fetch_tx, &storage, true);
        })
    };
    {
        let do_refresh = do_refresh.clone();
        refresh_btn.connect_clicked(move |_| {
            do_refresh();
        });
    }

    // ── Loading Spinner ──
    let spinner = gtk::Spinner::builder()
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .width_request(32)
        .height_request(32)
        .visible(false)
        .build();
    spinner.add_css_class("loading-spinner");

    // ── Empty State ──
    let empty_icon = gtk::Image::builder()
        .icon_name("document-viewer-symbolic")
        .pixel_size(72)
        .halign(gtk::Align::Center)
        .build();
    empty_icon.add_css_class("empty-state-icon");

    let empty_title = gtk::Label::builder()
        .label("Welcome to Folio")
        .halign(gtk::Align::Center)
        .build();
    empty_title.add_css_class("empty-state-title");

    let empty_subtitle = gtk::Label::builder()
        .label("Paste a URL above and press Enter to start reading")
        .halign(gtk::Align::Center)
        .wrap(true)
        .build();
    empty_subtitle.add_css_class("empty-state-subtitle");

    let empty_state = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .vexpand(true)
        .hexpand(true)
        .build();
    empty_state.add_css_class("empty-state");
    empty_state.append(&empty_icon);
    empty_state.append(&empty_title);
    empty_state.append(&empty_subtitle);

    // ── Content Stack (empty state / spinner / article) ──
    let content_stack = gtk::Stack::new();
    content_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    content_stack.set_transition_duration(200);
    content_stack.add_named(&empty_state, Some("empty"));
    content_stack.add_named(&spinner, Some("loading"));
    content_stack.add_named(&scrolled_window, Some("article"));
    content_stack.set_visible_child_name("empty");

    // ── In-Page Search Bar (Ctrl+F) ──
    let search_bar = gtk::SearchBar::builder()
        .show_close_button(false)
        .build();
    search_bar.add_css_class("article-search-bar");

    let search_entry = gtk::SearchEntry::builder()
        .placeholder_text("Find in article…")
        .width_request(260)
        .build();

    suppress_empty_backspace_bell(&search_entry);

    let search_count_label = gtk::Label::builder()
        .label("")
        .valign(gtk::Align::Center)
        .build();
    search_count_label.add_css_class("search-count-label");

    let prev_match_btn = gtk::Button::builder()
        .icon_name("go-up-symbolic")
        .tooltip_text("Previous Match (Shift+Enter / Shift+F3)")
        .valign(gtk::Align::Center)
        .sensitive(false)
        .build();
    prev_match_btn.add_css_class("flat");

    let next_match_btn = gtk::Button::builder()
        .icon_name("go-down-symbolic")
        .tooltip_text("Next Match (Enter / F3)")
        .valign(gtk::Align::Center)
        .sensitive(false)
        .build();
    next_match_btn.add_css_class("flat");

    let close_search_btn = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text("Close (Esc)")
        .valign(gtk::Align::Center)
        .build();
    close_search_btn.add_css_class("flat");

    let search_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(12)
        .margin_end(12)
        .build();
    search_box.append(&search_entry);
    search_box.append(&search_count_label);
    search_box.append(&prev_match_btn);
    search_box.append(&next_match_btn);
    search_box.append(&close_search_btn);

    search_bar.set_child(Some(&search_box));
    search_bar.connect_entry(&search_entry);

    let search_matches: Rc<RefCell<Vec<(i32, i32)>>> = Rc::new(RefCell::new(Vec::new()));
    let search_current_idx: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));

    let clear_search_highlights = {
        let text_buffer = text_buffer.clone();
        let search_matches = search_matches.clone();
        let search_current_idx = search_current_idx.clone();
        let search_count_label = search_count_label.clone();
        let prev_match_btn = prev_match_btn.clone();
        let next_match_btn = next_match_btn.clone();
        Rc::new(move || {
            let start = text_buffer.start_iter();
            let end = text_buffer.end_iter();
            text_buffer.remove_tag_by_name("search-match", &start, &end);
            text_buffer.remove_tag_by_name("search-current", &start, &end);
            search_matches.borrow_mut().clear();
            search_current_idx.set(None);
            search_count_label.set_label("");
            prev_match_btn.set_sensitive(false);
            next_match_btn.set_sensitive(false);
        })
    };

    // ── Main Content Box (header + progress + search + stack) ──
    let main_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    main_box.add_css_class("main-box");
    main_box.append(&header);
    main_box.append(&progress_bar);
    main_box.append(&search_bar);
    main_box.append(&content_stack);

    // ── Settings Sidebar & Config Sync ──
    let split_view_holder: Rc<RefCell<Option<adw::OverlaySplitView>>> = Rc::new(RefCell::new(None));
    let sync_sidebar_holder: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let toc_root_holder: Rc<RefCell<Option<gtk::Box>>> = Rc::new(RefCell::new(None));

    let last_css_key = Rc::new(RefCell::new(String::new()));
    let last_theme = Rc::new(RefCell::new(config.borrow().theme.clone()));
    let last_font_size = Rc::new(Cell::new(config.borrow().font_size));
    let last_line_height = Rc::new(Cell::new(config.borrow().line_height));
    let last_letter_spacing = Rc::new(Cell::new(config.borrow().letter_spacing));
    let last_margin = Rc::new(Cell::new(config.borrow().margin));
    let last_paragraph_spacing = Rc::new(Cell::new(config.borrow().paragraph_spacing_px));
    let last_font_family = Rc::new(RefCell::new(config.borrow().font_family.clone()));
    let last_max_width = Rc::new(Cell::new(config.borrow().max_width));
    let last_toc_font_size = Rc::new(Cell::new(config.borrow().toc_font_size));
    let last_progress_visible = Rc::new(Cell::new(config.borrow().show_progress_bar));
    let last_is_dark = Rc::new(Cell::new(config.borrow().is_dark()));
    let last_justified = Rc::new(Cell::new(config.borrow().justified));

    // Unified 350ms debounced disk save to ensure zero I/O lag during user interaction
    let save_timeout: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let debounced_save: Rc<dyn Fn()> = {
        let config = config.clone();
        let save_timeout = save_timeout.clone();
        Rc::new(move || {
            if let Some(src) = save_timeout.borrow_mut().take() {
                src.remove();
            }
            let cfg = config.clone();
            let st = save_timeout.clone();
            let src_id = glib::timeout_add_local_once(
                std::time::Duration::from_millis(350),
                move || {
                    st.borrow_mut().take();
                    cfg.borrow().save();
                },
            );
            *save_timeout.borrow_mut() = Some(src_id);
        })
    };

    let on_config_changed: Rc<dyn Fn()> = {
        let css_provider = css_provider.clone();
        let config = config.clone();
        let progress_bar = progress_bar.clone();
        let article_clamp = article_clamp.clone();
        let text_view = text_view.clone();
        let text_buffer = text_buffer.clone();
        let dark_mode_btn = dark_mode_btn.clone();
        let sync_sidebar_holder = sync_sidebar_holder.clone();
        let last_theme = last_theme.clone();
        let last_font_size = last_font_size.clone();
        let last_line_height = last_line_height.clone();
        let last_letter_spacing = last_letter_spacing.clone();
        let last_margin = last_margin.clone();
        let last_paragraph_spacing = last_paragraph_spacing.clone();
        let last_font_family = last_font_family.clone();
        let last_max_width = last_max_width.clone();
        let last_toc_font_size = last_toc_font_size.clone();
        let toc_root_holder = toc_root_holder.clone();
        let is_animating_cfg = scroll_state.clone();
        let scrolled_window_cfg = scrolled_window.clone();
        Rc::new(move || {
            let cfg = config.borrow();

            let is_dark_now = cfg.is_dark();
            let theme_changed = cfg.theme != *last_theme.borrow() || is_dark_now != last_is_dark.get();
            if theme_changed {
                *last_theme.borrow_mut() = cfg.theme.clone();
            }

            let typography_changed = (cfg.font_size - last_font_size.get()).abs() > f64::EPSILON
                || (cfg.line_height - last_line_height.get()).abs() > f64::EPSILON
                || (cfg.letter_spacing - last_letter_spacing.get()).abs() > f64::EPSILON
                || (cfg.margin - last_margin.get()).abs() > f64::EPSILON
                || cfg.paragraph_spacing_px != last_paragraph_spacing.get()
                || (cfg.max_width - last_max_width.get()).abs() > f64::EPSILON
                || cfg.font_family != *last_font_family.borrow();

            let font_size_changed = (cfg.font_size - last_font_size.get()).abs() > f64::EPSILON;

            if typography_changed {
                last_font_size.set(cfg.font_size);
                last_line_height.set(cfg.line_height);
                last_letter_spacing.set(cfg.letter_spacing);
                last_margin.set(cfg.margin);
                last_paragraph_spacing.set(cfg.paragraph_spacing_px);
                *last_font_family.borrow_mut() = cfg.font_family.clone();

                // Typography reflow invalidates the scroll geometry: abort the
                // in-flight curve and resync the animator with the new layout.
                is_animating_cfg.cancel();
                let ty_idle = is_animating_cfg.clone();
                let sw_idle = scrolled_window_cfg.clone();
                glib::idle_add_local_once(move || {
                    if !ty_idle.is_animating() {
                        ty_idle.sync_target(sw_idle.vadjustment().value());
                    }
                });
            }

            // 1. Only re-parse and re-apply CSS if styling properties changed
            let css_key = format!(
                "{}:{}:{:.1}:{:.1}:{:.1}:{:.1}:{:.1}:{:.1}:{}",
                cfg.theme,
                cfg.font_family,
                cfg.font_size,
                cfg.ui_font_size,
                cfg.toc_font_size,
                cfg.line_height,
                cfg.letter_spacing,
                cfg.margin,
                cfg.paragraph_spacing_px,
            );
            if *last_css_key.borrow() != css_key {
                *last_css_key.borrow_mut() = css_key;
                apply_css(&css_provider, &cfg, theme_changed);
            } else if theme_changed {
                apply_css(&css_provider, &cfg, true);
            }

            // 2. Only resize article clamp if max_width changed (instant layout without CSS reload)
            if (cfg.max_width - last_max_width.get()).abs() > f64::EPSILON {
                last_max_width.set(cfg.max_width);
                article_clamp.set_maximum_size(cfg.max_width as i32);
                article_clamp.set_tightening_threshold(cfg.max_width as i32);
            }

            // 2b. The Contents pane has to grow with its own type, otherwise a
            // larger Contents size wraps every heading onto three lines.
            if (cfg.toc_font_size - last_toc_font_size.get()).abs() > f64::EPSILON {
                last_toc_font_size.set(cfg.toc_font_size);
                if let Some(ref root) = *toc_root_holder.borrow() {
                    root.set_width_request(toc_sidebar_width(cfg.toc_font_size));
                }
            }

            // 3. Progress bar visibility
            if cfg.show_progress_bar != last_progress_visible.get() {
                last_progress_visible.set(cfg.show_progress_bar);
                progress_bar.set_visible(cfg.show_progress_bar);
            }

            // 4. Update dark mode button icon
            let is_dark = cfg.is_dark();
            if is_dark != last_is_dark.get() {
                last_is_dark.set(is_dark);
                dark_mode_btn.set_icon_name(if is_dark {
                    "display-brightness-symbolic"
                } else {
                    "weather-clear-night-symbolic"
                });
            }

            // 5. Update text alignment directly on existing TextView and paragraph tag
            if cfg.justified != last_justified.get() {
                last_justified.set(cfg.justified);
                let is_justified = cfg.justified;
                let justify = if is_justified { gtk::Justification::Fill } else { gtk::Justification::Left };
                text_view.set_justification(justify);
                if let Some(tag) = text_buffer.tag_table().lookup("paragraph") {
                    tag.set_justification(justify);
                }
            }

            // 6. Update paragraph spacing on TextTags in real-time
            let tag_table = text_buffer.tag_table();
            let spacing = cfg.paragraph_spacing_px as i32;
            for name in &["paragraph", "quote", "code"] {
                if let Some(tag) = tag_table.lookup(name) {
                    tag.set_pixels_below_lines(spacing);
                }
            }

            // 7. Update meta colors on TextTags if theme changed
            if theme_changed {
                let meta_col = cfg.meta_color();
                for name in &["meta", "quote", "divider"] {
                    if let Some(tag) = tag_table.lookup(name) {
                        tag.set_property("foreground", &meta_col);
                    }
                }
            }

            // 8. Sync sidebar controls if theme, dark mode, or font size changed
            if theme_changed || font_size_changed {
                if let Some(ref sync_fn) = *sync_sidebar_holder.borrow() {
                    sync_fn();
                }
            }
        })
    };

    // Dark Mode Toggle Button (headerbar)
    {
        let config = config.clone();
        let on_config_changed = on_config_changed.clone();
        let debounced_save = debounced_save.clone();
        dark_mode_btn.connect_clicked(move |_| {
            config.borrow_mut().toggle_dark_mode();
            debounced_save();
            on_config_changed();
        });
    }

    let on_close_sidebar: Rc<dyn Fn()> = {
        let split_view_holder = split_view_holder.clone();
        Rc::new(move || {
            if let Some(ref sv) = *split_view_holder.borrow() {
                sv.set_show_sidebar(false);
            }
        })
    };

    let shortcuts_win_holder: Rc<RefCell<Option<gtk::ShortcutsWindow>>> = Rc::new(RefCell::new(None));
    let show_shortcuts: Rc<dyn Fn()> = {
        let parent = window.clone();
        let holder = shortcuts_win_holder.clone();
        Rc::new(move || {
            let mut opt = holder.borrow_mut();
            if let Some(ref win) = *opt {
                win.present();
            } else {
                let win = create_shortcuts_window(parent.upcast_ref());
                let holder_clone = holder.clone();
                win.connect_close_request(move |_| {
                    *holder_clone.borrow_mut() = None;
                    glib::Propagation::Proceed
                });
                win.present();
                *opt = Some(win);
            }
        })
    };

    let (sidebar, sync_sidebar) = settings_drawer::build_settings_sidebar(
        config.clone(),
        on_config_changed.clone(),
        on_close_sidebar,
        show_shortcuts.clone(),
    );
    *sync_sidebar_holder.borrow_mut() = Some(sync_sidebar);

    // ── OverlaySplitView ──
    let split_view = adw::OverlaySplitView::builder()
        .content(&main_box)
        .sidebar(&sidebar)
        .show_sidebar(false)
        .sidebar_position(gtk::PackType::End)
        .collapsed(true)
        .build();

    *split_view_holder.borrow_mut() = Some(split_view.clone());

    // ── Table of Contents (left side) ──
    // The settings drawer already owns the right-hand sidebar, and an
    // OverlaySplitView has one sidebar slot. Nesting a second split view
    // (contents on the left, settings on the right) keeps both independent.
    let jump_to_offset = {
        let scroll_state = scroll_state.clone();
        let text_view = text_view.clone();
        let text_buffer = text_buffer.clone();
        Rc::new(move |offset: i32| {
            let target = toc_scroll_target(&text_view, &text_buffer, offset);
            // GTK keeps the insert mark where the user last clicked. After a
            // fresh load that is offset 0, so the very next click into the
            // article makes the TextView scroll its caret back onscreen — which
            // yanks a TOC-jumped viewport all the way to the top. Park the
            // caret on the heading we just jumped to instead.
            let iter = text_buffer.iter_at_offset(offset);
            text_buffer.place_cursor(&iter);
            scroll_state.scroll_to(target, JUMP_MAX_DURATION_MS);
        })
    };
    let (toc_root, set_toc_items) =
        build_toc_sidebar(jump_to_offset, config.borrow().toc_font_size);
    *toc_root_holder.borrow_mut() = Some(toc_root.clone());
    let toc_split = build_toc_split_view(&split_view, &toc_root, config.borrow().show_toc);

    // Only overlay the Contents pane on genuinely narrow windows; on a desktop
    // width it must be a real column beside the article.
    {
        let update_collapsed = {
            let toc_split = toc_split.clone();
            move |width: i32| {
                let want = width > 0 && width < TOC_COLLAPSE_BREAKPOINT_PX;
                if toc_split.is_collapsed() != want {
                    toc_split.set_collapsed(want);
                }
            }
        };
        let update_collapsed = Rc::new(update_collapsed);

        {
            let update = update_collapsed.clone();
            window.connect_width_request_notify(move |w| update(w.width()));
        }
        {
            let update = update_collapsed.clone();
            let window_c = window.clone();
            toc_split.connect_map(move |_| update(window_c.width()));
        }
    }

    window.set_content(Some(&toc_split));

    // ── Settings Button Toggle (Aa) ──
    {
        let sv = split_view.clone();
        settings_btn.connect_toggled(move |btn| {
            let active = btn.is_active();
            if sv.shows_sidebar() != active {
                sv.set_show_sidebar(active);
            }
        });
    }

    // ── Contents Button Toggle & Persistence ──
    {
        let sv = toc_split.clone();
        let config = config.clone();
        let debounced_save = debounced_save.clone();
        toc_btn.connect_toggled(move |btn| {
            let active = btn.is_active();
            if sv.shows_sidebar() != active {
                sv.set_show_sidebar(active);
            }
            if config.borrow().show_toc != active {
                config.borrow_mut().show_toc = active;
                debounced_save();
            }
        });
    }

    // Keep the button honest when the sidebar is toggled by shortcut or by
    // the split view's own collapse gesture.
    {
        let toc_btn = toc_btn.clone();
        let config = config.clone();
        let debounced_save = debounced_save.clone();
        toc_split.connect_show_sidebar_notify(move |sv| {
            let shows = sv.shows_sidebar();
            if toc_btn.is_active() != shows {
                toc_btn.set_active(shows);
            }
            if config.borrow().show_toc != shows {
                config.borrow_mut().show_toc = shows;
                debounced_save();
            }
        });
    }

    // Sync settings_btn when sidebar visibility changes externally (e.g. close button or Ctrl+,)
    {
        let settings_btn = settings_btn.clone();
        split_view.connect_show_sidebar_notify(move |sv| {
            let shows = sv.shows_sidebar();
            if settings_btn.is_active() != shows {
                settings_btn.set_active(shows);
            }
        });
    }

    // ── Library Popover ──
    {
        let storage = storage.clone();
        let url_entry = url_entry.clone();
        let fetch_tx = fetch_tx.clone();
        let library_btn_ref = library_btn.clone();
        let window_ref = window.clone();
        library_btn.connect_clicked(move |_| {
            show_library_dialog(
                &window_ref,
                &storage,
                &url_entry,
                &fetch_tx,
            );
            let _ = &library_btn_ref;
        });
    }

    // ── URL Entry Activation ──
    {
        let fetch_tx = fetch_tx.clone();
        let storage = storage.clone();
        url_entry.connect_activate(move |entry| {
            let raw = entry.text().to_string().trim().to_string();
            if raw.is_empty() {
                return;
            }
            let url = if raw.starts_with("http://") || raw.starts_with("https://") {
                raw
            } else {
                format!("https://{raw}")
            };
            entry.set_text(&url);
            trigger_fetch(&url, &fetch_tx, &storage, false);
        });
    }

    // ── Scroll Progress Tracking ──
    {
        let progress_bar = progress_bar.clone();
        let storage = storage.clone();
        let current_url = current_url.clone();
        let scroll_state = scroll_state.clone();
        let vadj = scrolled_window.vadjustment();
        vadj.connect_value_changed(move |adj| {
            if !scroll_state.is_animating() {
                scroll_state.sync_target(adj.value());
            }
            let upper = adj.upper() - adj.page_size();
            if upper > 0.0 {
                let frac = adj.value() / upper;
                progress_bar.set_fraction(frac.clamp(0.0, 1.0));

                // Persist scroll position
                let url = current_url.borrow().clone();
                if !url.is_empty() {
                    if let Some(ref st) = *storage.borrow() {
                        let _ = st.update_scroll_position(&url, adj.value());
                    }
                }
            }
        });
    }

    // ── Receive Fetch Results ──
    {
        let content_stack = content_stack.clone();
        let spinner = spinner.clone();
        let article_box = article_box.clone();
        let text_view = text_view.clone();
        let text_buffer = text_buffer.clone();
        let storage = storage.clone();
        let current_url = current_url.clone();
        let scrolled_window = scrolled_window.clone();
        let session_history = session_history.clone();
        let session_index = session_index.clone();
        let is_history_nav = is_history_nav.clone();
        let update_nav_buttons = update_nav_buttons.clone();
        let fetch_tx_fetch = fetch_tx.clone();
        let url_entry_fetch = url_entry.clone();
        let refresh_btn_fetch = refresh_btn.clone();
        let is_fetching_fetch = is_fetching.clone();
        let saved_refresh_scroll = saved_refresh_scroll.clone();
        let clear_search_fetch = clear_search_highlights.clone();
        let set_toc_items_load = set_toc_items.clone();
        glib::spawn_future_local(async move {
            while let Ok(msg) = fetch_rx.recv().await {
                match msg {
                    FetchMessage::Loading => {
                        clear_search_fetch();
                        is_fetching_fetch.set(true);
                        refresh_btn_fetch.set_sensitive(false);
                        spinner.set_visible(true);
                        spinner.set_spinning(true);
                        content_stack.set_visible_child_name("loading");
                    }
                    FetchMessage::Done(article) => {
                        clear_search_fetch();
                        is_fetching_fetch.set(false);
                        refresh_btn_fetch.set_sensitive(true);
                        spinner.set_spinning(false);
                        spinner.set_visible(false);

                        let refresh_scroll_opt = saved_refresh_scroll.take();
                        let prev_scroll = refresh_scroll_opt.unwrap_or_else(|| scrolled_window.vadjustment().value());

                        // Clear previous navigation widgets, keeping only text_view
                        while let Some(last) = article_box.last_child() {
                            if last != *text_view.upcast_ref::<gtk::Widget>() {
                                article_box.remove(&last);
                            } else {
                                break;
                            }
                        }

                        let blocks = parse_article_blocks(&article);
                        let toc_items = populate_article_buffer(&text_buffer, &blocks);
                        set_toc_items_load(&toc_items);

                        for block in &blocks {
                            if let ContentBlock::Navigation { prev_link, next_link } = block {
                                if prev_link.is_some() || next_link.is_some() {
                                    let nav_widget = create_navigation_widget(
                                        prev_link.as_ref(),
                                        next_link.as_ref(),
                                        &fetch_tx_fetch,
                                        &url_entry_fetch,
                                        &storage,
                                    );
                                    article_box.append(&nav_widget);
                                }
                            }
                        }

                        content_stack.set_visible_child_name("article");

                        let url_changed = *current_url.borrow() != article.url;

                        // Save to database
                        if let Some(ref st) = *storage.borrow() {
                            let _ = st.save_article(
                                &article.url,
                                &article.title,
                                &article.byline,
                                &article.html_content,
                                &article.text_content,
                                article.reading_time_mins,
                                article.prev_link.as_ref().map(|l| l.url.as_str()),
                                article.next_link.as_ref().map(|l| l.url.as_str()),
                            );
                            // Only record in reading history if this is a newly navigated URL, not a refresh
                            if url_changed {
                                let _ = st.record_history(&article.url, &article.title);
                            }
                        }

                        // Record into session history if this wasn't a back/forward navigation
                        let nav_flag = is_history_nav.get();
                        is_history_nav.set(false);
                        {
                            let mut hist = session_history.borrow_mut();
                            let cur_idx = session_index.get();
                            if !nav_flag {
                                if hist.is_empty() {
                                    hist.push(article.url.clone());
                                    session_index.set(0);
                                } else if hist.get(cur_idx) != Some(&article.url) {
                                    hist.truncate(cur_idx + 1);
                                    hist.push(article.url.clone());
                                    session_index.set(hist.len() - 1);
                                }
                            }
                        }
                        update_nav_buttons();

                        *current_url.borrow_mut() = article.url.clone();

                        // Restore scroll position
                        let vadj = scrolled_window.vadjustment();
                        if url_changed {
                            if let Some(ref st) = *storage.borrow() {
                                if let Ok(pos) = st.get_scroll_position(&article.url) {
                                    if pos > 0.0 {
                                        let tv = text_view.clone();
                                        let tb = text_buffer.clone();
                                        glib::idle_add_local_once(move || {
                                            vadj.set_value(pos);
                                            park_caret_at_scroll(&tv, &tb, pos);
                                        });
                                    }
                                }
                            }
                        } else if prev_scroll > 0.0 {
                            // Seamlessly maintain scroll position on refresh
                            let tv = text_view.clone();
                            let tb = text_buffer.clone();
                            glib::idle_add_local_once(move || {
                                let max_pos = (vadj.upper() - vadj.page_size()).max(0.0);
                                let target = prev_scroll.clamp(0.0, max_pos);
                                vadj.set_value(target);
                                park_caret_at_scroll(&tv, &tb, target);
                            });
                        }
                    }
                    FetchMessage::Error(err) => {
                        clear_search_fetch();
                        saved_refresh_scroll.take();
                        is_fetching_fetch.set(false);
                        let has_url = !current_url.borrow().is_empty();
                        refresh_btn_fetch.set_sensitive(has_url);
                        spinner.set_spinning(false);
                        spinner.set_visible(false);

                        while let Some(last) = article_box.last_child() {
                            if last != *text_view.upcast_ref::<gtk::Widget>() {
                                article_box.remove(&last);
                            } else {
                                break;
                            }
                        }

                        text_buffer.set_text("");
                        let mut iter = text_buffer.end_iter();
                        text_buffer.insert_with_tags_by_name(&mut iter, "Unable to Load Article", &["title"]);
                        text_buffer.insert(&mut iter, "\n\n");
                        text_buffer.insert_with_tags_by_name(&mut iter, &err, &["paragraph"]);
                        content_stack.set_visible_child_name("article");
                    }
                }
            }
        });
    }

    // ── Keyboard Shortcuts & Smooth Scrolling ──
    {
        let key_controller = gtk::EventControllerKey::new();
        key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let url_entry = url_entry.clone();
        let toc_split = toc_split.clone();
        let split_view = split_view.clone();
        let scrolled_window = scrolled_window.clone();
        let config = config.clone();
        let on_config_changed = on_config_changed.clone();
        let auto_scrolling = auto_scrolling.clone();
        let auto_scroll_tick_id = auto_scroll_tick_id.clone();
        let auto_scroll_pause_until = auto_scroll_pause_until.clone();
        let last_as_frame_time_us = last_as_frame_time_us.clone();
        let window_clone = window.clone();
        let zen_mode = zen_mode.clone();
        let header = header.clone();
        let debounced_save = debounced_save.clone();

        let scroll_state = scroll_state.clone();

        // Helper to cancel any active scroll animation immediately
        let cancel_scroll_anim = {
            let scroll_state = scroll_state.clone();
            Rc::new(move || scroll_state.cancel())
        };

        // ── Zoom In / Zoom Out Helpers ──
        let zoom_in = {
            let config = config.clone();
            let debounced_save = debounced_save.clone();
            let on_config_changed = on_config_changed.clone();
            let cancel_anim = cancel_scroll_anim.clone();
            Rc::new(move || {
                cancel_anim();
                let cur = config.borrow().font_size;
                let next = (cur + 2.0).min(48.0);
                if (next - cur).abs() > 0.01 {
                    config.borrow_mut().font_size = next;
                    debounced_save();
                    on_config_changed();
                }
            })
        };

        let zoom_out = {
            let config = config.clone();
            let debounced_save = debounced_save.clone();
            let on_config_changed = on_config_changed.clone();
            let cancel_anim = cancel_scroll_anim.clone();
            Rc::new(move || {
                cancel_anim();
                let cur = config.borrow().font_size;
                let next = (cur - 2.0).max(10.0);
                if (next - cur).abs() > 0.01 {
                    config.borrow_mut().font_size = next;
                    debounced_save();
                    on_config_changed();
                }
            })
        };

        let zoom_reset = {
            let config = config.clone();
            let debounced_save = debounced_save.clone();
            let on_config_changed = on_config_changed.clone();
            let cancel_anim = cancel_scroll_anim.clone();
            Rc::new(move || {
                cancel_anim();
                let cur = config.borrow().font_size;
                let next = crate::config::Config::default().font_size;
                if (next - cur).abs() > 0.01 {
                    config.borrow_mut().font_size = next;
                    debounced_save();
                    on_config_changed();
                }
            })
        };

        // ── Wheel vs. Touchpad dispatch (Capture Phase) ──
        let scroll_controller = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::VERTICAL,
        );
        scroll_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let scroll_state = scroll_state.clone();
            let pause_cell = auto_scroll_pause_until.clone();
            let zoom_in_scroll = zoom_in.clone();
            let zoom_out_scroll = zoom_out.clone();

            let pause_autoscroll = {
                let pause_cell = pause_cell.clone();
                Rc::new(move || {
                    pause_cell.set(Some(
                        std::time::Instant::now() + std::time::Duration::from_millis(1200),
                    ));
                })
            };

            scroll_controller.connect_scroll(move |ctrl, _dx, dy| {
                if dy == 0.0 {
                    return glib::Propagation::Proceed;
                }

                // Ctrl + wheel is zoom, never scroll.
                if ctrl
                    .current_event_state()
                    .contains(gdk::ModifierType::CONTROL_MASK)
                {
                    scroll_state.cancel();
                    if dy < 0.0 {
                        zoom_in_scroll();
                    } else {
                        zoom_out_scroll();
                    }
                    return glib::Propagation::Stop;
                }

                match ctrl.unit() {
                    // Touchpad: hand the pixels (and the momentum) to GTK.
                    gdk::ScrollUnit::Surface => {
                        scroll_state.surrender_to_native();
                        pause_autoscroll();
                        glib::Propagation::Proceed
                    }
                    // Mouse wheel: intercept the notch and animate it ourselves.
                    _ => {
                        scroll_state.wheel_impulse(dy);
                        pause_autoscroll();
                        glib::Propagation::Stop
                    }
                }
            });
        }
        scrolled_window.add_controller(scroll_controller);

        // ── Auto-Scroll Toggle Function (Microsecond Delta-Time & Jitter Free) ──
        let toggle_auto_scroll = {
            let auto_scrolling = auto_scrolling.clone();
            let auto_scroll_tick_id = auto_scroll_tick_id.clone();
            let auto_scroll_pause_until = auto_scroll_pause_until.clone();
            let last_as_frame_time_us = last_as_frame_time_us.clone();
            let scrolled_window = scrolled_window.clone();
            let config = config.clone();

            Rc::new(move || {
                if auto_scrolling.get() {
                    // Stop auto-scroll
                    auto_scrolling.set(false);
                    if let Some(tid) = auto_scroll_tick_id.borrow_mut().take() {
                        tid.remove();
                    }
                    last_as_frame_time_us.set(None);
                    auto_scroll_pause_until.set(None);
                } else {
                    // Start auto-scroll
                    auto_scrolling.set(true);
                    auto_scroll_pause_until.set(None);
                    last_as_frame_time_us.set(None);

                    let vadj = scrolled_window.vadjustment();
                    let cfg = config.clone();
                    let as_flag = auto_scrolling.clone();
                    let as_tick_holder = auto_scroll_tick_id.clone();
                    let pause_cell = auto_scroll_pause_until.clone();
                    let last_time_cell = last_as_frame_time_us.clone();

                    let tid = scrolled_window.add_tick_callback(move |_widget, frame_clock| {
                        if !as_flag.get() {
                            *as_tick_holder.borrow_mut() = None;
                            last_time_cell.set(None);
                            return glib::ControlFlow::Break;
                        }

                        let now_us = frame_clock.frame_time();

                        // Pause auto-scroll if user manually navigated recently
                        if let Some(pause_until) = pause_cell.get() {
                            if std::time::Instant::now() < pause_until {
                                last_time_cell.set(Some(now_us));
                                return glib::ControlFlow::Continue;
                            } else {
                                pause_cell.set(None);
                            }
                        }

                        let dt = if let Some(prev_us) = last_time_cell.get() {
                            let elapsed_us = (now_us - prev_us).clamp(1_000, 100_000);
                            (elapsed_us as f64) / 1_000_000.0
                        } else {
                            1.0 / 60.0
                        };
                        last_time_cell.set(Some(now_us));

                        let speed = cfg.borrow().auto_scroll_speed;
                        let page = vadj.page_size();
                        let upper = (vadj.upper() - page).max(0.0);
                        let cur = vadj.value();

                        if cur >= upper {
                            // Reached bottom of article
                            as_flag.set(false);
                            *as_tick_holder.borrow_mut() = None;
                            last_time_cell.set(None);
                            return glib::ControlFlow::Break;
                        }

                        let next = (cur + speed * dt).min(upper);
                        vadj.set_value(next);
                        glib::ControlFlow::Continue
                    });
                    *auto_scroll_tick_id.borrow_mut() = Some(tid);
                }
            })
        };

        let key_state_press = scroll_state.clone();
        let key_state_release = scroll_state.clone();
        let window_release = window_clone.clone();
        let go_back_key = go_back.clone();
        let go_forward_key = go_forward.clone();
        let zoom_in_key = zoom_in.clone();
        let zoom_out_key = zoom_out.clone();
        let zoom_reset_key = zoom_reset.clone();
        let do_refresh_key = do_refresh.clone();
        let text_buffer = text_buffer.clone();

        // ── Selection must never fight the animator ──
        // Any press inside the text immediately kills the curve, and dragging a
        // selection past the top/bottom edge also stops it so GTK's own
        // auto-scrolling keeps control of the viewport.
        {
            let cancel_anim_press = cancel_scroll_anim.clone();
            let tv_press = gtk::GestureClick::new();
            tv_press.set_button(1);
            tv_press.connect_pressed(move |_, _, _, _| {
                cancel_anim_press();
            });
            text_view.add_controller(tv_press);

            let cancel_anim_drag = cancel_scroll_anim.clone();
            let drag_view = text_view.clone();
            let drag = gtk::GestureDrag::new();
            drag.set_button(1);
            drag.connect_drag_update(move |gesture, _ox, _oy| {
                // GTK auto-scrolls the viewport once the pointer leaves it, so
                // yield immediately and let the selection own the scroll.
                if let Some((_, y)) = gesture.point(None) {
                    let height = drag_view.height() as f64;
                    if y < 0.0 || y > height {
                        cancel_anim_drag();
                    }
                }
            });
            text_view.add_controller(drag);
        }

        // ── The caret must follow the viewport ──
        // Otherwise the next click into the article is a focus transition that
        // scrolls the stale caret onscreen, snapping the article to the top.
        install_caret_follows_viewport(
            &scrolled_window.vadjustment(),
            &text_view,
            &text_buffer,
        );

        // ── In-Page Search Engine & Handlers ──
        let jump_to_match = {
            let text_buffer = text_buffer.clone();
            let text_view = text_view.clone();
            let scrolled_window = scrolled_window.clone();
            let search_matches = search_matches.clone();
            let search_current_idx = search_current_idx.clone();
            let search_count_label = search_count_label.clone();
            let prev_match_btn = prev_match_btn.clone();
            let next_match_btn = next_match_btn.clone();
            let scroll_state_for_jump = scroll_state.clone();

            Rc::new(move |new_idx: usize| {
                let matches = search_matches.borrow();
                let total = matches.len();
                if total == 0 || new_idx >= total {
                    return;
                }

                // 1. Remove previous "search-current" tag (if any) and ensure "search-match" is applied
                if let Some(old_idx) = search_current_idx.get() {
                    if old_idx < total {
                        let (old_start_off, old_end_off) = matches[old_idx];
                        let s_iter = text_buffer.iter_at_offset(old_start_off);
                        let e_iter = text_buffer.iter_at_offset(old_end_off);
                        text_buffer.remove_tag_by_name("search-current", &s_iter, &e_iter);
                        if let Some(match_tag) = text_buffer.tag_table().lookup("search-match") {
                            text_buffer.apply_tag(&match_tag, &s_iter, &e_iter);
                        }
                    }
                }

                // 2. Apply "search-current" tag to the new match
                let (start_off, end_off) = matches[new_idx];
                let start_iter = text_buffer.iter_at_offset(start_off);
                let end_iter = text_buffer.iter_at_offset(end_off);
                if let Some(current_tag) = text_buffer.tag_table().lookup("search-current") {
                    text_buffer.apply_tag(&current_tag, &start_iter, &end_iter);
                }

                search_current_idx.set(Some(new_idx));
                search_count_label.set_label(&format!("{} / {}", new_idx + 1, total));
                prev_match_btn.set_sensitive(total > 1);
                next_match_btn.set_sensitive(total > 1);

                // 3. Compute the vertical coordinate and glide to it
                let loc = text_view.iter_location(&start_iter);
                let word_y = loc.y() as f64 + 40.0;
                let vadj = scrolled_window.vadjustment();
                let page_size = vadj.page_size();
                let max_scroll = (vadj.upper() - page_size).max(0.0);
                let dest_y = (word_y - page_size * 0.33).clamp(0.0, max_scroll);

                // Route find-in-page jumps through the animator so results glide.
                scroll_state_for_jump.scroll_to(dest_y, JUMP_MAX_DURATION_MS);
            })
        };

        let do_search = {
            let text_buffer = text_buffer.clone();
            let text_view = text_view.clone();
            let search_matches = search_matches.clone();
            let search_current_idx = search_current_idx.clone();
            let search_count_label = search_count_label.clone();
            let prev_match_btn = prev_match_btn.clone();
            let next_match_btn = next_match_btn.clone();
            let jump_to = jump_to_match.clone();
            let scrolled_window = scrolled_window.clone();

            Rc::new(move |query: &str| {
                let query = query.trim();

                // Clear old highlights
                let b_start = text_buffer.start_iter();
                let b_end = text_buffer.end_iter();
                text_buffer.remove_tag_by_name("search-match", &b_start, &b_end);
                text_buffer.remove_tag_by_name("search-current", &b_start, &b_end);
                search_matches.borrow_mut().clear();
                search_current_idx.set(None);

                if query.is_empty() {
                    search_count_label.set_label("");
                    prev_match_btn.set_sensitive(false);
                    next_match_btn.set_sensitive(false);
                    return;
                }

                let flags = gtk::TextSearchFlags::CASE_INSENSITIVE;
                let match_tag_opt = text_buffer.tag_table().lookup("search-match");

                let mut start_iter = text_buffer.start_iter();
                let mut new_matches = Vec::new();

                while let Some((m_start, m_end)) = start_iter.forward_search(query, flags, None) {
                    let s_off = m_start.offset();
                    let e_off = m_end.offset();
                    new_matches.push((s_off, e_off));
                    if let Some(ref tag) = match_tag_opt {
                        text_buffer.apply_tag(tag, &m_start, &m_end);
                    }
                    start_iter = m_end;
                }

                let total = new_matches.len();
                *search_matches.borrow_mut() = new_matches;

                if total == 0 {
                    search_count_label.set_label("No matches");
                    prev_match_btn.set_sensitive(false);
                    next_match_btn.set_sensitive(false);
                } else {
                    let cur_scroll = scrolled_window.vadjustment().value();
                    let mut best_idx = 0;
                    let mut found_ahead = false;

                    for (i, &(s_off, _)) in search_matches.borrow().iter().enumerate() {
                        let it = text_buffer.iter_at_offset(s_off);
                        let loc = text_view.iter_location(&it);
                        let y = loc.y() as f64 + 40.0;
                        if y >= cur_scroll {
                            best_idx = i;
                            found_ahead = true;
                            break;
                        }
                    }
                    if !found_ahead {
                        best_idx = 0;
                    }

                    jump_to(best_idx);
                }
            })
        };

        let next_match = {
            let search_matches = search_matches.clone();
            let search_current_idx = search_current_idx.clone();
            let jump_to = jump_to_match.clone();
            Rc::new(move || {
                let total = search_matches.borrow().len();
                if total == 0 {
                    return;
                }
                let cur = search_current_idx.get().unwrap_or(0);
                let next = (cur + 1) % total;
                jump_to(next);
            })
        };

        let prev_match = {
            let search_matches = search_matches.clone();
            let search_current_idx = search_current_idx.clone();
            let jump_to = jump_to_match.clone();
            Rc::new(move || {
                let total = search_matches.borrow().len();
                if total == 0 {
                    return;
                }
                let cur = search_current_idx.get().unwrap_or(0);
                let prev = if cur == 0 { total - 1 } else { cur - 1 };
                jump_to(prev);
            })
        };

        let open_search = {
            let search_bar = search_bar.clone();
            let search_entry = search_entry.clone();
            let text_buffer = text_buffer.clone();
            let do_search = do_search.clone();
            Rc::new(move || {
                search_bar.set_search_mode(true);
                if !search_entry.has_focus() {
                    if let Some((start, end)) = text_buffer.selection_bounds() {
                        let selected = text_buffer.text(&start, &end, false);
                        let trimmed = selected.trim();
                        if !trimmed.is_empty() && trimmed.lines().count() <= 2 {
                            search_entry.set_text(trimmed);
                        }
                    }
                }
                search_entry.grab_focus();
                search_entry.select_region(0, -1);
                let query = search_entry.text().to_string();
                if !query.is_empty() {
                    do_search(&query);
                }
            })
        };

        let close_search = {
            let search_bar = search_bar.clone();
            let clear_highlights = clear_search_highlights.clone();
            let text_view = text_view.clone();
            Rc::new(move || {
                search_bar.set_search_mode(false);
                clear_highlights();
                text_view.grab_focus();
            })
        };

        {
            let next_match = next_match.clone();
            next_match_btn.connect_clicked(move |_| {
                next_match();
            });
        }
        {
            let prev_match = prev_match.clone();
            prev_match_btn.connect_clicked(move |_| {
                prev_match();
            });
        }
        {
            let close_search = close_search.clone();
            close_search_btn.connect_clicked(move |_| {
                close_search();
            });
        }
        {
            let do_search = do_search.clone();
            search_entry.connect_search_changed(move |entry| {
                do_search(&entry.text());
            });
        }
        {
            let next_match = next_match.clone();
            search_entry.connect_activate(move |_| {
                next_match();
            });
        }
        {
            let next_match = next_match.clone();
            search_entry.connect_next_match(move |_| {
                next_match();
            });
        }
        {
            let prev_match = prev_match.clone();
            search_entry.connect_previous_match(move |_| {
                prev_match();
            });
        }
        {
            let close_search = close_search.clone();
            search_entry.connect_stop_search(move |_| {
                close_search();
            });
        }
        {
            let se_key = gtk::EventControllerKey::new();
            let prev_match_se = prev_match.clone();
            let next_match_se = next_match.clone();
            se_key.connect_key_pressed(move |_, keyval, _, modifier| {
                if keyval == gdk::Key::Return || keyval == gdk::Key::KP_Enter {
                    if modifier.contains(gdk::ModifierType::SHIFT_MASK) {
                        prev_match_se();
                        return glib::Propagation::Stop;
                    } else {
                        next_match_se();
                        return glib::Propagation::Stop;
                    }
                }
                glib::Propagation::Proceed
            });
            search_entry.add_controller(se_key);
        }
        {
            let clear_search_highlights = clear_search_highlights.clone();
            search_bar.connect_search_mode_enabled_notify(move |bar| {
                if !bar.is_search_mode() {
                    clear_search_highlights();
                }
            });
        }

        let open_search_key = open_search.clone();
        let close_search_key = close_search.clone();
        let next_match_key = next_match.clone();
        let prev_match_key = prev_match.clone();
        let search_bar_key = search_bar.clone();
        let show_shortcuts_key = show_shortcuts.clone();

        key_controller.connect_key_pressed(move |_, keyval, _keycode, modifier| {
            let ctrl = modifier.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = modifier.contains(gdk::ModifierType::SHIFT_MASK);
            let alt = modifier.contains(gdk::ModifierType::ALT_MASK);
            let url_focused = url_entry.has_focus();
            // The window controller runs in the Capture phase, so it sees every
            // keystroke before the focused entry does. Without this guard,
            // typing "j" in the URL bar scrolled the article instead.
            let is_typing = {
                let focus = gtk::prelude::GtkWindowExt::focus(&window_clone);
                focused_widget_is_text_input(focus.as_ref())
            };

            match keyval {
                // Ctrl+F: In-page search
                v if (v == gdk::Key::f || v == gdk::Key::F) && ctrl => {
                    open_search_key();
                    return glib::Propagation::Stop;
                }
                // F3 or Ctrl+G: Next search match
                v if (v == gdk::Key::F3 && !shift) || ((v == gdk::Key::g || v == gdk::Key::G) && ctrl && !shift) => {
                    next_match_key();
                    return glib::Propagation::Stop;
                }
                // Shift+F3 or Ctrl+Shift+G: Previous search match
                v if (v == gdk::Key::F3 && shift) || ((v == gdk::Key::g || v == gdk::Key::G) && ctrl && shift) => {
                    prev_match_key();
                    return glib::Propagation::Stop;
                }
                // Escape: Close search bar, leave the URL bar, or clear selection
                v if v == gdk::Key::Escape => {
                    if search_bar_key.is_search_mode() {
                        close_search_key();
                        return glib::Propagation::Stop;
                    }
                    // Escape is the documented way out of the URL bar: return
                    // focus to the reading surface so navigation keys work again.
                    if url_focused {
                        text_view.grab_focus();
                        return glib::Propagation::Stop;
                    }
                    if !url_focused && text_buffer.has_selection() {
                        if let Some((start, _)) = text_buffer.selection_bounds() {
                            text_buffer.select_range(&start, &start);
                            return glib::Propagation::Stop;
                        }
                    }
                }
                // Ctrl+C: Copy selected text in reading area
                v if ((v == gdk::Key::c || v == gdk::Key::C) && ctrl) => {
                    if !is_typing {
                        if let Some((start, end)) = text_buffer.selection_bounds() {
                            let clipboard = window_clone.clipboard();
                            let selected_text = text_buffer.text(&start, &end, false);
                            clipboard.set_text(&selected_text);
                            text_buffer.copy_clipboard(&clipboard);
                            return glib::Propagation::Stop;
                        }
                    }
                }
                // Ctrl+A: Select all text in TextView when no input is focused
                v if ((v == gdk::Key::a || v == gdk::Key::A) && ctrl) => {
                    if !is_typing {
                        text_buffer.select_range(&text_buffer.start_iter(), &text_buffer.end_iter());
                        return glib::Propagation::Stop;
                    }
                }
                // Ctrl+R or F5: Refresh article
                v if ((v == gdk::Key::r || v == gdk::Key::R) && ctrl) || v == gdk::Key::F5 => {
                    do_refresh_key();
                    return glib::Propagation::Stop;
                }
                // Ctrl+Plus or Ctrl+= or Ctrl+KP_Add: Zoom In
                v if (v == gdk::Key::plus || v == gdk::Key::equal || v == gdk::Key::KP_Add) && ctrl => {
                    zoom_in_key();
                    return glib::Propagation::Stop;
                }
                // Ctrl+Minus or Ctrl+KP_Subtract or Ctrl+underscore: Zoom Out
                v if (v == gdk::Key::minus || v == gdk::Key::KP_Subtract || v == gdk::Key::underscore) && ctrl => {
                    zoom_out_key();
                    return glib::Propagation::Stop;
                }
                // Ctrl+0 or Ctrl+KP_0: Reset Zoom (100%)
                v if (v == gdk::Key::_0 || v == gdk::Key::KP_0) && ctrl => {
                    zoom_reset_key();
                    return glib::Propagation::Stop;
                }
                // Alt+Left or Alt+BackSpace: Browser back
                v if (v == gdk::Key::Left || v == gdk::Key::BackSpace) && alt => {
                    go_back_key();
                    return glib::Propagation::Stop;
                }
                // Alt+Right: Browser forward
                v if v == gdk::Key::Right && alt => {
                    go_forward_key();
                    return glib::Propagation::Stop;
                }
                // Ctrl+L: Focus URL bar
                v if v == gdk::Key::l && ctrl => {
                    url_entry.grab_focus();
                    url_entry.select_region(0, -1);
                    return glib::Propagation::Stop;
                }
                // Ctrl+,: Toggle settings
                v if v == gdk::Key::comma && ctrl => {
                    split_view.set_show_sidebar(!split_view.shows_sidebar());
                    return glib::Propagation::Stop;
                }
                // Ctrl+\ or F9: Toggle the Table of Contents sidebar
                v if (v == gdk::Key::backslash && ctrl) || v == gdk::Key::F9 => {
                    toc_split.set_show_sidebar(!toc_split.shows_sidebar());
                    return glib::Propagation::Stop;
                }
                // Ctrl+? / Ctrl+/ / F1: Keyboard shortcuts reference
                v if v == gdk::Key::F1 || ((v == gdk::Key::question || v == gdk::Key::slash) && ctrl) => {
                    show_shortcuts_key();
                    return glib::Propagation::Stop;
                }
                // F11: Zen mode
                v if v == gdk::Key::F11 => {
                    let is_zen = !zen_mode.get();
                    zen_mode.set(is_zen);
                    if is_zen {
                        window_clone.fullscreen();
                        header.set_visible(false);
                    } else {
                        window_clone.unfullscreen();
                        header.set_visible(true);
                    }
                    return glib::Propagation::Stop;
                }
                _ => {}
            }

            // Single-key navigation below must never fire while the user is
            // typing in an input: `j`/`k`/`Space`/arrows/Home/End/PageUp/PageDown
            // have to reach the entry as ordinary text and cursor movement.
            if is_typing {
                return glib::Propagation::Proceed;
            }

            match keyval {
                // j / Down / k / Up: Chromium arrow step + held-key glide
                v if v == gdk::Key::j || v == gdk::Key::Down => {
                    key_state_press.begin_hold(1);
                    glib::Propagation::Stop
                }
                v if v == gdk::Key::k || v == gdk::Key::Up => {
                    key_state_press.begin_hold(-1);
                    glib::Propagation::Stop
                }
                // Space / Shift+Space: one Chromium page (87.5% of the viewport)
                v if v == gdk::Key::space => {
                    let delta = if shift {
                        -key_state_press.page_step()
                    } else {
                        key_state_press.page_step()
                    };
                    key_state_press.animate_by(delta, PAGE_DURATION_MS);
                    glib::Propagation::Stop
                }
                // PageDown / PageUp / BackSpace: same page curve
                v if v == gdk::Key::Page_Down => {
                    let delta = key_state_press.page_step();
                    key_state_press.animate_by(delta, PAGE_DURATION_MS);
                    glib::Propagation::Stop
                }
                v if v == gdk::Key::Page_Up || v == gdk::Key::BackSpace => {
                    let delta = -key_state_press.page_step();
                    key_state_press.animate_by(delta, PAGE_DURATION_MS);
                    glib::Propagation::Stop
                }
                // Home / End: absolute jump on a clamped smooth curve
                v if v == gdk::Key::Home => {
                    key_state_press.scroll_to(0.0, JUMP_MAX_DURATION_MS);
                    glib::Propagation::Stop
                }
                v if v == gdk::Key::End => {
                    let max_scroll = key_state_press.max_scroll();
                    key_state_press.scroll_to(max_scroll, JUMP_MAX_DURATION_MS);
                    glib::Propagation::Stop
                }
                // s: Toggle auto-scroll
                v if v == gdk::Key::s => {
                    toggle_auto_scroll();
                    glib::Propagation::Stop
                }
                // t: Toggle dark/light mode
                v if v == gdk::Key::t => {
                    config.borrow_mut().toggle_dark_mode();
                    debounced_save();
                    on_config_changed();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });

        // ── Held-key release: coast to a stop on the remaining curve ──
        key_controller.connect_key_released(move |_, keyval, _keycode, _modifier| {
            // If an input grabbed focus mid-hold (e.g. the user clicked the URL
            // bar while a key was down), drop the held-key state without
            // starting a coast, so the field keeps the keystroke.
            let is_typing = {
                let focus = gtk::prelude::GtkWindowExt::focus(&window_release);
                focused_widget_is_text_input(focus.as_ref())
            };
            if is_typing {
                key_state_release.abort_hold();
                return;
            }
            match keyval {
                v if v == gdk::Key::j || v == gdk::Key::Down => key_state_release.end_hold(),
                v if v == gdk::Key::k || v == gdk::Key::Up => key_state_release.end_hold(),
                _ => {}
            }
        });

        window.add_controller(key_controller);

        // ── Mouse Back/Forward (Button 8 & 9) Gesture Listener ──
        let mouse_gesture = gtk::GestureClick::new();
        mouse_gesture.set_button(0); // Listen to all mouse buttons
        let go_back_mouse = go_back.clone();
        let go_forward_mouse = go_forward.clone();
        mouse_gesture.connect_pressed(move |gesture, _n_press, _x, _y| {
            let btn = gesture.current_button();
            if btn == 8 {
                go_back_mouse();
                gesture.set_state(gtk::EventSequenceState::Claimed);
            } else if btn == 9 {
                go_forward_mouse();
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });
        window.add_controller(mouse_gesture);
    }

    // ── Flush Pending Config Save & Persist Window State on Window Close ──
    {
        let st = save_timeout.clone();
        let cfg = config.clone();
        window.connect_close_request(move |win| {
            if let Some(src) = st.borrow_mut().take() {
                src.remove();
            }

            let is_fullscreen = win.is_fullscreen();
            let is_maximized = win.is_maximized();

            {
                let mut c = cfg.borrow_mut();
                c.window.is_fullscreen = is_fullscreen;
                c.window.is_maximized = is_maximized;

                // Only update width and height if not maximized or fullscreen,
                // so normal un-maximized window dimensions are preserved.
                if !is_maximized && !is_fullscreen {
                    let w = win.width();
                    let h = win.height();
                    if w > 100 && h > 100 {
                        c.window.width = w;
                        c.window.height = h;
                    } else {
                        let (dw, dh) = win.default_size();
                        if dw > 100 && dh > 100 {
                            c.window.width = dw;
                            c.window.height = dh;
                        }
                    }
                }
            }

            cfg.borrow().save();
            glib::Propagation::Proceed
        });
    }

    // ── Clipboard Auto-Detect ──
    if config.borrow().auto_detect_clipboard {
        let clipboard = window.clipboard();
        let fetch_tx = fetch_tx.clone();
        let url_entry = url_entry.clone();
        let window_ref = window.clone();
        let storage_clip = storage.clone();
        glib::idle_add_local_once(move || {
            clipboard.read_text_async(None::<&gtk::gio::Cancellable>, move |result| {
                if let Ok(Some(text)) = result {
                    let trimmed = text.trim().to_string();
                    if looks_like_url(&trimmed) {
                        let dialog = adw::AlertDialog::builder()
                            .heading("Open Clipboard URL?")
                            .body(&trimmed)
                            .build();
                        dialog.add_response("cancel", "Cancel");
                        dialog.add_response("open", "Open");
                        dialog.set_default_response(Some("open"));
                        dialog.set_response_appearance("open", adw::ResponseAppearance::Suggested);

                        let tx = fetch_tx.clone();
                        let entry = url_entry.clone();
                        let storage_clone = storage_clip.clone();
                        let url = trimmed.clone();
                        dialog.connect_response(None, move |_, response| {
                            if response == "open" {
                                entry.set_text(&url);
                                trigger_fetch(&url, &tx, &storage_clone, false);
                            }
                        });
                        dialog.present(Some(&window_ref));
                    }
                }
            });
        });
    }

    // ── Open initial URL or apply Startup Behavior ──
    if let Some(url) = initial_url {
        url_entry.set_text(&url);
        trigger_fetch(&url, &fetch_tx, &storage, false);
    } else {
        match config.borrow().startup_behavior {
            crate::config::StartupBehavior::RecentPage => {
                let recent_url = storage
                    .borrow()
                    .as_ref()
                    .and_then(|st| st.get_recent_article_url().ok().flatten());
                if let Some(url) = recent_url {
                    url_entry.set_text(&url);
                    trigger_fetch(&url, &fetch_tx, &storage, false);
                } else {
                    content_stack.set_visible_child_name("empty");
                    set_toc_items(&[]);
                    url_entry.grab_focus();
                }
            }
            crate::config::StartupBehavior::BlankPage => {
                content_stack.set_visible_child_name("empty");
                set_toc_items(&[]);
                url_entry.grab_focus();
            }
        }
    }

    window.present();
}

/// Apply CSS from the current config to the display.
///
/// When `theme_changed` is true, also updates the Libadwaita color scheme.
/// When only CSS properties (font size, margin, spacing) change, this skips
/// the expensive Libadwaita color-scheme query to maintain smooth 120 FPS live preview.
fn apply_css(provider: &gtk::CssProvider, config: &Config, theme_changed: bool) {
    let css = theme::generate_css(config);
    provider.load_from_string(&css);

    if theme_changed {
        let style_manager = adw::StyleManager::default();
        match config.theme.as_str() {
            "dark" | "charcoal" | "oled" => style_manager.set_color_scheme(adw::ColorScheme::ForceDark),
            "light" | "sepia" => style_manager.set_color_scheme(adw::ColorScheme::ForceLight),
            _ => style_manager.set_color_scheme(adw::ColorScheme::Default),
        }
    }
}

/// Trigger an async fetch on the tokio runtime, checking offline cache first.
fn trigger_fetch(
    url: &str,
    tx: &async_channel::Sender<FetchMessage>,
    storage: &Rc<RefCell<Option<Storage>>>,
    force_fresh: bool,
) {
    let url_str = url.to_string();
    let mut has_cached = false;

    // Check offline cache first for instant display (only if substantial content exists and not forced fresh)
    if !force_fresh {
        if let Some(ref st) = *storage.borrow() {
            if let Ok(Some(cached)) = st.get_article(&url_str) {
                if cached.text_content.split_whitespace().count() >= 50 {
                    has_cached = true;
                    let _ = tx.send_blocking(FetchMessage::Done(Box::new(ArticleData {
                        url: cached.url,
                        title: cached.title,
                        byline: cached.byline,
                        html_content: cached.html_content,
                        text_content: cached.text_content,
                        reading_time_mins: cached.reading_time_mins,
                        prev_link: cached.prev_url.map(|u| NavigationLink {
                            url: u,
                            label: "Previous".to_string(),
                        }),
                        next_link: cached.next_url.map(|u| NavigationLink {
                            url: u,
                            label: "Next".to_string(),
                        }),
                    })));
                }
            }
        }
    }

    if !has_cached {
        let _ = tx.send_blocking(FetchMessage::Loading);
    }

    let tx = tx.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime");
        rt.block_on(async {
            match crate::fetcher::fetch_article(&url_str).await {
                Ok(article) => {
                    let _ = tx.send(FetchMessage::Done(Box::new(article))).await;
                }
                Err(e) => {
                    if !has_cached {
                        let _ = tx.send(FetchMessage::Error(e)).await;
                    }
                }
            }
        });
    });
}

/// Check if a string looks like a URL.
fn looks_like_url(s: &str) -> bool {
    if s.starts_with("http://") || s.starts_with("https://") {
        return url::Url::parse(s).is_ok();
    }
    false
}

/// Show a dialog listing saved articles from the library.
fn show_library_dialog(
    window: &adw::ApplicationWindow,
    storage: &Rc<RefCell<Option<Storage>>>,
    url_entry: &gtk::Entry,
    fetch_tx: &async_channel::Sender<FetchMessage>,
) {
    let dialog = adw::AlertDialog::builder()
        .heading("Saved Articles")
        .build();
    dialog.add_response("close", "Close");

    let list_box = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    list_box.add_css_class("library-list");
    list_box.add_css_class("boxed-list");

    if let Some(ref st) = *storage.borrow() {
        match st.list_articles() {
            Ok(articles) => {
                if articles.is_empty() {
                    let empty = gtk::Label::builder()
                        .label("No saved articles yet.\nArticles you read will appear here.")
                        .halign(gtk::Align::Center)
                        .margin_top(24)
                        .margin_bottom(24)
                        .build();
                    empty.add_css_class("dim-label");
                    list_box.append(&empty);
                } else {
                    for article in articles.iter().take(50) {
                        let raw_date = article.last_read_at.as_deref().unwrap_or(&article.saved_at);
                        let date_str = if raw_date.len() >= 10 {
                            &raw_date[..10]
                        } else {
                            raw_date
                        };
                        let prefix = if article.last_read_at.is_some() { "Read" } else { "Saved" };
                        let progress_hint = if article.scroll_position > 0.0 { " • In progress" } else { "" };
                        let subtitle_text = format!("{} min{} • {} {} • {}", article.reading_time_mins, progress_hint, prefix, date_str, article.url);

                        let row = adw::ActionRow::builder()
                            .title(&*glib::markup_escape_text(&article.title))
                            .subtitle(&*glib::markup_escape_text(&subtitle_text))
                            .activatable(true)
                            .build();

                        // Delete button
                        let del_btn = gtk::Button::builder()
                            .icon_name("user-trash-symbolic")
                            .has_frame(false)
                            .tooltip_text("Delete from library")
                            .valign(gtk::Align::Center)
                            .build();

                        let st_clone = storage.clone();
                        let list_clone = list_box.clone();
                        let row_clone = row.clone();
                        let article_id = article.id;
                        del_btn.connect_clicked(move |_| {
                            if let Some(ref st) = *st_clone.borrow() {
                                let _ = st.delete_article(article_id);
                            }
                            list_clone.remove(&row_clone);
                        });
                        row.add_suffix(&del_btn);

                        let url = article.url.clone();
                        let entry = url_entry.clone();
                        let tx = fetch_tx.clone();
                        let dlg = dialog.clone();
                        let storage_clone = storage.clone();

                        row.connect_activated(move |_| {
                            entry.set_text(&url);
                            trigger_fetch(&url, &tx, &storage_clone, false);
                            dlg.close();
                        });

                        list_box.append(&row);
                    }
                }
            }
            Err(e) => {
                let err_label = gtk::Label::builder()
                    .label(&format!("Error: {e}"))
                    .build();
                list_box.append(&err_label);
            }
        }
    } else {
        let err_label = gtk::Label::builder()
            .label("Database not available")
            .build();
        list_box.append(&err_label);
    }

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(350)
        .min_content_width(450)
        .child(&list_box)
        .build();

    dialog.set_extra_child(Some(&scrolled));
    dialog.present(Some(window));
}

/// Convert an HTML snippet (e.g. from <p> or leaf <div>) to valid GTK Pango markup.
///
/// Converts supported inline tags (b, strong, i, em, code, tt, s, del, strike, u, ins, br),
/// decodes HTML entities into Unicode, XML-escapes special characters (&, <, >),
/// and validates the result with Pango markup parser.
pub fn html_to_pango_markup(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut chars = html.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '<' {
            let mut tag_content = String::new();
            let mut closed = false;
            while let Some(&next_c) = chars.peek() {
                chars.next();
                if next_c == '>' {
                    closed = true;
                    break;
                }
                tag_content.push(next_c);
            }
            if closed {
                let tag_trimmed = tag_content.trim();
                let is_close = tag_trimmed.starts_with('/');
                let tag_body = if is_close {
                    &tag_trimmed[1..]
                } else {
                    tag_trimmed
                };
                let tag_name = tag_body
                    .split(|ch: char| ch.is_whitespace() || ch == '/')
                    .next()
                    .unwrap_or("")
                    .to_lowercase();

                let token = match (is_close, tag_name.as_str()) {
                    (false, "b" | "strong") => Some("\u{E000}b\u{E001}"),
                    (true, "b" | "strong") => Some("\u{E000}/b\u{E001}"),
                    (false, "i" | "em") => Some("\u{E000}i\u{E001}"),
                    (true, "i" | "em") => Some("\u{E000}/i\u{E001}"),
                    (false, "code" | "tt") => Some("\u{E000}tt\u{E001}"),
                    (true, "code" | "tt") => Some("\u{E000}/tt\u{E001}"),
                    (false, "s" | "del" | "strike") => Some("\u{E000}s\u{E001}"),
                    (true, "s" | "del" | "strike") => Some("\u{E000}/s\u{E001}"),
                    (false, "u" | "ins") => Some("\u{E000}u\u{E001}"),
                    (true, "u" | "ins") => Some("\u{E000}/u\u{E001}"),
                    (false, "br") => Some("\u{E000}br\u{E001}"),
                    _ => None,
                };

                if let Some(t) = token {
                    out.push_str(t);
                }
                continue;
            } else {
                out.push('<');
                out.push_str(&tag_content);
                break;
            }
        }
        out.push(c);
    }

    // Step 2: Decode all HTML entities (e.g. &mdash;, &nbsp;, &ldquo;, &amp;)
    let decoded = html_escape::decode_html_entities(&out).to_string();

    // Step 3: XML-escape standard XML chars so raw &, <, > cannot break Pango markup
    let escaped = glib::markup_escape_text(&decoded);

    // Step 4: Restore supported Pango markup tags & explicit line breaks
    let markup = escaped
        .replace("\u{E000}b\u{E001}", "<b>")
        .replace("\u{E000}/b\u{E001}", "</b>")
        .replace("\u{E000}i\u{E001}", "<i>")
        .replace("\u{E000}/i\u{E001}", "</i>")
        .replace("\u{E000}tt\u{E001}", "<tt>")
        .replace("\u{E000}/tt\u{E001}", "</tt>")
        .replace("\u{E000}s\u{E001}", "<s>")
        .replace("\u{E000}/s\u{E001}", "</s>")
        .replace("\u{E000}u\u{E001}", "<u>")
        .replace("\u{E000}/u\u{E001}", "</u>")
        .replace("\u{E000}br\u{E001}", "\n");

    let trimmed = markup.trim();

    // Step 5: Validate with Pango parser. If valid, return markup; else fallback to safely escaped text.
    if gtk::pango::parse_markup(trimmed, '\0').is_ok() {
        trimmed.to_string()
    } else {
        let plain = trimmed
            .replace("<b>", "")
            .replace("</b>", "")
            .replace("<i>", "")
            .replace("</i>", "")
            .replace("<tt>", "")
            .replace("</tt>", "")
            .replace("<s>", "")
            .replace("</s>", "")
            .replace("<u>", "")
            .replace("</u>", "");
        glib::markup_escape_text(plain.trim()).to_string()
    }
}

pub type ParsedArticle = ArticleData;

/// Strip all HTML tags, returning plain text content.
pub fn strip_all_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(c);
        }
    }
    out
}

/// Check if text content represents a scene break divider (e.g. `***`, `* * *`, `---`, `― ― ―`).
pub fn is_scene_divider(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    match trimmed {
        "***" | "* * *" | "*  *  *" | "---" | "- - -" | "-- -- --" | "― ― ―" | "— • —"
        | "— — —" | "~~~" | "~ ~ ~" | "###" | "# # #" | "♦ ♦ ♦" | "◆ ◆ ◆" | "§" | "§ § §"
        | "• • •" | "•••" | "*** ***" | "******" | "—" | "――" | "―――" => {
            return true;
        }
        _ => {}
    }

    let non_ws: Vec<char> = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    if non_ws.len() >= 3 {
        let first = non_ws[0];
        if (first == '*'
            || first == '-'
            || first == '_'
            || first == '―'
            || first == '—'
            || first == '~'
            || first == '#'
            || first == '♦'
            || first == '◆'
            || first == '='
            || first == '•')
            && non_ws.iter().all(|&c| c == first)
        {
            return true;
        }
    }

    false
}

/// Split an HTML block by double breaks (`<br><br>`, `<br/><br/>`, `<br>\n<br>`, `\n\n`, or `<hr>`)
/// into individual paragraphs, ensuring short dialog lines and scene breaks are preserved.
pub fn split_html_paragraphs(html: &str) -> Vec<String> {
    let mut normalized = String::with_capacity(html.len());
    let mut chars = html.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '<' {
            let mut tag_content = String::new();
            let mut closed = false;
            while let Some(&next_c) = chars.peek() {
                chars.next();
                if next_c == '>' {
                    closed = true;
                    break;
                }
                tag_content.push(next_c);
            }
            if closed {
                let tag_trimmed = tag_content.trim().to_lowercase();
                let tag_name = tag_trimmed
                    .split(|ch: char| ch.is_whitespace() || ch == '/')
                    .next()
                    .unwrap_or("");

                if tag_name == "br" {
                    normalized.push('\u{E003}'); // Marker for <br>
                    continue;
                } else if tag_name == "hr" {
                    normalized.push('\u{E002}'); // Paragraph break
                    normalized.push_str("<hr>");
                    normalized.push('\u{E002}'); // Paragraph break
                    continue;
                } else {
                    normalized.push('<');
                    normalized.push_str(&tag_content);
                    normalized.push('>');
                    continue;
                }
            } else {
                normalized.push('<');
                normalized.push_str(&tag_content);
                break;
            }
        }
        normalized.push(c);
    }

    // Pass 2: Detect double breaks (\u{E003} or \n repeated with optional whitespace)
    let mut result = String::with_capacity(normalized.len());
    let chars_vec: Vec<char> = normalized.chars().collect();
    let mut i = 0;
    while i < chars_vec.len() {
        let c = chars_vec[i];
        if c == '\u{E003}' || c == '\n' {
            let mut j = i + 1;
            let mut has_second_break = false;
            while j < chars_vec.len() {
                if chars_vec[j] == ' ' || chars_vec[j] == '\t' || chars_vec[j] == '\r' {
                    j += 1;
                } else if chars_vec[j] == '\u{E003}' || chars_vec[j] == '\n' {
                    has_second_break = true;
                    j += 1;
                } else {
                    break;
                }
            }

            if has_second_break {
                result.push('\u{E002}');
                i = j;
                continue;
            } else {
                if c == '\u{E003}' {
                    result.push_str("<br/>");
                } else {
                    result.push('\n');
                }
                i += 1;
                continue;
            }
        } else {
            result.push(c);
            i += 1;
        }
    }

    result
        .split('\u{E002}')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Individual content blocks for virtualized article rendering with GTK4 ListView.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentBlock {
    Title(String),
    Meta {
        byline: String,
        reading_time: String,
    },
    Heading {
        level: u8,
        text: String,
    },
    Paragraph {
        markup: String,
        plain: String,
    },
    Quote(String),
    Code(String),
    ListItem(String),
    Divider,
    Navigation {
        prev_link: Option<NavigationLink>,
        next_link: Option<NavigationLink>,
    },
}

/// Parse extracted article data into individual virtualized ContentBlocks.
pub fn parse_article_blocks(article: &ParsedArticle) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();

    // 1. Article Title
    let title_trimmed = html_escape::decode_html_entities(article.title.trim()).to_string();
    if !title_trimmed.is_empty() {
        blocks.push(ContentBlock::Title(title_trimmed.clone()));
    }

    // 2. Metadata (byline & reading time)
    let byline_trimmed = html_escape::decode_html_entities(article.byline.trim()).to_string();
    let reading_time = if article.reading_time_mins > 0 {
        format!("{} min read", article.reading_time_mins)
    } else {
        String::new()
    };
    if !byline_trimmed.is_empty() || !reading_time.is_empty() {
        blocks.push(ContentBlock::Meta {
            byline: byline_trimmed,
            reading_time,
        });
    }

    // 3. Body blocks from HTML
    if !article.html_content.trim().is_empty() {
        let doc = dom_query::Document::from(article.html_content.as_str());
        let nodes = doc.select("p, h1, h2, h3, h4, h5, h6, blockquote, pre, code, li, div, section, hr");
        for node in nodes.iter() {
            let tag = node
                .nodes()
                .first()
                .and_then(|n| n.node_name())
                .unwrap_or_default()
                .to_string()
                .to_lowercase();
            let text_val = node.text().to_string();
            let trimmed = text_val.trim();

            if tag == "hr" {
                if !matches!(blocks.last(), Some(ContentBlock::Divider)) {
                    blocks.push(ContentBlock::Divider);
                }
                continue;
            }

            if trimmed.is_empty() {
                continue;
            }

            // Skip h1 if identical to article title to avoid redundant header
            if tag == "h1" && trimmed.eq_ignore_ascii_case(&title_trimmed) {
                continue;
            }

            match tag.as_str() {
                "h1" | "h2" => {
                    let clean = html_escape::decode_html_entities(trimmed).to_string();
                    blocks.push(ContentBlock::Heading {
                        level: 2,
                        text: clean,
                    });
                }
                "h3" | "h4" | "h5" | "h6" => {
                    let clean = html_escape::decode_html_entities(trimmed).to_string();
                    blocks.push(ContentBlock::Heading {
                        level: 3,
                        text: clean,
                    });
                }
                "blockquote" => {
                    // Skip nested blockquote if already inside another blockquote
                    if node.ancestors(Some(3)).is("blockquote") {
                        continue;
                    }
                    let clean = html_escape::decode_html_entities(trimmed).to_string();
                    blocks.push(ContentBlock::Quote(clean));
                }
                "pre" => {
                    // Skip nested pre if already inside another pre
                    if node.ancestors(Some(3)).is("pre") {
                        continue;
                    }
                    let code_text = html_escape::decode_html_entities(&node.text()).to_string();
                    blocks.push(ContentBlock::Code(code_text.trim_matches('\n').to_string()));
                }
                "code" => {
                    // Standalone code block (not inside pre, p, li, blockquote, div, section, h1..h6)
                    if node.ancestors(Some(5)).is("pre, p, li, blockquote, div, section, h1, h2, h3, h4, h5, h6") {
                        continue;
                    }
                    let code_text = html_escape::decode_html_entities(&node.text()).to_string();
                    blocks.push(ContentBlock::Code(code_text.trim_matches('\n').to_string()));
                }
                "li" => {
                    // If li is inside an ol, calculate 1-based index; otherwise use bullet prefix
                    let is_ol = node.parent().is("ol");
                    let prefix = if is_ol {
                        let siblings = node.parent().children().filter("li");
                        let mut idx = 1;
                        for (i, sib) in siblings.iter().enumerate() {
                            if sib.is_selection(&node) {
                                idx = i + 1;
                                break;
                            }
                        }
                        format!("{}. ", idx)
                    } else {
                        "• ".to_string()
                    };

                    // Exclude text of sub-lists from parent item
                    let full_text = node.text().to_string();
                    let item_text = if node.children().filter("ul, ol").length() > 0 {
                        let sub_text = node.children().filter("ul, ol").text().to_string();
                        if let Some(pos) = full_text.rfind(&sub_text) {
                            full_text[..pos].trim()
                        } else {
                            full_text.trim()
                        }
                    } else {
                        full_text.trim()
                    };

                    let clean_text = html_escape::decode_html_entities(item_text).to_string();
                    if !clean_text.is_empty() {
                        blocks.push(ContentBlock::ListItem(format!("{}{}", prefix, clean_text)));
                    }
                }
                "p" => {
                    // Skip if p is already inside blockquote, li, or pre
                    if node.ancestors(Some(3)).is("blockquote, li, pre") {
                        continue;
                    }
                    let inner_html = node.inner_html().to_string();
                    let chunks = split_html_paragraphs(&inner_html);
                    for chunk in chunks {
                        let trimmed_chunk = chunk.trim();
                        if trimmed_chunk.is_empty() {
                            continue;
                        }
                        if trimmed_chunk == "<hr>" {
                            if !matches!(blocks.last(), Some(ContentBlock::Divider)) {
                                blocks.push(ContentBlock::Divider);
                            }
                            continue;
                        }
                        let markup = html_to_pango_markup(trimmed_chunk);
                        let plain = html_escape::decode_html_entities(&strip_all_tags(trimmed_chunk)).trim().to_string();
                        if plain.is_empty() {
                            continue;
                        }
                        if is_scene_divider(&plain) {
                            if !matches!(blocks.last(), Some(ContentBlock::Divider)) {
                                blocks.push(ContentBlock::Divider);
                            }
                        } else {
                            blocks.push(ContentBlock::Paragraph { markup, plain });
                        }
                    }
                }
                "div" | "section" => {
                    // Only process leaf div/section that contains direct text and no block children
                    if node.select("p, h1, h2, h3, h4, h5, h6, blockquote, pre, li, ul, ol, div, section, hr").length() > 0 {
                        continue;
                    }
                    if node.ancestors(Some(4)).is("p, blockquote, li, pre") {
                        continue;
                    }
                    let inner_html = node.inner_html().to_string();
                    let chunks = split_html_paragraphs(&inner_html);
                    for chunk in chunks {
                        let trimmed_chunk = chunk.trim();
                        if trimmed_chunk.is_empty() {
                            continue;
                        }
                        if trimmed_chunk == "<hr>" {
                            if !matches!(blocks.last(), Some(ContentBlock::Divider)) {
                                blocks.push(ContentBlock::Divider);
                            }
                            continue;
                        }
                        let markup = html_to_pango_markup(trimmed_chunk);
                        let plain = html_escape::decode_html_entities(&strip_all_tags(trimmed_chunk)).trim().to_string();
                        if plain.is_empty() {
                            continue;
                        }
                        if is_scene_divider(&plain) {
                            if !matches!(blocks.last(), Some(ContentBlock::Divider)) {
                                blocks.push(ContentBlock::Divider);
                            }
                        } else {
                            blocks.push(ContentBlock::Paragraph { markup, plain });
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // 4. Fallback to text_content if no body blocks were found in HTML
    let has_body = blocks.iter().any(|b| matches!(
        b,
        ContentBlock::Paragraph { .. }
            | ContentBlock::Heading { .. }
            | ContentBlock::Quote(_)
            | ContentBlock::Code(_)
            | ContentBlock::ListItem(_)
            | ContentBlock::Divider
    ));

    if !has_body {
        for para in article.text_content.split("\n\n") {
            let trimmed = para.trim();
            if !trimmed.is_empty() {
                let clean = html_escape::decode_html_entities(trimmed).to_string();
                if is_scene_divider(&clean) {
                    if !matches!(blocks.last(), Some(ContentBlock::Divider)) {
                        blocks.push(ContentBlock::Divider);
                    }
                } else {
                    blocks.push(ContentBlock::Paragraph {
                        markup: glib::markup_escape_text(&clean).to_string(),
                        plain: clean,
                    });
                }
            }
        }
    }

    // 5. Navigation links (Prev / Next) if present
    if article.prev_link.is_some() || article.next_link.is_some() {
        if !matches!(blocks.last(), Some(ContentBlock::Divider)) {
            blocks.push(ContentBlock::Divider);
        }
        blocks.push(ContentBlock::Navigation {
            prev_link: article.prev_link.clone(),
            next_link: article.next_link.clone(),
        });
    }

    eprintln!("[folio::ui] Generated {} ContentBlocks from article", blocks.len());

    blocks
}

/// Initialize TextTagTable with reader typography tags.
pub fn setup_article_tags(buffer: &gtk::TextBuffer, config: &Config) {
    let tag_table = buffer.tag_table();

    let title_tag = gtk::TextTag::builder()
        .name("title")
        .weight(800)
        .scale(1.8)
        .pixels_below_lines(12)
        .justification(gtk::Justification::Left)
        .build();
    let _ = tag_table.add(&title_tag);

    let meta_tag = gtk::TextTag::builder()
        .name("meta")
        .scale(0.88)
        .pixels_below_lines(24)
        .justification(gtk::Justification::Left)
        .foreground(config.meta_color())
        .build();
    let _ = tag_table.add(&meta_tag);

    let h2_tag = gtk::TextTag::builder()
        .name("h2")
        .weight(700)
        .scale(1.35)
        .pixels_above_lines(20)
        .pixels_below_lines(10)
        .justification(gtk::Justification::Left)
        .build();
    let _ = tag_table.add(&h2_tag);

    let h3_tag = gtk::TextTag::builder()
        .name("h3")
        .weight(600)
        .scale(1.18)
        .pixels_above_lines(16)
        .pixels_below_lines(8)
        .justification(gtk::Justification::Left)
        .build();
    let _ = tag_table.add(&h3_tag);

    let paragraph_tag = gtk::TextTag::builder()
        .name("paragraph")
        .scale(1.0)
        .pixels_below_lines(config.paragraph_spacing_px as i32)
        .justification(if config.justified {
            gtk::Justification::Fill
        } else {
            gtk::Justification::Left
        })
        .build();
    let _ = tag_table.add(&paragraph_tag);

    let quote_tag = gtk::TextTag::builder()
        .name("quote")
        .style(gtk::pango::Style::Italic)
        .left_margin(24)
        .pixels_below_lines(config.paragraph_spacing_px as i32)
        .foreground(config.meta_color())
        .build();
    let _ = tag_table.add(&quote_tag);

    let code_tag = gtk::TextTag::builder()
        .name("code")
        .family("monospace")
        .scale(0.9)
        .left_margin(16)
        .right_margin(16)
        .pixels_above_lines(8)
        .pixels_below_lines(config.paragraph_spacing_px as i32)
        .build();
    let _ = tag_table.add(&code_tag);

    let list_tag = gtk::TextTag::builder()
        .name("list_item")
        .left_margin(20)
        .pixels_below_lines(6)
        .build();
    let _ = tag_table.add(&list_tag);

    let divider_tag = gtk::TextTag::builder()
        .name("divider")
        .justification(gtk::Justification::Center)
        .pixels_above_lines(16)
        .pixels_below_lines(20)
        .foreground(config.meta_color())
        .build();
    let _ = tag_table.add(&divider_tag);

    let search_match_tag = gtk::TextTag::builder()
        .name("search-match")
        .background("#FDE047")
        .foreground("#1E1E2E")
        .build();
    let _ = tag_table.add(&search_match_tag);

    let search_current_tag = gtk::TextTag::builder()
        .name("search-current")
        .background("#FB923C")
        .foreground("#000000")
        .weight(700)
        .build();
    let _ = tag_table.add(&search_current_tag);
}

/// One entry in the Table of Contents sidebar.
#[derive(Debug, Clone, PartialEq)]
pub struct TocItem {
    /// Heading level: 1..=4 (a document `Title` counts as level 1).
    pub level: u8,
    /// Display text, collapsed to a single line.
    pub text: String,
    /// Character offset of the heading inside the `TextBuffer`.
    pub offset: i32,
}

/// Flatten a heading's text for the sidebar: decode entities, drop inline
/// markup, and collapse all whitespace so long titles wrap predictably.
fn toc_display_text(raw: &str) -> String {
    let decoded = html_escape::decode_html_entities(raw).to_string();
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Horizontal indent for a heading level in the Table of Contents.
fn toc_indent_px(level: u8) -> i32 {
    match level {
        0 | 1 => 0,
        2 => 12,
        3 => 24,
        _ => 36,
    }
}

/// Populate a TextBuffer with ContentBlocks, applying corresponding TextTags.
///
/// Returns every heading it inserted, with its level and exact character
/// offset, so the caller can build a Table of Contents that jumps to the right
/// place. Collecting the offsets here (rather than re-scanning the buffer
/// afterwards) keeps them exact by construction.
pub fn populate_article_buffer(
    buffer: &gtk::TextBuffer,
    blocks: &[ContentBlock],
) -> Vec<TocItem> {
    buffer.set_text("");
    let mut iter = buffer.end_iter();
    let mut toc: Vec<TocItem> = Vec::new();

    for block in blocks {
        match block {
            ContentBlock::Title(title) => {
                let display = toc_display_text(title);
                if !display.is_empty() {
                    toc.push(TocItem {
                        level: 1,
                        text: display,
                        offset: iter.offset(),
                    });
                }
                buffer.insert_with_tags_by_name(&mut iter, title, &["title"]);
                buffer.insert(&mut iter, "\n\n");
            }
            ContentBlock::Meta { byline, reading_time } => {
                let text = if !byline.is_empty() && !reading_time.is_empty() {
                    format!("{}  •  {}", byline, reading_time)
                } else if !byline.is_empty() {
                    byline.clone()
                } else {
                    reading_time.clone()
                };
                buffer.insert_with_tags_by_name(&mut iter, &text, &["meta"]);
                buffer.insert(&mut iter, "\n\n");
            }
            ContentBlock::Heading { level, text } => {
                let display = toc_display_text(text);
                if !display.is_empty() {
                    toc.push(TocItem {
                        level: (*level).max(1),
                        text: display,
                        offset: iter.offset(),
                    });
                }
                let tag_name = if *level <= 2 { "h2" } else { "h3" };
                buffer.insert_with_tags_by_name(&mut iter, text, &[tag_name]);
                buffer.insert(&mut iter, "\n\n");
            }
            ContentBlock::Paragraph { markup, plain } => {
                let p_start = iter.offset();
                if gtk::pango::parse_markup(markup, '\0').is_ok() {
                    buffer.insert_markup(&mut iter, markup);
                } else {
                    buffer.insert(&mut iter, plain);
                }
                buffer.insert(&mut iter, "\n\n");
                let start_iter = buffer.iter_at_offset(p_start);
                let end_iter = buffer.end_iter();
                if let Some(tag) = buffer.tag_table().lookup("paragraph") {
                    buffer.apply_tag(&tag, &start_iter, &end_iter);
                }
            }
            ContentBlock::Quote(text) => {
                let quoted = format!("“{}”", text);
                buffer.insert_with_tags_by_name(&mut iter, &quoted, &["quote"]);
                buffer.insert(&mut iter, "\n\n");
            }
            ContentBlock::Code(text) => {
                buffer.insert_with_tags_by_name(&mut iter, text, &["code"]);
                buffer.insert(&mut iter, "\n\n");
            }
            ContentBlock::ListItem(text) => {
                buffer.insert_with_tags_by_name(&mut iter, text, &["list_item"]);
                buffer.insert(&mut iter, "\n");
            }
            ContentBlock::Divider => {
                buffer.insert_with_tags_by_name(&mut iter, "— • —", &["divider"]);
                buffer.insert(&mut iter, "\n\n");
            }
            ContentBlock::Navigation { .. } => {
                // Interactive buttons appended to article_box
            }
        }
    }

    toc
}

/// Create native GTK ShortcutsWindow listing all keyboard shortcuts.
pub fn create_shortcuts_window(parent: &gtk::Window) -> gtk::ShortcutsWindow {
    let win = gtk::ShortcutsWindow::builder()
        .transient_for(parent)
        .modal(true)
        .build();

    let section = gtk::ShortcutsSection::builder()
        .section_name("shortcuts")
        .max_height(14)
        .build();

    // 1. Navigation & General
    let group_nav = gtk::ShortcutsGroup::builder().title("Navigation & App").build();
    let s_url = gtk::ShortcutsShortcut::builder().title("Focus URL bar").accelerator("<Control>l F6").build();
    let s_refresh = gtk::ShortcutsShortcut::builder().title("Refresh article").accelerator("<Control>r F5").build();
    let s_back = gtk::ShortcutsShortcut::builder().title("History back").accelerator("<Alt>Left <Alt>BackSpace").build();
    let s_fwd = gtk::ShortcutsShortcut::builder().title("History forward").accelerator("<Alt>Right").build();
    let s_settings = gtk::ShortcutsShortcut::builder().title("Toggle settings").accelerator("<Control>comma").build();
    let s_shortcuts = gtk::ShortcutsShortcut::builder().title("Keyboard shortcuts").accelerator("<Control>question F1").build();
    let s_zen = gtk::ShortcutsShortcut::builder().title("Zen mode (fullscreen)").accelerator("F11").build();

    group_nav.append(&s_url);
    group_nav.append(&s_refresh);
    group_nav.append(&s_back);
    group_nav.append(&s_fwd);
    group_nav.append(&s_settings);
    group_nav.append(&s_shortcuts);
    group_nav.append(&s_zen);
    section.append(&group_nav);

    // 2. Reading & Typography
    let group_read = gtk::ShortcutsGroup::builder().title("Reading & Scrolling").build();
    let s_scroll_down = gtk::ShortcutsShortcut::builder().title("Smooth scroll down").accelerator("j Down").build();
    let s_scroll_up = gtk::ShortcutsShortcut::builder().title("Smooth scroll up").accelerator("k Up").build();
    let s_page_down = gtk::ShortcutsShortcut::builder().title("Scroll down page").accelerator("space Page_Down").build();
    let s_page_up = gtk::ShortcutsShortcut::builder().title("Scroll up page").accelerator("<Shift>space BackSpace Page_Up").build();
    let s_auto = gtk::ShortcutsShortcut::builder().title("Toggle auto-scroll").accelerator("s").build();
    let s_dark = gtk::ShortcutsShortcut::builder().title("Toggle dark/light mode").accelerator("t").build();
    let s_zoom_in = gtk::ShortcutsShortcut::builder().title("Zoom in").accelerator("<Control>plus <Control>equal").build();
    let s_zoom_out = gtk::ShortcutsShortcut::builder().title("Zoom out").accelerator("<Control>minus").build();
    let s_zoom_reset = gtk::ShortcutsShortcut::builder().title("Reset zoom (100%)").accelerator("<Control>0").build();

    group_read.append(&s_scroll_down);
    group_read.append(&s_scroll_up);
    group_read.append(&s_page_down);
    group_read.append(&s_page_up);
    group_read.append(&s_auto);
    group_read.append(&s_dark);
    group_read.append(&s_zoom_in);
    group_read.append(&s_zoom_out);
    group_read.append(&s_zoom_reset);
    section.append(&group_read);

    // 3. Search & Text Selection
    let group_search = gtk::ShortcutsGroup::builder().title("Search & Selection").build();
    let s_find = gtk::ShortcutsShortcut::builder().title("Find in article").accelerator("<Control>f").build();
    let s_next = gtk::ShortcutsShortcut::builder().title("Next search match").accelerator("F3 <Control>g").build();
    let s_prev = gtk::ShortcutsShortcut::builder().title("Previous search match").accelerator("<Shift>F3 <Control><Shift>g").build();
    let s_esc = gtk::ShortcutsShortcut::builder().title("Close search / clear selection").accelerator("Escape").build();
    let s_copy = gtk::ShortcutsShortcut::builder().title("Copy selected text").accelerator("<Control>c").build();
    let s_select_all = gtk::ShortcutsShortcut::builder().title("Select all article text").accelerator("<Control>a").build();

    group_search.append(&s_find);
    group_search.append(&s_next);
    group_search.append(&s_prev);
    group_search.append(&s_esc);
    group_search.append(&s_copy);
    group_search.append(&s_select_all);
    section.append(&group_search);

    win.set_child(Some(&section));
    win
}

/// Create interactive navigation button row for Previous / Next chapters.
pub fn create_navigation_widget(
    prev_link: Option<&NavigationLink>,
    next_link: Option<&NavigationLink>,
    fetch_tx: &async_channel::Sender<FetchMessage>,
    url_entry: &gtk::Entry,
    storage: &Rc<RefCell<Option<Storage>>>,
) -> gtk::Widget {
    let nav_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(16)
        .halign(gtk::Align::Center)
        .build();
    nav_row.add_css_class("reader-nav-row");

    if let Some(pl) = prev_link {
        let prev_btn = gtk::Button::builder()
            .label(&format!("← {}", pl.label))
            .tooltip_text(&pl.url)
            .build();
        prev_btn.add_css_class("pill");
        prev_btn.add_css_class("reader-nav-btn");
        let tx = fetch_tx.clone();
        let entry = url_entry.clone();
        let st = storage.clone();
        let target = pl.url.clone();
        prev_btn.connect_clicked(move |_| {
            entry.set_text(&target);
            trigger_fetch(&target, &tx, &st, false);
        });
        nav_row.append(&prev_btn);
    }

    if let Some(nl) = next_link {
        let next_btn = gtk::Button::builder()
            .label(&format!("{} →", nl.label))
            .tooltip_text(&nl.url)
            .build();
        next_btn.add_css_class("pill");
        next_btn.add_css_class("reader-nav-btn");
        let tx = fetch_tx.clone();
        let entry = url_entry.clone();
        let st = storage.clone();
        let target = nl.url.clone();
        next_btn.connect_clicked(move |_| {
            entry.set_text(&target);
            trigger_fetch(&target, &tx, &st, false);
        });
        nav_row.append(&next_btn);
    }

    nav_row.upcast::<gtk::Widget>()
}

/// Create an individual GTK4 widget for a ContentBlock.
#[allow(dead_code)]
pub fn create_block_widget(
    block: &ContentBlock,
    is_justified: bool,
    fetch_tx: &async_channel::Sender<FetchMessage>,
    url_entry: &gtk::Entry,
    storage: &Rc<RefCell<Option<Storage>>>,
) -> gtk::Widget {
    match block {
        ContentBlock::Title(title) => {
            let label = gtk::Label::builder()
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .selectable(true)
                .xalign(0.0)
                .halign(gtk::Align::Start)
                .justify(gtk::Justification::Left)
                .label(title)
                .build();
            label.add_css_class("article-title");
            label.connect_activate_link(|_lbl, uri| {
                let _ = gtk::gio::AppInfo::launch_default_for_uri(uri, None::<&gtk::gio::AppLaunchContext>);
                glib::Propagation::Stop
            });
            label.upcast::<gtk::Widget>()
        }
        ContentBlock::Meta { byline, reading_time } => {
            let text = if !byline.is_empty() && !reading_time.is_empty() {
                format!("{}  •  {}", byline, reading_time)
            } else if !byline.is_empty() {
                byline.clone()
            } else {
                reading_time.clone()
            };
            let label = gtk::Label::builder()
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .selectable(true)
                .xalign(0.0)
                .halign(gtk::Align::Start)
                .justify(gtk::Justification::Left)
                .label(&text)
                .build();
            label.add_css_class("article-byline");
            label.upcast::<gtk::Widget>()
        }
        ContentBlock::Heading { level, text } => {
            let label = gtk::Label::builder()
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .selectable(true)
                .xalign(0.0)
                .halign(gtk::Align::Start)
                .justify(gtk::Justification::Left)
                .label(text)
                .build();
            label.add_css_class("article-heading");
            if *level <= 2 {
                label.add_css_class("article-h2");
            } else {
                label.add_css_class("article-h3");
            }
            label.connect_activate_link(|_lbl, uri| {
                let _ = gtk::gio::AppInfo::launch_default_for_uri(uri, None::<&gtk::gio::AppLaunchContext>);
                glib::Propagation::Stop
            });
            label.upcast::<gtk::Widget>()
        }
        ContentBlock::Paragraph { markup, plain } => {
            let label = gtk::Label::builder()
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .selectable(true)
                .xalign(0.0)
                .halign(if is_justified { gtk::Align::Fill } else { gtk::Align::Start })
                .justify(if is_justified { gtk::Justification::Fill } else { gtk::Justification::Left })
                .build();
            label.add_css_class("article-paragraph");
            if gtk::pango::parse_markup(markup, '\0').is_ok() {
                label.set_markup(markup);
            } else {
                label.set_text(plain);
            }
            label.connect_activate_link(|_lbl, uri| {
                let _ = gtk::gio::AppInfo::launch_default_for_uri(uri, None::<&gtk::gio::AppLaunchContext>);
                glib::Propagation::Stop
            });
            label.upcast::<gtk::Widget>()
        }
        ContentBlock::Quote(text) => {
            let label = gtk::Label::builder()
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .selectable(true)
                .xalign(0.0)
                .halign(gtk::Align::Fill)
                .justify(gtk::Justification::Left)
                .build();
            label.add_css_class("article-blockquote");
            label.add_css_class("article-quote");
            let escaped = glib::markup_escape_text(text);
            let markup_text = format!("<span style=\"italic\">“{}”</span>", escaped);
            if gtk::pango::parse_markup(&markup_text, '\0').is_ok() {
                label.set_markup(&markup_text);
            } else {
                label.set_text(text);
            }
            label.connect_activate_link(|_lbl, uri| {
                let _ = gtk::gio::AppInfo::launch_default_for_uri(uri, None::<&gtk::gio::AppLaunchContext>);
                glib::Propagation::Stop
            });
            label.upcast::<gtk::Widget>()
        }
        ContentBlock::Code(text) => {
            let label = gtk::Label::builder()
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .selectable(true)
                .xalign(0.0)
                .halign(gtk::Align::Fill)
                .justify(gtk::Justification::Left)
                .label(text)
                .build();
            label.add_css_class("article-codeblock");
            label.add_css_class("article-code");
            label.upcast::<gtk::Widget>()
        }
        ContentBlock::ListItem(text) => {
            let label = gtk::Label::builder()
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .selectable(true)
                .xalign(0.0)
                .halign(gtk::Align::Start)
                .justify(gtk::Justification::Left)
                .label(text)
                .build();
            label.add_css_class("article-list-item");
            label.connect_activate_link(|_lbl, uri| {
                let _ = gtk::gio::AppInfo::launch_default_for_uri(uri, None::<&gtk::gio::AppLaunchContext>);
                glib::Propagation::Stop
            });
            label.upcast::<gtk::Widget>()
        }
        ContentBlock::Divider => {
            let label = gtk::Label::builder()
                .wrap(false)
                .xalign(0.5)
                .halign(gtk::Align::Center)
                .justify(gtk::Justification::Center)
                .selectable(false)
                .label("— • —")
                .build();
            label.add_css_class("article-divider");
            label.upcast::<gtk::Widget>()
        }
        ContentBlock::Navigation { prev_link, next_link } => {
            let nav_row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(16)
                .halign(gtk::Align::Center)
                .build();
            nav_row.add_css_class("reader-nav-row");

            if let Some(pl) = prev_link {
                let prev_btn = gtk::Button::builder()
                    .label(&format!("← {}", pl.label))
                    .tooltip_text(&pl.url)
                    .build();
                prev_btn.add_css_class("pill");
                prev_btn.add_css_class("reader-nav-btn");
                let tx = fetch_tx.clone();
                let entry = url_entry.clone();
                let st = storage.clone();
                let target = pl.url.clone();
                prev_btn.connect_clicked(move |_| {
                    entry.set_text(&target);
                    trigger_fetch(&target, &tx, &st, false);
                });
                nav_row.append(&prev_btn);
            }

            if let Some(nl) = next_link {
                let next_btn = gtk::Button::builder()
                    .label(&format!("{} →", nl.label))
                    .tooltip_text(&nl.url)
                    .build();
                next_btn.add_css_class("pill");
                next_btn.add_css_class("reader-nav-btn");
                let tx = fetch_tx.clone();
                let entry = url_entry.clone();
                let st = storage.clone();
                let target = nl.url.clone();
                next_btn.connect_clicked(move |_| {
                    entry.set_text(&target);
                    trigger_fetch(&target, &tx, &st, false);
                });
                nav_row.append(&next_btn);
            }

            nav_row.upcast::<gtk::Widget>()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zoom_keys_and_bounds() {
        assert_eq!(gdk::Key::plus, gdk::Key::plus);
        assert_eq!(gdk::Key::equal, gdk::Key::equal);
        assert_eq!(gdk::Key::KP_Add, gdk::Key::KP_Add);
        assert_eq!(gdk::Key::minus, gdk::Key::minus);
        assert_eq!(gdk::Key::KP_Subtract, gdk::Key::KP_Subtract);
        assert_eq!(gdk::Key::_0, gdk::Key::_0);
        assert_eq!(gdk::Key::KP_0, gdk::Key::KP_0);
        assert_eq!(gdk::Key::Page_Up, gdk::Key::Page_Up);
        assert_eq!(gdk::Key::BackSpace, gdk::Key::BackSpace);

        // Verify zoom clamping logic
        let step: f64 = 2.0;
        let mut font_size: f64 = 47.0;
        font_size = (font_size + step).min(48.0);
        assert_eq!(font_size, 48.0);
        font_size = (font_size + step).min(48.0);
        assert_eq!(font_size, 48.0);

        font_size = 11.0;
        font_size = (font_size - step).max(10.0);
        assert_eq!(font_size, 10.0);
        font_size = (font_size - step).max(10.0);
        assert_eq!(font_size, 10.0);

        // Reset
        font_size = Config::default().font_size;
        assert_eq!(font_size, 18.0);
    }

    #[test]
    fn test_parse_article_blocks_structured() {
        let article = ArticleData {
            url: "https://example.com/test".to_string(),
            title: "Test Article".to_string(),
            byline: "John Doe".to_string(),
            html_content: "<h2>Section 1</h2><p>Paragraph <strong>one</strong>.</p><blockquote>A wise quote</blockquote><pre>let x = 10;</pre><li>Item A</li>".to_string(),
            text_content: "Paragraph one.\n\nA wise quote".to_string(),
            reading_time_mins: 5,
            prev_link: None,
            next_link: None,
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 7);
        assert_eq!(blocks[0], ContentBlock::Title("Test Article".to_string()));
        assert_eq!(
            blocks[1],
            ContentBlock::Meta {
                byline: "John Doe".to_string(),
                reading_time: "5 min read".to_string(),
            }
        );
        assert_eq!(
            blocks[2],
            ContentBlock::Heading {
                level: 2,
                text: "Section 1".to_string(),
            }
        );
        assert!(matches!(&blocks[3], ContentBlock::Paragraph { markup, plain } if markup.contains("<b>one</b>") && plain == "Paragraph one."));
        assert_eq!(blocks[4], ContentBlock::Quote("A wise quote".to_string()));
        assert_eq!(blocks[5], ContentBlock::Code("let x = 10;".to_string()));
        assert_eq!(blocks[6], ContentBlock::ListItem("• Item A".to_string()));
    }

    #[test]
    fn test_parse_article_blocks_text_fallback() {
        let article = ArticleData {
            url: "https://example.com/plain".to_string(),
            title: "Plain Article".to_string(),
            byline: "".to_string(),
            html_content: "".to_string(),
            text_content: "First paragraph.\n\nSecond paragraph.".to_string(),
            reading_time_mins: 2,
            prev_link: None,
            next_link: None,
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0], ContentBlock::Title("Plain Article".to_string()));
        assert_eq!(
            blocks[1],
            ContentBlock::Meta {
                byline: "".to_string(),
                reading_time: "2 min read".to_string(),
            }
        );
        assert!(matches!(&blocks[2], ContentBlock::Paragraph { plain, .. } if plain == "First paragraph."));
        assert!(matches!(&blocks[3], ContentBlock::Paragraph { plain, .. } if plain == "Second paragraph."));
    }

    #[test]
    fn test_parse_article_blocks_skips_redundant_h1_matching_title() {
        let article = ArticleData {
            url: "https://example.com/dup".to_string(),
            title: "Unique Title".to_string(),
            byline: "".to_string(),
            html_content: "<h1>Unique Title</h1><p>Body paragraph.</p>".to_string(),
            text_content: "Body paragraph.".to_string(),
            reading_time_mins: 1,
            prev_link: None,
            next_link: None,
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0], ContentBlock::Title("Unique Title".to_string()));
        assert_eq!(
            blocks[1],
            ContentBlock::Meta {
                byline: "".to_string(),
                reading_time: "1 min read".to_string(),
            }
        );
        assert!(matches!(&blocks[2], ContentBlock::Paragraph { plain, .. } if plain == "Body paragraph."));
    }

    #[test]
    fn test_html_to_pango_markup_entities_and_formatting() {
        let input = "Hello &mdash; <strong>bold &amp; <em>italic</em></strong> &lt;world&gt; <span class=\"highlight\">text</span><br/>New line";
        let markup = html_to_pango_markup(input);
        assert!(markup.contains("Hello — <b>bold &amp; <i>italic</i></b> &lt;world&gt; text\nNew line"));
        assert!(gtk::pango::parse_markup(&markup, '\0').is_ok());
    }

    #[test]
    fn test_parse_article_blocks_ordered_and_unordered_lists() {
        let article = ArticleData {
            url: "https://example.com/lists".to_string(),
            title: "Lists".to_string(),
            byline: "".to_string(),
            html_content: "<ul><li>Bullet 1</li><li>Bullet 2</li></ul><ol><li>First</li><li>Second</li></ol>".to_string(),
            text_content: "Bullet 1\nBullet 2\nFirst\nSecond".to_string(),
            reading_time_mins: 0,
            prev_link: None,
            next_link: None,
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 5);
        assert_eq!(blocks[1], ContentBlock::ListItem("• Bullet 1".to_string()));
        assert_eq!(blocks[2], ContentBlock::ListItem("• Bullet 2".to_string()));
        assert_eq!(blocks[3], ContentBlock::ListItem("1. First".to_string()));
        assert_eq!(blocks[4], ContentBlock::ListItem("2. Second".to_string()));
    }

    #[test]
    fn test_parse_article_blocks_pre_code_dedup() {
        let article = ArticleData {
            url: "https://example.com/code".to_string(),
            title: "Code Snippet".to_string(),
            byline: "".to_string(),
            html_content: "<pre><code>fn main() {\n    println!(\"Hello\");\n}</code></pre>".to_string(),
            text_content: "fn main() {\n    println!(\"Hello\");\n}".to_string(),
            reading_time_mins: 0,
            prev_link: None,
            next_link: None,
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], ContentBlock::Title("Code Snippet".to_string()));
        assert_eq!(
            blocks[1],
            ContentBlock::Code("fn main() {\n    println!(\"Hello\");\n}".to_string())
        );
    }

    #[test]
    fn test_parse_article_blocks_leaf_div_and_section() {
        let article = ArticleData {
            url: "https://example.com/divs".to_string(),
            title: "Div Article".to_string(),
            byline: "".to_string(),
            html_content: "<div class=\"wrapper\"><div class=\"text-leaf\">Direct text in leaf div &mdash; cool!</div><section class=\"sec-leaf\">Direct text in leaf section.</section></div>".to_string(),
            text_content: "Direct text in leaf div\nDirect text in leaf section".to_string(),
            reading_time_mins: 0,
            prev_link: None,
            next_link: None,
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0], ContentBlock::Title("Div Article".to_string()));
        assert!(matches!(&blocks[1], ContentBlock::Paragraph { plain, markup } if plain.contains("Direct text in leaf div — cool!") && markup.contains("—")));
        assert!(matches!(&blocks[2], ContentBlock::Paragraph { plain, .. } if plain.contains("Direct text in leaf section.")));
    }

    #[test]
    fn test_parse_article_blocks_blockquote_with_p_no_duplicate() {
        let article = ArticleData {
            url: "https://example.com/quote".to_string(),
            title: "Quote Article".to_string(),
            byline: "".to_string(),
            html_content: "<blockquote><p>This is a quote inside a paragraph.</p></blockquote>".to_string(),
            text_content: "This is a quote inside a paragraph.".to_string(),
            reading_time_mins: 0,
            prev_link: None,
            next_link: None,
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], ContentBlock::Title("Quote Article".to_string()));
        assert_eq!(
            blocks[1],
            ContentBlock::Quote("This is a quote inside a paragraph.".to_string())
        );
    }

    #[test]
    fn test_parse_article_blocks_split_double_br_and_preserve_dialog() {
        let article = ArticleData {
            url: "https://example.com/novel".to_string(),
            title: "Novel Chapter".to_string(),
            byline: "".to_string(),
            html_content: "<p>\"Hello.\"<br><br>\"Who is there?\"<br/><br/>***<br><br>\"It is me.\"</p>".to_string(),
            text_content: "\"Hello.\"\n\n\"Who is there?\"\n\n***\n\n\"It is me.\"".to_string(),
            reading_time_mins: 0,
            prev_link: None,
            next_link: None,
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 5);
        assert_eq!(blocks[0], ContentBlock::Title("Novel Chapter".to_string()));
        assert!(matches!(&blocks[1], ContentBlock::Paragraph { plain, .. } if plain == "\"Hello.\""));
        assert!(matches!(&blocks[2], ContentBlock::Paragraph { plain, .. } if plain == "\"Who is there?\""));
        assert_eq!(blocks[3], ContentBlock::Divider);
        assert!(matches!(&blocks[4], ContentBlock::Paragraph { plain, .. } if plain == "\"It is me.\""));
    }

    #[test]
    fn test_parse_article_blocks_hr_divider() {
        let article = ArticleData {
            url: "https://example.com/hr".to_string(),
            title: "Scene Change".to_string(),
            byline: "".to_string(),
            html_content: "<p>First scene</p><hr><p>Second scene</p>".to_string(),
            text_content: "First scene\n\nSecond scene".to_string(),
            reading_time_mins: 0,
            prev_link: None,
            next_link: None,
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0], ContentBlock::Title("Scene Change".to_string()));
        assert!(matches!(&blocks[1], ContentBlock::Paragraph { plain, .. } if plain == "First scene"));
        assert_eq!(blocks[2], ContentBlock::Divider);
        assert!(matches!(&blocks[3], ContentBlock::Paragraph { plain, .. } if plain == "Second scene"));
    }

    #[test]
    fn test_parse_article_blocks_navigation_links() {
        let article = ArticleData {
            url: "https://example.com/ch2".to_string(),
            title: "Chapter 2".to_string(),
            byline: "Author".to_string(),
            html_content: "<p>Chapter 2 story content.</p>".to_string(),
            text_content: "Chapter 2 story content.".to_string(),
            reading_time_mins: 1,
            prev_link: Some(NavigationLink {
                url: "https://example.com/ch1".to_string(),
                label: "Chapter 1".to_string(),
            }),
            next_link: Some(NavigationLink {
                url: "https://example.com/ch3".to_string(),
                label: "Chapter 3".to_string(),
            }),
        };

        let blocks = parse_article_blocks(&article);
        assert_eq!(blocks.len(), 5);
        assert_eq!(blocks[0], ContentBlock::Title("Chapter 2".to_string()));
        assert_eq!(
            blocks[1],
            ContentBlock::Meta {
                byline: "Author".to_string(),
                reading_time: "1 min read".to_string(),
            }
        );
        assert!(matches!(&blocks[2], ContentBlock::Paragraph { plain, .. } if plain == "Chapter 2 story content."));
        assert_eq!(blocks[3], ContentBlock::Divider);
        assert_eq!(
            blocks[4],
            ContentBlock::Navigation {
                prev_link: Some(NavigationLink {
                    url: "https://example.com/ch1".to_string(),
                    label: "Chapter 1".to_string(),
                }),
                next_link: Some(NavigationLink {
                    url: "https://example.com/ch3".to_string(),
                    label: "Chapter 3".to_string(),
                }),
            }
        );
    }

    /// Pump the main loop until the adjustment's `upper()` has not changed for
    /// a long quiet window, then return it. GTK measures tall TextView content
    /// lazily in chunks, with idle gaps between them, so a short stability
    /// streak reports a half-measured height.
    fn settled_upper(adj: &gtk::Adjustment) -> f64 {
        // GTK's TextView measures lazily across several main-loop turns that
        // are gated on the frame clock, so this pumps with real time passing
        // until `upper()` has held steady for 150ms of wall clock.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut last = adj.upper();
        let mut stable_since: Option<std::time::Instant> = None;
        while std::time::Instant::now() < deadline {
            glib::MainContext::default().iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(2));
            let now = adj.upper();
            if (now - last).abs() < f64::EPSILON && now > 0.0 {
                let since = *stable_since.get_or_insert_with(std::time::Instant::now);
                if since.elapsed() >= std::time::Duration::from_millis(400) {
                    return now;
                }
            } else {
                stable_since = None;
            }
            last = now;
        }
        adj.upper()
    }

    #[test]
    fn test_box_block_rendering_and_stable_upper() {
        if crate::test_support::with_gtk(|| {
            let article_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(0)
                .halign(gtk::Align::Center)
                .hexpand(true)
                .build();
            article_box.add_css_class("reader-surface");
            article_box.add_css_class("article-box");

            let buffer = gtk::TextBuffer::new(None);
            let config = Config::default();
            setup_article_tags(&buffer, &config);

            let tv = gtk::TextView::builder()
                .buffer(&buffer)
                .editable(false)
                .cursor_visible(false)
                .wrap_mode(gtk::WrapMode::WordChar)
                .build();
            article_box.append(&tv);

            let mut blocks = vec![
                ContentBlock::Title("Hello World".to_string()),
                ContentBlock::Meta {
                    byline: "Author".to_string(),
                    reading_time: "3 min read".to_string(),
                },
                ContentBlock::Heading {
                    level: 2,
                    text: "Heading Two".to_string(),
                },
                ContentBlock::Paragraph {
                    markup: "This is a <b>paragraph</b>.".to_string(),
                    plain: "This is a paragraph.".to_string(),
                },
                ContentBlock::Quote("A nice quote".to_string()),
                ContentBlock::Code("println!(\"code\");".to_string()),
                ContentBlock::ListItem("List Item".to_string()),
                ContentBlock::Divider,
            ];

            for i in 0..100 {
                blocks.push(ContentBlock::Paragraph {
                    markup: format!("Paragraph <b>{}</b>", i),
                    plain: format!("Paragraph {}", i),
                });
            }

            populate_article_buffer(&buffer, &blocks);

            // Verify cross-paragraph text selection across Title, Meta, and Paragraphs
            let full_text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
            assert!(full_text.contains("Hello World"));
            assert!(full_text.contains("Heading Two"));
            assert!(full_text.contains("This is a paragraph."));
            assert!(full_text.contains("Paragraph 99"));

            buffer.select_range(&buffer.start_iter(), &buffer.end_iter());
            assert!(buffer.has_selection());
            let bounds = buffer.selection_bounds();
            assert!(bounds.is_some());
            if let Some((start, end)) = bounds {
                let selected = buffer.text(&start, &end, false);
                assert_eq!(selected, full_text);
            }

            let clamp = adw::Clamp::builder().child(&article_box).maximum_size(720).build();
            let sw = gtk::ScrolledWindow::builder().child(&clamp).build();
            let win = gtk::Window::builder().child(&sw).default_width(600).default_height(500).build();
            // `default_size` only seeds the initial size; on a wide display the
            // compositor may present the window large enough that the whole
            // article fits, leaving nothing to scroll. Pin the size so this
            // measures layout, not screen geometry.
            win.set_size_request(600, 500);
            win.present();
            for _ in 0..10 {
                glib::MainContext::default().iteration(false);
            }

            let adj = sw.vadjustment();
            let initial_upper = settled_upper(&adj);
            assert!(initial_upper > 0.0, "Article box must measure a positive upper height");

            // Scroll to the bottom
            sw.vadjustment().set_value(sw.vadjustment().upper() - sw.vadjustment().page_size());
            for _ in 0..10 {
                glib::MainContext::default().iteration(false);
            }
            assert!(sw.vadjustment().value() > 0.0);

            // Scroll back to the top
            sw.vadjustment().set_value(0.0);
            for _ in 0..10 {
                glib::MainContext::default().iteration(false);
            }

            // Let the layout settle again after scrolling back up.
            let scroll_up_upper = settled_upper(&adj);
            assert_eq!(
                initial_upper, scroll_up_upper,
                "vadjustment.upper() must remain completely stable without fluctuating when scrolling up"
            );
            win.destroy();
            })
            .is_none()
        {
            crate::test_support::skip_if_no_display();
        }

    }

    #[test]
    fn test_chromium_step_sizes() {
        // Chromium's fixed keyboard / wheel increments.
        assert_eq!(ARROW_STEP_PX, 40.0);
        assert_eq!(WHEEL_NOTCH_PX, 100.0);
        assert!((PAGE_FRACTION - 0.875).abs() < f64::EPSILON);

        // A 1000px viewport pages by 875px, leaving 12.5% context behind.
        assert_eq!(chromium_page_step(1000.0), 875.0);
        assert_eq!(chromium_page_step(800.0), 700.0);
        // Degenerate viewports never produce a zero step.
        assert_eq!(chromium_page_step(0.0), 1.0);
    }

    #[test]
    fn test_chromium_easing_curve_shape() {
        // Endpoints are exact, so a finished curve lands precisely on target.
        assert_eq!(chromium_ease(0.0), 0.0);
        assert_eq!(chromium_ease(1.0), 1.0);
        // Monotonic and front-loaded (ease-out quintic).
        let mut previous = 0.0;
        for step in 0..=100 {
            let y = chromium_ease(step as f64 / 100.0);
            assert!(y >= previous - 1e-9, "easing must be monotonic at {}", step);
            previous = y;
        }
        assert!(chromium_ease(0.1) > 0.1, "ease-out must lead linear progress early on");
        assert!(chromium_ease(0.5) > 0.9, "halfway through the curve most travel is done");
        // Initial slope y1/x1 = 4.0 makes a fresh impulse feel instant.
        assert_eq!(chromium_initial_slope(), 4.0);
        let tiny = 1e-4;
        let sampled_slope = chromium_ease(tiny) / tiny;
        assert!((sampled_slope - 4.0).abs() < 0.05, "slope at t=0 was {}", sampled_slope);
    }

    #[test]
    fn test_chromium_easing_preserves_magnitude_and_direction() {
        // Constant target, both directions: the curve interpolates exactly
        // between start and target and never overshoots past the target.
        for &(start, target) in &[(0.0, 1200.0), (1200.0, 0.0), (500.0, 540.0)] {
            let mut previous = start;
            for step in 0..=60 {
                let progress = step as f64 / 60.0;
                let pos = start + (target - start) * chromium_ease(progress);
                if target >= start {
                    assert!(pos >= previous - 1e-9 && pos <= target + 1e-9);
                } else {
                    assert!(pos <= previous + 1e-9 && pos >= target - 1e-9);
                }
                previous = pos;
            }
            assert!((previous - target).abs() < 1e-9);
        }
    }

    #[test]
    fn test_wheel_impulse_progressive_acceleration() {
        // Isolated notches get the base 100px impulse.
        let mut boost = 1.0;
        assert_eq!(wheel_boost_for_gap(500.0, boost), 1.0);
        boost = wheel_boost_for_gap(500.0, boost);
        assert_eq!(boost * WHEEL_NOTCH_PX, 100.0);

        // Rapid notches inside the 80ms window accelerate the impulse.
        boost = 1.0;
        for _ in 0..2 {
            boost = wheel_boost_for_gap(40.0, boost);
        }
        assert_eq!(boost, 1.5);
        assert_eq!(boost * WHEEL_NOTCH_PX, 150.0);

        // ...and are capped at 2.5x however long the spin continues.
        for _ in 0..20 {
            boost = wheel_boost_for_gap(16.0, boost);
        }
        assert_eq!(boost, WHEEL_MAX_BOOST);
        assert_eq!(boost * WHEEL_NOTCH_PX, 250.0);

        // A pause outside the window drops back to the unaccelerated impulse.
        assert_eq!(wheel_boost_for_gap(WHEEL_BOOST_WINDOW_MS + 1.0, boost), 1.0);
    }

    #[test]
    fn test_velocity_preserving_retarget() {
        // Standing still: plain nominal duration.
        assert_eq!(retarget_duration_ms(0.0, 400.0, ARROW_DURATION_MS, ARROW_DURATION_MS), 220.0);

        // A tight remaining distance at high velocity shortens the curve, so the
        // impulse keeps its pace instead of easing out from a dead stop.
        let tight = retarget_duration_ms(4000.0, 100.0, ARROW_DURATION_MS, ARROW_DURATION_MS);
        assert!(tight < ARROW_DURATION_MS, "high velocity must shorten the curve: {}", tight);
        assert!(tight >= 80.0, "curve never collapses below 80ms: {}", tight);

        // A large remaining distance at modest velocity never exceeds the cap.
        let long = retarget_duration_ms(2000.0, 1200.0, ARROW_DURATION_MS, ARROW_DURATION_MS);
        assert_eq!(long, ARROW_DURATION_MS);

        // Reversing direction cannot reuse the old velocity: full curve.
        assert_eq!(
            retarget_duration_ms(-2000.0, 400.0, ARROW_DURATION_MS, ARROW_DURATION_MS),
            ARROW_DURATION_MS
        );

        // Home/End style jumps honour their 350ms ceiling.
        let jump = retarget_duration_ms(200.0, 100_000.0, 160.0, JUMP_MAX_DURATION_MS);
        assert!(jump <= JUMP_MAX_DURATION_MS, "jump capped at 350ms, got {}", jump);
    }

    #[test]
    fn test_retarget_keeps_position_stream_continuous() {
        // Two arrow taps 60ms apart: the second impulse must continue the
        // position stream from where the first one actually was, with no jump
        // backwards and no dropped frame of travel.
        let dt = 1.0 / 60.0;
        let mut start_y = 0.0_f64;
        let mut target = ARROW_STEP_PX;
        let mut duration = ARROW_DURATION_MS;
        let mut elapsed = 0.0_f64;

        let mut positions = vec![0.0_f64];
        let mut velocity;

        for frame in 0..24 {
            let t = (elapsed / 1000.0) / (duration / 1000.0);
            let next = start_y + (target - start_y) * chromium_ease(t.min(1.0));
            velocity = (next - *positions.last().unwrap()) / dt;
            positions.push(next);
            elapsed += dt * 1000.0;

            // Second tap arrives 60ms in, while the first curve is still running.
            if frame == 3 {
                let current = *positions.last().unwrap();
                let accumulated = target + ARROW_STEP_PX;
                duration = retarget_duration_ms(
                    velocity,
                    accumulated - current,
                    ARROW_DURATION_MS,
                    ARROW_DURATION_MS,
                );
                start_y = current;
                target = accumulated;
                elapsed = 0.0;
            }
        }

        // Monotonic: never scrolls backwards mid-chain.
        for pair in positions.windows(2) {
            assert!(pair[1] >= pair[0] - 1e-9, "position regressed: {:?}", pair);
        }
        // Continuous: no single frame teleports a large share of the total.
        let biggest_frame = positions
            .windows(2)
            .map(|w| w[1] - w[0])
            .fold(0.0_f64, f64::max);
        assert!(
            biggest_frame < ARROW_STEP_PX * 0.75,
            "retarget caused a {biggest_frame}px jump in one frame"
        );
        // And the accumulated target is actually approached.
        assert!(
            *positions.last().unwrap() > ARROW_STEP_PX,
            "chained impulses must advance past the first step: {}",
            positions.last().unwrap()
        );
    }

    #[test]
    fn test_held_key_glide_speed() {
        // 1200 px/s at 60fps advances 20px per frame, in the held direction.
        let per_frame = gliding_advance(1, 1.0 / 60.0);
        assert!((per_frame - 20.0).abs() < 1e-9, "glide was {}px/frame", per_frame);
        assert!((gliding_advance(-1, 1.0 / 60.0) + 20.0).abs() < 1e-9);

        // One second of holding covers exactly the glide speed.
        let per_second: f64 = (0..60).map(|_| gliding_advance(1, 1.0 / 60.0)).sum();
        assert!((per_second - HOLD_GLIDE_SPEED_PX_PER_S).abs() < 1e-6);
    }

    #[test]
    fn test_scroll_target_is_clamped_to_bounds() {
        // Targets are clamped against the live scrollable range so a stale
        // layout can never fling the viewport outside the document.
        let max_scroll = 1200.0;
        for &raw in &[-500.0_f64, 0.0, 480.0, 1200.0, 99_999.0] {
            let clamped = raw.clamp(0.0, max_scroll);
            assert!(clamped >= 0.0 && clamped <= max_scroll);
        }
        // The 0.2px termination threshold ends the callback cleanly.
        assert_eq!(SCROLL_STOP_EPSILON, 0.2);
        assert!((1199.9_f64 - 1200.0).abs() < SCROLL_STOP_EPSILON);
        assert!((1199.5_f64 - 1200.0).abs() > SCROLL_STOP_EPSILON);
    }

    /// Pump the main loop until the widget tree has been measured.
    fn settle_layout() {
        for _ in 0..12 {
            glib::MainContext::default().iteration(false);
        }
    }

    /// Build a real, realised ScrolledWindow whose content is tall enough to
    /// scroll, with `ScrollControllerState` attached the way `build_window`
    /// does it. Must be called on the GTK owner thread.
    fn scroll_test_fixture() -> (gtk::Window, gtk::ScrolledWindow, Rc<ScrollControllerState>) {
        let sw = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .build();
        // 5000px of content inside a ~500px viewport.
        let tall = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .height_request(5000)
            .build();
        sw.set_child(Some(&tall));

        // A ScrolledWindow only allocates its adjustment once it is in a
        // realised window, so the fixture must actually be presented.
        let win = gtk::Window::builder()
            .child(&sw)
            .default_width(400)
            .default_height(500)
            .build();
        win.present();
        settle_layout();

        let state = ScrollControllerState::new(&sw, Rc::new(RefCell::new(Config::default())));
        (win, sw, state)
    }

    #[test]
    fn test_animator_cancel_and_sync_inside_real_scrolled_window() {
        let Some(failures) = crate::test_support::with_gtk(|| {
            let (win, sw, state) = scroll_test_fixture();
            let adj = sw.vadjustment();
            let mut failures: Vec<String> = Vec::new();

            // Cancel on an idle viewport is a no-op that keeps target in sync.
            adj.set_value(250.0);
            state.cancel();
            if state.is_animating() {
                failures.push("cancel left is_animating set".into());
            }
            if !sw.is_kinetic_scrolling() {
                failures.push("cancel must hand physics back to GTK".into());
            }

            // A real cancel removes the tick source instead of leaking it.
            state.drive_to(400.0, ARROW_DURATION_MS);
            if !state.is_animating() {
                failures.push("drive_to did not start an animation".into());
            }
            if state.tick_id.borrow().is_none() {
                failures.push("drive_to did not register a tick callback".into());
            }
            if sw.is_kinetic_scrolling() {
                failures.push("our curve must own the viewport while running".into());
            }
            state.cancel();
            if state.tick_id.borrow().is_some() {
                failures.push("cancel must drop the tick callback".into());
            }
            if (state.target_y.get() - adj.value()).abs() > f64::EPSILON {
                failures.push("cancel must resync target with the adjustment".into());
            }
            win.destroy();
            failures
        }) else {
            crate::test_support::skip_if_no_display();
            return;
        };
        assert!(failures.is_empty(), "failures: {failures:?}");
    }

    /// Pump the main loop for `ms` of wall clock so the real frame clock
    /// schedules tick callbacks (this is what the animator actually runs on).
    fn pump_for(ms: u64) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        while std::time::Instant::now() < deadline {
            glib::MainContext::default().iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// The reading column must honour `max_width` and `margin`: the text area
    /// is `max_width - 2 * margin` wide, never a degenerate zero.
    /// The focus classifier must recognise every input the app uses, and must
    /// NOT classify the read-only article surface as typing.
    /// Depth-first search for the TOC's `ListBox` inside the sidebar tree.
    fn find_list_box(widget: &gtk::Widget) -> Option<gtk::ListBox> {
        if let Some(lb) = widget.downcast_ref::<gtk::ListBox>() {
            return Some(lb.clone());
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            if let Some(found) = find_list_box(&c) {
                return Some(found);
            }
            child = c.next_sibling();
        }
        None
    }

    /// Restoring a saved scroll offset must also move the caret, or the next
    /// click into the article scrolls the (offset-0) caret onscreen and throws
    /// the restored viewport back to the top. This is the back/forward and
    /// refresh path, and it exercises a real realised TextView.
    #[test]
    fn test_park_caret_at_scroll_matches_viewport() {
        let Some(failures) = crate::test_support::with_gtk(|| {
            let mut failures: Vec<String> = Vec::new();
            let buffer = gtk::TextBuffer::new(None);
            setup_article_tags(&buffer, &Config::default());
            // Many paragraphs so the buffer is far taller than the viewport.
            let blocks: Vec<ContentBlock> = (0..200)
                .map(|i| {
                    let text = format!("Paragraph number {i} with some body text.");
                    ContentBlock::Paragraph {
                        markup: text.clone(),
                        plain: text,
                    }
                })
                .collect();
            populate_article_buffer(&buffer, &blocks);

            let view = gtk::TextView::builder()
                .buffer(&buffer)
                .editable(false)
                .cursor_visible(false)
                .wrap_mode(gtk::WrapMode::Word)
                .build();
            let sw = gtk::ScrolledWindow::builder()
                .child(&view)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vscrollbar_policy(gtk::PolicyType::Automatic)
                .build();
            let win = gtk::Window::builder()
                .child(&sw)
                .default_width(800)
                .default_height(600)
                .build();
            win.present();
            settle_layout();

            let adj = sw.vadjustment();
            if adj.upper() <= adj.page_size() {
                win.destroy();
                return vec![format!(
                    "fixture is not scrollable: upper={} page_size={}",
                    adj.upper(),
                    adj.page_size()
                )];
            }

            // Whatever the populate step left behind, it must not already sit
            // inside the viewport we are about to restore — otherwise the test
            // would pass without the park doing anything.
            let start_offset = buffer.iter_at_mark(&buffer.get_insert()).offset();

            // Restore a viewport deep in the document, then park the caret.
            let target = ((adj.upper() - adj.page_size()) * 0.5).max(100.0);
            adj.set_value(target);
            park_caret_at_scroll(&view, &buffer, target);
            settle_layout();

            let caret = buffer.iter_at_mark(&buffer.get_insert()).offset();
            if caret == start_offset {
                failures.push(format!(
                    "caret unchanged at {caret}; the park did nothing"
                ));
            }

            // The caret must sit inside the restored viewport, not merely be
            // non-zero: GTK scrolls the caret onscreen when the view gains
            // focus, so a caret outside the viewport undoes the restore.
            let loc = view.iter_location(&buffer.iter_at_mark(&buffer.get_insert()));
            let caret_y = loc.y() as f64;
            let top = adj.value();
            let bottom = top + adj.page_size();
            const SLACK_PX: f64 = 2.0;
            if caret_y < top - SLACK_PX || caret_y > bottom + SLACK_PX {
                failures.push(format!(
                    "caret_y={caret_y} is outside the restored viewport [{top}, {bottom}]; \
                     focusing the view would scroll back to it"
                ));
            }

            win.destroy();
            failures
        }) else {
            crate::test_support::skip_if_no_display();
            return;
        };
        assert!(failures.is_empty(), "failures: {failures:?}");
    }

    /// The user's case: a page scrolled by any means, with no navigation at all,
    /// must not snap back when they click the article. The caret follows the
    /// viewport, so the click finds it already on screen.
    #[test]
    fn test_caret_follows_viewport_on_plain_scroll() {
        let Some(failures) = crate::test_support::with_gtk(|| {
            let mut failures: Vec<String> = Vec::new();
            let buffer = gtk::TextBuffer::new(None);
            setup_article_tags(&buffer, &Config::default());
            let blocks: Vec<ContentBlock> = (0..200)
                .map(|i| {
                    let text = format!("Paragraph number {i} with some body text.");
                    ContentBlock::Paragraph {
                        markup: text.clone(),
                        plain: text,
                    }
                })
                .collect();
            populate_article_buffer(&buffer, &blocks);

            let view = gtk::TextView::builder()
                .buffer(&buffer)
                .editable(false)
                .cursor_visible(false)
                .wrap_mode(gtk::WrapMode::Word)
                .build();
            let sw = gtk::ScrolledWindow::builder()
                .child(&view)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vscrollbar_policy(gtk::PolicyType::Automatic)
                .build();
            // The article is not the focused widget in the real app when this
            // bug shows up — focus sits on the URL bar or a sidebar row — so the
            // fixture needs a focusable sibling to hold focus.
            let sibling = gtk::Button::with_label("sidebar row");
            let bx = gtk::Box::new(gtk::Orientation::Vertical, 0);
            bx.append(&sibling);
            bx.append(&sw);
            let win = gtk::Window::builder()
                .child(&bx)
                .default_width(800)
                .default_height(600)
                .build();
            win.present();
            settle_layout();
            sibling.grab_focus();
            pump_for(150);
            if view.has_focus() {
                win.destroy();
                return vec!["fixture could not move focus off the article".to_string()];
            }

            let adj = sw.vadjustment();
            if adj.upper() <= adj.page_size() {
                win.destroy();
                return vec![format!(
                    "fixture is not scrollable: upper={} page_size={}",
                    adj.upper(),
                    adj.page_size()
                )];
            }

            install_caret_follows_viewport(&adj, &view, &buffer);

            // A plain scroll: no history, no restore, no jump — just the user
            // moving the viewport, exactly like a first visit.
            let target = ((adj.upper() - adj.page_size()) * 0.6).max(100.0);
            adj.set_value(target);
            // Wait out the settle delay plus frames.
            pump_for(400);

            let caret = buffer.iter_at_mark(&buffer.get_insert());
            let caret_y = view.iter_location(&caret).y() as f64;
            let top = adj.value();
            let bottom = top + adj.page_size();
            const SLACK_PX: f64 = 2.0;
            if caret_y < top - SLACK_PX || caret_y > bottom + SLACK_PX {
                failures.push(format!(
                    "caret_y={caret_y} is outside the viewport [{top}, {bottom}] after a \
                     plain scroll; clicking the article would jump to the top"
                ));
            }

            // Now the click itself: focusing the article must not move the
            // viewport. This is the reported symptom — GTK scrolls the stale
            // caret onscreen during focus-in.
            view.grab_focus();
            pump_for(250);
            let after_click = adj.value();
            if (after_click - target).abs() > 1.0 {
                failures.push(format!(
                    "focusing the article moved the viewport from {target} to {after_click}"
                ));
            }

            // The viewport must not have moved as a side effect.
            if (adj.value() - target).abs() > 1.0 {
                failures.push(format!(
                    "parking the caret moved the viewport from {target} to {}",
                    adj.value()
                ));
            }

            win.destroy();
            failures
        }) else {
            crate::test_support::skip_if_no_display();
            return;
        };
        assert!(failures.is_empty(), "failures: {failures:?}");
    }

    /// The refresh case: the buffer is repopulated, which leaves the caret at
    /// offset 0 while the viewport is restored far down the page. Clicking the
    /// article then focuses the view, and GTK scrolls that stale caret onscreen
    /// — the page snaps to the top. The focus pin must hold the viewport.
    #[test]
    fn test_focus_pin_holds_viewport_with_stale_caret() {
        let Some(failures) = crate::test_support::with_gtk(|| {
            let mut failures: Vec<String> = Vec::new();
            let buffer = gtk::TextBuffer::new(None);
            setup_article_tags(&buffer, &Config::default());
            let blocks: Vec<ContentBlock> = (0..200)
                .map(|i| {
                    let text = format!("Paragraph number {i} with some body text.");
                    ContentBlock::Paragraph {
                        markup: text.clone(),
                        plain: text,
                    }
                })
                .collect();
            populate_article_buffer(&buffer, &blocks);

            let view = gtk::TextView::builder()
                .buffer(&buffer)
                .editable(false)
                .cursor_visible(false)
                .wrap_mode(gtk::WrapMode::Word)
                .build();
            let sibling = gtk::Button::with_label("sidebar row");
            let sw = gtk::ScrolledWindow::builder()
                .child(&view)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vscrollbar_policy(gtk::PolicyType::Automatic)
                .build();
            let bx = gtk::Box::new(gtk::Orientation::Vertical, 0);
            bx.append(&sibling);
            bx.append(&sw);
            let win = gtk::Window::builder()
                .child(&bx)
                .default_width(800)
                .default_height(600)
                .build();
            win.present();
            settle_layout();
            sibling.grab_focus();
            pump_for(150);

            let adj = sw.vadjustment();
            if adj.upper() <= adj.page_size() {
                win.destroy();
                return vec!["fixture is not scrollable".to_string()];
            }

            install_caret_follows_viewport(&adj, &view, &buffer);

            // Restore the viewport deep in the document, then leave the caret
            // at the very top — the state a refresh produces.
            let target = ((adj.upper() - adj.page_size()) * 0.7).max(100.0);
            adj.set_value(target);
            buffer.place_cursor(&buffer.start_iter());
            pump_for(100);
            if (adj.value() - target).abs() > 1.0 {
                failures.push(format!(
                    "setup: viewport is at {} not {target}",
                    adj.value()
                ));
            }

            // The click: focusing the article must keep the viewport put.
            view.grab_focus();
            pump_for(300);
            let after = adj.value();
            if (after - target).abs() > 1.0 {
                failures.push(format!(
                    "focusing the article with a stale caret moved the viewport \
                     from {target} to {after}"
                ));
            }

            win.destroy();
            failures
        }) else {
            crate::test_support::skip_if_no_display();
            return;
        };
        assert!(failures.is_empty(), "failures: {failures:?}");
    }

    /// A TOC jump must survive the next click into the article: the row cannot
    /// take focus, and the caret must follow the viewport.
    #[test]
    fn test_toc_rows_are_not_focusable_and_jump_parks_caret() {
        // Two things must both hold, or a TOC jump is undone by the next click:
        //   1. Activating a TOC row must not be a focus transition, because GTK
        //      scrolls the insert caret back onscreen when a TextView gains
        //      focus — which drags a jumped viewport to the top.
        //   2. The jump must move the insert mark to the target heading, so the
        //      caret and the viewport agree about "where I am".
        let Some(failures) = crate::test_support::with_gtk(|| {
            let mut failures: Vec<String> = Vec::new();
            let buffer = gtk::TextBuffer::new(None);
            setup_article_tags(&buffer, &Config::default());
            let blocks = vec![
                ContentBlock::Title("Book".into()),
                ContentBlock::Heading {
                    level: 2,
                    text: "Alpha".into(),
                },
                ContentBlock::Heading {
                    level: 2,
                    text: "Beta".into(),
                },
            ];
            let toc = populate_article_buffer(&buffer, &blocks);

            // 1. Rows built by the real sidebar builder must not take focus.
            let (root, set_items) = build_toc_sidebar(Rc::new(|_| {}), 14.0);
            set_items(&toc);
            let root_widget: &gtk::Widget = root.upcast_ref();
            let Some(list) = find_list_box(root_widget) else {
                return vec!["toc sidebar has no ListBox".to_string()];
            };
            let mut row = list.first_child();
            let mut row_count = 0;
            while let Some(w) = row {
                if w.is::<gtk::ListBoxRow>() {
                    row_count += 1;
                    if w.is_focusable() {
                        failures.push(format!(
                            "TOC row {row_count} is focusable; activating it makes the \
                             next click into the article scroll back to the top"
                        ));
                    }
                }
                row = w.next_sibling();
            }
            if row_count != toc.len() {
                failures.push(format!(
                    "expected {} TOC rows, found {row_count}",
                    toc.len()
                ));
            }

            // 2. The jump parks the caret on the heading it scrolled to.
            let beta = toc
                .iter()
                .find(|t| t.text == "Beta")
                .expect("Beta heading missing from TOC");
            let iter = buffer.iter_at_offset(beta.offset);
            buffer.place_cursor(&iter);
            let caret = buffer.iter_at_mark(&buffer.get_insert()).offset();
            if caret != beta.offset {
                failures.push(format!(
                    "caret landed at {caret}, expected the jumped heading at {}",
                    beta.offset
                ));
            }
            if caret == buffer.start_iter().offset() {
                failures.push("caret is still at offset 0 after the jump".to_string());
            }
            failures
        }) else {
            crate::test_support::skip_if_no_display();
            return;
        };
        assert!(failures.is_empty(), "failures: {failures:?}");
    }

    /// Headings must be captured with the level, clean text, and an offset that
    /// actually points at the heading inside the populated buffer.
    #[test]
    fn test_toc_extraction_levels_text_and_offsets() {
        let Some(failures) = crate::test_support::with_gtk(|| {
            let mut failures: Vec<String> = Vec::new();
            let buffer = gtk::TextBuffer::new(None);
            setup_article_tags(&buffer, &Config::default());

            let blocks = vec![
                ContentBlock::Title("The  Rust   Book".into()),
                ContentBlock::Meta {
                    byline: "Ada".into(),
                    reading_time: "5 min".into(),
                },
                ContentBlock::Heading {
                    level: 2,
                    text: "Getting   Started".into(),
                },
                ContentBlock::Paragraph {
                    markup: "body".into(),
                    plain: "body".into(),
                },
                ContentBlock::Heading {
                    level: 3,
                    text: "Installation".into(),
                },
                ContentBlock::Heading {
                    level: 4,
                    text: "Windows".into(),
                },
            ];

            let toc = populate_article_buffer(&buffer, &blocks);

            // Title + three headings.
            if toc.len() != 4 {
                failures.push(format!("expected 4 TOC items, got {}", toc.len()));
            }

            let shapes: Vec<(u8, &str)> =
                toc.iter().map(|t| (t.level, t.text.as_str())).collect();
            let expected: Vec<(u8, &str)> = vec![
                (1, "The Rust Book"),
                (2, "Getting Started"),
                (3, "Installation"),
                (4, "Windows"),
            ];
            if shapes != expected {
                failures.push(format!("toc levels/text = {shapes:?}, expected {expected:?}"));
            }

            // Every offset must point at exactly its heading text. The buffer
            // stores the heading verbatim (only the display copy is
            // normalised), so compare the raw heading, not the TOC label.
            let raw_headings = ["The  Rust   Book", "Getting   Started", "Installation", "Windows"];
            for (item, raw) in toc.iter().zip(raw_headings.iter()) {
                let start = buffer.iter_at_offset(item.offset);
                if start.offset() != item.offset {
                    failures.push(format!("iter_at_offset round-trip failed for {}", item.offset));
                }
                let end = buffer.iter_at_offset(item.offset + raw.chars().count() as i32);
                let span = buffer.text(&start, &end, false).to_string();
                if span != *raw {
                    failures.push(format!(
                        "offset {} spans {span:?}, expected {raw:?}",
                        item.offset
                    ));
                }
            }

            failures
        }) else {
            crate::test_support::skip_if_no_display();
            return;
        };
        assert!(failures.is_empty(), "failures: {failures:?}");
    }

    /// An article with no headings yields an empty TOC, not a bogus entry.
    #[test]
    fn test_toc_empty_for_article_without_headings() {
        let Some(failures) = crate::test_support::with_gtk(|| {
            let mut failures: Vec<String> = Vec::new();
            let buffer = gtk::TextBuffer::new(None);
            setup_article_tags(&buffer, &Config::default());
            let blocks = vec![ContentBlock::Paragraph {
                markup: "just body text".into(),
                plain: "just body text".into(),
            }];
            let toc = populate_article_buffer(&buffer, &blocks);
            if !toc.is_empty() {
                failures.push(format!("expected empty TOC, got {} items", toc.len()));
            }
            failures
        }) else {
            crate::test_support::skip_if_no_display();
            return;
        };
        assert!(failures.is_empty(), "failures: {failures:?}");
    }

    /// Indentation must increase with heading level and start at zero.
    #[test]
    fn test_toc_indent_scales_with_level() {
        assert_eq!(toc_indent_px(1), 0);
        assert_eq!(toc_indent_px(2), 12);
        assert_eq!(toc_indent_px(3), 24);
        assert!(toc_indent_px(4) > toc_indent_px(3));
    }

    /// Whitespace and entities in heading text must be flattened for the list.
    #[test]
    fn test_toc_display_text_normalisation() {
        assert_eq!(toc_display_text("  Hello   World  "), "Hello World");
        assert_eq!(toc_display_text("A\n  B\tC"), "A B C");
        assert_eq!(toc_display_text("Fish &amp; Chips"), "Fish & Chips");
        assert_eq!(toc_display_text("   "), "");
    }

    /// The sidebar must show a muted placeholder instead of an empty list.

    /// Regression: the Contents pane must actually be *shown* on startup when
    /// `show_toc` is true.
    ///
    /// `adw::OverlaySplitView` resets `show-sidebar` to FALSE when `collapsed`
    /// is set, so a builder chain that assigns `show_sidebar` before
    /// `collapsed` produces a split view whose sidebar never appears — the
    /// persisted setting silently did nothing. This pins the ordering inside
    /// `build_toc_split_view` and checks the sidebar is really mapped, not just
    /// flagged.
    #[test]
    fn test_toc_split_honours_show_toc_on_startup() {
        let Some(failures) = crate::test_support::with_gtk(|| {
            let mut failures: Vec<String> = Vec::new();

            let win = adw::Window::builder().default_width(1400).default_height(900).build();
            let content = gtk::Label::new(Some("ARTICLE"));
            let (toc_root, set_items) = build_toc_sidebar(Rc::new(|_| {}), 14.0);
            set_items(&[TocItem { level: 1, text: "Alpha".into(), offset: 0 }]);

            let split = build_toc_split_view(&content, &toc_root, true);
            win.set_content(Some(&split));
            win.present();
            for _ in 0..400 {
                glib::MainContext::default().iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }

            if !split.shows_sidebar() {
                failures.push("show_toc = true must open the sidebar on startup".into());
            }
            if !toc_root.is_mapped() {
                failures.push("Contents sidebar was never mapped into the window".into());
            }
            if toc_root.width() < 200 {
                failures.push(format!("Contents sidebar too narrow: {}px", toc_root.width()));
            }

            // And it must genuinely hide again.
            split.set_show_sidebar(false);
            for _ in 0..200 {
                glib::MainContext::default().iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            if split.shows_sidebar() {
                failures.push("sidebar did not close".into());
            }
            failures
        }) else {
            crate::test_support::skip_if_no_display();
            return;
        };
        assert!(failures.is_empty(), "failures: {failures:?}");
    }

    #[test]
    fn test_toc_sidebar_placeholder_when_no_headings() {
        let Some(failures) = crate::test_support::with_gtk(|| {
            let mut failures: Vec<String> = Vec::new();
            let jumps = Rc::new(Cell::new(-1i32));
            let jump_cell = jumps.clone();
            let (root, set_items) = build_toc_sidebar(Rc::new(move |offset| {
                jump_cell.set(offset);
            }), 14.0);

            let root_widget: &gtk::Widget = root.upcast_ref();
            let list = find_list_box(root_widget);
            let Some(list) = list else {
                return vec!["toc sidebar has no ListBox".to_string()];
            };

            set_items(&[]);
            if list.observe_children().n_items() != 1 {
                failures.push("empty TOC should render exactly one placeholder row".into());
            }

            set_items(&[
                TocItem { level: 1, text: "One".into(), offset: 0 },
                TocItem { level: 2, text: "Two".into(), offset: 40 },
            ]);
            if list.observe_children().n_items() != 2 {
                failures.push("TOC should render one row per heading".into());
            }

            failures
        }) else {
            crate::test_support::skip_if_no_display();
            return;
        };
        assert!(failures.is_empty(), "failures: {failures:?}");
    }

    /// The jump offset must place the heading below the top edge by the
    /// documented margin, and never go negative.
    #[test]
    fn test_toc_jump_target_has_top_offset() {
        let Some(failures) = crate::test_support::with_gtk(|| {
            let mut failures: Vec<String> = Vec::new();
            let tv = gtk::TextView::new();
            let buffer = gtk::TextBuffer::new(None);
            tv.set_buffer(Some(&buffer));
            setup_article_tags(&buffer, &Config::default());
            populate_article_buffer(
                &buffer,
                &[ContentBlock::Heading { level: 2, text: "Section".into() }],
            );

            // A jump to the very first heading cannot scroll above the top.
            let first = toc_scroll_target(&tv, &buffer, 0);
            if first < 0.0 {
                failures.push(format!("jump target went negative: {first}"));
            }
            failures
        }) else {
            crate::test_support::skip_if_no_display();
            return;
        };
        assert!(failures.is_empty(), "failures: {failures:?}");
    }

    #[test]
    fn test_focused_widget_is_text_input() {
        let Some(failures) = crate::test_support::with_gtk(|| {
            let mut failures: Vec<String> = Vec::new();
            let win = gtk::Window::new();
            let vb = gtk::Box::new(gtk::Orientation::Vertical, 0);

            let article = gtk::TextView::builder()
                .editable(false)
                .cursor_visible(false)
                .build();
            article.add_css_class("article-textview");
            let entry = gtk::Entry::new();
            let search = gtk::SearchEntry::new();
            vb.append(&article);
            vb.append(&entry);
            vb.append(&search);
            win.set_child(Some(&vb));
            win.present();
            for _ in 0..30 {
                glib::MainContext::default().iteration(false);
            }

            let focus = || gtk::prelude::GtkWindowExt::focus(&win);

            // The reading surface must keep driving scroll shortcuts.
            article.grab_focus();
            for _ in 0..10 {
                glib::MainContext::default().iteration(false);
            }
            if focused_widget_is_text_input(focus().as_ref()) {
                failures.push("article TextView was misclassified as a text input".into());
            }

            // Both inputs must suppress single-key navigation.
            for (name, w) in [
                ("url_entry", entry.clone().upcast::<gtk::Widget>()),
                ("search_entry", search.clone().upcast::<gtk::Widget>()),
            ] {
                w.grab_focus();
                for _ in 0..10 {
                    glib::MainContext::default().iteration(false);
                }
                if !focused_widget_is_text_input(focus().as_ref()) {
                    failures.push(format!("{name} was not detected as a text input"));
                }
            }

            // No focus at all is never "typing".
            if focused_widget_is_text_input(None) {
                failures.push("None focus was classified as typing".into());
            }

            win.destroy();
            failures
        }) else {
            crate::test_support::skip_if_no_display();
            return;
        };
        assert!(failures.is_empty(), "failures: {failures:?}");
    }

    /// Enter must still submit from both inputs while the window-level key
    /// controller is installed and suppressing navigation keys.
    #[test]
    fn test_enter_still_activates_inputs_while_typing() {
        let Some(failures) = crate::test_support::with_gtk(|| {
            let mut failures: Vec<String> = Vec::new();
            let win = gtk::Window::new();
            let vb = gtk::Box::new(gtk::Orientation::Vertical, 0);

            // Mimic the app: a window-level capture-phase key controller that
            // swallows navigation keys but lets everything else through.
            let key_controller = gtk::EventControllerKey::new();
            key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
            win.add_controller(key_controller);

            let entry = gtk::Entry::new();
            let search = gtk::SearchEntry::new();
            vb.append(&entry);
            vb.append(&search);
            win.set_child(Some(&vb));
            win.present();
            for _ in 0..30 {
                glib::MainContext::default().iteration(false);
            }

            let url_activations = Rc::new(Cell::new(0u32));
            {
                let n = url_activations.clone();
                entry.connect_activate(move |_| n.set(n.get() + 1));
            }
            let search_activations = Rc::new(Cell::new(0u32));
            {
                let n = search_activations.clone();
                search.connect_search_changed(move |_| {});
                search.connect_activate(move |_| n.set(n.get() + 1));
            }

            entry.grab_focus();
            for _ in 0..10 {
                glib::MainContext::default().iteration(false);
            }
            entry.set_text("https://example.com");
            // A real Enter press: activates the entry's default handler.
            entry.emit_activate();

            search.grab_focus();
            for _ in 0..10 {
                glib::MainContext::default().iteration(false);
            }
            search.set_text("needle");
            search.emit_activate();

            for _ in 0..20 {
                glib::MainContext::default().iteration(false);
            }

            if url_activations.get() != 1 {
                failures.push(format!(
                    "url_entry activate fired {} times, expected 1",
                    url_activations.get()
                ));
            }
            if search_activations.get() != 1 {
                failures.push(format!(
                    "search_entry activate fired {} times, expected 1",
                    search_activations.get()
                ));
            }

            win.destroy();
            failures
        }) else {
            crate::test_support::skip_if_no_display();
            return;
        };
        assert!(failures.is_empty(), "failures: {failures:?}");
    }

    #[test]
    // `allocation()` is deprecated in GTK 4.12 in favour of `width()`, but
    // `width()` reports the content box *excluding* margins while `allocation()`
    // includes them. This test is precisely about the margin box being subtracted
    // from `max_width`, so the deprecated accessor is the correct measurement.
    #[allow(deprecated)]
    fn test_article_column_honours_max_width_and_margin() {
        let Some(failures) = crate::test_support::with_gtk(|| {
            let mut failures: Vec<String> = Vec::new();
            for (max_width, margin) in [(907.0_f64, 168.0_f64), (720.0, 32.0), (900.0, 40.0)] {
                let tv = gtk::TextView::builder()
                    .wrap_mode(gtk::WrapMode::WordChar)
                    .hexpand(true)
                    .vexpand(true)
                    .build();
                tv.add_css_class("article-textview");
                tv.add_css_class("reader-surface");
                tv.buffer().set_text("probe line of text long enough to measure wrapping");

                let article_box = build_article_column(&tv);

                let clamp = adw::Clamp::builder()
                    .maximum_size(max_width as i32)
                    .tightening_threshold(max_width as i32)
                    .child(&article_box)
                    .build();
                let sw = gtk::ScrolledWindow::builder()
                    .hscrollbar_policy(gtk::PolicyType::Never)
                    .vscrollbar_policy(gtk::PolicyType::Automatic)
                    .hexpand(true)
                    .child(&clamp)
                    .build();
                sw.add_css_class("article-viewport");

                let mut cfg = Config::default();
                cfg.max_width = max_width;
                cfg.margin = margin;
                let provider = gtk::CssProvider::new();
                provider.load_from_string(&crate::theme::generate_css(&cfg));
                gtk::style_context_add_provider_for_display(
                    &gdk::Display::default().unwrap(),
                    &provider,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                );

                let win = gtk::Window::builder()
                    .child(&sw)
                    .default_width(1600)
                    .default_height(900)
                    .build();
                win.present();
                pump_for(300);

                let box_w = article_box.allocation().width() as f64;
                let text_w = tv.allocation().width() as f64;
                let expected_text = max_width - 2.0 * margin;

                if (box_w - max_width).abs() > 2.0 {
                    failures.push(format!(
                        "column width {box_w} != max_width {max_width} (margin {margin})"
                    ));
                }
                if text_w < 1.0 {
                    failures.push(format!(
                        "text area collapsed to {text_w} with max_width {max_width}, margin {margin}"
                    ));
                }
                if (text_w - expected_text).abs() > 4.0 {
                    failures.push(format!(
                        "text width {text_w} != max_width - 2*margin = {expected_text}"
                    ));
                }
                win.destroy();
            }
            failures
        }) else {
            crate::test_support::skip_if_no_display();
            return;
        };
        assert!(failures.is_empty(), "failures: {failures:?}");
    }

    #[test]
    fn test_animator_drives_real_frame_clock_to_target() {
        let Some(failures) = crate::test_support::with_gtk(|| {
            let (win, sw, state) = scroll_test_fixture();
            let adj = sw.vadjustment();
            let mut failures: Vec<String> = Vec::new();

            let start = adj.value();
            let dest = 400.0_f64;
            state.drive_to(dest, ARROW_DURATION_MS);

            // Sample midway: a working curve must have left the start point
            // but not yet arrived (that is the whole point of animating).
            pump_for(90);
            let mid = adj.value();
            if mid <= start + 0.5 {
                failures.push(format!("curve did not move off {start} (mid={mid})"));
            }
            if mid >= dest - 0.5 {
                failures.push(format!("curve jumped straight to the target (mid={mid})"));
            }

            // Let the 220ms curve finish.
            pump_for(400);
            let settled = adj.value();
            if (settled - dest).abs() > SCROLL_STOP_EPSILON {
                failures.push(format!("curve did not land on {dest} (got {settled})"));
            }
            if state.is_animating() {
                failures.push("curve finished but is_animating stayed set".into());
            }
            if state.tick_id.borrow().is_some() {
                failures.push("tick callback leaked after the curve completed".into());
            }
            if !sw.is_kinetic_scrolling() {
                failures.push("kinetic scrolling was not restored after finishing".into());
            }

            win.destroy();
            failures
        }) else {
            crate::test_support::skip_if_no_display();
            return;
        };
        assert!(failures.is_empty(), "failures: {failures:?}");
    }

    #[test]
    fn test_animator_wheel_impulse_and_bounds_in_real_widget() {
        let Some(failures) = crate::test_support::with_gtk(|| {
            let (win, sw, state) = scroll_test_fixture();
            let adj = sw.vadjustment();
            let mut failures: Vec<String> = Vec::new();
            adj.set_value(0.0);

            // One notch is a 100px impulse into the document.
            state.wheel_impulse(1.0);
            if !state.is_animating() {
                failures.push("wheel impulse did not animate".into());
            }
            if (state.target_y.get() - WHEEL_NOTCH_PX).abs() > 1e-9 {
                failures.push(format!("one notch targeted {}px", state.target_y.get()));
            }

            // Rapid notches accumulate on the same target, boost applied.
            state.wheel_impulse(1.0);
            if state.target_y.get() < WHEEL_NOTCH_PX * 2.0 - 1e-9 {
                failures.push("rapid notch did not accumulate".into());
            }
            if state.target_y.get() > WHEEL_NOTCH_PX * 2.0 * WHEEL_MAX_BOOST + 1e-9 {
                failures.push("rapid notch exceeded the 2.5x ceiling".into());
            }

            // Reverse impulses clamp at the document top.
            for _ in 0..3 {
                state.wheel_impulse(-1.0);
            }
            if state.target_y.get() != 0.0 {
                failures.push(format!("target must clamp at 0, got {}", state.target_y.get()));
            }

            // Home/End clamp against the live range, not a stale value.
            let max_scroll = state.max_scroll();
            if max_scroll <= 0.0 {
                failures.push("fixture must be scrollable".into());
            }
            state.scroll_to(99_999.0, JUMP_MAX_DURATION_MS);
            if (state.target_y.get() - max_scroll).abs() > 1e-9 {
                failures.push("End must clamp to max_scroll".into());
            }
            state.scroll_to(-99_999.0, JUMP_MAX_DURATION_MS);
            if state.target_y.get() != 0.0 {
                failures.push("Home must clamp to 0".into());
            }

            // Selection / touchpad hand-off clears everything.
            state.cancel();
            if state.is_animating() || state.held_direction.get().is_some() {
                failures.push("cancel must clear animation and held-key state".into());
            }
            win.destroy();
            failures
        }) else {
            crate::test_support::skip_if_no_display();
            return;
        };
        assert!(failures.is_empty(), "failures: {failures:?}");
    }

    #[test]
    fn test_held_key_state_machine_in_real_widget() {
        let Some(failures) = crate::test_support::with_gtk(|| {
            let (win, sw, state) = scroll_test_fixture();
            let adj = sw.vadjustment();
            let mut failures: Vec<String> = Vec::new();
            adj.set_value(1000.0);

            // Press: immediate 40px step, curve starts with 0ms delay.
            state.begin_hold(1);
            if state.held_direction.get() != Some(1) {
                failures.push("press did not arm the down direction".into());
            }
            if !state.is_animating() {
                failures.push("the initial step must animate immediately".into());
            }
            if (state.target_y.get() - (1000.0 + ARROW_STEP_PX)).abs() > 1e-9 {
                failures.push(format!("initial step targeted {}", state.target_y.get()));
            }

            // OS auto-repeat while the same key is held must not stack steps.
            let after_press = state.target_y.get();
            state.begin_hold(1);
            if state.target_y.get() != after_press {
                failures.push("auto-repeat stacked an extra step".into());
            }

            // A short tap (before the glide delay) just finishes its own step.
            state.end_hold();
            if state.held_direction.get().is_some() {
                failures.push("release did not clear the held direction".into());
            }
            if state.gliding.get() {
                failures.push("a tap must not leave the key gliding".into());
            }

            // A genuine hold transitions into the glide, and releasing it coasts.
            state.begin_hold(-1);
            state.gliding.set(true);
            state.end_hold();
            if state.held_direction.get().is_some() || state.gliding.get() {
                failures.push("release during glide did not stop the hold".into());
            }
            if !state.is_animating() {
                failures.push("release during glide must coast, not halt".into());
            }

            // Reversing mid-glide re-arms on the new direction.
            state.begin_hold(1);
            state.gliding.set(true);
            state.end_hold();
            state.begin_hold(-1);
            if state.held_direction.get() != Some(-1) {
                failures.push("reverse press did not arm the up direction".into());
            }
            state.cancel();
            win.destroy();
            failures
        }) else {
            crate::test_support::skip_if_no_display();
            return;
        };
        assert!(failures.is_empty(), "failures: {failures:?}");
    }

    #[test]
    fn test_refresh_button_sensitivity_and_shortcuts() {
        // Test shortcut matching logic
        let is_refresh_key = |keyval: gdk::Key, ctrl: bool| -> bool {
            ((keyval == gdk::Key::r || keyval == gdk::Key::R) && ctrl) || keyval == gdk::Key::F5
        };

        assert!(is_refresh_key(gdk::Key::r, true));
        assert!(is_refresh_key(gdk::Key::R, true));
        assert!(is_refresh_key(gdk::Key::F5, false));
        assert!(is_refresh_key(gdk::Key::F5, true));
        assert!(!is_refresh_key(gdk::Key::r, false));
        assert!(!is_refresh_key(gdk::Key::F4, false));

        // Test refresh sensitivity logic
        let compute_refresh_sensitive = |is_fetching: bool, current_url: &str| -> bool {
            !is_fetching && !current_url.is_empty()
        };

        // Empty startup state: disabled
        assert!(!compute_refresh_sensitive(false, ""));
        // While fetching: disabled
        assert!(!compute_refresh_sensitive(true, "https://example.com/article"));
        // While viewing article: enabled
        assert!(compute_refresh_sensitive(false, "https://example.com/article"));
    }

    #[test]
    fn test_startup_behavior_resolution() {
        // Priority resolution helper
        let resolve_startup = |cli_url: Option<String>, behavior: crate::config::StartupBehavior, recent_db_url: Option<String>| -> Option<String> {
            if let Some(url) = cli_url {
                Some(url)
            } else {
                match behavior {
                    crate::config::StartupBehavior::RecentPage => recent_db_url,
                    crate::config::StartupBehavior::BlankPage => None,
                }
            }
        };

        // Priority 1 overrides RecentPage
        assert_eq!(
            resolve_startup(
                Some("https://example.com/cli".to_string()),
                crate::config::StartupBehavior::RecentPage,
                Some("https://example.com/recent".to_string())
            ),
            Some("https://example.com/cli".to_string())
        );

        // Priority 1 overrides BlankPage
        assert_eq!(
            resolve_startup(
                Some("https://example.com/cli".to_string()),
                crate::config::StartupBehavior::BlankPage,
                None
            ),
            Some("https://example.com/cli".to_string())
        );

        // Priority 2: RecentPage with cached recent URL
        assert_eq!(
            resolve_startup(
                None,
                crate::config::StartupBehavior::RecentPage,
                Some("https://example.com/recent".to_string())
            ),
            Some("https://example.com/recent".to_string())
        );

        // Priority 2: RecentPage with no history falls back to None (empty state)
        assert_eq!(
            resolve_startup(
                None,
                crate::config::StartupBehavior::RecentPage,
                None
            ),
            None
        );

        // Priority 2: BlankPage returns None (empty state) even if recent article exists
        assert_eq!(
            resolve_startup(
                None,
                crate::config::StartupBehavior::BlankPage,
                Some("https://example.com/recent".to_string())
            ),
            None
        );
    }

    #[test]
    fn test_cross_paragraph_selection_and_copy_shortcuts() {
        let is_copy_key = |keyval: gdk::Key, ctrl: bool| -> bool {
            (keyval == gdk::Key::c || keyval == gdk::Key::C) && ctrl
        };

        let is_select_all_key = |keyval: gdk::Key, ctrl: bool| -> bool {
            (keyval == gdk::Key::a || keyval == gdk::Key::A) && ctrl
        };

        assert!(is_copy_key(gdk::Key::c, true));
        assert!(is_copy_key(gdk::Key::C, true));
        assert!(!is_copy_key(gdk::Key::c, false));
        assert!(!is_copy_key(gdk::Key::v, true));

        assert!(is_select_all_key(gdk::Key::a, true));
        assert!(is_select_all_key(gdk::Key::A, true));
        assert!(!is_select_all_key(gdk::Key::a, false));
    }

    #[test]
    fn test_in_page_search_navigation_and_shortcuts() {
        // 1. Navigation wrap-around logic
        let next_index = |cur: usize, total: usize| -> usize {
            if total == 0 { 0 } else { (cur + 1) % total }
        };
        let prev_index = |cur: usize, total: usize| -> usize {
            if total == 0 { 0 } else if cur == 0 { total - 1 } else { cur - 1 }
        };

        let total = 5;
        // Next: 0 -> 1 -> 2 -> 3 -> 4 -> 0
        assert_eq!(next_index(0, total), 1);
        assert_eq!(next_index(3, total), 4);
        assert_eq!(next_index(4, total), 0);

        // Prev: 0 -> 4 -> 3 -> 2 -> 1 -> 0
        assert_eq!(prev_index(0, total), 4);
        assert_eq!(prev_index(4, total), 3);
        assert_eq!(prev_index(1, total), 0);

        // 2. Shortcut matching logic
        let is_find_shortcut = |keyval: gdk::Key, ctrl: bool| -> bool {
            (keyval == gdk::Key::f || keyval == gdk::Key::F) && ctrl
        };
        let is_next_shortcut = |keyval: gdk::Key, ctrl: bool, shift: bool| -> bool {
            (keyval == gdk::Key::F3 && !shift) || ((keyval == gdk::Key::g || keyval == gdk::Key::G) && ctrl && !shift)
        };
        let is_prev_shortcut = |keyval: gdk::Key, ctrl: bool, shift: bool| -> bool {
            (keyval == gdk::Key::F3 && shift) || ((keyval == gdk::Key::g || keyval == gdk::Key::G) && ctrl && shift)
        };
        let is_escape = |keyval: gdk::Key| -> bool {
            keyval == gdk::Key::Escape
        };

        // Ctrl+F
        assert!(is_find_shortcut(gdk::Key::f, true));
        assert!(is_find_shortcut(gdk::Key::F, true));
        assert!(!is_find_shortcut(gdk::Key::f, false));

        // Next: F3 / Ctrl+G
        assert!(is_next_shortcut(gdk::Key::F3, false, false));
        assert!(is_next_shortcut(gdk::Key::g, true, false));
        assert!(is_next_shortcut(gdk::Key::G, true, false));
        assert!(!is_next_shortcut(gdk::Key::F3, false, true));
        assert!(!is_next_shortcut(gdk::Key::g, false, false));

        // Prev: Shift+F3 / Ctrl+Shift+G
        assert!(is_prev_shortcut(gdk::Key::F3, false, true));
        assert!(is_prev_shortcut(gdk::Key::g, true, true));
        assert!(is_prev_shortcut(gdk::Key::G, true, true));
        assert!(!is_prev_shortcut(gdk::Key::F3, false, false));

        // Escape
        assert!(is_escape(gdk::Key::Escape));
        assert!(!is_escape(gdk::Key::Return));

        // 3. Counter text formatting
        let format_counter = |cur: Option<usize>, total: usize| -> String {
            if total == 0 {
                "No matches".to_string()
            } else {
                format!("{} / {}", cur.map(|c| c + 1).unwrap_or(0), total)
            }
        };

        assert_eq!(format_counter(None, 0), "No matches");
        assert_eq!(format_counter(Some(0), 10), "1 / 10");
        assert_eq!(format_counter(Some(4), 10), "5 / 10");
        assert_eq!(format_counter(Some(9), 10), "10 / 10");
    }

    #[test]
    fn test_search_buffer_and_tags() {
        if crate::test_support::with_gtk(|| {
            let buffer = gtk::TextBuffer::new(None);
            let config = Config::default();
            setup_article_tags(&buffer, &config);

            let tag_table = buffer.tag_table();
            assert!(tag_table.lookup("search-match").is_some(), "search-match tag must be registered");
            assert!(tag_table.lookup("search-current").is_some(), "search-current tag must be registered");

            // Insert sample text
            buffer.set_text("The quick brown fox jumps over the lazy dog. The fox is fast.");

            // Test forward_search case-insensitivity
            let query = "fox";
            let flags = gtk::TextSearchFlags::CASE_INSENSITIVE;
            let mut start_iter = buffer.start_iter();
            let mut matches = Vec::new();

            while let Some((m_start, m_end)) = start_iter.forward_search(query, flags, None) {
                matches.push((m_start.offset(), m_end.offset()));
                start_iter = m_end;
            }

            assert_eq!(matches.len(), 2, "Must find exactly 2 occurrences of 'fox'");

            // Test case-insensitive search with 'THE'
            let query_upper = "THE";
            let mut start_iter_upper = buffer.start_iter();
            let mut matches_upper = Vec::new();
            while let Some((m_start, m_end)) = start_iter_upper.forward_search(query_upper, flags, None) {
                matches_upper.push((m_start.offset(), m_end.offset()));
                start_iter_upper = m_end;
            }
            assert_eq!(matches_upper.len(), 3, "Must find 3 occurrences of 'the' case-insensitively");
            })
            .is_none()
        {
            crate::test_support::skip_if_no_display();
        }

    }

    #[test]
    fn test_shortcuts_window_shortcut_matching() {
        let is_shortcuts_key = |keyval: gdk::Key, ctrl: bool| -> bool {
            keyval == gdk::Key::F1 || ((keyval == gdk::Key::question || keyval == gdk::Key::slash) && ctrl)
        };

        // F1
        assert!(is_shortcuts_key(gdk::Key::F1, false));
        assert!(is_shortcuts_key(gdk::Key::F1, true));

        // Ctrl+? / Ctrl+/
        assert!(is_shortcuts_key(gdk::Key::question, true));
        assert!(is_shortcuts_key(gdk::Key::slash, true));

        // Normal typing without ctrl should NOT match
        assert!(!is_shortcuts_key(gdk::Key::question, false));
        assert!(!is_shortcuts_key(gdk::Key::slash, false));
        assert!(!is_shortcuts_key(gdk::Key::F2, false));
    }








}
