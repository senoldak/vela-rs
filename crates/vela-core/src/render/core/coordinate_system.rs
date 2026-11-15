//! The ONE authoritative coordinate transform, shared by every layer (data
//! backend, canvas2d chrome, DOM overlays). Two decoupled axes:
//!
//! - X: a fractional **logical bar index** mapped to pixels by the viewport
//!   (`bar_spacing` + `right_offset`); bar TIME to logical is a separate mapping
//!   over the loaded bar times so time and spacing stay independent.
//! - Y: a per-pane **linear/log/percent price** mapped within that pane's bounds.
//!
//! Everything is in media (CSS) pixels; [`CoordinateSystem::to_bitmap`] converts
//! to device pixels for crisp drawing. Keeping a single instance is what keeps
//! all layers aligned.

use crate::indicators::classics::math::js_round;

use super::bar_time::{bar_time_to_logical, logical_to_bar_time, median_interval};
use super::viewport_state::{ViewportState, default_viewport};

/// A pane's price window (after autoscale + margins). `log` means a
/// logarithmic mapping; `invert` means the axis is flipped (high at the
/// bottom), stamped per frame from the pane. `None` reads as `false`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PriceScale {
    pub min: f64,
    pub max: f64,
    pub log: Option<bool>,
    pub invert: Option<bool>,
}

impl PriceScale {
    /// A linear, upright window.
    pub fn new(min: f64, max: f64) -> Self {
        Self {
            min,
            max,
            log: None,
            invert: None,
        }
    }

    pub fn is_log(&self) -> bool {
        self.log == Some(true)
    }

    pub fn is_inverted(&self) -> bool {
        self.invert == Some(true)
    }

    /// Whether the window can be mapped logarithmically (positive, non-empty).
    pub fn log_usable(&self) -> bool {
        self.is_log() && self.min > 0.0 && self.max > self.min
    }
}

/// A pane's vertical pixel extent within the chart's data area (media px).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PaneBounds {
    pub top: f64,
    pub height: f64,
}

/// A from/to pair of logical indices or epoch-ms times.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    pub from: f64,
    pub to: f64,
}

#[derive(Clone, Debug)]
pub struct CoordinateSystem {
    width_px: f64,
    data_height_px: f64,
    device_pixel_ratio: f64,
    viewport: ViewportState,
    pitch_scale: f64,
    times: Vec<i64>,
    interval_ms: f64,
}

impl Default for CoordinateSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl CoordinateSystem {
    pub fn new() -> Self {
        Self {
            width_px: 0.0,
            data_height_px: 0.0,
            device_pixel_ratio: 1.0,
            viewport: default_viewport(),
            pitch_scale: 1.0,
            times: Vec::new(),
            interval_ms: 0.0,
        }
    }

    // ── geometry inputs (owned by the renderer) ──

    pub fn set_size(&mut self, width: f64, data_height: f64, dpr: f64) {
        self.width_px = width;
        self.data_height_px = data_height;
        self.device_pixel_ratio = if dpr > 0.0 { dpr } else { 1.0 };
    }

    pub fn set_bars(&mut self, times: &[i64]) {
        self.times = times.to_vec();
        self.interval_ms = median_interval(&self.times);
    }

    /// Append one new bar time in O(1). The bar cadence is stable, so the
    /// median interval is NOT re-derived (a full re-sort per live tick would be
    /// O(n log n)). It is only derived lazily, via the robust median over the
    /// bars seen so far, while it is still unknown (cold start at fewer than
    /// two bars). `bar_count` reads the length, so the new bar is visible
    /// immediately.
    pub fn append_bar(&mut self, time: i64) {
        self.times.push(time);
        if self.interval_ms <= 0.0 && self.times.len() >= 2 {
            self.interval_ms = median_interval(&self.times);
        }
    }

    pub fn set_viewport(&mut self, viewport: ViewportState) {
        self.viewport = viewport;
    }

    pub fn viewport(&self) -> ViewportState {
        self.viewport
    }

    /// Spacing multiplier applied to the center-to-center pixel PITCH between
    /// adjacent bars (and hence the crosshair's snap step), independent of the
    /// zoom (`bar_spacing`). `1` = default; `>1` spreads bars apart with real
    /// gaps; `<1` tightens them. It changes only where bars SIT
    /// ([`logical_to_x`](Self::logical_to_x) / [`x_to_logical`](Self::x_to_logical)),
    /// never their raw zoom value, so candle bodies keep their width: the
    /// extra pitch becomes gap. Body width tracks
    /// [`body_spacing`](Self::body_spacing), so a `<1` multiplier shrinks
    /// bodies to fit instead of overlapping.
    pub fn set_pitch_scale(&mut self, scale: f64) {
        self.pitch_scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
    }

