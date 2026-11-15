//! The pure decision logic of the original's `InputController` and
//! `KeyboardController`: what a gesture or key MEANS, with no DOM.
//!
//! The controllers themselves are DOM event wiring (listener registration,
//! pointer capture, cursor styles, long-press timers) and belong to the
//! browser crate, which feeds these functions the numbers it reads from its
//! events. Extracted here, because the original's tests exercise them without a
//! DOM:
//!
//! - wheel handling: zoom anchoring ([`wheel_zoom_anchor`]), horizontal-pan
//!   detection ([`is_horizontal_wheel`], [`wheel_pan_delta`],
//!   [`wheel_pan_right_offset`]) and the whole decision [`classify_wheel`];
//! - gesture regions ([`region_at`]) and the double-click / double-tap routing
//!   ([`double_activate`]);
//! - drag arithmetic: pan ([`drag_pan_right_offset`]), time-axis zoom
//!   ([`time_axis_zoom_spacing`]), fling release ([`fling_velocity`],
//!   [`smooth_velocity`]), tap classification ([`is_tap_release`],
//!   [`pairs_as_double_tap`]);
//! - pinch ([`pinch_bar_spacing`], [`pinch_pinned_right_offset`]);
//! - the free-pan trigger match ([`matches_free_pan`]) and the effective
//!   magnet mode ([`effective_snap_mode`]);
//! - keyboard: key to action ([`key_to_action`], the original's `keyToAction`).
//!
//! Not extracted: the press-handling state machine (`onDown` / `onMove` /
//! `onUp`), which interleaves these decisions with listener, timer and drawings
//! callbacks, and the `KeyboardController` dispatch (a `switch` over
//! [`KeyAction`] onto host callbacks).

use crate::drawings::SnapMode;
use crate::options::{FreePanEvent, FreePanTrigger};

use super::coordinate_system::CoordinateSystem;
use super::viewport_state::{ViewportState, clamp_bar_spacing};

/// Px/ms below which a release is treated as a stop (no fling).
pub const FLING_MIN_SPEED: f64 = 0.04;
/// If the last move is older than this (ms) at release, don't fling.
pub const FLING_STALE_MS: f64 = 60.0;
/// Px of motion before a press counts as a drag (vs a click).
pub const DRAG_SLOP: f64 = 2.0;
/// Touch hold (ms) before a data-area press becomes crosshair-inspect mode.
pub const LONG_PRESS_MS: f64 = 350.0;
/// A resting finger wobbles far more than a mouse: within this radius a hold
/// still counts as stationary (long-press keeps arming), beyond it the gesture
/// is a pan.
pub const TOUCH_SLOP: f64 = 8.0;
/// A touch release still counts as a TAP within this landing-to-lift
/// displacement: a quick tap's contact patch slides further than the hold
/// wobble above, and judging the release (not the latched mid-gesture travel)
/// means an out-and-back wobble stays a tap.
pub const TOUCH_TAP_SLOP: f64 = 14.0;
/// Two clean taps this close together (time and space) are a double-tap, the
/// touch dblclick. The browser never synthesizes dblclick here (the controller
/// owns the touch stream), so the pairing is detected by hand. 350ms matches
/// the platform double-tap windows; 300 was tight enough to drop casual pairs.
pub const DOUBLE_TAP_MS: f64 = 350.0;
pub const DOUBLE_TAP_SLOP: f64 = 30.0;
/// Time-axis horizontal-zoom sensitivity: dragging left zooms in
/// (`e^(dx * k)`). Kept low so the zoom takes a deliberate, sizeable drag
/// (about 2x over 170px) rather than a twitch.
pub const TIME_SCALE_K: f64 = 0.004;
/// Wheel-zoom sensitivity: bar spacing scales by `e^(-deltaY * k)` per notch. A
/// typical mouse-wheel notch (deltaY about 100) is a clearly visible step.
pub const WHEEL_ZOOM_K: f64 = 0.004;
/// Wheel-over-the-price-axis rescale: each notch acts as a small axis DRAG:
/// deltaY is mapped to drag pixels at this ratio, so a notch (deltaY about 100)
/// reads as a 25px pull (span x1.10). Deliberately slower than the time
/// wheel-zoom: the scale is a fine adjustment, not a navigation gesture.
pub const WHEEL_PRICE_DRAG_PX: f64 = 0.25;
/// Shift+Arrow pans this many bars at once.
pub const PAN_BARS: f64 = 10.0;

/// Whether `e` satisfies the configured [`FreePanTrigger`] (mouse/pen only;
/// touch stays a time pan).
pub fn matches_free_pan(trigger: &FreePanTrigger, e: &FreePanEvent) -> bool {
    if matches!(trigger, FreePanTrigger::Enabled(false)) || e.pointer_type == "touch" {
        return false;
    }
    if let FreePanTrigger::Custom(predicate) = trigger {
        return predicate(e);
    }
    if e.button != 0 {
        return false;
    }
    match trigger {
        FreePanTrigger::Enabled(true) | FreePanTrigger::Drag => {
            !e.alt_key && !e.shift_key && !e.ctrl_key && !e.meta_key
        }
        FreePanTrigger::Alt => e.alt_key && !e.shift_key && !e.ctrl_key && !e.meta_key,
        FreePanTrigger::Shift => e.shift_key && !e.alt_key && !e.ctrl_key && !e.meta_key,
        FreePanTrigger::Mod => (e.ctrl_key || e.meta_key) && !e.alt_key && !e.shift_key,
        FreePanTrigger::Enabled(false) | FreePanTrigger::Custom(_) => false,
    }
}

/// Which strip a gesture started over: the data plot, the right price axis, the
/// bottom time axis, a sub-pane separator (drag to resize the panes above/below
/// it), or one of the gesture modes a press can enter: a drawings-layer
/// gesture, or the touch-only two-finger pinch, long-press crosshair
/// inspection, and a long-press that opened an axis sheet (no further drag for
/// that gesture).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DragRegion {
    Data,
    Price,
    Time,
    Separator,
    Drawing,
    Pinch,
    Crosshair,
    AxisSheet,
}

