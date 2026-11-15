//! Candle level-of-detail tiers, keyed by bar spacing (px between bar centers).
//! Shared by both geometry backends so canvas2d and WebGL2 thin and aggregate at the
//! same zoom thresholds.
//!
//! - `Full`: high-low wick plus a body (enough room for a visible body).
//! - `Wick`: high-low stick only (too thin for a body).
//! - `Aggregate`: sub-pixel spacing. Bars sharing a pixel column collapse to one
//!   high-low stick, so draw cost stays bounded by screen width when zoomed far out
//!   (true LOD) instead of growing with the bar count.

use crate::indicators::classics::math::{js_max, js_round};
use crate::model::Ohlcv;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CandleTier {
    Full,
    Wick,
    Aggregate,
}

impl CandleTier {
    pub fn as_str(self) -> &'static str {
        match self {
            CandleTier::Full => "full",
            CandleTier::Wick => "wick",
            CandleTier::Aggregate => "aggregate",
        }
    }
}

/// Below this spacing a candle has no body (wick-only).
pub const CANDLE_BODY_MIN_SPACING: f64 = 3.0;
/// Wick line width in CSS px at wide zoom, rounded to whole device pixels per frame so it
/// stays crisp.
pub const CANDLE_WICK_W: f64 = 1.5;
/// Below this spacing bars are bucketed per pixel column (aggregated).
pub const CANDLE_AGG_MAX_SPACING: f64 = 1.0;

/// Wick line width (CSS px) for a given bar spacing. The body is about `spacing * 0.7`
/// wide (`half = floor(spacing * 0.7) / 2`), so a fixed wick reads as a solid body once
/// bars get tight. Cap the wick at the body's half-width and floor it at 1px: a crisp
/// ~1.5px stick at wide zoom that tapers to a 1px hair when zoomed out, never growing as
/// wide as the candle body. Shared by both backends so canvas2d and WebGL2 thin the wick
/// at the same thresholds.
pub fn wick_width(spacing: f64) -> f64 {
    let half_body = js_max(0.5, f64::floor(spacing * 0.7) / 2.0);
    half_body.clamp(1.0, CANDLE_WICK_W)
}

pub fn candle_tier(spacing: f64) -> CandleTier {
    if spacing < CANDLE_AGG_MAX_SPACING {
        CandleTier::Aggregate
    } else if spacing < CANDLE_BODY_MIN_SPACING {
        CandleTier::Wick
    } else {
        CandleTier::Full
    }
}

/// Snap a CSS-px Y coordinate to the device-pixel grid, the vertical counterpart of
/// [`candle_geometry`]'s X snapping, applied to candle body tops and bottoms and wick
/// ends. An edge on a whole device pixel rasterizes as one hard step; a fractional one
/// leaves a blended anti-aliasing row that reads as a darker rim on the body. The cost is
/// up to half a device pixel of true position, invisible at any zoom. Shared by both
/// backends so canvas2d and WebGL2 land candles on the same rows.
pub fn snap_y(y_css: f64, dpr: f64) -> f64 {
    js_round(y_css * dpr) / dpr
}

/// Wick and body layout of one candle, in CSS px, with every edge on the device-pixel
/// grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CandleGeometry {
    /// Wick left edge.
    pub wick_x: f64,
    /// Wick width.
    pub wick_w: f64,
    /// Body left edge; always centered on the wick.
    pub body_x: f64,
    /// Body width.
    pub body_w: f64,
    /// The shared wick and body centerline (the wick stroke's x in canvas2d).
    pub center: f64,
}

