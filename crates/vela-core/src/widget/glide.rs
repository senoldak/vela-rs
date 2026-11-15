//! Keyboard zoom glide: the eased "follow" animation toward a target visible range
//! (pure math plus the glide's state machine). Keyboard PAN doesn't glide here: it goes
//! through `chart.pan_by`, the renderer's drag-equivalent scroll (same clamp and easing as
//! a pointer drag).
//!
//! The original drives the glide with `requestAnimationFrame`. Here the browser crate owns
//! the frame loop: after [`Glider::zoom`] returns [`GlideSchedule::NextFrame`] it asks for
//! a frame and calls [`Glider::frame`] from it, scheduling another while that returns
//! `true`, and cancels the pending frame when [`Glider::stop`] is called.

use crate::indicators::VisibleRange;
use crate::indicators::classics::math::{js_max, js_round};
use crate::vela::Vela;

const MIN_SPAN_MS: f64 = 60_000.0;
const FOLLOW: f64 = 0.25;
const EPS_FRAC: f64 = 0.0015;
pub const ZOOM_IN: f64 = 0.8;
pub const ZOOM_OUT: f64 = 1.25;
/// Fraction of the visible width one pan key-press scrolls (fed to `chart.pan_by`).
pub const PAN_FAST: f64 = 0.2;

/// A time span in epoch ms. Floating point: the easing lands between whole milliseconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    pub from: f64,
    pub to: f64,
}

impl Range {
    fn of(range: VisibleRange) -> Self {
        Range {
            from: range.from as f64,
            to: range.to as f64,
        }
    }

    /// The chart takes whole milliseconds.
    fn to_visible(self) -> VisibleRange {
        VisibleRange {
            from: js_round(self.from) as i64,
            to: js_round(self.to) as i64,
        }
    }
}

/// Right-anchored zoom target (like the renderer's wheel default).
pub fn zoom_target(base: Range, factor: f64) -> Range {
    let new_span = js_max(MIN_SPAN_MS, (base.to - base.from) * factor);
    Range {
        from: base.to - new_span,
        to: base.to,
    }
}

/// One easing step toward `target`; snaps when both edges are within ~0.15% of the span.
pub fn follow_step(cur: Range, target: Range) -> (Range, bool) {
    follow_step_with(cur, target, FOLLOW, EPS_FRAC)
}

pub fn follow_step_with(cur: Range, target: Range, follow: f64, eps_frac: f64) -> (Range, bool) {
    let eps = js_max(MIN_SPAN_MS, (target.to - target.from) * eps_frac);
    let nf = cur.from + (target.from - cur.from) * follow;
    let nt = cur.to + (target.to - cur.to) * follow;
    let done = (target.from - nf).abs() <= eps && (target.to - nt).abs() <= eps;
    (
        if done {
            target
        } else {
            Range { from: nf, to: nt }
        },
        done,
    )
}