impl DragRegion {
    pub fn as_str(self) -> &'static str {
        match self {
            DragRegion::Data => "data",
            DragRegion::Price => "price",
            DragRegion::Time => "time",
            DragRegion::Separator => "separator",
            DragRegion::Drawing => "drawing",
            DragRegion::Pinch => "pinch",
            DragRegion::Crosshair => "crosshair",
            DragRegion::AxisSheet => "axis-sheet",
        }
    }
}

/// The controller's feature switches that decide gesture regions and the wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputSettings {
    /// When true (the default), wheel-zoom pins the right edge instead of the
    /// bar under the cursor.
    pub right_edge_zoom: bool,
    /// When true (the default), the price/time axis strips are draggable to
    /// rescale/zoom. When false, every press in those strips behaves as a
    /// normal data-area pan.
    pub axis_drag: bool,
    /// When true (the default), the separators between stacked panes are
    /// draggable to resize the panes above/below. When false, a press there is a
    /// normal data-area pan.
    pub pane_resize: bool,
}

impl Default for InputSettings {
    fn default() -> Self {
        Self {
            right_edge_zoom: true,
            axis_drag: true,
            pane_resize: true,
        }
    }
}

/// Classify a pixel into the plot, an axis strip (only when `axis_drag`), or a
/// sub-pane separator (only when `pane_resize`). A separator spans the FULL
/// width (data area + right scale gutter), so it is checked first and wins over
/// the price-axis strip where they cross: keeping the divider grabbable all the
/// way across the scale. `separator_at(y)` is the host's "is pixel `y` on a
/// draggable separator" probe.
pub fn region_at(
    width: f64,
    height: f64,
    x: f64,
    y: f64,
    settings: InputSettings,
    separator_at: &dyn Fn(f64) -> bool,
) -> DragRegion {
    if settings.pane_resize && y <= height && separator_at(y) {
        return DragRegion::Separator;
    }
    if settings.axis_drag {
        if x > width && y <= height {
            return DragRegion::Price; // right price-axis strip
        }
        if y > height && x <= width {
            return DragRegion::Time; // bottom time-axis strip
        }
    }
    DragRegion::Data
}

/// What a double-click / double-tap does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoubleActivation {
    /// Re-enable autoscale for the pane/column at the point.
    ResetPriceScale,
    /// Restore the two panes adjacent to the separator to an even split.
    ResetPaneSize,
    /// Fit the view to content (same as keyboard `0`).
    ResetView,
    /// Toggle the pane (collapse sub panes / maximize a study pane).
    DataDblClick,
    /// A drawing interaction, not a pane gesture.
    Ignore,
}

/// Shared double-click / double-tap routing, by the region under the point.
/// `last_press_drawing` / `prev_press_drawing` say whether the last two primary
/// presses were claimed by the drawings layer: a tool-driven click pair must
/// never fall through to the pane maximize toggle, even when the tool disarmed
/// itself between the two clicks. (A drawing double-click that opens its
/// settings is decided by the drawings layer before this runs.)
pub fn double_activate(
    region: DragRegion,
    last_press_drawing: bool,
    prev_press_drawing: bool,
) -> DoubleActivation {
    match region {
        DragRegion::Price => DoubleActivation::ResetPriceScale,
        DragRegion::Separator => DoubleActivation::ResetPaneSize,
        DragRegion::Time => DoubleActivation::ResetView,
        _ if last_press_drawing || prev_press_drawing => DoubleActivation::Ignore,
        _ => DoubleActivation::DataDblClick,
    }
}

/// The logical bar + its pixel that a wheel-zoom keeps pinned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WheelAnchor {
    pub logical: f64,
    pub x: f64,
}

/// The logical bar + its pixel that a wheel-zoom keeps pinned. `right_edge`
/// (the default) pins the right edge / latest bar (`right_offset` stays put
/// while zooming); otherwise the bar under the cursor (zoom toward the
/// pointer).
pub fn wheel_zoom_anchor(
    coords: &CoordinateSystem,
    cursor_x: f64,
    right_edge: bool,
) -> WheelAnchor {
    if right_edge {
        return WheelAnchor {
            logical: coords.right_edge_logical(),
            x: coords.width(),
        };
    }
    WheelAnchor {
        logical: coords.x_to_logical(cursor_x),
        x: cursor_x,
    }
}

/// A wheel gesture pans through time when its horizontal delta dominates: a
/// trackpad two-finger sideways swipe, or a tilt/horizontal mouse wheel. A
/// vertical-dominant gesture (a normal wheel notch) keeps zooming. Ties fall
/// through to zoom.
pub fn is_horizontal_wheel(delta_x: f64, delta_y: f64) -> bool {
    delta_x.abs() > delta_y.abs()
}

/// The `right_offset` after a horizontal wheel pan of `delta_x` pixels. Panning
/// by whole pixels (`delta_x / bar_spacing` bars) makes the chart track the
/// fingers 1:1. `delta_x > 0` (scroll/swipe right) moves forward toward the
/// latest bars, matching a leftward drag, which increases `right_offset` the
/// same way.
pub fn wheel_pan_right_offset(right_offset: f64, delta_x: f64, bar_spacing: f64) -> f64 {
    right_offset + delta_x / bar_spacing
}

/// The pixel delta a wheel gesture pans by, or `None` when it should zoom
/// instead. Pans on a horizontal-dominant gesture (trackpad swipe / tilt
/// wheel), and on Shift+wheel: scrolling through history like a document. Most
/// browsers already remap Shift+wheel into `deltaX` (caught by the horizontal
/// branch); the shift fallback covers those that keep the delta vertical.
/// Scroll down/right means toward the latest bars.
pub fn wheel_pan_delta(delta_x: f64, delta_y: f64, shift: bool) -> Option<f64> {
    if is_horizontal_wheel(delta_x, delta_y) {
        return Some(delta_x);
    }
    shift.then_some(delta_y)
}