/// Snap one candle's wick and body to the device-pixel grid so the candle stays
/// SYMMETRIC: the wick column is snapped first, then the body is built around it with a
/// device-pixel width of the same parity as the wick's, so both share an exact center and
/// the body extends the same number of device pixels on each side of the wick. (Snapping
/// the two independently lets a 1px wick land on one half of the body, which reads as a
/// lopsided candle once zoomed out.) Body width is constant for a given spacing and dpr,
/// so the gap between candles is uniform to within one device pixel, the raster-grid
/// minimum. Shared by both backends so canvas2d and WebGL2 lay candles out identically.
pub fn candle_geometry(x_css: f64, spacing: f64, dpr: f64, body_scale: f64) -> CandleGeometry {
    let wick_dev = js_max(1.0, js_round(wick_width(spacing) * dpr));
    let wick_left_dev = js_round(x_css * dpr - wick_dev / 2.0);
    let mut body_dev = js_max(
        wick_dev,
        js_round(f64::floor(spacing * 0.7 * body_scale) * dpr),
    );
    if (body_dev - wick_dev) % 2.0 != 0.0 {
        body_dev += 1.0; // parity-match so the overhang splits evenly
    }
    let side_dev = (body_dev - wick_dev) / 2.0;
    CandleGeometry {
        wick_x: wick_left_dev / dpr,
        wick_w: wick_dev / dpr,
        body_x: (wick_left_dev - side_dev) / dpr,
        body_w: body_dev / dpr,
        center: (wick_left_dev + wick_dev / 2.0) / dpr,
    }
}

/// One aggregate-tier stick: a contiguous run of SAME-COLOR price coverage inside one
/// pixel column.
#[derive(Clone, Debug, PartialEq)]
pub struct AggregatedStick {
    /// The rounded CSS-px column shared by the stick's bars (canvas2d strokes at
    /// `x + 0.5`).
    pub x: f64,
    pub hi: f64,
    pub lo: f64,
    /// The resolved paint shared by every bar in the run: the grouping key.
    pub color: String,
}

/// A slot of a bar array that may hold no bar (the original's `OHLCV | undefined`).
pub trait BarSlot {
    fn bar(&self) -> Option<&Ohlcv>;
}

impl BarSlot for Ohlcv {
    fn bar(&self) -> Option<&Ohlcv> {
        Some(self)
    }
}

impl BarSlot for Option<Ohlcv> {
    fn bar(&self) -> Option<&Ohlcv> {
        self.as_ref()
    }
}

/// One in-progress coverage run of the current column (price space, index-tracked).
struct Coverage {
    lo: f64,
    hi: f64,
    color: String,
    /// The run's most recent bar: the column's paint order key.
    last_idx: usize,
}

fn flush(
    col: f64,
    runs: &mut Vec<Coverage>,
    y_of: &dyn Fn(f64) -> f64,
    out: &mut Vec<AggregatedStick>,
) {
    if runs.is_empty() {
        return;
    }
    // Coalesce same-color runs whose void is sub-pixel: it cannot render anyway.
    // Same-color runs are disjoint (overlaps merged on insert), so in `lo` order the
    // previous run of a color is the one just below.
    runs.sort_by(|a, b| a.lo.total_cmp(&b.lo));
    let mut merged: Vec<Coverage> = Vec::new();
    for next in runs.drain(..) {
        let prev = merged.iter().rposition(|m| m.color == next.color);
        match prev {
            Some(k) if (y_of(merged[k].hi) - y_of(next.lo)).abs() < 1.0 => {
                merged[k].hi = next.hi;
                if next.last_idx > merged[k].last_idx {
                    merged[k].last_idx = next.last_idx;
                }
            }
            _ => merged.push(next),
        }
    }
    merged.sort_by_key(|r| r.last_idx);
    for r in merged {
        out.push(AggregatedStick {
            x: col,
            hi: r.hi,
            lo: r.lo,
            color: r.color,
        });
    }
}