/// Whether the chart's renderer eases zooms (`animZoom` truthy). A renderer without the
/// feature keeps the keyboard glide: it has no zoom animation of its own to agree with.
fn zoom_animated(chart: &Vela) -> bool {
    let control = chart.renderer();
    if !control.supports("animZoom") {
        return true;
    }
    match control.get("animZoom") {
        None | Some(serde_json::Value::Null) | Some(serde_json::Value::Bool(false)) => false,
        Some(serde_json::Value::Number(n)) => n.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Some(serde_json::Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// What the frame loop owes after a glider call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlideSchedule {
    /// A glide is running: request an animation frame and call [`Glider::frame`] from it.
    NextFrame,
    /// Nothing to schedule (a frame is already pending, or the glide is over).
    Idle,
}

/// Drives eased range changes on a chart; repeated calls retarget the running glide.
///
/// The glide eases ITS OWN range, never the chart's read-back: `get_visible_range` is
/// data-bounded (whole bars, `to` capped at the newest bar) and the renderer clamps what
/// gets applied. Chasing that read-back can never converge on a clamped step, leaving
/// the frame loop re-applying a stale target forever (and grinding the span down when
/// panning past the newest bar). The internal range converges geometrically, so the
/// glide always terminates; the renderer is free to clamp each applied frame.
#[derive(Debug, Default)]
pub struct Glider {
    cur: Option<Range>,
    target: Option<Range>,
    frame_pending: bool,
}

impl Glider {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a glide is running.
    pub fn active(&self) -> bool {
        self.target.is_some()
    }

    pub fn zoom(&mut self, chart: Option<&Vela>, factor: f64) -> GlideSchedule {
        self.to(chart, |base| zoom_target(base, factor))
    }

    /// Stop gliding and forget the range; the caller cancels its pending frame.
    pub fn stop(&mut self) {
        self.frame_pending = false;
        self.cur = None;
        self.target = None;
    }

    fn to(&mut self, chart: Option<&Vela>, make: impl FnOnce(Range) -> Range) -> GlideSchedule {
        let base = self
            .target
            .or_else(|| chart.and_then(Vela::get_visible_range).map(Range::of));
        let (Some(chart), Some(base)) = (chart, base) else {
            return GlideSchedule::Idle;
        };
        let target = make(base);
        // The chart's zoom animation switched off (`animations.zoom` / the settings dialog)
        // applies to the keyboard too: jump straight to the target, no glide.
        if !zoom_animated(chart) {
            self.stop();
            chart.set_visible_range(target.to_visible());
            return GlideSchedule::Idle;
        }
        // Seed from the chart on a fresh glide; keep it while retargeting.
        self.cur.get_or_insert(base);
        self.target = Some(target);
        if self.frame_pending {
            GlideSchedule::Idle
        } else {
            self.frame_pending = true;
            GlideSchedule::NextFrame
        }
    }

    /// One animation frame. Returns whether to request another.
    pub fn frame(&mut self, chart: Option<&Vela>) -> bool {
        self.frame_pending = false;
        let (Some(chart), Some(cur), Some(target)) = (chart, self.cur, self.target) else {
            self.stop();
            return false;
        };
        let (next, done) = follow_step(cur, target);
        self.cur = Some(next);
        chart.set_visible_range(next.to_visible());
        if done {
            self.stop();
            false
        } else {
            self.frame_pending = true;
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: fn(f64, f64) -> Range = |from, to| Range { from, to };

    #[test]
    fn zoom_targets_are_right_anchored() {
        let t = zoom_target(R(0.0, 1_000_000.0), ZOOM_IN);
        assert_eq!(t, R(200_000.0, 1_000_000.0));
        let out = zoom_target(R(0.0, 1_000_000.0), ZOOM_OUT);
        assert_eq!(out, R(-250_000.0, 1_000_000.0));
    }

    #[test]
    fn zoom_never_narrows_below_a_minute() {
        let t = zoom_target(R(0.0, 70_000.0), 0.1);
        assert_eq!(t.to - t.from, MIN_SPAN_MS);
        assert_eq!(t.to, 70_000.0);
    }

    #[test]
    fn follow_step_eases_a_quarter_of_the_way_and_snaps_when_close() {
        let (next, done) = follow_step(R(0.0, 1_000_000.0), R(400_000.0, 1_000_000.0));
        assert!(!done);
        assert_eq!(next, R(100_000.0, 1_000_000.0));
        // Both edges within a minute of the target: snap onto it.
        let target = R(400_000.0, 1_000_000.0);
        let (snapped, done) = follow_step(R(399_000.0, 1_000_000.0), target);
        assert!(done);
        assert_eq!(snapped, target);
    }

    #[test]
    fn following_converges_to_the_target() {
        let target = R(5_000_000.0, 9_000_000.0);
        let mut cur = R(0.0, 10_000_000.0);
        let mut steps = 0;
        loop {
            let (next, done) = follow_step(cur, target);
            cur = next;
            steps += 1;
            if done {
                break;
            }
            assert!(steps < 200, "must terminate");
        }
        assert_eq!(cur, target);
    }

    #[test]
    fn a_glider_without_a_chart_does_nothing() {
        let mut g = Glider::new();
        assert_eq!(g.zoom(None, ZOOM_IN), GlideSchedule::Idle);
        assert!(!g.active());
        assert!(!g.frame(None));
    }

    // `widget-glider.test.ts`: the glide eases ITS OWN range, so a target the chart would
    // clamp (its read-back never reaches it) still ends: the loop below never reads a chart.
    const HOUR: f64 = 3_600_000.0;

    fn glide(from: Range, target: Range) -> (Range, usize) {
        let mut cur = from;
        for steps in 1..=1000 {
            let (next, done) = follow_step(cur, target);
            cur = next;
            if done {
                return (cur, steps);
            }
        }
        panic!("the glide never ended");
    }

    #[test]
    fn zooming_toward_an_unreachable_span_terminates() {
        // Ten Ctrl+Up presses retarget from the pending target each time.
        let mut target = R(0.0, 200.0 * HOUR);
        for _ in 0..10 {
            target = zoom_target(target, ZOOM_IN);
        }
        let (end, steps) = glide(R(0.0, 200.0 * HOUR), target);
        assert!(steps < 200, "{steps} frames");
        assert_eq!(end, target);
        assert_eq!(target.to, 200.0 * HOUR, "right edge stays anchored");
    }

    #[test]
    fn a_reachable_glide_ends_on_the_halved_span_with_the_right_edge_pinned() {
        let target = zoom_target(R(100.0 * HOUR, 200.0 * HOUR), 0.5);
        let (end, _) = glide(R(100.0 * HOUR, 200.0 * HOUR), target);
        assert!((end.to - 200.0 * HOUR).abs() < 1e6);
        assert!(((end.to - end.from) - 50.0 * HOUR).abs() < 1e6);
    }

    #[test]
    fn stop_forgets_the_glide() {
        let mut g = Glider::new();
        g.cur = Some(R(0.0, 1.0));
        g.target = Some(R(0.0, 2.0));
        g.frame_pending = true;
        assert!(g.active());
        g.stop();
        assert!(!g.active());
        assert!(!g.frame(None), "no zombie frame after stop");
    }
}