/// A wheel event as the controller sees it, relative to the plot element.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WheelInput {
    pub x: f64,
    pub y: f64,
    pub delta_x: f64,
    pub delta_y: f64,
    pub shift: bool,
    /// Ctrl or Cmd held.
    pub ctrl_or_meta: bool,
}

/// What a wheel event does to the chart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WheelIntent {
    /// Over the right price-axis strip the wheel rescales THAT scale, like a
    /// slow axis drag: scroll down expands the span (zoom out), scroll up
    /// compresses it (zoom in). Each notch is an independent micro-drag (grab
    /// at `x`,`y`, rescale by `drag_px`, release), so consecutive notches
    /// compound from the live window.
    PriceScale { x: f64, y: f64, drag_px: f64 },
    /// Pan through time: apply this viewport (the renderer clamps it).
    Pan(ViewportState),
    /// Eased cursor/edge-anchored zoom: glide bar spacing to `target`, pinning
    /// `anchor` (`zoomTo(target, anchor.logical, anchor.x)`).
    Zoom {
        target_bar_spacing: f64,
        anchor: WheelAnchor,
    },
}

/// Decide a wheel event. A horizontal-dominant gesture (trackpad two-finger
/// swipe / tilt wheel) and Shift+wheel pan through time instead of zooming.
/// Holding Ctrl/Cmd overrides the sticky right-edge anchor and zooms toward the
/// cursor's bar. Smooth multiplicative zoom: scroll up (`delta_y < 0`) zooms in.
/// `separator_at` is the host's separator probe (see [`region_at`]).
pub fn classify_wheel(
    coords: &CoordinateSystem,
    wheel: &WheelInput,
    settings: InputSettings,
    separator_at: &dyn Fn(f64) -> bool,
) -> WheelIntent {
    let region = region_at(
        coords.width(),
        coords.height(),
        wheel.x,
        wheel.y,
        settings,
        separator_at,
    );
    if region == DragRegion::Price && wheel.delta_y != 0.0 {
        return WheelIntent::PriceScale {
            x: wheel.x,
            y: wheel.y,
            drag_px: wheel.delta_y * WHEEL_PRICE_DRAG_PX,
        };
    }
    let vp = coords.viewport();
    if let Some(pan) = wheel_pan_delta(wheel.delta_x, wheel.delta_y, wheel.shift) {
        return WheelIntent::Pan(ViewportState {
            bar_spacing: vp.bar_spacing,
            right_offset: wheel_pan_right_offset(vp.right_offset, pan, coords.px_per_bar()),
        });
    }
    let right_edge = settings.right_edge_zoom && !wheel.ctrl_or_meta;
    WheelIntent::Zoom {
        target_bar_spacing: clamp_bar_spacing(
            vp.bar_spacing * (-wheel.delta_y * WHEEL_ZOOM_K).exp(),
        ),
        anchor: wheel_zoom_anchor(coords, wheel.x, right_edge),
    }
}

/// The magnet mode actually applied: holding Ctrl/Cmd forces `Strong`, else the
/// sticky toolbar mode.
pub fn effective_snap_mode(momentary_override: bool, sticky: SnapMode) -> SnapMode {
    if momentary_override {
        SnapMode::Strong
    } else {
        sticky
    }
}

/// The bar spacing a pinch has reached: the start spacing scaled by the
/// finger-distance ratio, clamped to the viewport's zoom bounds. `start_dist` is
/// floored at 1px so a degenerate two-fingers-on-one-point start cannot divide
/// by zero.
pub fn pinch_bar_spacing(start_bar_spacing: f64, start_dist: f64, dist: f64) -> f64 {
    clamp_bar_spacing(start_bar_spacing * (dist / start_dist.max(1.0)))
}

/// The `right_offset` that pins logical index `anchor_logical` at pixel `x` for
/// a given effective pitch (bar spacing x spacing multiplier): the inverse of
/// `logical_to_x`. Pinning the pinch's start-midpoint logical at the LIVE
/// midpoint x makes the bars track the fingers exactly: spread to zoom, drag
/// both to pan, in one gesture.
pub fn pinch_pinned_right_offset(
    anchor_logical: f64,
    bar_count: usize,
    width: f64,
    x: f64,
    px_per_bar: f64,
) -> f64 {
    anchor_logical - (bar_count as f64 - 1.0) + (width - x) / px_per_bar
}

/// The viewport a data-area drag applies: drag right reveals earlier bars, so
/// `right_offset` decreases. Tracks by the effective pitch (zoom x spacing
/// multiplier) so the chart follows the cursor 1:1.
pub fn drag_pan_right_offset(start_right_offset: f64, dx: f64, px_per_bar: f64) -> f64 {
    start_right_offset - dx / px_per_bar
}

/// The bar spacing a time-axis drag reaches. Pins the right edge (the caller
/// keeps `right_offset`): `logical_to_x(right_edge) == width` for any bar
/// spacing, so only the spacing changes. Drag left zooms in.
pub fn time_axis_zoom_spacing(start_bar_spacing: f64, start_x: f64, x: f64) -> f64 {
    clamp_bar_spacing(start_bar_spacing * ((start_x - x) * TIME_SCALE_K).exp())
}

/// The click/drag slop for a pointer type: a resting finger wobbles far more
/// than a mouse, so a touch keeps its tap-hood through [`TOUCH_SLOP`] of travel.
pub fn drag_slop(is_touch: bool) -> f64 {
    if is_touch { TOUCH_SLOP } else { DRAG_SLOP }
}

/// The low-passed pointer velocity (px/ms) after a move of `x - last_x` over
/// `dt` ms. `None` when no time has passed (the velocity and its reference
/// point stay as they were).
pub fn smooth_velocity(vx: f64, x: f64, last_x: f64, dt: f64) -> Option<f64> {
    (dt > 0.0).then(|| vx * 0.6 + ((x - last_x) / dt) * 0.4)
}

