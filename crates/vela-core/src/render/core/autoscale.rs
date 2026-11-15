//! Per-pane price autoscale: the window a pane shows, from the data visible in
//! a range of bar indices. Backend-agnostic and cheap (only the visible slice
//! is scanned).

use crate::indicators::classics::math::js_max;
use crate::model::{IndicatorModel, Ohlcv, SeriesData, SeriesSpec};

use super::chart_config::{ChartMargins, DEFAULT_MARGINS};
use super::coordinate_system::PriceScale;

/// A min/max pair of prices (what visible drawings contribute to the scale).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValueExtent {
    pub min: f64,
    pub max: f64,
}

/// The per-model index offset when every model is whole-chart (offset 0).
pub fn no_offset(_id: &str) -> i64 {
    0
}

/// Running min/max over the finite values offered to it.
struct Extent {
    min: f64,
    max: f64,
}

impl Extent {
    fn new() -> Self {
        Self {
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
        }
    }

    fn consider(&mut self, v: Option<f64>) {
        if let Some(v) = v.filter(|v| v.is_finite()) {
            if v < self.min {
                self.min = v;
            }
            if v > self.max {
                self.max = v;
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.min == f64::INFINITY || self.max == f64::NEG_INFINITY
    }
}

/// `items[index]` for a signed index; `None` out of range (an `undefined` read in the original).
fn at<T>(items: &[T], index: i64) -> Option<&T> {
    usize::try_from(index).ok().and_then(|i| items.get(i))
}

/// The inclusive bar-index range `i0..=i1`; empty when `i1 < i0`.
fn indices(i0: i64, i1: i64) -> impl Iterator<Item = i64> {
    i0..=i1
}

/// Per-pane price window from the data visible in `[i0, i1]` (bar indices).
/// Considers candles (price pane), every value/candle series on the pane, and
/// price lines. `i1` may lie past the last bar: candles end there, series
/// points past it still count.
///
/// - `drawings`: the extent of the visible Pine drawings, which also expand the scale.
/// - `offset_of`: per-model index offset (chart bar index of the model's
///   anchor; [`no_offset`] = whole-chart).
/// - `margins`: top/bottom whitespace as percent of pane height (see [`ChartMargins`]).
#[allow(clippy::too_many_arguments)]
pub fn compute_pane_scale<'a>(
    models: impl IntoIterator<Item = &'a IndicatorModel>,
    bars: &[Ohlcv],
    include_candles: bool,
    i0: i64,
    i1: i64,
    drawings: Option<ValueExtent>,
    log: bool,
    offset_of: &dyn Fn(&str) -> i64,
    margins: ChartMargins,
) -> PriceScale {
    let mut ext = Extent::new();

    if include_candles {
        for i in indices(i0, i1) {
            if let Some(b) = at(bars, i) {
                ext.consider(Some(b.high));
                ext.consider(Some(b.low));
            }
        }
    }

    for model in models {
        let off = offset_of(&model.id);
        for s in &model.series {
            // force_overlay series render (and scale) on the PRICE pane, not their own:
            // the price pane folds them back in via overlay_series_range.
            if s.overlay {
                continue;
            }
            consider_series(s, i0, i1, off, &mut ext);
        }
        for pl in &model.price_lines {
            ext.consider(Some(pl.price));
        }
    }

    // Visible Pine drawings (boxes/lines/labels/...) also expand the scale.
    if let Some(d) = drawings {
        ext.consider(Some(d.min));
        ext.consider(Some(d.max));
    }

    if ext.is_empty() {
        return PriceScale::new(0.0, 1.0);
    }
    let (min, max) = (ext.min, ext.max);
    // A span of a few ulps is rounding noise, not a range: scale it like a flat series.
    if max - min <= js_max(min.abs(), max.abs()) * 1e-12 {
        let mid = (min + max) / 2.0;
        let pad = match mid.abs() * 0.1 {
            p if p == 0.0 || p.is_nan() => 1.0,
            p => p,
        };
        return PriceScale {
            min: mid - pad,
            max: mid + pad,
            log: Some(log && mid - pad > 0.0),
            invert: None,
        };
    }
    // The margins are shares of the pane's PIXEL height, so the data fills the middle
    // `1 - top - bottom`; over the data SPAN that is `top / content` above and
    // `bottom / content` below. `content` is floored so a degenerate pair can't divide by <= 0.
    let content = js_max(0.1, 1.0 - (margins.top + margins.bottom) / 100.0);
    let above = margins.top / 100.0 / content;
    let below = margins.bottom / 100.0 / content;
    // Logarithmic: apply the margins in log space (requires a positive range).
    if log && min > 0.0 {
        let lmin = min.ln();
        let lmax = max.ln();
        let lspan = lmax - lmin;
        return PriceScale {
            min: (lmin - lspan * below).exp(),
            max: (lmax + lspan * above).exp(),
            log: Some(true),
            invert: None,
        };
    }
    let span = max - min;
    PriceScale::new(min - span * below, max + span * above)
}

/// [`compute_pane_scale`] with the default margins and no model offsets.
pub fn compute_pane_scale_default<'a>(
    models: impl IntoIterator<Item = &'a IndicatorModel>,
    bars: &[Ohlcv],
    include_candles: bool,
    i0: i64,
    i1: i64,
) -> PriceScale {
    compute_pane_scale(
        models,
        bars,
        include_candles,
        i0,
        i1,
        None,
        false,
        &no_offset,
        DEFAULT_MARGINS,
    )
}