    pub fn spacing_scale(&self) -> f64 {
        self.pitch_scale
    }

    /// Effective center-to-center pixel pitch between adjacent bars (zoom
    /// times spacing multiplier).
    pub fn px_per_bar(&self) -> f64 {
        self.viewport.bar_spacing * self.pitch_scale
    }

    /// Pixel basis for element (candle/bar/column) BODY width. Kept at the raw
    /// zoom pitch so bodies keep their width while the multiplier only adds
    /// gap; capped at the effective pitch so a `<1` multiplier tightens bodies
    /// to fit rather than overlapping the neighbours.
    pub fn body_spacing(&self) -> f64 {
        self.viewport.bar_spacing * self.pitch_scale.min(1.0)
    }

    pub fn width(&self) -> f64 {
        self.width_px
    }

    pub fn height(&self) -> f64 {
        self.data_height_px
    }

    pub fn dpr(&self) -> f64 {
        self.device_pixel_ratio
    }

    pub fn bar_count(&self) -> usize {
        self.times.len()
    }

    /// The loaded bar times (epoch ms).
    pub fn times(&self) -> &[i64] {
        &self.times
    }

    pub fn bar_interval(&self) -> f64 {
        self.interval_ms
    }

    /// The logical index that sits exactly at the right pixel edge of the chart.
    pub fn right_edge_logical(&self) -> f64 {
        self.times.len() as f64 - 1.0 + self.viewport.right_offset
    }

    // ── X axis: logical bar index <-> pixel (via the effective pitch) ──

    pub fn logical_to_x(&self, logical: f64) -> f64 {
        self.width_px - (self.right_edge_logical() - logical) * self.px_per_bar()
    }

    pub fn x_to_logical(&self, x: f64) -> f64 {
        self.right_edge_logical() - (self.width_px - x) / self.px_per_bar()
    }

    // ── bar time <-> logical <-> pixel ──

    pub fn time_to_logical(&self, ms: f64) -> f64 {
        bar_time_to_logical(ms, &self.times, self.interval_ms)
    }

    pub fn logical_to_time(&self, logical: f64) -> f64 {
        logical_to_bar_time(logical, &self.times, self.interval_ms)
    }

    pub fn time_to_x(&self, ms: f64) -> f64 {
        self.logical_to_x(self.time_to_logical(ms))
    }

    // ── Y axis: price <-> pixel (per pane scale + bounds; linear or logarithmic) ──
    //   `t` is the fraction from min (0) to max (1); a normal axis puts max at
    //   the top (`1 - t`), an inverted axis puts it at the bottom (`t`).

    pub fn price_to_y(&self, price: f64, scale: &PriceScale, bounds: &PaneBounds) -> f64 {
        if scale.log_usable() && price > 0.0 {
            let lo = scale.min.ln();
            let t = (price.ln() - lo) / (scale.max.ln() - lo);
            return bounds.top + bounds.height * if scale.is_inverted() { t } else { 1.0 - t };
        }
        let span = scale.max - scale.min;
        if span <= 0.0 {
            return bounds.top + bounds.height / 2.0;
        }
        let t = (price - scale.min) / span;
        bounds.top + bounds.height * if scale.is_inverted() { t } else { 1.0 - t }
    }

    pub fn y_to_price(&self, y: f64, scale: &PriceScale, bounds: &PaneBounds) -> f64 {
        if bounds.height <= 0.0 {
            return scale.min;
        }
        // 0 at the top edge ... 1 at the bottom
        let u = (y - bounds.top) / bounds.height;
        let t = if scale.is_inverted() { u } else { 1.0 - u };
        if scale.log_usable() {
            let lo = scale.min.ln();
            return (lo + t * (scale.max.ln() - lo)).exp();
        }
        scale.min + (scale.max - scale.min) * t
    }

    // ── visible ranges ──

    pub fn visible_logical_range(&self) -> Range {
        Range {
            from: self.x_to_logical(0.0),
            to: self.x_to_logical(self.width_px),
        }
    }

    pub fn visible_time_range(&self) -> Range {
        let r = self.visible_logical_range();
        Range {
            from: self.logical_to_time(r.from),
            to: self.logical_to_time(r.to),
        }
    }

    // ── media (CSS px) <-> bitmap (device px) ──