/// The `right_offset` velocity (logical units per ms) to fling with on release,
/// or `None` for a stop. Only a finger flicks: a mouse release stops dead, so a
/// scrub lands exactly where the pointer let go instead of coasting past it.
/// `since_last_move_ms` is the age of the last tracked move at release.
pub fn fling_velocity(
    vx: f64,
    since_last_move_ms: f64,
    px_per_bar: f64,
    was_touch: bool,
) -> Option<f64> {
    let stale = since_last_move_ms > FLING_STALE_MS;
    (was_touch && !stale && vx.abs() > FLING_MIN_SPEED).then(|| -vx / px_per_bar)
}

/// Click/tap-hood of a release. A mouse uses the latched travel (`moved`)
/// alone; a touch must ALSO land within [`TOUCH_TAP_SLOP`] of where it lifted
/// (`release_dist` from the press point): the wider release radius forgives a
/// quick tap's sliding contact patch, but a gesture that latched `moved` (a
/// one-way 9-14px drag) already panned the chart and must not double as a
/// click/tap on release.
pub fn is_tap_release(dragging: bool, moved: bool, was_touch: bool, release_dist: f64) -> bool {
    dragging && !moved && (!was_touch || release_dist <= TOUCH_TAP_SLOP)
}

/// Whether a clean tap at (`x`, `y`, time `t`) pairs with the previous one
/// (`last_t == 0` means none pending) into a double-tap, the touch equivalent of
/// dblclick. The caller routes the pair by the FIRST tap: that one was aimed;
/// the second is the sloppier confirmation and can drift onto a neighboring
/// region and mis-route the pair.
pub fn pairs_as_double_tap(last_t: f64, last_x: f64, last_y: f64, t: f64, x: f64, y: f64) -> bool {
    last_t > 0.0 && t - last_t <= DOUBLE_TAP_MS && (x - last_x).hypot(y - last_y) <= DOUBLE_TAP_SLOP
}

/// Where a Home/End key jumps the focused bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    First,
    Last,
}

/// A classified keyboard intent (pure result of [`key_to_action`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum KeyAction {
    /// Pan the view by `bars` (positive: toward the latest/right, negative: earlier/left).
    Pan { bars: f64 },
    /// Step the zoom one notch (+1 in, -1 out), right-edge anchored.
    Zoom { direction: i32 },
    /// Move the focused/crosshair bar by `delta`, scrolling it into view + announcing it.
    Step { delta: i32 },
    /// Jump the focused bar to the first or last bar.
    Edge(Edge),
    /// Reset the view (fit content + drop manual scales).
    Reset,
    /// Scroll back to the most recent bars, keeping the current zoom.
    Realtime,
    /// Hide the crosshair (and stop announcing).
    Clear,
}

/// A keydown, reduced to what the mapping reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyInput<'a> {
    pub key: &'a str,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub meta: bool,
}