/// Fold one series' visible values (bars or points + base) into `ext`.
fn consider_series(s: &SeriesSpec, i0: i64, i1: i64, off: i64, ext: &mut Extent) {
    // A series with no on-chart presence (off the pane and off the price scale: a
    // legend/data-window-only readout, a hidden fill anchor) must not stretch the
    // window: nothing of it is there to keep in view.
    if !s.in_scale() {
        return;
    }
    match &s.data {
        SeriesData::Candle { bars, .. } => {
            for i in indices(i0, i1) {
                if let Some(b) = at(bars, i.saturating_sub(off)) {
                    ext.consider(Some(b.high));
                    ext.consider(Some(b.low));
                }
            }
        }
        SeriesData::LineLike {
            kind,
            points,
            style,
        } => {
            use crate::model::LineLikeKind::{Columns, Histogram};
            for i in indices(i0, i1) {
                ext.consider(at(points, i.saturating_sub(off)).and_then(|p| p.value));
            }
            // Histogram/columns grow from their base (default 0): include it so
            // an all-positive plot autoscales from a visible zero line.
            if matches!(kind, Histogram | Columns) {
                ext.consider(Some(style.base.unwrap_or(0.0)));
            } else if style.base.is_some() {
                ext.consider(style.base);
            }
        }
        SeriesData::Markers { .. } => {}
    }
}

/// Visible range of the `force_overlay` series across the given models: these
/// render on the PRICE pane (whatever pane their indicator routed to), so the
/// price pane's autoscale folds them in the way it folds force_overlay
/// drawings. `None` when there are none.
pub fn overlay_series_range<'a>(
    models: impl IntoIterator<Item = &'a IndicatorModel>,
    i0: i64,
    i1: i64,
    offset_of: &dyn Fn(&str) -> i64,
) -> Option<ValueExtent> {
    let mut ext = Extent::new();
    for model in models {
        let off = offset_of(&model.id);
        for s in &model.series {
            if s.overlay {
                consider_series(s, i0, i1, off, &mut ext);
            }
        }
    }
    (ext.min != f64::INFINITY).then_some(ValueExtent {
        min: ext.min,
        max: ext.max,
    })
}