    pub fn to_bitmap(&self, media_px: f64) -> f64 {
        js_round(media_px * self.device_pixel_ratio)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_dpr(dpr: f64) -> CoordinateSystem {
        let mut cs = CoordinateSystem::new();
        cs.set_size(800.0, 200.0, dpr);
        cs.set_bars(&[1000, 2000, 3000, 4000, 5000]); // 5 bars, interval 1000ms
        cs.set_viewport(ViewportState {
            bar_spacing: 100.0,
            right_offset: 1.0,
        });
        cs
    }

    fn setup() -> CoordinateSystem {
        setup_dpr(1.0)
    }

    fn close(a: f64, b: f64, eps: f64) {
        assert!((a - b).abs() < eps, "{a} vs {b}");
    }

    #[test]
    fn maps_logical_bar_index_to_pixels_via_bar_spacing_and_right_offset() {
        let cs = setup();
        assert_eq!(cs.bar_count(), 5);
        assert_eq!(cs.right_edge_logical(), 5.0); // (n-1) + rightOffset = 4 + 1
        assert_eq!(cs.logical_to_x(5.0), 800.0); // right edge
        assert_eq!(cs.logical_to_x(4.0), 700.0); // last bar, 1 bar-width from the edge
        assert_eq!(cs.logical_to_x(0.0), 300.0);
    }

    #[test]
    fn x_inverts_exactly() {
        let cs = setup();
        for l in [-3.0, 0.0, 2.5, 4.0, 7.0] {
            close(cs.x_to_logical(cs.logical_to_x(l)), l, 1e-9);
        }
        assert_eq!(cs.x_to_logical(800.0), 5.0);
        assert_eq!(cs.x_to_logical(300.0), 0.0);
        assert_eq!(cs.x_to_logical(0.0), -3.0);
    }

    #[test]
    fn reports_the_visible_logical_range_from_the_pixel_edges() {
        assert_eq!(
            setup().visible_logical_range(),
            Range {
                from: -3.0,
                to: 5.0
            }
        );
    }

    #[test]
    fn pitch_scale_defaults_to_one() {
        let cs = setup();
        assert_eq!(cs.spacing_scale(), 1.0);
        assert_eq!(cs.px_per_bar(), 100.0);
        assert_eq!(cs.body_spacing(), 100.0);
        assert_eq!(cs.logical_to_x(4.0), 700.0);
    }

    #[test]
    fn pitch_scale_spreads_bars_while_the_right_edge_stays_pinned() {
        let mut cs = setup();
        cs.set_pitch_scale(2.0);
        assert_eq!(cs.px_per_bar(), 200.0);
        assert_eq!(cs.logical_to_x(5.0), 800.0);
        assert_eq!(cs.logical_to_x(4.0), 600.0);
        assert_eq!(cs.logical_to_x(3.0), 400.0);
    }

    #[test]
    fn x_to_logical_stays_the_exact_inverse_under_a_pitch_scale() {
        let mut cs = setup();
        cs.set_pitch_scale(2.5);
        for l in [-3.0, 0.0, 2.5, 4.0, 7.0] {
            close(cs.x_to_logical(cs.logical_to_x(l)), l, 1e-9);
        }
        assert_eq!(cs.px_per_bar(), 250.0);
    }

    #[test]
    fn body_width_keeps_the_raw_pitch_above_one_and_shrinks_below_one() {
        let mut cs = setup();
        cs.set_pitch_scale(3.0);
        assert_eq!(cs.body_spacing(), 100.0);
        cs.set_pitch_scale(0.4);
        close(cs.body_spacing(), 40.0, 1e-9);
        close(cs.px_per_bar(), 40.0, 1e-9);
    }

    #[test]
    fn a_non_positive_pitch_scale_falls_back_to_one() {
        let mut cs = setup();
        cs.set_pitch_scale(0.0);
        assert_eq!(cs.spacing_scale(), 1.0);
        cs.set_pitch_scale(-2.0);
        assert_eq!(cs.spacing_scale(), 1.0);
        cs.set_pitch_scale(f64::NAN);
        assert_eq!(cs.spacing_scale(), 1.0);
        cs.set_pitch_scale(f64::INFINITY);
        assert_eq!(cs.spacing_scale(), 1.0);
    }

    #[test]
    fn latest_bar_is_off_screen_exactly_when_right_offset_is_negative() {
        let latest_bar_x = |right_offset: f64| {
            let mut cs = CoordinateSystem::new();
            cs.set_size(800.0, 200.0, 1.0);
            cs.set_bars(&[1000, 2000, 3000, 4000, 5000]);
            cs.set_viewport(ViewportState {
                bar_spacing: 100.0,
                right_offset,
            });
            cs.logical_to_x(cs.bar_count() as f64 - 1.0)
        };
        assert!(latest_bar_x(-1.0) > 800.0);
        assert!(latest_bar_x(-0.001) > 800.0);
        assert_eq!(latest_bar_x(0.0), 800.0);
        assert!(latest_bar_x(6.0) < 800.0);
    }

    #[test]
    fn time_maps_to_logical_interpolating_and_extrapolating() {
        let cs = setup();
        assert_eq!(cs.time_to_logical(1000.0), 0.0);
        assert_eq!(cs.time_to_logical(5000.0), 4.0);
        assert_eq!(cs.time_to_logical(3000.0), 2.0);
        close(cs.time_to_logical(2500.0), 1.5, 1e-9);
        close(cs.time_to_logical(500.0), -0.5, 1e-9);
        close(cs.time_to_logical(6000.0), 5.0, 1e-9);
    }

    #[test]
    fn logical_inverts_back_to_time() {
        let cs = setup();
        assert_eq!(cs.logical_to_time(0.0), 1000.0);
        assert_eq!(cs.logical_to_time(4.0), 5000.0);
        assert_eq!(cs.logical_to_time(2.0), 3000.0);
        close(cs.logical_to_time(-0.5), 500.0, 1e-6);
        close(cs.logical_to_time(5.0), 6000.0, 1e-6);
        assert_eq!(
            cs.visible_time_range(),
            Range {
                from: cs.logical_to_time(-3.0),
                to: 6000.0
            }
        );
    }

    #[test]
    fn append_bar_grows_the_series_without_re_deriving_the_interval() {
        let mut cs = setup();
        assert_eq!(cs.bar_count(), 5);
        cs.append_bar(6000);
        assert_eq!(cs.bar_count(), 6);
        assert_eq!(cs.logical_to_time(5.0), 6000.0);
        assert_eq!(cs.bar_interval(), 1000.0);
        assert_eq!(cs.time_to_logical(6000.0), 5.0);
    }

    #[test]
    fn append_bar_lazily_establishes_the_interval_on_a_cold_start() {
        let mut cs = CoordinateSystem::new();
        cs.set_size(800.0, 200.0, 1.0);
        cs.set_bars(&[]);
        assert_eq!(cs.bar_interval(), 0.0);
        cs.append_bar(1000);
        assert_eq!(cs.bar_count(), 1);
        assert_eq!(cs.bar_interval(), 0.0);
        cs.append_bar(3000);
        assert_eq!(cs.bar_count(), 2);
        assert_eq!(cs.bar_interval(), 2000.0);
    }

    #[test]
    fn set_bars_owns_its_buffer() {
        let mut cs = CoordinateSystem::new();
        let mut times = vec![1000, 2000];
        cs.set_bars(&times);
        times.push(3000);
        assert_eq!(cs.bar_count(), 2);
    }

    #[test]
    fn maps_price_within_scale_and_bounds_with_an_inverted_y() {
        let cs = setup();
        let scale = PriceScale::new(0.0, 10.0);
        let bounds = PaneBounds {
            top: 0.0,
            height: 200.0,
        };
        assert_eq!(cs.price_to_y(10.0, &scale, &bounds), 0.0);
        assert_eq!(cs.price_to_y(0.0, &scale, &bounds), 200.0);
        assert_eq!(cs.price_to_y(5.0, &scale, &bounds), 100.0);
        for p in [0.0, 2.5, 7.5, 10.0] {
            close(
                cs.y_to_price(cs.price_to_y(p, &scale, &bounds), &scale, &bounds),
                p,
                1e-9,
            );
        }
    }

    #[test]
    fn rounds_media_px_to_device_px_via_dpr() {
        assert_eq!(setup_dpr(1.0).to_bitmap(100.0), 100.0);
        assert_eq!(setup_dpr(1.25).to_bitmap(100.0), 125.0);
        assert_eq!(setup_dpr(1.5).to_bitmap(101.0), 152.0);
        assert_eq!(setup_dpr(2.0).to_bitmap(100.0), 200.0);
        // half-pixels round toward +infinity, like Math.round
        assert_eq!(setup_dpr(1.0).to_bitmap(-0.5), 0.0);
        assert_eq!(setup_dpr(1.0).to_bitmap(2.5), 3.0);
    }

    #[test]
    fn a_non_positive_dpr_falls_back_to_one() {
        assert_eq!(setup_dpr(0.0).dpr(), 1.0);
        assert_eq!(setup_dpr(-2.0).dpr(), 1.0);
    }

    #[test]
    fn maps_price_logarithmically_when_the_scale_is_log() {
        let cs = setup();
        let scale = PriceScale {
            log: Some(true),
            ..PriceScale::new(10.0, 1000.0)
        };
        let bounds = PaneBounds {
            top: 0.0,
            height: 200.0,
        };
        assert_eq!(cs.price_to_y(1000.0, &scale, &bounds), 0.0);
        assert_eq!(cs.price_to_y(10.0, &scale, &bounds), 200.0);
        close(cs.price_to_y(100.0, &scale, &bounds), 100.0, 1e-9);
        for p in [10.0, 31.6, 100.0, 316.0, 1000.0] {
            close(
                cs.y_to_price(cs.price_to_y(p, &scale, &bounds), &scale, &bounds),
                p,
                1e-6,
            );
        }
    }

    #[test]
    fn falls_back_to_linear_when_a_log_scale_has_a_non_positive_min() {
        let cs = setup();
        let scale = PriceScale {
            log: Some(true),
            ..PriceScale::new(-5.0, 5.0)
        };
        let bounds = PaneBounds {
            top: 0.0,
            height: 200.0,
        };
        assert_eq!(cs.price_to_y(0.0, &scale, &bounds), 100.0);
    }

    #[test]
    fn a_non_positive_price_on_a_log_scale_maps_linearly_without_nan() {
        let cs = setup();
        let scale = PriceScale {
            log: Some(true),
            ..PriceScale::new(10.0, 1000.0)
        };
        let bounds = PaneBounds {
            top: 0.0,
            height: 200.0,
        };
        assert!(cs.price_to_y(0.0, &scale, &bounds).is_finite());
    }

    #[test]
    fn a_flat_window_maps_to_the_pane_middle_and_a_zero_height_pane_to_the_min() {
        let cs = setup();
        let flat = PriceScale::new(5.0, 5.0);
        let bounds = PaneBounds {
            top: 20.0,
            height: 100.0,
        };
        assert_eq!(cs.price_to_y(5.0, &flat, &bounds), 70.0);
        let zero = PaneBounds {
            top: 0.0,
            height: 0.0,
        };
        assert_eq!(cs.y_to_price(10.0, &PriceScale::new(3.0, 9.0), &zero), 3.0);
    }

    #[test]
    fn price_to_y_flips_for_an_inverted_axis_and_y_to_price_round_trips() {
        let cs = CoordinateSystem::new();
        let bounds = PaneBounds {
            top: 0.0,
            height: 100.0,
        };
        let normal = PriceScale::new(0.0, 100.0);
        let inverted = PriceScale {
            invert: Some(true),
            ..PriceScale::new(0.0, 100.0)
        };
        close(cs.price_to_y(100.0, &normal, &bounds), 0.0, 1e-9);
        close(cs.price_to_y(0.0, &normal, &bounds), 100.0, 1e-9);
        close(cs.price_to_y(100.0, &inverted, &bounds), 100.0, 1e-9);
        close(cs.price_to_y(0.0, &inverted, &bounds), 0.0, 1e-9);
        close(cs.y_to_price(0.0, &inverted, &bounds), 0.0, 1e-9);
        close(cs.y_to_price(100.0, &inverted, &bounds), 100.0, 1e-9);
        close(
            cs.y_to_price(cs.price_to_y(42.0, &inverted, &bounds), &inverted, &bounds),
            42.0,
            1e-9,
        );
    }

    #[test]
    fn an_empty_or_single_bar_series_still_maps() {
        let mut cs = CoordinateSystem::new();
        cs.set_size(100.0, 50.0, 1.0);
        assert_eq!(cs.bar_count(), 0);
        assert_eq!(cs.right_edge_logical(), -1.0 + 6.0);
        cs.set_bars(&[1000]);
        assert_eq!(cs.right_edge_logical(), 6.0);
        assert_eq!(cs.time_to_logical(1000.0), 0.0);
    }

    #[test]
    fn nan_prices_stay_nan() {
        let cs = setup();
        let bounds = PaneBounds {
            top: 0.0,
            height: 100.0,
        };
        assert!(
            cs.price_to_y(f64::NAN, &PriceScale::new(0.0, 1.0), &bounds)
                .is_nan()
        );
    }
}