/// Aggregate-tier bucketing shared by both backends: bars whose centers round to the same
/// pixel column collapse into high-low sticks. Coverage is kept as the UNION of the
/// bars' true high-low ranges, one stick per contiguous run, instead of one min-to-max
/// span, so a PRICE GAP between bars sharing the column (the bars around an overnight
/// jump, once zoomed far out) stays a visible void instead of being painted over as a
/// solid connection. Runs whose separation is under one pixel (`y_of` measures it) merge
/// anyway: an invisible void is not worth a second stick, and ordinary contiguous data
/// keeps producing exactly one stick per column.
///
/// Runs are kept PER COLOR (`color_of` resolves each bar's paint: direction, `barcolor()`,
/// wick setting): bars of different colors never merge, so a down bar's long wick stays
/// the down color even when the up bar next to it shares the column. Merging them into
/// one first-open to last-close stick would recolor that wick by whichever bar closed
/// last. Within a column the sticks come out in order of their most recent bar, so where
/// runs of different colors overlap in price the latest bar paints on top, the same result
/// drawing the bars one by one would give.
///
/// Bars run over `[i0, i1]`; an index past the slice is a hole and is skipped (the
/// original's `undefined` entries).
pub fn aggregate_candle_columns<B: BarSlot>(
    bars: &[B],
    i0: usize,
    i1: usize,
    x_of: &dyn Fn(usize) -> f64,
    y_of: &dyn Fn(f64) -> f64,
    color_of: &dyn Fn(&Ohlcv) -> String,
) -> Vec<AggregatedStick> {
    let mut out = Vec::new();
    let mut col = f64::NAN;
    // The current column's runs, any color; typically length 1.
    let mut runs: Vec<Coverage> = Vec::new();
    for i in i0..=i1 {
        let Some(b) = bars.get(i).and_then(BarSlot::bar) else {
            continue;
        };
        if b.high <= b.low {
            continue;
        }
        let x = js_round(x_of(i));
        // `x != col` is also true for the first bar, where `col` is NaN.
        if x != col {
            flush(col, &mut runs, y_of, &mut out);
            col = x;
        }
        // Merge the bar's range into every overlapping run OF ITS COLOR (usually zero or
        // one).
        let color = color_of(b);
        let mut lo = b.low;
        let mut hi = b.high;
        let mut k = runs.len();
        while k > 0 {
            k -= 1;
            let r = &runs[k];
            if r.color != color || r.lo > hi || r.hi < lo {
                continue;
            }
            if r.lo < lo {
                lo = r.lo;
            }
            if r.hi > hi {
                hi = r.hi;
            }
            runs.remove(k);
        }
        // `i` is the newest bar, so it is the run's last.
        runs.push(Coverage {
            lo,
            hi,
            color,
            last_idx: i,
        });
    }
    flush(col, &mut runs, y_of, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // native candle LOD tiers

    #[test]
    fn draws_a_full_body_when_bars_are_wide_enough() {
        assert_eq!(candle_tier(CANDLE_BODY_MIN_SPACING), CandleTier::Full);
        assert_eq!(candle_tier(8.0), CandleTier::Full);
        assert_eq!(candle_tier(120.0), CandleTier::Full);
    }

    #[test]
    fn drops_the_body_between_the_aggregate_and_body_thresholds() {
        assert_eq!(candle_tier(CANDLE_AGG_MAX_SPACING), CandleTier::Wick);
        assert_eq!(candle_tier(2.0), CandleTier::Wick);
        assert_eq!(
            candle_tier(CANDLE_BODY_MIN_SPACING - 0.001),
            CandleTier::Wick
        );
    }

    #[test]
    fn aggregates_per_pixel_column_below_the_sub_pixel_threshold() {
        assert_eq!(
            candle_tier(CANDLE_AGG_MAX_SPACING - 0.001),
            CandleTier::Aggregate
        );
        assert_eq!(candle_tier(0.5), CandleTier::Aggregate);
        assert_eq!(candle_tier(0.0), CandleTier::Aggregate);
        assert_eq!(CandleTier::Aggregate.as_str(), "aggregate");
    }

    #[test]
    fn a_nan_spacing_draws_full_candles() {
        // Both comparisons are false, as in the original.
        assert_eq!(candle_tier(f64::NAN), CandleTier::Full);
    }

    #[test]
    fn thresholds_stay_ordered() {
        const { assert!(CANDLE_AGG_MAX_SPACING < CANDLE_BODY_MIN_SPACING) };
    }

    // candle wick width

    #[test]
    fn uses_the_full_crisp_width_when_bars_are_wide() {
        assert_eq!(wick_width(20.0), CANDLE_WICK_W);
        assert_eq!(wick_width(8.0), CANDLE_WICK_W);
        assert_eq!(wick_width(5.0), CANDLE_WICK_W); // halfBody = floor(3.5)/2 = 1.5 = cap
    }

    #[test]
    fn tapers_to_a_one_pixel_hair_when_zoomed_out() {
        assert_eq!(wick_width(2.0), 1.0);
        assert_eq!(wick_width(CANDLE_BODY_MIN_SPACING), 1.0);
        assert_eq!(wick_width(1.0), 1.0);
        assert_eq!(wick_width(0.5), 1.0);
    }

    #[test]
    fn never_exceeds_the_body_half_width_and_never_drops_below_one_pixel() {
        let mut s = 0.25;
        while s <= 40.0 {
            let w = wick_width(s);
            let half_body = js_max(0.5, f64::floor(s * 0.7) / 2.0);
            assert!(w >= 1.0);
            assert!(w <= CANDLE_WICK_W);
            assert!(w <= js_max(1.0, half_body));
            s += 0.25;
        }
    }

    // candle geometry (device-pixel snapping)

    const DPRS: [f64; 4] = [1.0, 1.5, 2.0, 3.0];
    const SPACINGS: [f64; 9] = [3.0, 3.5, 4.0, 4.7, 5.0, 6.3, 8.0, 12.5, 20.0];

    fn close(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn keeps_the_wick_exactly_centered_in_the_body_at_every_zoom_and_dpr() {
        for dpr in DPRS {
            for spacing in SPACINGS {
                let mut x = 0.0;
                while x < 40.0 {
                    let g = candle_geometry(x, spacing, dpr, 1.0);
                    let wick_center = g.wick_x + g.wick_w / 2.0;
                    let body_center = g.body_x + g.body_w / 2.0;
                    assert!(close(body_center, wick_center, 1e-9));
                    assert!(close(g.center, wick_center, 1e-9));
                    x += spacing / 3.0 + 0.137;
                }
            }
        }
    }

    #[test]
    fn lands_every_edge_on_the_device_pixel_grid() {
        for dpr in DPRS {
            for spacing in SPACINGS {
                let g = candle_geometry(7.3, spacing, dpr, 1.0);
                for edge in [g.wick_x, g.wick_x + g.wick_w, g.body_x, g.body_x + g.body_w] {
                    let dev = edge * dpr;
                    assert!((dev - js_round(dev)).abs() < 1e-6);
                }
            }
        }
    }

    #[test]
    fn gives_every_candle_the_same_body_width() {
        for dpr in DPRS {
            for spacing in SPACINGS {
                let mut widths: Vec<f64> = Vec::new();
                for i in 0..50 {
                    let x = f64::from(i) * spacing + 0.31;
                    let g = candle_geometry(x, spacing, dpr, 1.0);
                    if !widths.contains(&g.body_w) {
                        widths.push(g.body_w);
                    }
                    assert!((g.center - x).abs() <= 0.5 / dpr + 1e-6);
                }
                assert_eq!(widths.len(), 1);
            }
        }
    }

    #[test]
    fn never_makes_the_body_narrower_than_the_wick() {
        for dpr in DPRS {
            for spacing in SPACINGS {
                let g = candle_geometry(5.0, spacing, dpr, 1.0);
                assert!(g.body_w >= g.wick_w);
            }
        }
    }

    #[test]
    fn a_body_scale_narrows_the_body_but_keeps_it_centered() {
        let full = candle_geometry(100.0, 10.0, 1.0, 1.0);
        let slim = candle_geometry(100.0, 10.0, 1.0, 0.5);
        assert!(slim.body_w < full.body_w);
        assert!(close(slim.body_x + slim.body_w / 2.0, slim.center, 1e-9));
    }

    // candle-snap-y

    #[test]
    fn snap_y_produces_whole_device_pixels_at_fractional_dpr() {
        for y in [311.3, 38.8, 0.49, 662.01, 99.999] {
            for dpr in [1.0, 1.25, 1.5, 2.0, 2.625] {
                let dev = snap_y(y, dpr) * dpr;
                assert!(close(dev, js_round(dev), 1e-9), "y={y} dpr={dpr}");
            }
        }
    }

    #[test]
    fn snap_y_moves_a_coordinate_by_at_most_half_a_device_pixel() {
        for y in [311.3, 38.8, 0.49, 662.01] {
            for dpr in [1.0, 1.25, 1.5, 2.0] {
                assert!((snap_y(y, dpr) - y).abs() <= 0.5 / dpr + 1e-9);
            }
        }
    }

    #[test]
    fn snap_y_keeps_an_already_snapped_coordinate_unchanged() {
        assert_eq!(snap_y(311.2, 1.25), 311.2); // 311.2 x 1.25 = 389 exactly
        assert_eq!(snap_y(48.0, 1.25), 48.0);
    }

    #[test]
    fn snapped_edges_share_the_same_lattice_as_candle_geometry() {
        let dpr = 1.25;
        let g = candle_geometry(415.37, 9.0, dpr, 1.0);
        let top_dev = snap_y(123.456, dpr) * dpr;
        let left_dev = g.wick_x * dpr;
        assert!(close(top_dev, js_round(top_dev), 1e-9));
        assert!(close(left_dev, js_round(left_dev), 1e-9));
    }

    // aggregateCandleColumns

    fn bar(time: i64, open: f64, high: f64, low: f64, close: f64) -> Option<Ohlcv> {
        Some(Ohlcv::new(time, open, high, low, close, Some(0.0)))
    }

    /// 10 px per price unit, chart top at price 100.
    fn y_of(price: f64) -> f64 {
        (100.0 - price) * 10.0
    }

    fn one_column(_: usize) -> f64 {
        0.0
    }

    fn dir_color(b: &Ohlcv) -> String {
        if b.close >= b.open { "up" } else { "down" }.to_string()
    }

    fn stick(x: f64, hi: f64, lo: f64, color: &str) -> AggregatedStick {
        AggregatedStick {
            x,
            hi,
            lo,
            color: color.to_string(),
        }
    }

    fn aggregate(bars: &[Option<Ohlcv>]) -> Vec<AggregatedStick> {
        aggregate_candle_columns(bars, 0, bars.len() - 1, &one_column, &y_of, &dir_color)
    }

    #[test]
    fn collapses_overlapping_same_color_bars_into_one_stick() {
        let bars = [
            bar(1, 10.0, 12.0, 9.0, 11.0),
            bar(2, 11.0, 13.0, 10.0, 12.0),
            bar(3, 12.0, 14.0, 11.0, 13.0),
        ];
        assert_eq!(aggregate(&bars), vec![stick(0.0, 14.0, 9.0, "up")]);
    }

    #[test]
    fn keeps_a_price_gap_inside_a_column_as_a_void() {
        let bars = [
            bar(1, 10.0, 12.0, 9.0, 11.0),
            bar(2, 40.0, 42.0, 39.0, 41.0),
        ];
        assert_eq!(
            aggregate(&bars),
            vec![stick(0.0, 12.0, 9.0, "up"), stick(0.0, 42.0, 39.0, "up")]
        );
    }

    #[test]
    fn coalesces_a_sub_pixel_void() {
        // 0.05 price units = 0.5 px: below one pixel, the runs merge back into one.
        let bars = [
            bar(1, 10.0, 12.0, 9.0, 11.0),
            bar(2, 12.1, 12.15, 12.05, 12.1),
        ];
        assert_eq!(aggregate(&bars), vec![stick(0.0, 12.15, 9.0, "up")]);
    }

    #[test]
    fn a_later_bar_bridging_two_disjoint_same_color_runs_merges_them() {
        let bars = [
            bar(1, 10.0, 12.0, 9.0, 11.0),
            bar(2, 40.0, 42.0, 39.0, 41.0),
            bar(3, 25.0, 41.0, 10.0, 30.0),
        ];
        assert_eq!(aggregate(&bars), vec![stick(0.0, 42.0, 9.0, "up")]);
    }

    #[test]
    fn emits_one_stick_per_pixel_column_and_skips_holes_and_zero_range_bars() {
        let bars = [
            bar(1, 10.0, 12.0, 9.0, 11.0),
            bar(2, 11.0, 13.0, 10.0, 12.0),
            None,
            bar(4, 20.0, 20.0, 20.0, 20.0), // zero-range: skipped
            bar(5, 30.0, 32.0, 29.0, 31.0),
        ];
        // Two bars in column 0, the last one in column 1.
        let x_of = |i: usize| if i < 2 { 0.0 } else { 1.0 };
        let sticks = aggregate_candle_columns(&bars, 0, 4, &x_of, &y_of, &dir_color);
        assert_eq!(
            sticks,
            vec![stick(0.0, 13.0, 9.0, "up"), stick(1.0, 32.0, 29.0, "up")]
        );
    }

    #[test]
    fn sticks_carry_their_own_bars_color() {
        // Below the gap: a down bar (open 12, close 10). Above: an up bar (40 to 42).
        let bars = [
            bar(1, 12.0, 13.0, 9.0, 10.0),
            bar(2, 40.0, 43.0, 39.0, 42.0),
        ];
        let sticks = aggregate(&bars);
        assert_eq!(sticks.iter().find(|s| s.hi == 13.0).unwrap().color, "down");
        assert_eq!(sticks.iter().find(|s| s.hi == 43.0).unwrap().color, "up");
    }

    #[test]
    fn a_down_bars_long_wick_keeps_the_down_color_when_an_up_bar_shares_its_column() {
        let down = bar(1, 94.4, 94.5, 82.0, 94.1);
        let up = bar(2, 94.1, 100.2, 94.05, 100.1);
        assert_eq!(
            aggregate(&[down, up]),
            vec![
                stick(0.0, 94.5, 82.0, "down"),
                stick(0.0, 100.2, 94.05, "up")
            ]
        );
    }

    #[test]
    fn paints_a_columns_sticks_latest_bar_last() {
        let a = bar(1, 20.0, 30.0, 10.0, 15.0);
        let b = bar(2, 18.0, 22.0, 16.0, 21.0);
        let colors = |sticks: Vec<AggregatedStick>| -> Vec<String> {
            sticks.into_iter().map(|s| s.color).collect()
        };
        assert_eq!(colors(aggregate(&[a, b])), ["down", "up"]);
        assert_eq!(colors(aggregate(&[b, a])), ["up", "down"]);
    }

    #[test]
    fn groups_by_the_resolved_paint() {
        let bars = [
            bar(1, 10.0, 12.0, 9.0, 11.0),
            bar(2, 11.0, 13.0, 10.0, 12.0),
            bar(3, 12.0, 14.0, 11.0, 13.0),
        ];
        let tinted = |b: &Ohlcv| {
            if b.time == 2 {
                "#ff0".to_string()
            } else {
                dir_color(b)
            }
        };
        let sticks = aggregate_candle_columns(&bars, 0, 2, &one_column, &y_of, &tinted);
        assert_eq!(
            sticks,
            vec![stick(0.0, 13.0, 10.0, "#ff0"), stick(0.0, 14.0, 9.0, "up")]
        );
    }

    #[test]
    fn an_empty_or_out_of_range_window_yields_no_sticks() {
        let none: [Ohlcv; 0] = [];
        assert!(aggregate_candle_columns(&none, 0, 0, &one_column, &y_of, &dir_color).is_empty());
        let bars = [bar(1, 10.0, 12.0, 9.0, 11.0)];
        assert!(aggregate_candle_columns(&bars, 5, 9, &one_column, &y_of, &dir_color).is_empty());
        assert!(aggregate_candle_columns(&bars, 3, 1, &one_column, &y_of, &dir_color).is_empty());
    }

    #[test]
    fn nan_prices_do_not_panic() {
        // As in the original, a NaN bar is not "flat" (the comparison is false), so it
        // yields a NaN stick the canvas ignores; the finite bar is unaffected.
        let bars = [
            bar(1, f64::NAN, f64::NAN, f64::NAN, f64::NAN),
            bar(2, 10.0, 12.0, 9.0, 11.0),
        ];
        let sticks = aggregate(&bars);
        assert!(sticks.contains(&stick(0.0, 12.0, 9.0, "up")));
    }
}