/// Map a keydown to a chart action. Arrow keys step the crosshair bar-to-bar
/// (Shift = pan a chunk); Alt+Shift+Right jumps back to the latest bars; `+`/`-`
/// zoom; Home/End jump to the data edges; `0` resets; Escape clears the
/// crosshair. `None` for keys the chart doesn't own.
pub fn key_to_action(e: &KeyInput<'_>) -> Option<KeyAction> {
    // Ctrl/Cmd chords belong to the HOST (a widget/workspace keymap: pan/zoom glides,
    // undo, ...). The chart's own navigation is plain-key only: if a chorded arrow also
    // stepped the crosshair here, its scroll-into-view would fight the host's pan glide
    // (the same keystroke handled twice reads as a bounce).
    if e.ctrl || e.meta {
        return None;
    }
    match e.key {
        "ArrowLeft" => Some(if e.shift {
            KeyAction::Pan { bars: -PAN_BARS }
        } else {
            KeyAction::Step { delta: -1 }
        }),
        "ArrowRight" => {
            if e.alt && e.shift {
                return Some(KeyAction::Realtime);
            }
            Some(if e.shift {
                KeyAction::Pan { bars: PAN_BARS }
            } else {
                KeyAction::Step { delta: 1 }
            })
        }
        "+" | "=" => Some(KeyAction::Zoom { direction: 1 }),
        "-" | "_" => Some(KeyAction::Zoom { direction: -1 }),
        "Home" => Some(KeyAction::Edge(Edge::First)),
        "End" => Some(KeyAction::Edge(Edge::Last)),
        "0" => Some(KeyAction::Reset),
        "Escape" => Some(KeyAction::Clear),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn coords() -> CoordinateSystem {
        let mut cs = CoordinateSystem::new();
        cs.set_size(800.0, 200.0, 1.0);
        cs.set_bars(&[1000, 2000, 3000, 4000, 5000]); // 5 bars, interval 1000
        cs.set_viewport(ViewportState {
            bar_spacing: 100.0,
            right_offset: 1.0,
        });
        cs
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-6, "{a} vs {b}");
    }

    const NO_SEPARATOR: &dyn Fn(f64) -> bool = &|_| false;

    // ── wheelZoomAnchor ──

    #[test]
    fn cursor_mode_pins_the_logical_under_the_pointer_at_the_cursor_pixel() {
        let cs = coords();
        let a = wheel_zoom_anchor(&cs, 300.0, false);
        assert_eq!(a.x, 300.0);
        assert_eq!(a.logical, cs.x_to_logical(300.0));
    }

    #[test]
    fn right_edge_mode_pins_the_right_edge_logical_at_the_right_pixel_edge() {
        let cs = coords();
        let a = wheel_zoom_anchor(&cs, 300.0, true);
        assert_eq!(a.x, cs.width()); // 800
        assert_eq!(a.logical, cs.right_edge_logical()); // (n-1)+rightOffset = 5
    }

    // ── horizontal wheel / trackpad ──

    #[test]
    fn a_horizontal_dominant_gesture_pans_and_a_vertical_one_zooms() {
        assert!(is_horizontal_wheel(30.0, 4.0)); // sideways two-finger swipe: pan
        assert!(!is_horizontal_wheel(4.0, 30.0)); // normal notch: zoom
        assert!(!is_horizontal_wheel(10.0, 10.0)); // ties fall through to zoom
        assert!(!is_horizontal_wheel(0.0, 0.0));
    }

    #[test]
    fn scrolling_right_moves_forward_toward_the_latest_bars() {
        // barSpacing 10: a 50px swipe pans exactly 5 bars (1:1 with the fingers).
        close(wheel_pan_right_offset(0.0, 50.0, 10.0), 5.0);
        close(wheel_pan_right_offset(3.0, -20.0, 10.0), 1.0); // scroll left: back into history
    }

    #[test]
    fn shift_wheel_scrolls_through_history_instead_of_zooming() {
        assert_eq!(wheel_pan_delta(0.0, 120.0, false), None); // normal notch: zoom
        assert_eq!(wheel_pan_delta(0.0, 120.0, true), Some(120.0)); // shift: pan forward
        assert_eq!(wheel_pan_delta(0.0, -120.0, true), Some(-120.0)); // shift + scroll up: back into history
    }

    #[test]
    fn a_horizontal_dominant_gesture_pans_by_delta_x_with_or_without_shift() {
        // Browsers that remap Shift+wheel into deltaX land here too.
        assert_eq!(wheel_pan_delta(120.0, 0.0, false), Some(120.0));
        assert_eq!(wheel_pan_delta(120.0, 0.0, true), Some(120.0));
        assert_eq!(wheel_pan_delta(-30.0, 4.0, true), Some(-30.0));
    }

    // ── wheel over the price axis ──

    fn wheel(x: f64, y: f64, delta_y: f64) -> WheelInput {
        WheelInput {
            x,
            y,
            delta_x: 0.0,
            delta_y,
            shift: false,
            ctrl_or_meta: false,
        }
    }

    #[test]
    fn a_notch_over_the_axis_strip_grabs_that_scale_and_rescales_it_as_a_25px_drag() {
        let cs = coords(); // 800x200 plot: x > 800 is the price-axis strip
        let down = classify_wheel(
            &cs,
            &wheel(820.0, 100.0, 100.0),
            InputSettings::default(),
            NO_SEPARATOR,
        );
        assert_eq!(
            down,
            WheelIntent::PriceScale {
                x: 820.0,
                y: 100.0,
                drag_px: 25.0
            }
        );
        let up = classify_wheel(
            &cs,
            &wheel(820.0, 100.0, -100.0),
            InputSettings::default(),
            NO_SEPARATOR,
        );
        assert_eq!(
            up,
            WheelIntent::PriceScale {
                x: 820.0,
                y: 100.0,
                drag_px: -25.0
            }
        );
    }

    #[test]
    fn a_wheel_over_the_data_area_still_time_zooms() {
        let cs = coords();
        let intent = classify_wheel(
            &cs,
            &wheel(400.0, 100.0, 100.0),
            InputSettings::default(),
            NO_SEPARATOR,
        );
        let WheelIntent::Zoom {
            target_bar_spacing,
            anchor,
        } = intent
        else {
            panic!("expected a zoom, got {intent:?}");
        };
        close(target_bar_spacing, 100.0 * (-0.4f64).exp());
        // the default pins the right edge
        assert_eq!(anchor, wheel_zoom_anchor(&cs, 400.0, true));
    }

    #[test]
    fn axis_drag_off_turns_the_axis_wheel_back_into_a_plain_zoom() {
        let cs = coords();
        let settings = InputSettings {
            axis_drag: false,
            ..Default::default()
        };
        let intent = classify_wheel(&cs, &wheel(820.0, 100.0, 100.0), settings, NO_SEPARATOR);
        assert!(matches!(intent, WheelIntent::Zoom { .. }));
    }

    #[test]
    fn a_zero_delta_over_the_axis_does_not_rescale() {
        let cs = coords();
        let intent = classify_wheel(
            &cs,
            &wheel(820.0, 100.0, 0.0),
            InputSettings::default(),
            NO_SEPARATOR,
        );
        assert!(matches!(intent, WheelIntent::Zoom { .. }));
    }

    #[test]
    fn ctrl_zooms_toward_the_cursor_instead_of_the_right_edge() {
        let cs = coords();
        let mut w = wheel(300.0, 100.0, -100.0);
        w.ctrl_or_meta = true;
        let WheelIntent::Zoom {
            target_bar_spacing,
            anchor,
        } = classify_wheel(&cs, &w, InputSettings::default(), NO_SEPARATOR)
        else {
            panic!("expected a zoom");
        };
        assert!(target_bar_spacing > 100.0); // scroll up zooms in
        assert_eq!(
            anchor,
            WheelAnchor {
                logical: cs.x_to_logical(300.0),
                x: 300.0
            }
        );
        // and the setting off pins the cursor too
        let settings = InputSettings {
            right_edge_zoom: false,
            ..Default::default()
        };
        let plain = classify_wheel(&cs, &wheel(300.0, 100.0, -100.0), settings, NO_SEPARATOR);
        assert!(matches!(plain, WheelIntent::Zoom { anchor, .. } if anchor.x == 300.0));
    }

    #[test]
    fn a_horizontal_swipe_or_shift_wheel_pans_the_viewport() {
        let cs = coords(); // pitch 100, rightOffset 1
        let mut swipe = wheel(400.0, 100.0, 4.0);
        swipe.delta_x = 50.0;
        let intent = classify_wheel(&cs, &swipe, InputSettings::default(), NO_SEPARATOR);
        assert_eq!(
            intent,
            WheelIntent::Pan(ViewportState {
                bar_spacing: 100.0,
                right_offset: 1.5
            })
        );
        let mut shift = wheel(400.0, 100.0, -200.0);
        shift.shift = true;
        let intent = classify_wheel(&cs, &shift, InputSettings::default(), NO_SEPARATOR);
        assert_eq!(
            intent,
            WheelIntent::Pan(ViewportState {
                bar_spacing: 100.0,
                right_offset: -1.0
            })
        );
    }

    #[test]
    fn the_wheel_zoom_target_is_clamped_to_the_zoom_bounds() {
        let cs = coords();
        let WheelIntent::Zoom {
            target_bar_spacing, ..
        } = classify_wheel(
            &cs,
            &wheel(400.0, 100.0, -1e6),
            InputSettings::default(),
            NO_SEPARATOR,
        )
        else {
            panic!("expected a zoom");
        };
        assert_eq!(target_bar_spacing, 1000.0);
    }

    #[test]
    fn the_wheel_pan_follows_the_effective_pitch() {
        let mut cs = coords();
        cs.set_pitch_scale(2.0); // pitch 200
        let mut swipe = wheel(400.0, 100.0, 0.0);
        swipe.delta_x = 100.0;
        let WheelIntent::Pan(vp) =
            classify_wheel(&cs, &swipe, InputSettings::default(), NO_SEPARATOR)
        else {
            panic!("expected a pan");
        };
        assert_eq!(vp.right_offset, 1.5);
        assert_eq!(vp.bar_spacing, 100.0);
    }

    #[test]
    fn the_input_settings_default_to_on() {
        assert_eq!(
            InputSettings::default(),
            InputSettings {
                right_edge_zoom: true,
                axis_drag: true,
                pane_resize: true
            }
        );
    }

    // ── regions ──

    fn region(x: f64, y: f64, settings: InputSettings, sep: bool) -> DragRegion {
        region_at(800.0, 200.0, x, y, settings, &move |_| sep)
    }

    #[test]
    fn classifies_the_price_strip_time_strip_and_data_plot() {
        let d = InputSettings::default();
        assert_eq!(region(850.0, 100.0, d, false), DragRegion::Price); // x > 800
        assert_eq!(region(400.0, 250.0, d, false), DragRegion::Time); // y > 200
        assert_eq!(region(400.0, 100.0, d, false), DragRegion::Data);
        assert_eq!(region(850.0, 250.0, d, false), DragRegion::Data); // bottom-right corner: neither axis
    }

    #[test]
    fn everything_is_data_when_axis_drag_is_disabled() {
        let s = InputSettings {
            axis_drag: false,
            ..Default::default()
        };
        assert_eq!(region(850.0, 100.0, s, false), DragRegion::Data);
        assert_eq!(region(400.0, 250.0, s, false), DragRegion::Data);
    }

    #[test]
    fn a_separator_inside_the_data_area_is_a_separator() {
        assert_eq!(
            region(400.0, 150.0, InputSettings::default(), true),
            DragRegion::Separator
        );
    }

    #[test]
    fn a_normal_data_point_without_a_separator_is_data() {
        assert_eq!(
            region(400.0, 150.0, InputSettings::default(), false),
            DragRegion::Data
        );
    }

    #[test]
    fn a_separator_wins_over_the_price_axis_strip_where_they_cross() {
        // The separator spans the full width, so a separator y in the right gutter resolves to
        // 'separator': you can grab the divider over the scale, not scale the axis.
        assert_eq!(
            region(820.0, 150.0, InputSettings::default(), true),
            DragRegion::Separator
        );
    }

    #[test]
    fn the_price_strip_when_not_on_a_separator_y() {
        assert_eq!(
            region(820.0, 150.0, InputSettings::default(), false),
            DragRegion::Price
        );
    }

    #[test]
    fn the_time_strip_below_the_panes_is_time_even_over_a_separator_y() {
        assert_eq!(
            region(400.0, 220.0, InputSettings::default(), true),
            DragRegion::Time
        );
    }

    #[test]
    fn separators_are_ignored_when_pane_resize_is_disabled() {
        let s = InputSettings {
            pane_resize: false,
            ..Default::default()
        };
        assert_eq!(region(400.0, 150.0, s, true), DragRegion::Data);
    }

    #[test]
    fn the_separator_probe_is_only_asked_inside_the_data_height() {
        let probe: &dyn Fn(f64) -> bool = &|_| panic!("probed outside the plot");
        assert_eq!(
            region_at(800.0, 200.0, 400.0, 250.0, InputSettings::default(), probe),
            DragRegion::Time
        );
    }

    // ── double-click routing ──

    #[test]
    fn double_clicks_route_by_region() {
        assert_eq!(
            double_activate(DragRegion::Time, false, false),
            DoubleActivation::ResetView
        );
        assert_eq!(
            double_activate(DragRegion::Data, false, false),
            DoubleActivation::DataDblClick
        );
        assert_eq!(
            double_activate(DragRegion::Price, false, false),
            DoubleActivation::ResetPriceScale
        );
        assert_eq!(
            double_activate(DragRegion::Separator, false, false),
            DoubleActivation::ResetPaneSize
        );
    }

    #[test]
    fn a_tool_driven_click_pair_never_toggles_the_pane() {
        assert_eq!(
            double_activate(DragRegion::Data, true, false),
            DoubleActivation::Ignore
        );
        assert_eq!(
            double_activate(DragRegion::Data, false, true),
            DoubleActivation::Ignore
        );
        // but an axis double-click still resets its axis
        assert_eq!(
            double_activate(DragRegion::Time, true, true),
            DoubleActivation::ResetView
        );
    }

    // ── pinch ──

    #[test]
    fn pinch_scales_bar_spacing_by_the_finger_distance_ratio() {
        close(pinch_bar_spacing(10.0, 100.0, 200.0), 20.0); // fingers spread 2x: zoom in 2x
        close(pinch_bar_spacing(10.0, 200.0, 100.0), 5.0); // fingers close 2x: zoom out 2x
        close(pinch_bar_spacing(10.0, 100.0, 100.0), 10.0);
    }

    #[test]
    fn pinch_clamps_to_the_viewport_zoom_bounds() {
        assert_eq!(
            pinch_bar_spacing(10.0, 100.0, 1e9),
            clamp_bar_spacing(f64::MAX)
        );
        assert_eq!(pinch_bar_spacing(10.0, 1e9, 1.0), clamp_bar_spacing(0.0));
    }

    #[test]
    fn pinch_never_divides_by_a_degenerate_start_distance() {
        assert!(pinch_bar_spacing(10.0, 0.0, 50.0).is_finite());
    }

    #[test]
    fn pinch_pinned_right_offset_keeps_the_anchor_logical_under_the_anchor_pixel() {
        let mut cs = CoordinateSystem::new();
        cs.set_size(800.0, 200.0, 1.0);
        cs.set_bars(&[1000, 2000, 3000, 4000, 5000]);
        cs.set_viewport(ViewportState {
            bar_spacing: 50.0,
            right_offset: 2.0,
        });
        let mid_x = 300.0;
        let anchor = cs.x_to_logical(mid_x);
        // Zoom to a new spacing, pinning `anchor` at mid_x (pitch multiplier is 1 here).
        let bar_spacing = 80.0;
        let right_offset =
            pinch_pinned_right_offset(anchor, cs.bar_count(), cs.width(), mid_x, bar_spacing);
        cs.set_viewport(ViewportState {
            bar_spacing,
            right_offset,
        });
        close(cs.logical_to_x(anchor), mid_x);
    }

    #[test]
    fn a_pure_pinch_pan_shifts_the_view_with_the_fingers() {
        let mut cs = CoordinateSystem::new();
        cs.set_size(800.0, 200.0, 1.0);
        cs.set_bars(&[1000, 2000, 3000, 4000, 5000]);
        cs.set_viewport(ViewportState {
            bar_spacing: 50.0,
            right_offset: 2.0,
        });
        let anchor = cs.x_to_logical(300.0);
        let right_offset =
            pinch_pinned_right_offset(anchor, cs.bar_count(), cs.width(), 400.0, 50.0);
        cs.set_viewport(ViewportState {
            bar_spacing: 50.0,
            right_offset,
        });
        close(cs.logical_to_x(anchor), 400.0); // the anchored bar followed the fingers
    }

    // ── drag arithmetic ──

    #[test]
    fn a_data_drag_tracks_the_cursor_one_to_one() {
        // dragging right by one pitch reveals one earlier bar
        assert_eq!(drag_pan_right_offset(3.0, 20.0, 20.0), 2.0);
        assert_eq!(drag_pan_right_offset(3.0, -40.0, 20.0), 5.0);
    }

    #[test]
    fn dragging_the_time_axis_left_zooms_in_and_right_zooms_out() {
        assert!(time_axis_zoom_spacing(10.0, 400.0, 300.0) > 10.0);
        assert!(time_axis_zoom_spacing(10.0, 400.0, 500.0) < 10.0);
        assert_eq!(time_axis_zoom_spacing(10.0, 400.0, 400.0), 10.0);
        // about 2x over ~170px
        let doubled = time_axis_zoom_spacing(10.0, 400.0, 400.0 - 173.0);
        assert!((doubled - 20.0).abs() < 0.2, "{doubled}");
        assert_eq!(
            time_axis_zoom_spacing(10.0, 0.0, 1e9),
            clamp_bar_spacing(0.0)
        );
    }

    #[test]
    fn touch_keeps_tap_hood_through_a_wider_slop() {
        assert_eq!(drag_slop(false), 2.0);
        assert_eq!(drag_slop(true), 8.0);
    }

    #[test]
    fn the_pointer_velocity_is_low_passed() {
        assert_eq!(smooth_velocity(0.0, 10.0, 0.0, 10.0), Some(0.4));
        assert_eq!(smooth_velocity(1.0, 0.0, 0.0, 10.0), Some(0.6));
        assert_eq!(smooth_velocity(1.0, 10.0, 0.0, 0.0), None);
        assert_eq!(smooth_velocity(1.0, 10.0, 0.0, -5.0), None);
    }

    #[test]
    fn only_a_fresh_fast_finger_release_flings() {
        // 0.5 px/ms rightward on a 10px pitch: rightOffset decreases at 0.05 bars/ms
        assert_eq!(fling_velocity(0.5, 10.0, 10.0, true), Some(-0.05));
        assert_eq!(fling_velocity(0.5, 10.0, 10.0, false), None); // a mouse release stops dead
        assert_eq!(fling_velocity(0.5, 61.0, 10.0, true), None); // stale
        assert_eq!(fling_velocity(0.5, 60.0, 10.0, true), Some(-0.05)); // the stale bound is exclusive
        assert_eq!(fling_velocity(0.04, 10.0, 10.0, true), None); // too slow
        assert_eq!(fling_velocity(-0.5, 10.0, 10.0, true), Some(0.05));
    }

    #[test]
    fn tap_hood_needs_an_unmoved_press_and_for_touch_a_near_release() {
        assert!(is_tap_release(true, false, false, 100.0)); // a mouse ignores the release distance
        assert!(!is_tap_release(true, true, false, 0.0));
        assert!(!is_tap_release(false, false, false, 0.0));
        assert!(is_tap_release(true, false, true, 14.0));
        assert!(!is_tap_release(true, false, true, 14.1));
    }

    #[test]
    fn two_close_clean_taps_pair_into_a_double_tap() {
        assert!(pairs_as_double_tap(
            1000.0, 100.0, 100.0, 1300.0, 110.0, 110.0
        ));
        assert!(pairs_as_double_tap(
            1000.0, 100.0, 100.0, 1350.0, 100.0, 130.0
        )); // both bounds inclusive
        assert!(!pairs_as_double_tap(
            1000.0, 100.0, 100.0, 1351.0, 100.0, 100.0
        )); // too slow
        assert!(!pairs_as_double_tap(
            1000.0, 100.0, 100.0, 1100.0, 100.0, 131.0
        )); // too far
        assert!(!pairs_as_double_tap(0.0, 100.0, 100.0, 100.0, 100.0, 100.0)); // no first tap pending
    }

    // ── free pan ──

    fn press() -> FreePanEvent {
        FreePanEvent {
            button: 0,
            alt_key: false,
            shift_key: false,
            ctrl_key: false,
            meta_key: false,
            pointer_type: "mouse".into(),
        }
    }

    #[test]
    fn free_pan_matches_plain_drag_modifier_presets_and_custom_predicates_while_ignoring_touch() {
        let plain = press();
        let with = |f: &dyn Fn(&mut FreePanEvent)| {
            let mut e = press();
            f(&mut e);
            e
        };
        assert!(!matches_free_pan(&FreePanTrigger::Enabled(false), &plain));
        assert!(matches_free_pan(&FreePanTrigger::Enabled(true), &plain));
        assert!(matches_free_pan(&FreePanTrigger::Drag, &plain));
        assert!(!matches_free_pan(
            &FreePanTrigger::Enabled(true),
            &with(&|e| e.alt_key = true)
        ));
        assert!(!matches_free_pan(
            &FreePanTrigger::Enabled(true),
            &with(&|e| e.pointer_type = "touch".into())
        ));
        assert!(matches_free_pan(
            &FreePanTrigger::Alt,
            &with(&|e| e.alt_key = true)
        ));
        assert!(matches_free_pan(
            &FreePanTrigger::Shift,
            &with(&|e| e.shift_key = true)
        ));
        assert!(matches_free_pan(
            &FreePanTrigger::Mod,
            &with(&|e| e.ctrl_key = true)
        ));
        assert!(matches_free_pan(
            &FreePanTrigger::Mod,
            &with(&|e| e.meta_key = true)
        ));
        let button_four = FreePanTrigger::Custom(Rc::new(|e| e.button == 4));
        assert!(matches_free_pan(&button_four, &with(&|e| e.button = 4)));
        assert!(!matches_free_pan(&button_four, &plain));
    }

    #[test]
    fn free_pan_modifier_presets_are_exclusive_and_need_the_primary_button() {
        let mut e = press();
        e.alt_key = true;
        e.shift_key = true;
        assert!(!matches_free_pan(&FreePanTrigger::Alt, &e));
        assert!(!matches_free_pan(&FreePanTrigger::Shift, &e));
        assert!(!matches_free_pan(&FreePanTrigger::Mod, &e));
        let mut middle = press();
        middle.button = 1;
        assert!(!matches_free_pan(&FreePanTrigger::Drag, &middle));
        // a custom predicate chooses its own buttons but still never sees touch
        let any = FreePanTrigger::Custom(Rc::new(|_| true));
        assert!(matches_free_pan(&any, &middle));
        let mut touch = press();
        touch.pointer_type = "touch".into();
        assert!(!matches_free_pan(&any, &touch));
    }

    // ── magnet ──

    #[test]
    fn the_effective_snap_mode_is_strong_while_the_modifier_is_held() {
        assert_eq!(effective_snap_mode(true, SnapMode::Off), SnapMode::Strong);
        assert_eq!(effective_snap_mode(true, SnapMode::Weak), SnapMode::Strong);
        assert_eq!(effective_snap_mode(false, SnapMode::Off), SnapMode::Off);
        assert_eq!(effective_snap_mode(false, SnapMode::Weak), SnapMode::Weak);
        assert_eq!(
            effective_snap_mode(false, SnapMode::Strong),
            SnapMode::Strong
        );
    }

    // ── keyboard ──

    fn key(k: &str) -> KeyInput<'_> {
        KeyInput {
            key: k,
            ..Default::default()
        }
    }

    #[test]
    fn arrows_step_the_crosshair_and_shift_pans_a_chunk() {
        assert_eq!(
            key_to_action(&key("ArrowLeft")),
            Some(KeyAction::Step { delta: -1 })
        );
        assert_eq!(
            key_to_action(&key("ArrowRight")),
            Some(KeyAction::Step { delta: 1 })
        );
        let shift = |k| KeyInput {
            shift: true,
            ..key(k)
        };
        assert_eq!(
            key_to_action(&shift("ArrowLeft")),
            Some(KeyAction::Pan { bars: -10.0 })
        );
        assert_eq!(
            key_to_action(&shift("ArrowRight")),
            Some(KeyAction::Pan { bars: 10.0 })
        );
    }

    #[test]
    fn alt_shift_right_jumps_back_to_the_most_recent_bars() {
        let chord = KeyInput {
            shift: true,
            alt: true,
            ..key("ArrowRight")
        };
        assert_eq!(key_to_action(&chord), Some(KeyAction::Realtime));
        // Alt alone (no Shift) is not the realtime shortcut: the plain step still wins.
        let alt = KeyInput {
            alt: true,
            ..key("ArrowRight")
        };
        assert_eq!(key_to_action(&alt), Some(KeyAction::Step { delta: 1 }));
        // and Alt+Shift+Left is a plain pan
        let left = KeyInput {
            shift: true,
            alt: true,
            ..key("ArrowLeft")
        };
        assert_eq!(key_to_action(&left), Some(KeyAction::Pan { bars: -10.0 }));
    }

    #[test]
    fn plus_minus_zoom_home_end_jump_zero_resets_escape_clears() {
        assert_eq!(
            key_to_action(&key("+")),
            Some(KeyAction::Zoom { direction: 1 })
        );
        assert_eq!(
            key_to_action(&key("=")),
            Some(KeyAction::Zoom { direction: 1 })
        );
        assert_eq!(
            key_to_action(&key("-")),
            Some(KeyAction::Zoom { direction: -1 })
        );
        assert_eq!(
            key_to_action(&key("_")),
            Some(KeyAction::Zoom { direction: -1 })
        );
        assert_eq!(
            key_to_action(&key("Home")),
            Some(KeyAction::Edge(Edge::First))
        );
        assert_eq!(
            key_to_action(&key("End")),
            Some(KeyAction::Edge(Edge::Last))
        );
        assert_eq!(key_to_action(&key("0")), Some(KeyAction::Reset));
        assert_eq!(key_to_action(&key("Escape")), Some(KeyAction::Clear));
        assert_eq!(key_to_action(&key("a")), None);
        assert_eq!(key_to_action(&key("")), None);
    }

    #[test]
    fn ctrl_and_cmd_chords_belong_to_the_host() {
        let ctrl = KeyInput {
            ctrl: true,
            ..key("ArrowLeft")
        };
        let meta = KeyInput {
            meta: true,
            ..key("0")
        };
        assert_eq!(key_to_action(&ctrl), None);
        assert_eq!(key_to_action(&meta), None);
    }
}