/// Expand a computed scale so `above_px` / `below_px` EXTRA pixels exist beyond
/// its max / min: pixel headroom for fixed-size chrome anchored to the data
/// (trade-marker stacks). Exact: after expansion the original `[min, max]`
/// occupies `height_px - above - below` pixels. No-op when the margins don't
/// fit (degenerate pane) or aren't needed.
pub fn expand_scale_by_pixels(
    scale: &PriceScale,
    height_px: f64,
    above_px: f64,
    below_px: f64,
) -> PriceScale {
    if above_px <= 0.0 && below_px <= 0.0 {
        return *scale;
    }
    let content = height_px - above_px - below_px;
    if content.is_nan() || content < 8.0 {
        return *scale;
    }
    if scale.log_usable() {
        let lmin = scale.min.ln();
        let lmax = scale.max.ln();
        let per_px = (lmax - lmin) / content;
        return PriceScale {
            min: (lmin - below_px * per_px).exp(),
            max: (lmax + above_px * per_px).exp(),
            ..*scale
        };
    }
    if scale.max <= scale.min {
        return *scale;
    }
    let per_px = (scale.max - scale.min) / content;
    PriceScale {
        min: scale.min - below_px * per_px,
        max: scale.max + above_px * per_px,
        ..*scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CandleKind, LineLikeKind, LineLikeStyle, LineStyle, PaneHint, PriceLine, SeriesDisplay,
        SeriesPoint,
    };

    fn bar(time: i64, high: f64, low: f64) -> Ohlcv {
        Ohlcv::new(time, 100.0, high, low, 100.0, Some(1.0))
    }

    /// The two bars the margin tests scale.
    fn two_bars() -> Vec<Ohlcv> {
        vec![bar(0, 200.0, 100.0), bar(1, 180.0, 120.0)]
    }

    fn model(id: &str, series: Vec<SeriesSpec>) -> IndicatorModel {
        IndicatorModel {
            id: id.into(),
            title: id.into(),
            shorttitle: None,
            display_title: None,
            display_shorttitle: None,
            overlay: true,
            pane_hint: PaneHint::Price,
            native: None,
            legend: None,
            pane_axis: None,
            pane_id: Some("price".into()),
            own_scale: false,
            anchor_time: None,
            series,
            fills: vec![],
            backgrounds: vec![],
            price_lines: vec![],
            lines: vec![],
            boxes: vec![],
            labels: vec![],
            polylines: vec![],
            linefills: vec![],
            tables: vec![],
            bar_colors: vec![],
            trades: vec![],
            inputs: vec![],
            input_values: Default::default(),
            props: vec![],
            prop_values: Default::default(),
        }
    }

    fn line_series(id: &str, kind: LineLikeKind, values: &[Option<f64>]) -> SeriesSpec {
        SeriesSpec {
            id: id.into(),
            title: id.into(),
            display_title: None,
            pane_id: "price".into(),
            z_order: None,
            visible: None,
            display: None,
            overlay: false,
            data: SeriesData::LineLike {
                kind,
                points: values
                    .iter()
                    .enumerate()
                    .map(|(i, v)| SeriesPoint {
                        time: i as i64,
                        value: *v,
                        color: None,
                    })
                    .collect(),
                style: LineLikeStyle {
                    color: "#fff".into(),
                    width: 1.0,
                    line_style: LineStyle::Solid,
                    base: None,
                },
            },
        }
    }

    fn line(id: &str, values: &[f64]) -> SeriesSpec {
        let v: Vec<Option<f64>> = values.iter().copied().map(Some).collect();
        line_series(id, LineLikeKind::Line, &v)
    }

    fn scale_with(
        models: &[IndicatorModel],
        bars: &[Ohlcv],
        candles: bool,
        i0: i64,
        i1: i64,
        log: bool,
        margins: ChartMargins,
    ) -> PriceScale {
        compute_pane_scale(
            models, bars, candles, i0, i1, None, log, &no_offset, margins,
        )
    }

    fn close(a: f64, b: f64, eps: f64) {
        assert!((a - b).abs() < eps, "{a} vs {b}");
    }

    /// Pixel shares of the window above the data max and below the data min.
    fn shares(scale: &PriceScale, data_min: f64, data_max: f64) -> (f64, f64) {
        let span = scale.max - scale.min;
        ((scale.max - data_max) / span, (data_min - scale.min) / span)
    }

    #[test]
    fn defaults_to_ten_percent_above_and_below() {
        let s = compute_pane_scale_default([], &two_bars(), true, 0, 1);
        let (above, below) = shares(&s, 100.0, 200.0);
        close(above, 0.1, 1e-10);
        close(below, 0.1, 1e-10);
    }

    #[test]
    fn honors_the_configured_percentages_as_pixel_shares() {
        let s = scale_with(
            &[],
            &two_bars(),
            true,
            0,
            1,
            false,
            ChartMargins {
                top: 20.0,
                bottom: 8.0,
                right: 10.0,
            },
        );
        let (above, below) = shares(&s, 100.0, 200.0);
        close(above, 0.2, 1e-10);
        close(below, 0.08, 1e-10);
    }

    #[test]
    fn zero_margins_fit_the_data_edge_to_edge() {
        let s = scale_with(
            &[],
            &two_bars(),
            true,
            0,
            1,
            false,
            ChartMargins {
                top: 0.0,
                bottom: 0.0,
                right: 10.0,
            },
        );
        assert_eq!(s, PriceScale::new(100.0, 200.0));
    }

    #[test]
    fn applies_the_same_pixel_shares_in_log_space() {
        let s = scale_with(
            &[],
            &two_bars(),
            true,
            0,
            1,
            true,
            ChartMargins {
                top: 30.0,
                bottom: 20.0,
                right: 10.0,
            },
        );
        let (lmin, lmax) = (s.min.ln(), s.max.ln());
        let lspan = lmax - lmin;
        close((lmax - 200f64.ln()) / lspan, 0.3, 1e-10);
        close((100f64.ln() - lmin) / lspan, 0.2, 1e-10);
        assert_eq!(s.log, Some(true));
    }

    #[test]
    fn a_degenerate_margin_pair_cannot_divide_by_zero() {
        let s = scale_with(
            &[],
            &two_bars(),
            true,
            0,
            1,
            false,
            ChartMargins {
                top: 80.0,
                bottom: 80.0,
                right: 10.0,
            },
        );
        assert!(s.min.is_finite() && s.max.is_finite() && s.max > s.min);
    }

    #[test]
    fn treats_a_one_ulp_range_like_a_flat_series() {
        let v = 24.43;
        let sliver = [Ohlcv::new(
            0,
            v,
            v + 2.0 * f64::EPSILON * v,
            v,
            v,
            Some(1.0),
        )];
        let flat = [Ohlcv::new(0, v, v, v, v, Some(1.0))];
        let s = compute_pane_scale_default([], &sliver, true, 0, 0);
        let f = compute_pane_scale_default([], &flat, true, 0, 0);
        close(s.min, f.min, 1e-9);
        close(s.max, f.max, 1e-9);
        assert!(s.max - s.min > 1.0);
    }

    #[test]
    fn a_flat_series_gets_a_symmetric_pad_and_zero_pads_to_one() {
        let flat = [bar(0, 50.0, 50.0)];
        let s = compute_pane_scale_default([], &flat, true, 0, 0);
        assert_eq!((s.min, s.max), (45.0, 55.0));
        assert_eq!(s.log, Some(false));
        let zero = [bar(0, 0.0, 0.0)];
        let z = compute_pane_scale_default([], &zero, true, 0, 0);
        assert_eq!((z.min, z.max), (-1.0, 1.0));
        let on_log = compute_pane_scale(
            [],
            &flat,
            true,
            0,
            0,
            None,
            true,
            &no_offset,
            DEFAULT_MARGINS,
        );
        assert_eq!(on_log.log, Some(true));
    }

    #[test]
    fn no_data_yields_the_unit_window() {
        assert_eq!(
            compute_pane_scale_default([], &[], true, 0, 10),
            PriceScale::new(0.0, 1.0)
        );
        assert_eq!(
            compute_pane_scale_default([], &two_bars(), false, 0, 1),
            PriceScale::new(0.0, 1.0)
        );
    }

    #[test]
    fn nan_and_infinite_values_are_skipped() {
        let bars = [
            bar(0, f64::NAN, f64::NAN),
            bar(1, f64::INFINITY, 4.0),
            bar(2, 8.0, 2.0),
        ];
        let s = scale_with(
            &[],
            &bars,
            true,
            0,
            2,
            false,
            ChartMargins {
                top: 0.0,
                bottom: 0.0,
                right: 0.0,
            },
        );
        assert_eq!((s.min, s.max), (2.0, 8.0));
    }

    #[test]
    fn the_index_range_is_clipped_to_the_bars_and_may_run_past_them() {
        let bars = two_bars();
        let s = scale_with(&[], &bars, true, -5, 40, false, DEFAULT_MARGINS);
        let (above, below) = shares(&s, 100.0, 200.0);
        close(above, 0.1, 1e-10);
        close(below, 0.1, 1e-10);
        // an inverted range scans nothing
        assert_eq!(
            compute_pane_scale_default([], &bars, true, 3, 1),
            PriceScale::new(0.0, 1.0)
        );
    }

    #[test]
    fn series_points_past_the_last_bar_still_count() {
        let m = model("ind", vec![line("a", &[10.0, 11.0, 12.0, 13.0, 14.0])]);
        let bars = [bar(0, 11.0, 10.0), bar(1, 12.0, 10.0), bar(2, 12.0, 11.0)];
        let covered = scale_with(
            std::slice::from_ref(&m),
            &bars,
            true,
            0,
            4,
            false,
            ChartMargins {
                top: 0.0,
                bottom: 0.0,
                right: 0.0,
            },
        );
        assert_eq!(covered.max, 14.0);
        let clipped = scale_with(
            std::slice::from_ref(&m),
            &bars,
            true,
            0,
            2,
            false,
            ChartMargins {
                top: 0.0,
                bottom: 0.0,
                right: 0.0,
            },
        );
        assert_eq!(clipped.max, 12.0);
    }

    const NO_MARGINS: ChartMargins = ChartMargins {
        top: 0.0,
        bottom: 0.0,
        right: 0.0,
    };

    #[test]
    fn force_overlay_series_stay_out_of_their_own_pane_and_into_the_overlay_range() {
        let mut overlay = line("o", &[500.0, 600.0]);
        overlay.overlay = true;
        let m = model("ind", vec![line("a", &[1.0, 2.0]), overlay]);
        let s = scale_with(
            std::slice::from_ref(&m),
            &[],
            false,
            0,
            1,
            false,
            NO_MARGINS,
        );
        assert_eq!((s.min, s.max), (1.0, 2.0));
        let range = overlay_series_range(std::slice::from_ref(&m), 0, 1, &no_offset);
        assert_eq!(
            range,
            Some(ValueExtent {
                min: 500.0,
                max: 600.0
            })
        );
        let none = model("plain", vec![line("a", &[1.0, 2.0])]);
        assert_eq!(overlay_series_range([&none], 0, 1, &no_offset), None);
    }

    #[test]
    fn a_series_off_the_pane_and_the_scale_does_not_stretch_it() {
        let mut hidden = line("h", &[1000.0, 2000.0]);
        hidden.display = Some(SeriesDisplay {
            pane: Some(false),
            price_scale: Some(false),
            ..Default::default()
        });
        let m = model("ind", vec![line("a", &[1.0, 2.0]), hidden]);
        let s = scale_with(
            std::slice::from_ref(&m),
            &[],
            false,
            0,
            1,
            false,
            NO_MARGINS,
        );
        assert_eq!((s.min, s.max), (1.0, 2.0));
    }

    #[test]
    fn model_offsets_shift_which_series_values_are_visible() {
        let m = model("m", vec![line("a", &[100.0, 200.0, 300.0])]);
        // the model starts at chart bar 5: bars 5..=7 read values 0..=2
        let offset = |_: &str| 5;
        let s = compute_pane_scale([&m], &[], false, 5, 6, None, false, &offset, NO_MARGINS);
        assert_eq!((s.min, s.max), (100.0, 200.0));
        // a negative offset reaches further into the model's own points
        let back = |_: &str| -1;
        let s = compute_pane_scale([&m], &[], false, 0, 0, None, false, &back, NO_MARGINS);
        assert!(s.min < 200.0 && s.max > 200.0); // flat window, padded
        // before the model begins nothing is read
        let s = compute_pane_scale([&m], &[], false, 0, 4, None, false, &offset, NO_MARGINS);
        assert_eq!(s, PriceScale::new(0.0, 1.0));
    }

    #[test]
    fn histogram_and_columns_include_their_base_default_zero() {
        let hist = model(
            "h",
            vec![line_series(
                "h",
                LineLikeKind::Histogram,
                &[Some(5.0), Some(8.0)],
            )],
        );
        let s = scale_with(
            std::slice::from_ref(&hist),
            &[],
            false,
            0,
            1,
            false,
            NO_MARGINS,
        );
        assert_eq!((s.min, s.max), (0.0, 8.0));
        let cols = model(
            "c",
            vec![line_series("c", LineLikeKind::Columns, &[Some(5.0)])],
        );
        let s = scale_with(
            std::slice::from_ref(&cols),
            &[],
            false,
            0,
            0,
            false,
            NO_MARGINS,
        );
        assert_eq!((s.min, s.max), (0.0, 5.0));
        // a line ignores an absent base
        let plain = model(
            "l",
            vec![line_series(
                "l",
                LineLikeKind::Line,
                &[Some(5.0), Some(8.0)],
            )],
        );
        let s = scale_with(
            std::slice::from_ref(&plain),
            &[],
            false,
            0,
            1,
            false,
            NO_MARGINS,
        );
        assert_eq!((s.min, s.max), (5.0, 8.0));
    }

    #[test]
    fn an_explicit_base_widens_a_line_series_too() {
        let mut spec = line_series("l", LineLikeKind::Area, &[Some(5.0), Some(8.0)]);
        if let SeriesData::LineLike { style, .. } = &mut spec.data {
            style.base = Some(1.0);
        }
        let m = model("m", vec![spec]);
        let s = scale_with(
            std::slice::from_ref(&m),
            &[],
            false,
            0,
            1,
            false,
            NO_MARGINS,
        );
        assert_eq!((s.min, s.max), (1.0, 8.0));
    }

    #[test]
    fn gaps_in_a_line_series_are_skipped() {
        let m = model(
            "m",
            vec![line_series(
                "l",
                LineLikeKind::Line,
                &[None, Some(3.0), None],
            )],
        );
        let s = scale_with(
            std::slice::from_ref(&m),
            &[],
            false,
            0,
            2,
            false,
            NO_MARGINS,
        );
        close(s.min, 2.7, 1e-12);
        close(s.max, 3.3, 1e-12);
    }

    #[test]
    fn candle_series_and_price_lines_and_drawings_all_widen_the_window() {
        let candles = SeriesSpec {
            id: "c".into(),
            title: "c".into(),
            display_title: None,
            pane_id: "price".into(),
            z_order: None,
            visible: None,
            display: None,
            overlay: false,
            data: SeriesData::Candle {
                kind: CandleKind::Candle,
                bars: vec![bar(0, 30.0, 20.0)],
                style: None,
                bar_colors: None,
            },
        };
        let mut m = model("m", vec![candles]);
        m.price_lines.push(PriceLine {
            id: "p".into(),
            pane_id: "price".into(),
            price: 5.0,
            color: None,
            line_style: None,
            width: None,
            title: None,
            display_title: None,
        });
        let s = compute_pane_scale(
            [&m],
            &[],
            false,
            0,
            0,
            Some(ValueExtent {
                min: 10.0,
                max: 90.0,
            }),
            false,
            &no_offset,
            NO_MARGINS,
        );
        assert_eq!((s.min, s.max), (5.0, 90.0));
    }

    #[test]
    fn expand_scale_by_pixels_reserves_exact_headroom() {
        let scale = PriceScale::new(0.0, 100.0);
        let out = expand_scale_by_pixels(&scale, 200.0, 30.0, 20.0);
        // content 150px for a span of 100 => 2/3 price per px
        close(out.max, 100.0 + 30.0 * (100.0 / 150.0), 1e-9);
        close(out.min, -20.0 * (100.0 / 150.0), 1e-9);
        // the original window occupies exactly `height - above - below` pixels
        let per_px = (out.max - out.min) / 200.0;
        close(100.0 / per_px, 150.0, 1e-9);
    }

    #[test]
    fn expand_scale_by_pixels_is_a_no_op_when_not_needed_or_it_does_not_fit() {
        let scale = PriceScale::new(0.0, 100.0);
        assert_eq!(expand_scale_by_pixels(&scale, 200.0, 0.0, -3.0), scale);
        assert_eq!(expand_scale_by_pixels(&scale, 30.0, 20.0, 5.0), scale);
        assert_eq!(expand_scale_by_pixels(&scale, f64::NAN, 5.0, 5.0), scale);
        let flat = PriceScale::new(4.0, 4.0);
        assert_eq!(expand_scale_by_pixels(&flat, 200.0, 5.0, 5.0), flat);
    }

    #[test]
    fn expand_scale_by_pixels_works_in_log_space_and_keeps_the_flags() {
        let scale = PriceScale {
            log: Some(true),
            invert: Some(true),
            ..PriceScale::new(10.0, 1000.0)
        };
        let out = expand_scale_by_pixels(&scale, 220.0, 10.0, 10.0);
        assert_eq!((out.log, out.invert), (Some(true), Some(true)));
        let (lmin, lmax) = (out.min.ln(), out.max.ln());
        close(
            (lmax - lmin) / 220.0 * 200.0,
            1000f64.ln() - 10f64.ln(),
            1e-9,
        );
    }

    #[test]
    fn expand_scale_by_pixels_reserves_exactly_the_asked_pixels_linear() {
        let out = expand_scale_by_pixels(&PriceScale::new(0.0, 100.0), 100.0, 10.0, 10.0);
        close(out.min, -12.5, 1e-9);
        close(out.max, 112.5, 1e-9);
        // The original [0, 100] now occupies 80 of the 100 px.
        let per_px = (out.max - out.min) / 100.0;
        close(100.0 / per_px, 80.0, 1e-9);
    }

    #[test]
    fn expand_scale_by_pixels_log_scales_expand_in_log_space() {
        let scale = PriceScale {
            log: Some(true),
            ..PriceScale::new(1.0, 100.0)
        };
        let out = expand_scale_by_pixels(&scale, 100.0, 0.0, 50.0);
        close(out.max, 100.0, 1e-9);
        close(out.min, 0.01, 1e-9);
        assert_eq!(out.log, Some(true));
    }

    #[test]
    fn expand_scale_by_pixels_degenerate_inputs_are_a_no_op() {
        let scale = PriceScale::new(5.0, 10.0);
        assert_eq!(expand_scale_by_pixels(&scale, 100.0, 0.0, 0.0), scale);
        assert_eq!(expand_scale_by_pixels(&scale, 20.0, 10.0, 10.0), scale); // margins don't fit
        assert_eq!(
            expand_scale_by_pixels(&PriceScale::new(5.0, 5.0), 100.0, 10.0, 10.0),
            PriceScale::new(5.0, 5.0)
        );
    }

    /// A study model with its own plot (10..20) and a force_overlay plot (1000..2000).
    fn study_model() -> IndicatorModel {
        let mut forced = line("forced", &[1000.0, 2000.0, 1500.0]);
        forced.overlay = true;
        model("st", vec![line("own", &[10.0, 20.0, 15.0]), forced])
    }

    #[test]
    fn force_overlay_series_do_not_stretch_the_study_window() {
        let scale = compute_pane_scale_default([&study_model()], &[], false, 0, 2);
        assert!(scale.max < 1000.0);
        assert!(scale.min <= 10.0);
    }

    #[test]
    fn overlay_series_range_returns_exactly_the_overlay_range() {
        assert_eq!(
            overlay_series_range([&study_model()], 0, 2, &no_offset),
            Some(ValueExtent {
                min: 1000.0,
                max: 2000.0
            })
        );
    }

    #[test]
    fn overlay_series_range_is_none_when_nothing_is_flagged() {
        let mut m = study_model();
        for s in &mut m.series {
            s.overlay = false;
        }
        assert_eq!(overlay_series_range([&m], 0, 2, &no_offset), None);
    }

    #[test]
    fn overlay_series_range_honors_the_visible_window_and_the_anchor_offset() {
        // Anchor offset 1: the model's index 0 is the chart's bar 1.
        assert_eq!(
            overlay_series_range([&study_model()], 1, 1, &|_: &str| 1),
            Some(ValueExtent {
                min: 1000.0,
                max: 1000.0
            })
        );
    }

    fn only(surface: &str) -> SeriesDisplay {
        SeriesDisplay {
            pane: Some(surface == "pane"),
            price_scale: Some(surface == "priceScale"),
            legend: Some(surface == "legend"),
            data_window: Some(surface == "dataWindow"),
        }
    }

    fn painted() -> SeriesSpec {
        line("painted", &[10.0, 12.0, 11.0])
    }

    fn far(display: Option<SeriesDisplay>, visible: Option<bool>) -> SeriesSpec {
        let mut s = line("far", &[1000.0, 1000.0, 1000.0]);
        s.display = display;
        s.visible = visible;
        s
    }

    #[test]
    fn a_legend_or_data_window_only_plot_far_outside_the_painted_range_leaves_the_scale_alone() {
        let hidden_pair = SeriesDisplay {
            pane: Some(false),
            price_scale: Some(false),
            ..Default::default()
        };
        for display in [only("legend"), only("dataWindow"), hidden_pair] {
            let m = model("ind", vec![painted(), far(Some(display), None)]);
            let scale = compute_pane_scale_default([&m], &[], false, 0, 2);
            assert!(scale.max < 100.0);
        }
    }

    #[test]
    fn a_price_scale_only_or_pane_only_plot_keeps_its_value_in_view() {
        for surface in ["priceScale", "pane"] {
            let m = model("ind", vec![painted(), far(Some(only(surface)), None)]);
            assert!(compute_pane_scale_default([&m], &[], false, 0, 2).max > 1000.0);
        }
    }

    #[test]
    fn a_visible_false_fill_anchor_no_longer_stretches_the_scale() {
        let m = model("ind", vec![painted(), far(None, Some(false))]);
        assert!(compute_pane_scale_default([&m], &[], false, 0, 2).max < 100.0);
    }

    #[test]
    fn display_applies_to_candle_series_and_to_overlay_folding_alike() {
        let candles = SeriesSpec {
            data: SeriesData::Candle {
                kind: CandleKind::Candle,
                bars: (0..3).map(|i| bar(i, 1002.0, 998.0)).collect(),
                style: None,
                bar_colors: None,
            },
            display: Some(only("dataWindow")),
            ..line("c", &[])
        };
        let m = model("ind", vec![painted(), candles]);
        assert!(compute_pane_scale_default([&m], &[], false, 0, 2).max < 100.0);
        let mut forced = line("forced", &[1000.0, 2000.0, 1500.0]);
        forced.overlay = true;
        forced.display = Some(only("legend"));
        assert_eq!(
            overlay_series_range([&model("ind", vec![forced])], 0, 2, &no_offset),
            None
        );
    }

    fn chart_bars(n: i64) -> Vec<Ohlcv> {
        (0..n)
            .map(|i| bar(i, 100.0 + i as f64, 100.0 - i as f64))
            .collect()
    }

    #[test]
    fn a_model_that_starts_before_the_chart_head_skips_its_leading_values() {
        // The chart holds the newest 4 bars of a series the model computed over 10: the model's
        // first 6 values fall off the left edge, so offset = -6 and chart bar i reads value i+6.
        let chart: Vec<Ohlcv> = chart_bars(10).split_off(6);
        let m = model(
            "m",
            vec![line(
                "m:line",
                &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 1000.0, 1001.0, 1002.0, 1003.0],
            )],
        );
        let scale = compute_pane_scale(
            [&m],
            &chart,
            false,
            0,
            3,
            None,
            false,
            &|_: &str| -6,
            DEFAULT_MARGINS,
        );
        assert!(scale.min <= 1000.0);
        assert!(scale.max >= 1003.0);
        assert!(scale.min > 6.0); // the pre-head values (1..6) are NOT scanned
    }

    #[test]
    fn an_anchored_model_scans_the_values_under_the_visible_window() {
        // Chart of 10 bars; the model ran over the LAST 4 (anchor index 6) with values 1000..1003.
        let chart = chart_bars(10);
        let m = model("m", vec![line("m:line", &[1000.0, 1001.0, 1002.0, 1003.0])]);
        let at_six = |_: &str| 6;
        let scale = compute_pane_scale(
            [&m],
            &chart,
            false,
            6,
            9,
            None,
            false,
            &at_six,
            DEFAULT_MARGINS,
        );
        assert!(scale.min <= 1000.0);
        assert!(scale.max >= 1003.0);
        // BEFORE the model's anchor: no model values at all
        let empty = compute_pane_scale(
            [&m],
            &chart,
            false,
            0,
            5,
            None,
            false,
            &at_six,
            DEFAULT_MARGINS,
        );
        assert_eq!(empty, PriceScale::new(0.0, 1.0)); // the "nothing visible" fallback
    }

    #[test]
    fn the_unanchored_path_reads_the_window_only() {
        let chart = chart_bars(4);
        let m = model("m", vec![line("m:line", &[10.0, 20.0, 30.0, 40.0])]);
        let scale = compute_pane_scale_default([&m], &chart, false, 1, 2);
        assert!(scale.min <= 20.0);
        assert!(scale.max >= 30.0);
        assert!(scale.min > 10.0 && scale.max < 40.0); // bars 0/3 (10, 40) excluded from the window
    }

    #[test]
    fn a_projected_value_keeps_the_window_covering_it_and_candles_stop_at_the_last_bar() {
        let bars = [bar(0, 11.0, 9.0), bar(1, 11.0, 9.0), bar(2, 11.0, 9.0)];
        let m = model("m", vec![line("a", &[10.0, 11.0, 12.0, 13.0, 500.0])]);
        assert!(compute_pane_scale_default([&m], &bars, true, 0, 2).max < 100.0);
        assert!(compute_pane_scale_default([&m], &bars, true, 0, 4).max > 500.0);
        let mut forced = line("a", &[10.0, 11.0, 12.0, 13.0, 500.0]);
        forced.overlay = true;
        assert_eq!(
            overlay_series_range([&model("m", vec![forced])], 0, 4, &no_offset).map(|e| e.max),
            Some(500.0)
        );
    }
}
