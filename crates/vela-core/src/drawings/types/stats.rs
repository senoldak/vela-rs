//! Data-driven statistical tools: the linear-regression channel, the anchored
//! VWAP and the fixed-range volume profile. Each fits against the chart's bars
//! (through [`Projector::bars_in_range`]) and degrades to an anchors-only
//! fallback when the renderer serves none.

use std::any::Any;
use std::cell::Cell;

use serde_json::{Value, json};

use crate::drawings::drawing::{
    AnchorSchema, AnchorSlot, Bounds, Drawing, DrawingBase, DrawingInit, Props, Span,
    apply_setting_default,
};
use crate::drawings::geometry::{FreeAxis, Projector};
use crate::drawings::hittest::{dist_to_segment, point_in_polygon};
use crate::drawings::registry::{DrawingTypeMeta, svg24};
use crate::drawings::schema::{
    FieldGroup, FieldKind, FieldOption, SettingsField, SettingsSchema, line_style_options,
};
use crate::drawings::style::DrawingStyle;
use crate::i18n::msg;
use crate::indicators::classics::math::{js_max, js_min, js_round};
use crate::model::{LineStyle, Ohlcv};
use crate::palette::{ACCENT, BEARISH, BULLISH, INFO, NEUTRAL};

// ── shared props helpers ──

/// Overwrite `out` with a string prop; a missing or mistyped entry leaves the default.
fn prop_str(props: &Props, key: &str, out: &mut String) {
    if let Some(s) = props.get(key).and_then(Value::as_str) {
        *out = s.to_string();
    }
}

fn prop_f64(props: &Props, key: &str, out: &mut f64) {
    if let Some(v) = props.get(key).and_then(Value::as_f64) {
        *out = v;
    }
}

fn prop_bool(props: &Props, key: &str, out: &mut bool) {
    if let Some(v) = props.get(key).and_then(Value::as_bool) {
        *out = v;
    }
}

fn prop_line_style(props: &Props, key: &str, out: &mut LineStyle) {
    match props.get(key).and_then(Value::as_str) {
        Some("solid") => *out = LineStyle::Solid,
        Some("dashed") => *out = LineStyle::Dashed,
        Some("dotted") => *out = LineStyle::Dotted,
        _ => {}
    }
}

fn line_style_json(style: LineStyle) -> Value {
    Value::String(
        match style {
            LineStyle::Solid => "solid",
            LineStyle::Dashed => "dashed",
            LineStyle::Dotted => "dotted",
        }
        .to_string(),
    )
}

/// Apply a `<prefix>.<key>` setting by editing the props bag (the original
/// assigns onto its style object, whose fields ARE the props); every other
/// path takes the shared default.
fn apply_prefixed_setting<D: Drawing>(d: &mut D, prefix: &str, path: &str, value: &Value) {
    match path
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('.'))
    {
        Some(key) => {
            let mut props = d.write_props().unwrap_or_default();
            props.insert(key.to_string(), value.clone());
            d.read_props(&props);
        }
        None => apply_setting_default(d, path, value),
    }
}

/// The fallback handles: the raw anchors, so a tool with no data stays grabbable.
fn anchor_handles(base: &DrawingBase, proj: &dyn Projector) -> Vec<(f64, f64)> {
    base.anchors_px(proj).unwrap_or_default()
}

/// The price span of the first two anchors (the fallback autoscale range).
fn two_anchor_span(base: &DrawingBase) -> Option<Span> {
    let a = base.anchors.first()?;
    let b = base.anchors.get(1)?;
    Some(Span {
        min: js_min(a.price, b.price),
        max: js_max(a.price, b.price),
    })
}

/// The two time anchors `(from, to)` ordered, or `None` without both.
fn time_range(base: &DrawingBase) -> Option<(f64, f64)> {
    let a = base.anchors.first()?;
    let b = base.anchors.get(1)?;
    Some((js_min(a.time, b.time), js_max(a.time, b.time)))
}

fn horizontal_pair_schema() -> AnchorSchema {
    // Two points bound the time range; the tool ignores their price (data-driven),
    // so handles are constrained to horizontal moves.
    AnchorSchema {
        min: 2,
        max: 2,
        slots: vec![
            AnchorSlot::new("start", FreeAxis::X),
            AnchorSlot::new("end", FreeAxis::X),
        ],
    }
}

// ───────────────────────── regression channel ─────────────────────────

/// Standard-deviation multiplier for the channel half-width (a ±2σ band around the fit line).
const DEVIATIONS: f64 = 2.0;

/// One bar the regression fits against (only `time` + `close` are used).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FitBar {
    pub time: f64,
    pub close: f64,
}

impl From<&Ohlcv> for FitBar {
    fn from(bar: &Ohlcv) -> Self {
        Self {
            time: bar.time as f64,
            close: bar.close,
        }
    }
}

/// The least-squares result over the bars in range: the fit line endpoints
/// (price), the band half-width, and the goodness-of-fit R² (0..1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegressionFit {
    /// First / last bar time in range (the channel's horizontal extent).
    pub t0: f64,
    pub t1: f64,
    /// Midline (fit) price at `t0` / `t1`.
    pub mid0: f64,
    pub mid1: f64,
    /// Channel half-width in price (`DEVIATIONS` × residual σ).
    pub dev: f64,
    /// Coefficient of determination, 0..1 (shown as a %).
    pub r2: f64,
    /// Bar count fitted.
    pub n: usize,
}

/// Per-line + fill cosmetics for the regression channel (round-trips through `props`).
#[derive(Clone, Debug, PartialEq)]
pub struct RegressionStyle {
    pub mid_color: String,
    pub mid_style: LineStyle,
    pub upper_color: String,
    pub upper_style: LineStyle,
    pub lower_color: String,
    pub lower_style: LineStyle,
    /// Fill between the midline and the upper band.
    pub upper_fill: String,
    /// Fill between the midline and the lower band.
    pub lower_fill: String,
    /// Show the R² readout at the start of the channel.
    pub show_r2: bool,
}

impl Default for RegressionStyle {
    fn default() -> Self {
        Self {
            mid_color: NEUTRAL.to_string(),
            mid_style: LineStyle::Solid,
            upper_color: BULLISH.to_string(),
            upper_style: LineStyle::Solid,
            lower_color: BEARISH.to_string(),
            lower_style: LineStyle::Solid,
            // translucent, mid to upper / lower
            upper_fill: format!("{BULLISH}26"),
            lower_fill: format!("{BEARISH}26"),
            show_r2: true,
        }
    }
}

/// Pixel layout the painter/hit-test consume: the three lines' endpoints + the R² readout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegressionLayout {
    pub x0: f64,
    pub x1: f64,
    pub mid_y0: f64,
    pub mid_y1: f64,
    pub upper_y0: f64,
    pub upper_y1: f64,
    pub lower_y0: f64,
    pub lower_y1: f64,
    pub r2: f64,
    pub show_r2: bool,
}

/// Least-squares line + residual σ + Pearson R² over `(index, close)`. `None` when < 2 bars.
pub fn compute_regression_fit(bars: &[FitBar]) -> Option<RegressionFit> {
    let count = bars.len();
    if count < 2 {
        return None;
    }
    let n = count as f64;
    let (mut sum_x, mut sum_y, mut sum_xy, mut sum_xx, mut sum_yy) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (i, bar) in bars.iter().enumerate() {
        let x = i as f64;
        let y = bar.close;
        sum_x += x;
        sum_y += y;
        sum_xy += x * y;
        sum_xx += x * x;
        sum_yy += y * y;
    }
    let denom = n * sum_xx - sum_x * sum_x;
    let slope = if denom == 0.0 {
        0.0
    } else {
        (n * sum_xy - sum_x * sum_y) / denom
    };
    let intercept = (sum_y - slope * sum_x) / n;
    let mut sse = 0.0;
    for (i, bar) in bars.iter().enumerate() {
        let resid = bar.close - (intercept + slope * i as f64);
        sse += resid * resid;
    }
    let sigma = (sse / n).sqrt();
    let den = (denom * (n * sum_yy - sum_y * sum_y)).sqrt();
    let r = if den == 0.0 {
        0.0
    } else {
        (n * sum_xy - sum_x * sum_y) / den
    };
    Some(RegressionFit {
        t0: bars[0].time,
        t1: bars[count - 1].time,
        mid0: intercept,
        mid1: intercept + slope * (n - 1.0),
        dev: DEVIATIONS * sigma,
        r2: js_max(0.0, js_min(1.0, r * r)),
        n: count,
    })
}

/// A linear-regression channel: two anchors bound a time range, and the tool
/// fits a least-squares trendline over the bars' closes in that range, drawing
/// a ±2σ band around it. The R² of the fit is shown (as an integer %) at the
/// start of the channel. The fit is DATA-driven (only the anchors' times
/// matter), so it recomputes live as an endpoint is dragged or new bars arrive.
#[derive(Clone, Debug)]
pub struct RegressionChannel {
    base: DrawingBase,
    /// Per-line + fill cosmetics (persisted via props).
    pub reg: RegressionStyle,
    /// Last computed price span (midline ± band), cached so autoscale's
    /// `price_range()`, which gets no projector, can fold the channel in.
    cached_range: Cell<Option<Span>>,
}

impl RegressionChannel {
    pub const TYPE: &'static str = "regressionchannel";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            reg: RegressionStyle::default(),
            cached_range: Cell::new(None),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }

    /// Fit against the bars in the anchor time-range, caching the price span for autoscale.
    pub fn fit(&self, proj: &dyn Projector) -> Option<RegressionFit> {
        let (from, to) = time_range(&self.base)?;
        let bars = proj.bars_in_range(from, to)?;
        let fit_bars: Vec<FitBar> = bars.iter().map(FitBar::from).collect();
        let f = compute_regression_fit(&fit_bars)?;
        self.cached_range.set(Some(Span {
            min: js_min(f.mid0, f.mid1) - f.dev,
            max: js_max(f.mid0, f.mid1) + f.dev,
        }));
        Some(f)
    }

    /// Resolve the fit to pixel geometry for the painter + hit-test.
    pub fn layout(&self, proj: &dyn Projector) -> Option<RegressionLayout> {
        let f = self.fit(proj)?;
        let y = |price: f64| self.base.y_of(price, proj);
        Some(RegressionLayout {
            x0: proj.x_of(f.t0),
            x1: proj.x_of(f.t1),
            mid_y0: y(f.mid0)?,
            mid_y1: y(f.mid1)?,
            upper_y0: y(f.mid0 + f.dev)?,
            upper_y1: y(f.mid1 + f.dev)?,
            lower_y0: y(f.mid0 - f.dev)?,
            lower_y1: y(f.mid1 - f.dev)?,
            r2: f.r2,
            show_r2: self.reg.show_r2,
        })
    }

    /// The band outline polygon (upper edge left to right, then lower edge right to left).
    pub fn band(&self, proj: &dyn Projector) -> Option<Vec<(f64, f64)>> {
        let l = self.layout(proj)?;
        Some(vec![
            (l.x0, l.upper_y0),
            (l.x1, l.upper_y1),
            (l.x1, l.lower_y1),
            (l.x0, l.lower_y0),
        ])
    }
}

impl Drawing for RegressionChannel {
    fn base(&self) -> &DrawingBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut DrawingBase {
        &mut self.base
    }

    fn type_key(&self) -> &'static str {
        Self::TYPE
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn anchor_schema(&self) -> AnchorSchema {
        horizontal_pair_schema()
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        let Some(l) = self.layout(proj) else {
            return false;
        };
        let poly = [
            (l.x0, l.upper_y0),
            (l.x1, l.upper_y1),
            (l.x1, l.lower_y1),
            (l.x0, l.lower_y0),
        ];
        if point_in_polygon(px, py, &poly) {
            return true;
        }
        [
            [l.x0, l.mid_y0, l.x1, l.mid_y1],
            [l.x0, l.upper_y0, l.x1, l.upper_y1],
            [l.x0, l.lower_y0, l.x1, l.lower_y1],
        ]
        .iter()
        .any(|s| dist_to_segment(px, py, s[0], s[1], s[2], s[3]) <= tol)
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        match self.layout(proj) {
            Some(l) => vec![(l.x0, l.mid_y0), (l.x1, l.mid_y1)],
            // no fit (e.g. no data yet): the raw anchors keep the tool grabbable
            None => anchor_handles(&self.base, proj),
        }
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        Bounds::of_points(&self.band(proj)?)
    }

    fn price_range(&self) -> Option<Span> {
        self.cached_range
            .get()
            .or_else(|| two_anchor_span(&self.base))
    }

    fn schema(&self) -> SettingsSchema {
        // Custom paths the settings popup renders as dedicated regression controls
        // (per-line color + style, the two area fills, and the R² toggle).
        macro_rules! field {
            ($path:expr, $id:literal, $text:literal, $kind:expr, $group:expr $(,)?) => {
                SettingsField::new(
                    $path,
                    msg(concat!("drawing.regressionchannel.", $id), $text),
                    $kind,
                )
                .group($group)
            };
        }
        SettingsSchema::new(vec![
            field!(
                "reg.midColor",
                "regMidColor",
                "Midline color",
                FieldKind::Color,
                FieldGroup::Line,
            ),
            field!(
                "reg.midStyle",
                "regMidStyle",
                "Midline style",
                FieldKind::LineStyle,
                FieldGroup::Line,
            ),
            field!(
                "reg.upperColor",
                "regUpperColor",
                "Upper line color",
                FieldKind::Color,
                FieldGroup::Line,
            ),
            field!(
                "reg.upperStyle",
                "regUpperStyle",
                "Upper line style",
                FieldKind::LineStyle,
                FieldGroup::Line,
            ),
            field!(
                "reg.lowerColor",
                "regLowerColor",
                "Lower line color",
                FieldKind::Color,
                FieldGroup::Line,
            ),
            field!(
                "reg.lowerStyle",
                "regLowerStyle",
                "Lower line style",
                FieldKind::LineStyle,
                FieldGroup::Line,
            ),
            field!(
                "reg.upperFill",
                "regUpperFill",
                "Upper fill",
                FieldKind::Color,
                FieldGroup::Fill,
            ),
            field!(
                "reg.lowerFill",
                "regLowerFill",
                "Lower fill",
                FieldKind::Color,
                FieldGroup::Fill,
            ),
            field!(
                "reg.showR2",
                "regShowR2",
                "Show R²",
                FieldKind::Boolean,
                FieldGroup::Behavior,
            ),
        ])
    }

    fn write_props(&self) -> Option<Props> {
        let r = &self.reg;
        let Value::Object(map) = json!({
            "midColor": r.mid_color,
            "midStyle": line_style_json(r.mid_style),
            "upperColor": r.upper_color,
            "upperStyle": line_style_json(r.upper_style),
            "lowerColor": r.lower_color,
            "lowerStyle": line_style_json(r.lower_style),
            "upperFill": r.upper_fill,
            "lowerFill": r.lower_fill,
            "showR2": r.show_r2,
        }) else {
            return None;
        };
        Some(map)
    }

    fn read_props(&mut self, props: &Props) {
        let mut r = RegressionStyle::default();
        prop_str(props, "midColor", &mut r.mid_color);
        prop_line_style(props, "midStyle", &mut r.mid_style);
        prop_str(props, "upperColor", &mut r.upper_color);
        prop_line_style(props, "upperStyle", &mut r.upper_style);
        prop_str(props, "lowerColor", &mut r.lower_color);
        prop_line_style(props, "lowerStyle", &mut r.lower_style);
        prop_str(props, "upperFill", &mut r.upper_fill);
        prop_str(props, "lowerFill", &mut r.lower_fill);
        prop_bool(props, "showR2", &mut r.show_r2);
        self.reg = r;
    }

    fn apply_setting(&mut self, path: &str, value: &Value) {
        apply_prefixed_setting(self, "reg", path, value);
    }
}

// ───────────────────────── anchored VWAP ─────────────────────────

/// One bar the VWAP accumulates over (typical price × volume).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VwapBar {
    pub time: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

impl From<&Ohlcv> for VwapBar {
    fn from(bar: &Ohlcv) -> Self {
        Self {
            time: bar.time as f64,
            high: bar.high,
            low: bar.low,
            close: bar.close,
            volume: bar.volume.unwrap_or(0.0),
        }
    }
}

/// One computed VWAP sample: the running VWAP plus its ±(mult·σ) band edges, per bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VwapPoint {
    pub time: f64,
    /// Volume-weighted average price up to this bar (the midline).
    pub mid: f64,
    /// `mid + multiplier · σ` (upper band).
    pub upper: f64,
    /// `mid − multiplier · σ` (lower band).
    pub lower: f64,
}

/// Cosmetics for the anchored VWAP (round-trips through `props`).
#[derive(Clone, Debug, PartialEq)]
pub struct VwapStyle {
    /// VWAP midline color.
    pub mid_color: String,
    pub mid_style: LineStyle,
    /// Standard-deviation multiplier for the band half-width.
    pub multiplier: f64,
    /// Upper band line color (fully transparent by default: the fill carries the visuals).
    pub upper_color: String,
    pub upper_style: LineStyle,
    /// Lower band line color (fully transparent by default).
    pub lower_color: String,
    pub lower_style: LineStyle,
    /// Fill between the upper and lower bands.
    pub band_fill: String,
}

impl Default for VwapStyle {
    fn default() -> Self {
        // Fully transparent variant of the band hue: bumping its opacity in the picker keeps the color.
        let transparent_band = format!("{INFO}00");
        Self {
            mid_color: INFO.to_string(),
            mid_style: LineStyle::Solid,
            multiplier: 1.0,
            upper_color: transparent_band.clone(),
            upper_style: LineStyle::Solid,
            lower_color: transparent_band,
            lower_style: LineStyle::Solid,
            // band hue at ~20% opacity
            band_fill: format!("{INFO}33"),
        }
    }
}

/// Volume-weighted average price (with ±σ bands) accumulated from the first
/// bar in `bars`. Uses the typical price `(H+L+C)/3` weighted by volume; the
/// band half-width is the volume-weighted standard deviation of the typical
/// price around the running VWAP, scaled by `mult`. When the series carries no
/// volume (feeds may omit it) every bar is weighted equally, so the tool still
/// produces a meaningful curve. `None` when there are no bars.
pub fn compute_vwap(bars: &[VwapBar], mult: f64) -> Option<Vec<VwapPoint>> {
    if bars.is_empty() {
        return None;
    }
    let total_vol: f64 = bars
        .iter()
        .map(|b| if b.volume > 0.0 { b.volume } else { 0.0 })
        .sum();
    let use_vol = total_vol > 0.0; // no volume anywhere: equal-weight fallback
    let (mut cum_w, mut cum_pv, mut cum_p2v) = (0.0, 0.0, 0.0);
    let mut out = Vec::with_capacity(bars.len());
    for b in bars {
        let tp = (b.high + b.low + b.close) / 3.0;
        let w = if use_vol { js_max(0.0, b.volume) } else { 1.0 };
        cum_w += w;
        cum_pv += tp * w;
        cum_p2v += tp * tp * w;
        let mid = if cum_w > 0.0 { cum_pv / cum_w } else { tp };
        let variance = if cum_w > 0.0 {
            js_max(0.0, cum_p2v / cum_w - mid * mid)
        } else {
            0.0
        };
        let dev = variance.sqrt() * mult;
        out.push(VwapPoint {
            time: b.time,
            mid,
            upper: mid + dev,
            lower: mid - dev,
        });
    }
    Some(out)
}

/// Pixel geometry the painter/hit-test consume: the three polylines (mid/upper/lower).
#[derive(Clone, Debug, PartialEq)]
pub struct VwapLayout {
    pub mid: Vec<(f64, f64)>,
    pub upper: Vec<(f64, f64)>,
    pub lower: Vec<(f64, f64)>,
}

/// True when the pixel is within `tol` of any segment of a polyline.
fn polyline_hit(px: f64, py: f64, pts: &[(f64, f64)], tol: f64) -> bool {
    pts.windows(2)
        .any(|w| dist_to_segment(px, py, w[0].0, w[0].1, w[1].0, w[1].1) <= tol)
}

/// An **anchored VWAP**: a single time anchor from which the volume-weighted
/// average price (plus ±σ bands and a fill between them) accumulates forward
/// across every bar to the right. The curve is DATA-driven (only the anchor's
/// time matters), so it recomputes live as the anchor is dragged or new bars
/// arrive.
#[derive(Clone, Debug)]
pub struct AnchoredVwap {
    base: DrawingBase,
    /// Cosmetics (persisted via props).
    pub vwap: VwapStyle,
    /// Last computed price span (min lower to max upper), cached so autoscale's
    /// `price_range()`, which gets no projector, folds the curve in.
    cached_range: Cell<Option<Span>>,
    /// Last computed time span (anchor bar to last bar), cached for `time_extent()` culling.
    cached_extent: Cell<Option<Span>>,
}

impl AnchoredVwap {
    pub const TYPE: &'static str = "anchoredvwap";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            vwap: VwapStyle::default(),
            cached_range: Cell::new(None),
            cached_extent: Cell::new(None),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }

    /// Compute the VWAP samples from the anchor time forward, caching the price + time spans.
    pub fn series(&self, proj: &dyn Projector) -> Option<Vec<VwapPoint>> {
        let a = self.base.anchors.first()?;
        let bars = proj.bars_in_range(a.time, f64::INFINITY)?;
        if bars.is_empty() {
            return None;
        }
        let vwap_bars: Vec<VwapBar> = bars.iter().map(VwapBar::from).collect();
        let pts = compute_vwap(&vwap_bars, self.vwap.multiplier)?;
        if let (Some(first), Some(last)) = (pts.first(), pts.last()) {
            let mut min = f64::INFINITY;
            let mut max = f64::NEG_INFINITY;
            for p in &pts {
                if p.lower < min {
                    min = p.lower;
                }
                if p.upper > max {
                    max = p.upper;
                }
            }
            self.cached_range.set(Some(Span { min, max }));
            self.cached_extent.set(Some(Span {
                min: first.time,
                max: last.time,
            }));
        }
        Some(pts)
    }

    /// Resolve the samples to pixel polylines for the painter + hit-test.
    pub fn layout(&self, proj: &dyn Projector) -> Option<VwapLayout> {
        let pts = self.series(proj)?;
        let mut layout = VwapLayout {
            mid: Vec::new(),
            upper: Vec::new(),
            lower: Vec::new(),
        };
        for p in &pts {
            let x = proj.x_of(p.time);
            let (Some(my), Some(uy), Some(ly)) = (
                self.base.y_of(p.mid, proj),
                self.base.y_of(p.upper, proj),
                self.base.y_of(p.lower, proj),
            ) else {
                continue;
            };
            layout.mid.push((x, my));
            layout.upper.push((x, uy));
            layout.lower.push((x, ly));
        }
        (!layout.mid.is_empty()).then_some(layout)
    }

    /// The band outline polygon (upper edge left to right, then lower edge right to left).
    pub fn band_polygon(layout: &VwapLayout) -> Vec<(f64, f64)> {
        layout
            .upper
            .iter()
            .chain(layout.lower.iter().rev())
            .copied()
            .collect()
    }
}

impl Drawing for AnchoredVwap {
    fn base(&self) -> &DrawingBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut DrawingBase {
        &mut self.base
    }

    fn type_key(&self) -> &'static str {
        Self::TYPE
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn anchor_schema(&self) -> AnchorSchema {
        // A single anchor bounds only the START time; the VWAP is data-driven and ignores
        // the anchor's price, so its handle is constrained to horizontal moves.
        AnchorSchema {
            min: 1,
            max: 1,
            slots: vec![AnchorSlot::new("anchor", FreeAxis::X)],
        }
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        let Some(l) = self.layout(proj) else {
            return false;
        };
        point_in_polygon(px, py, &Self::band_polygon(&l))
            || polyline_hit(px, py, &l.mid, tol)
            || polyline_hit(px, py, &l.upper, tol)
            || polyline_hit(px, py, &l.lower, tol)
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        // the grab handle sits on the curve's anchor end
        if let Some(first) = self.layout(proj).and_then(|l| l.mid.first().copied()) {
            return vec![first];
        }
        self.base.anchor_px(0, proj).into_iter().collect()
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        let l = self.layout(proj)?;
        let pts: Vec<(f64, f64)> = l.upper.iter().chain(l.lower.iter()).copied().collect();
        Bounds::of_points(&pts)
    }

    fn price_range(&self) -> Option<Span> {
        self.cached_range.get()
    }

    /// Spans anchor to last bar (cached from the last compute); `None` before
    /// the first paint so it is not culled from autoscale prematurely.
    fn time_extent(&self) -> Option<Span> {
        self.cached_extent.get()
    }

    fn schema(&self) -> SettingsSchema {
        // Custom paths the settings popup renders as dedicated VWAP controls (midline color +
        // style, the band multiplier, the two band-line colors, and the fill).
        macro_rules! field {
            ($path:expr, $id:literal, $text:literal, $kind:expr, $group:expr $(,)?) => {
                SettingsField::new(
                    $path,
                    msg(concat!("drawing.anchoredvwap.", $id), $text),
                    $kind,
                )
                .group($group)
            };
        }
        SettingsSchema::new(vec![
            field!(
                "vwap.midColor",
                "vwapMidColor",
                "VWAP color",
                FieldKind::Color,
                FieldGroup::Line,
            ),
            field!(
                "vwap.midStyle",
                "vwapMidStyle",
                "VWAP style",
                FieldKind::LineStyle,
                FieldGroup::Line,
            ),
            field!(
                "vwap.multiplier",
                "vwapMultiplier",
                "Band multiplier",
                FieldKind::Number,
                FieldGroup::Behavior,
            )
            .range(0.5, 5.0, 0.5),
            field!(
                "vwap.upperColor",
                "vwapUpperColor",
                "Upper band color",
                FieldKind::Color,
                FieldGroup::Line,
            ),
            field!(
                "vwap.lowerColor",
                "vwapLowerColor",
                "Lower band color",
                FieldKind::Color,
                FieldGroup::Line,
            ),
            field!(
                "vwap.bandFill",
                "vwapBandFill",
                "Band fill",
                FieldKind::Color,
                FieldGroup::Fill,
            ),
        ])
    }

    fn write_props(&self) -> Option<Props> {
        let v = &self.vwap;
        let Value::Object(map) = json!({
            "midColor": v.mid_color,
            "midStyle": line_style_json(v.mid_style),
            "multiplier": v.multiplier,
            "upperColor": v.upper_color,
            "upperStyle": line_style_json(v.upper_style),
            "lowerColor": v.lower_color,
            "lowerStyle": line_style_json(v.lower_style),
            "bandFill": v.band_fill,
        }) else {
            return None;
        };
        Some(map)
    }

    fn read_props(&mut self, props: &Props) {
        let mut v = VwapStyle::default();
        prop_str(props, "midColor", &mut v.mid_color);
        prop_line_style(props, "midStyle", &mut v.mid_style);
        prop_f64(props, "multiplier", &mut v.multiplier);
        prop_str(props, "upperColor", &mut v.upper_color);
        prop_line_style(props, "upperStyle", &mut v.upper_style);
        prop_str(props, "lowerColor", &mut v.lower_color);
        prop_line_style(props, "lowerStyle", &mut v.lower_style);
        prop_str(props, "bandFill", &mut v.band_fill);
        self.vwap = v;
    }

    fn apply_setting(&mut self, path: &str, value: &Value) {
        apply_prefixed_setting(self, "vwap", path, value);
    }
}

// ───────────────────────── fixed-range volume profile ─────────────────────────

/// Upper bound on the row count, so a hostile `rows` prop cannot exhaust memory.
/// The settings UI caps rows at 500.
const MAX_ROWS: f64 = 10_000.0;

/// One bar the fixed-range profile buckets (estimation uses O/H/L/C + volume).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrvpBar {
    pub time: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

impl From<&Ohlcv> for FrvpBar {
    fn from(bar: &Ohlcv) -> Self {
        Self {
            time: bar.time as f64,
            open: bar.open,
            high: bar.high,
            low: bar.low,
            close: bar.close,
            volume: bar.volume.unwrap_or(0.0),
        }
    }
}

/// One profile row: `[price, price + row_h)` with the up/down volume that landed in it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrvpRow {
    /// The row's LOWER price bound.
    pub price: f64,
    pub up: f64,
    pub down: f64,
}

impl FrvpRow {
    fn total(&self) -> f64 {
        self.up + self.down
    }
}

/// The bucketed fixed-range volume-by-price profile.
#[derive(Clone, Debug, PartialEq)]
pub struct FrvpProfile {
    pub rows: Vec<FrvpRow>,
    pub row_h: f64,
    pub min: f64,
    pub max_total: f64,
    pub poc: usize,
    pub va_from: usize,
    pub va_to: usize,
}

/// One sample of a developing level (POC / VA edge) at a bar's time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrvpDevelopingPoint {
    pub time: f64,
    pub price: f64,
}

/// Which side of the range the histogram grows from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FrvpAnchor {
    Left,
    Right,
}

impl FrvpAnchor {
    fn as_str(self) -> &'static str {
        match self {
            FrvpAnchor::Left => "left",
            FrvpAnchor::Right => "right",
        }
    }
}

/// Cosmetics + behavior for the fixed-range volume profile (round-trips through `props`).
#[derive(Clone, Debug, PartialEq)]
pub struct FrvpStyle {
    /// Number of equal-height price rows.
    pub rows: f64,
    /// Value-area coverage as a percent of total volume (0-100).
    pub value_area_pct: f64,
    /// Profile width as a percent of the anchor time-span (0-100).
    pub width_pct: f64,
    pub anchor: FrvpAnchor,
    /// Buy / up volume outside the value area (typically translucent).
    pub up_color: String,
    /// Sell / down volume outside the value area (typically translucent).
    pub down_color: String,
    /// Buy / up volume inside the value area.
    pub va_up_color: String,
    /// Sell / down volume inside the value area.
    pub va_down_color: String,
    pub show_vah: bool,
    pub vah_color: String,
    pub vah_style: LineStyle,
    pub show_val: bool,
    pub val_color: String,
    pub val_style: LineStyle,
    pub show_poc: bool,
    /// `None` means the theme's contrast ink (white on dark, black on light),
    /// resolved at paint time so it follows theme switches live; a set color always wins.
    pub poc_color: Option<String>,
    pub poc_style: LineStyle,
    pub show_developing_poc: bool,
    pub developing_poc_color: String,
    pub developing_poc_style: LineStyle,
    pub show_developing_va: bool,
    pub developing_va_color: String,
    pub developing_va_style: LineStyle,
}

impl Default for FrvpStyle {
    /// Value-area fills at 25% transparency; outside-VA fills at 60% transparency:
    /// the value area is the profile's emphasized region, the tails around it recede.
    fn default() -> Self {
        Self {
            rows: 24.0,
            value_area_pct: 70.0,
            width_pct: 35.0,
            anchor: FrvpAnchor::Left,
            up_color: format!("{BULLISH}66"),
            down_color: format!("{BEARISH}66"),
            va_up_color: format!("{BULLISH}BF"),
            va_down_color: format!("{BEARISH}BF"),
            show_vah: true,
            vah_color: NEUTRAL.to_string(),
            vah_style: LineStyle::Solid,
            show_val: true,
            val_color: NEUTRAL.to_string(),
            val_style: LineStyle::Solid,
            show_poc: true,
            poc_color: None, // theme contrast ink until the user picks a color
            poc_style: LineStyle::Solid,
            show_developing_poc: false,
            developing_poc_color: ACCENT.to_string(),
            developing_poc_style: LineStyle::Dotted,
            show_developing_va: false,
            developing_va_color: ACCENT.to_string(),
            developing_va_style: LineStyle::Dotted,
        }
    }
}

/// The row a price lands in, clamped to the grid; `None` for a NaN price or an empty grid.
fn clamp_index(k: f64, n: usize) -> Option<usize> {
    if n == 0 || k.is_nan() {
        return None;
    }
    if k < 0.0 {
        Some(0)
    } else if k >= n as f64 {
        Some(n - 1)
    } else {
        Some(k as usize)
    }
}

/// Grow the value area from the POC by repeatedly absorbing the larger adjacent
/// row until accumulated volume covers `value_area_frac` of the total.
/// Returns `(va_from, va_to)`.
fn grow_value_area(rows: &[FrvpRow], poc: usize, value_area_frac: f64) -> (usize, usize) {
    let n = rows.len();
    let mut total = 0.0;
    let mut max_total = 0.0;
    for row in rows {
        let t = row.total();
        total += t;
        if t > max_total {
            max_total = t;
        }
    }
    let target = total * js_min(1.0, js_max(0.0, value_area_frac));
    let (mut va_from, mut va_to) = (poc, poc);
    let mut acc = max_total;
    while acc < target && (va_from > 0 || va_to + 1 < n) {
        let below = if va_from > 0 {
            rows.get(va_from - 1).map_or(-1.0, FrvpRow::total)
        } else {
            -1.0
        };
        let above = if va_to + 1 < n {
            rows.get(va_to + 1).map_or(-1.0, FrvpRow::total)
        } else {
            -1.0
        };
        if above > below {
            va_to += 1;
            acc += above;
        } else {
            va_from -= 1;
            acc += below;
        }
    }
    (va_from, va_to)
}

/// An empty price grid of `n` rows from `min`, `row_h` apart.
fn empty_rows(n: usize, min: f64, row_h: f64) -> Vec<FrvpRow> {
    (0..n)
        .map(|k| FrvpRow {
            price: min + k as f64 * row_h,
            up: 0.0,
            down: 0.0,
        })
        .collect()
}

/// Add one bar's volume into `rows` using the estimation rule (any OHLC in a row gets the full volume).
fn accumulate_bar(rows: &mut [FrvpRow], min: f64, row_h: f64, bar: &FrvpBar) {
    let v = bar.volume;
    // Negated so a NaN value fails the check too.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    if !(v > 0.0) {
        return;
    }
    let n = rows.len();
    let up = bar.close >= bar.open;
    let mut hit: Vec<usize> = Vec::with_capacity(4);
    for p in [bar.open, bar.high, bar.low, bar.close] {
        if let Some(k) = clamp_index(((p - min) / row_h).floor(), n)
            && !hit.contains(&k)
        {
            hit.push(k);
        }
    }
    for k in hit {
        if let Some(row) = rows.get_mut(k) {
            if up {
                row.up += v;
            } else {
                row.down += v;
            }
        }
    }
}

/// POC index + max row total, or `None` when every row is empty.
fn find_poc(rows: &[FrvpRow]) -> Option<(usize, f64)> {
    let mut max_total = 0.0;
    let mut poc = 0;
    for (k, row) in rows.iter().enumerate() {
        let t = row.total();
        if t > max_total {
            max_total = t;
            poc = k;
        }
    }
    (max_total > 0.0).then_some((poc, max_total))
}

/// Estimation-based volume profile: for each bar, the full bar volume is added
/// to every price row that contains **any** of its OHLC prices. Volume is split
/// up/down by bar direction (`close >= open`). Unlike a proportional H-L
/// distribution this does not conserve total volume across rows: a wide bar
/// can contribute to several rows. `None` when the window holds no volume.
pub fn build_estimated_profile(
    bars: &[FrvpBar],
    row_count: f64,
    value_area_frac: f64,
) -> Option<FrvpProfile> {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for b in bars {
        // Negated so a NaN volume is skipped too.
        #[allow(clippy::neg_cmp_op_on_partial_ord)]
        if !(b.volume > 0.0) {
            continue;
        }
        if b.low < min {
            min = b.low;
        }
        if b.high > max {
            max = b.high;
        }
    }
    if !min.is_finite() {
        return None;
    }

    let (n, row_h) = if max > min {
        let wanted = js_max(1.0, js_round(row_count));
        if wanted.is_nan() {
            return None;
        }
        let n = wanted.min(MAX_ROWS) as usize;
        (n, (max - min) / n as f64)
    } else {
        (1, 1.0)
    };
    let mut rows = empty_rows(n, min, row_h);

    for b in bars {
        accumulate_bar(&mut rows, min, row_h, b);
    }

    let (poc, max_total) = find_poc(&rows)?;
    let (va_from, va_to) = grow_value_area(&rows, poc, value_area_frac);
    Some(FrvpProfile {
        rows,
        row_h,
        min,
        max_total,
        poc,
        va_from,
        va_to,
    })
}

/// Full compute for paint: the final profile plus optional developing POC / VA polylines.
#[derive(Clone, Debug, PartialEq)]
pub struct FrvpCompute {
    pub profile: FrvpProfile,
    /// Mid-price of the POC row after each successive bar.
    pub developing_poc: Vec<FrvpDevelopingPoint>,
    /// Top of the value area after each successive bar.
    pub developing_va_high: Vec<FrvpDevelopingPoint>,
    /// Bottom of the value area after each successive bar.
    pub developing_va_low: Vec<FrvpDevelopingPoint>,
}

/// Build the fixed-range profile over `bars` and (when requested) the
/// developing POC / VA series. Developing levels accumulate into the **final**
/// price grid bar by bar (same min / row height / row count as the finished
/// profile), so the lines track how POC and the value area evolve as the range
/// fills, rather than re-bucketing a changing price span.
pub fn compute_fixed_range_profile(
    bars: &[FrvpBar],
    row_count: f64,
    value_area_frac: f64,
    want_developing: bool,
) -> Option<FrvpCompute> {
    let profile = build_estimated_profile(bars, row_count, value_area_frac)?;
    let mut out = FrvpCompute {
        profile,
        developing_poc: Vec::new(),
        developing_va_high: Vec::new(),
        developing_va_low: Vec::new(),
    };
    if !want_developing || bars.is_empty() {
        return Some(out);
    }

    let (min, row_h) = (out.profile.min, out.profile.row_h);
    let mut rows = empty_rows(out.profile.rows.len(), min, row_h);
    for bar in bars {
        accumulate_bar(&mut rows, min, row_h, bar);
        let Some((poc, _)) = find_poc(&rows) else {
            continue;
        };
        let (va_from, va_to) = grow_value_area(&rows, poc, value_area_frac);
        let (Some(poc_row), Some(high_row), Some(low_row)) =
            (rows.get(poc), rows.get(va_to), rows.get(va_from))
        else {
            continue;
        };
        out.developing_poc.push(FrvpDevelopingPoint {
            time: bar.time,
            price: poc_row.price + row_h / 2.0,
        });
        out.developing_va_high.push(FrvpDevelopingPoint {
            time: bar.time,
            price: high_row.price + row_h,
        });
        out.developing_va_low.push(FrvpDevelopingPoint {
            time: bar.time,
            price: low_row.price,
        });
    }
    Some(out)
}

/// Pixel geometry the painter / hit-test consume.
#[derive(Clone, Debug, PartialEq)]
pub struct FrvpLayout {
    /// Left / right pixel edges of the time span.
    pub x0: f64,
    pub x1: f64,
    /// Pixel edge the histogram is anchored to (left or right of the span).
    pub anchor_x: f64,
    /// Pixel width of the largest row.
    pub max_w: f64,
    /// Sign: +1 grows right from `anchor_x`, -1 grows left.
    pub grow: i8,
    pub profile: FrvpProfile,
    /// Resolved y for each row's lower bound (length = rows + 1 for the top edge).
    pub y_edges: Vec<f64>,
    pub vah_y: Option<f64>,
    pub val_y: Option<f64>,
    pub poc_y: Option<f64>,
    pub developing_poc: Vec<(f64, f64)>,
    pub developing_va_high: Vec<(f64, f64)>,
    pub developing_va_low: Vec<(f64, f64)>,
}

impl FrvpLayout {
    /// The histogram's horizontal extent `(left, right)` from its anchor edge.
    fn histogram_span(&self) -> (f64, f64) {
        if self.grow == 1 {
            (self.anchor_x, self.anchor_x + self.max_w)
        } else {
            (self.anchor_x - self.max_w, self.anchor_x)
        }
    }

    /// The vertical extent `(y_lo, y_hi)` of the full price span.
    fn y_span(&self) -> Option<(f64, f64)> {
        let first = *self.y_edges.first()?;
        let last = *self.y_edges.last()?;
        Some((js_min(first, last), js_max(first, last)))
    }
}

/// A **fixed-range volume profile**: two time anchors bound a range, and the
/// tool buckets bar volume into equal-height price rows by an estimation rule
/// (a bar contributes its full volume to every row that contains any of its
/// OHLC prices). Histogram width, value area, POC / VAH / VAL lines, and
/// optional developing levels are data-driven: they recompute live as an
/// endpoint is dragged or new bars arrive.
#[derive(Clone, Debug)]
pub struct FixedRangeVolumeProfile {
    base: DrawingBase,
    /// Cosmetics + behavior (persisted via props).
    pub frvp: FrvpStyle,
    cached_range: Cell<Option<Span>>,
}

impl FixedRangeVolumeProfile {
    pub const TYPE: &'static str = "fixedrangevp";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            frvp: FrvpStyle::default(),
            cached_range: Cell::new(None),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }

    /// Bars in the anchor time-range (inclusive), or `None` when either anchor / the feed is missing.
    fn bars_in_span(&self, proj: &dyn Projector) -> Option<Vec<FrvpBar>> {
        let (from, to) = time_range(&self.base)?;
        let raw = proj.bars_in_range(from, to)?;
        if raw.is_empty() {
            return None;
        }
        Some(raw.iter().map(FrvpBar::from).collect())
    }

    /// Compute the profile (+ developing series when enabled), caching the price span for autoscale.
    pub fn compute(&self, proj: &dyn Projector) -> Option<FrvpCompute> {
        let bars = self.bars_in_span(proj)?;
        let s = &self.frvp;
        let want_dev = s.show_developing_poc || s.show_developing_va;
        let result =
            compute_fixed_range_profile(&bars, s.rows, s.value_area_pct / 100.0, want_dev)?;
        let profile = &result.profile;
        self.cached_range.set(Some(Span {
            min: profile.min,
            max: profile.min + profile.row_h * profile.rows.len() as f64,
        }));
        Some(result)
    }

    /// Resolve the compute to pixel geometry for the painter + hit-test.
    pub fn layout(&self, proj: &dyn Projector) -> Option<FrvpLayout> {
        let (t0, t1) = time_range(&self.base)?;
        let computed = self.compute(proj)?;
        let profile = computed.profile.clone();
        let x0 = proj.x_of(t0);
        let x1 = proj.x_of(t1);
        let span_w = (x1 - x0).abs();
        let max_w = js_max(
            1.0,
            js_min(100.0, js_max(0.0, self.frvp.width_pct)) / 100.0 * span_w,
        );
        let grow_right = self.frvp.anchor == FrvpAnchor::Left;
        let anchor_x = if grow_right {
            js_min(x0, x1)
        } else {
            js_max(x0, x1)
        };
        let grow = if grow_right { 1 } else { -1 };

        let mut y_edges = Vec::with_capacity(profile.rows.len() + 1);
        for k in 0..=profile.rows.len() {
            y_edges.push(
                self.base
                    .y_of(profile.min + k as f64 * profile.row_h, proj)?,
            );
        }

        let y_at = |price: f64| self.base.y_of(price, proj);
        let vah_price = profile.rows.get(profile.va_to)?.price + profile.row_h;
        let val_price = profile.rows.get(profile.va_from)?.price;
        let poc_price = profile.rows.get(profile.poc)?.price + profile.row_h / 2.0;

        let to_poly = |pts: &[FrvpDevelopingPoint]| -> Vec<(f64, f64)> {
            pts.iter()
                .filter_map(|p| Some((proj.x_of(p.time), y_at(p.price)?)))
                .collect()
        };

        Some(FrvpLayout {
            x0: js_min(x0, x1),
            x1: js_max(x0, x1),
            anchor_x,
            max_w,
            grow,
            y_edges,
            vah_y: y_at(vah_price),
            val_y: y_at(val_price),
            poc_y: y_at(poc_price),
            developing_poc: to_poly(&computed.developing_poc),
            developing_va_high: to_poly(&computed.developing_va_high),
            developing_va_low: to_poly(&computed.developing_va_low),
            profile,
        })
    }
}

impl Drawing for FixedRangeVolumeProfile {
    fn base(&self) -> &DrawingBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut DrawingBase {
        &mut self.base
    }

    fn type_key(&self) -> &'static str {
        Self::TYPE
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn anchor_schema(&self) -> AnchorSchema {
        horizontal_pair_schema()
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        let Some(l) = self.layout(proj) else {
            return false;
        };
        // Hit the histogram's bounding box (anchored strip covering the full price span).
        if let Some((y_lo, y_hi)) = l.y_span() {
            let (left, right) = l.histogram_span();
            let poly = [(left, y_lo), (right, y_lo), (right, y_hi), (left, y_hi)];
            if point_in_polygon(px, py, &poly) {
                return true;
            }
        }
        // Also hit the horizontal levels and developing polylines.
        for y in [l.vah_y, l.val_y, l.poc_y].into_iter().flatten() {
            if dist_to_segment(px, py, l.x0, y, l.x1, y) <= tol {
                return true;
            }
        }
        [
            &l.developing_poc,
            &l.developing_va_high,
            &l.developing_va_low,
        ]
        .into_iter()
        .any(|poly| polyline_hit(px, py, poly, tol))
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        if let Some(l) = self.layout(proj)
            && let Some(poc_y) = l.poc_y
        {
            return vec![(l.x0, poc_y), (l.x1, poc_y)];
        }
        anchor_handles(&self.base, proj)
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        let l = self.layout(proj)?;
        let (hist_left, hist_right) = l.histogram_span();
        let left = js_min(l.x0, hist_left);
        let right = js_max(l.x1, hist_right);
        let (y_lo, y_hi) = l.y_span()?;
        Some(Bounds {
            x: left,
            y: y_lo,
            w: right - left,
            h: y_hi - y_lo,
        })
    }

    fn price_range(&self) -> Option<Span> {
        self.cached_range
            .get()
            .or_else(|| two_anchor_span(&self.base))
    }

    fn schema(&self) -> SettingsSchema {
        // All FRVP controls live in the gear panel: the schema paths gate that UI branch.
        macro_rules! field {
            ($path:expr, $id:literal, $text:literal, $kind:expr, $group:expr $(,)?) => {
                SettingsField::new(
                    $path,
                    msg(concat!("drawing.fixedrangevp.", $id), $text),
                    $kind,
                )
                .group($group)
            };
        }
        macro_rules! style_field {
            ($path:expr, $id:literal, $text:literal $(,)?) => {
                field!($path, $id, $text, FieldKind::LineStyle, FieldGroup::Line)
                    .options(line_style_options())
            };
        }
        SettingsSchema::new(vec![
            field!(
                "frvp.rows",
                "frvpRows",
                "Rows",
                FieldKind::Number,
                FieldGroup::Behavior,
            )
            .range(1.0, 500.0, 1.0),
            field!(
                "frvp.valueAreaPct",
                "frvpValueAreaPct",
                "Value Area",
                FieldKind::Number,
                FieldGroup::Behavior,
            )
            .range(0.0, 100.0, 1.0),
            field!(
                "frvp.widthPct",
                "frvpWidthPct",
                "Width %",
                FieldKind::Number,
                FieldGroup::Behavior,
            )
            .range(0.0, 100.0, 1.0),
            field!(
                "frvp.anchor",
                "frvpAnchor",
                "Anchor",
                FieldKind::Select,
                FieldGroup::Behavior,
            )
            .options(vec![
                FieldOption::new("right", msg("drawing.fixedrangevp.option.right", "Right")),
                FieldOption::new("left", msg("drawing.fixedrangevp.option.left", "Left")),
            ]),
            field!(
                "frvp.upColor",
                "frvpUpColor",
                "Up Volume",
                FieldKind::Color,
                FieldGroup::Fill,
            ),
            field!(
                "frvp.downColor",
                "frvpDownColor",
                "Down Volume",
                FieldKind::Color,
                FieldGroup::Fill,
            ),
            field!(
                "frvp.vaUpColor",
                "frvpVaUpColor",
                "Value Area Up",
                FieldKind::Color,
                FieldGroup::Fill,
            ),
            field!(
                "frvp.vaDownColor",
                "frvpVaDownColor",
                "Value Area Down",
                FieldKind::Color,
                FieldGroup::Fill,
            ),
            field!(
                "frvp.showVah",
                "frvpShowVah",
                "VAH",
                FieldKind::Boolean,
                FieldGroup::Line,
            ),
            field!(
                "frvp.vahColor",
                "frvpVahColor",
                "VAH color",
                FieldKind::Color,
                FieldGroup::Line,
            ),
            style_field!("frvp.vahStyle", "frvpVahStyle", "VAH style"),
            field!(
                "frvp.showVal",
                "frvpShowVal",
                "VAL",
                FieldKind::Boolean,
                FieldGroup::Line,
            ),
            field!(
                "frvp.valColor",
                "frvpValColor",
                "VAL color",
                FieldKind::Color,
                FieldGroup::Line,
            ),
            style_field!("frvp.valStyle", "frvpValStyle", "VAL style"),
            field!(
                "frvp.showPoc",
                "frvpShowPoc",
                "POC",
                FieldKind::Boolean,
                FieldGroup::Line,
            ),
            field!(
                "frvp.pocColor",
                "frvpPocColor",
                "POC color",
                FieldKind::Color,
                FieldGroup::Line,
            ),
            style_field!("frvp.pocStyle", "frvpPocStyle", "POC style"),
            field!(
                "frvp.showDevelopingPoc",
                "frvpShowDevelopingPoc",
                "Developing POC",
                FieldKind::Boolean,
                FieldGroup::Line,
            ),
            field!(
                "frvp.developingPocColor",
                "frvpDevelopingPocColor",
                "Developing POC color",
                FieldKind::Color,
                FieldGroup::Line,
            ),
            style_field!(
                "frvp.developingPocStyle",
                "frvpDevelopingPocStyle",
                "Developing POC style",
            ),
            field!(
                "frvp.showDevelopingVa",
                "frvpShowDevelopingVa",
                "Developing VA",
                FieldKind::Boolean,
                FieldGroup::Line,
            ),
            field!(
                "frvp.developingVaColor",
                "frvpDevelopingVaColor",
                "Developing VA color",
                FieldKind::Color,
                FieldGroup::Line,
            ),
            style_field!(
                "frvp.developingVaStyle",
                "frvpDevelopingVaStyle",
                "Developing VA style",
            ),
        ])
    }

    fn write_props(&self) -> Option<Props> {
        let s = &self.frvp;
        let Value::Object(mut map) = json!({
            "rows": s.rows,
            "valueAreaPct": s.value_area_pct,
            "widthPct": s.width_pct,
            "anchor": s.anchor.as_str(),
            "upColor": s.up_color,
            "downColor": s.down_color,
            "vaUpColor": s.va_up_color,
            "vaDownColor": s.va_down_color,
            "showVah": s.show_vah,
            "vahColor": s.vah_color,
            "vahStyle": line_style_json(s.vah_style),
            "showVal": s.show_val,
            "valColor": s.val_color,
            "valStyle": line_style_json(s.val_style),
            "showPoc": s.show_poc,
            "pocStyle": line_style_json(s.poc_style),
            "showDevelopingPoc": s.show_developing_poc,
            "developingPocColor": s.developing_poc_color,
            "developingPocStyle": line_style_json(s.developing_poc_style),
            "showDevelopingVa": s.show_developing_va,
            "developingVaColor": s.developing_va_color,
            "developingVaStyle": line_style_json(s.developing_va_style),
        }) else {
            return None;
        };
        // An unset POC color is `undefined` in the original, so its key is absent.
        if let Some(color) = &s.poc_color {
            map.insert("pocColor".to_string(), Value::String(color.clone()));
        }
        Some(map)
    }

    fn read_props(&mut self, props: &Props) {
        let mut s = FrvpStyle::default();
        prop_f64(props, "rows", &mut s.rows);
        prop_f64(props, "valueAreaPct", &mut s.value_area_pct);
        prop_f64(props, "widthPct", &mut s.width_pct);
        match props.get("anchor").and_then(Value::as_str) {
            Some("left") => s.anchor = FrvpAnchor::Left,
            Some("right") => s.anchor = FrvpAnchor::Right,
            _ => {}
        }
        prop_str(props, "upColor", &mut s.up_color);
        prop_str(props, "downColor", &mut s.down_color);
        prop_str(props, "vaUpColor", &mut s.va_up_color);
        prop_str(props, "vaDownColor", &mut s.va_down_color);
        prop_bool(props, "showVah", &mut s.show_vah);
        prop_str(props, "vahColor", &mut s.vah_color);
        prop_line_style(props, "vahStyle", &mut s.vah_style);
        prop_bool(props, "showVal", &mut s.show_val);
        prop_str(props, "valColor", &mut s.val_color);
        prop_line_style(props, "valStyle", &mut s.val_style);
        prop_bool(props, "showPoc", &mut s.show_poc);
        s.poc_color = props
            .get("pocColor")
            .and_then(Value::as_str)
            .map(str::to_string);
        prop_line_style(props, "pocStyle", &mut s.poc_style);
        prop_bool(props, "showDevelopingPoc", &mut s.show_developing_poc);
        prop_str(props, "developingPocColor", &mut s.developing_poc_color);
        prop_line_style(props, "developingPocStyle", &mut s.developing_poc_style);
        prop_bool(props, "showDevelopingVa", &mut s.show_developing_va);
        prop_str(props, "developingVaColor", &mut s.developing_va_color);
        prop_line_style(props, "developingVaStyle", &mut s.developing_va_style);
        self.frvp = s;
    }

    fn apply_setting(&mut self, path: &str, value: &Value) {
        apply_prefixed_setting(self, "frvp", path, value);
    }
}

/// The registry entries of this family.
pub fn metas() -> Vec<DrawingTypeMeta> {
    vec![
        DrawingTypeMeta::new(
            RegressionChannel::TYPE,
            "channels",
            msg("drawing.regressionchannel.label", "Linear Regression"),
            svg24(
                r#"<line x1="3" y1="8" x2="21" y2="4"/><line x1="3" y1="16" x2="21" y2="12"/><line x1="3" y1="12" x2="21" y2="8" stroke-dasharray="3 3"/>"#,
            ),
            DrawingStyle::line(NEUTRAL, 1.0, LineStyle::Solid),
            |init| Box::new(RegressionChannel::new(init)),
        ),
        DrawingTypeMeta::new(
            AnchoredVwap::TYPE,
            "measure",
            msg("drawing.anchoredvwap.label", "Anchored VWAP"),
            svg24(r#"<line x1="5" y1="3" x2="5" y2="21"/><path d="M5 16c4 0 5-9 8-9s3 5 8 3"/>"#),
            DrawingStyle::line(INFO, 2.0, LineStyle::Solid),
            |init| Box::new(AnchoredVwap::new(init)),
        ),
        DrawingTypeMeta::new(
            FixedRangeVolumeProfile::TYPE,
            "measure",
            msg("drawing.fixedrangevp.label", "Fixed Range Volume Profile"),
            svg24(
                r#"<path d="M4 4v16"/><path d="M4 8h10"/><path d="M4 12h14"/><path d="M4 16h8"/><path d="M4 20h12"/>"#,
            ),
            DrawingStyle::line(BULLISH, 1.0, LineStyle::Solid),
            |init| Box::new(FixedRangeVolumeProfile::new(init)),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawings::drawing::DrawingInit;
    use crate::drawings::geometry::DrawingPoint;
    use crate::drawings::projector::LinearProjector;

    const MIN: f64 = 60_000.0;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn pt(time: f64, price: f64) -> DrawingPoint {
        DrawingPoint { time, price }
    }

    fn meta(key: &str) -> DrawingTypeMeta {
        metas().into_iter().find(|m| m.type_key == key).unwrap()
    }

    fn create(key: &str, anchors: Vec<DrawingPoint>) -> Box<dyn Drawing> {
        let m = meta(key);
        let mut init = DrawingInit::on_pane("price").anchors(anchors);
        init.style = Some(m.default_style.to_partial());
        (m.create)(&init)
    }

    /// Flat bars (O=H=L=C) one per minute from t=0, with volumes.
    fn flat_bars(rows: &[(f64, f64)]) -> Vec<Ohlcv> {
        rows.iter()
            .enumerate()
            .map(|(i, (p, v))| Ohlcv::new(i as i64 * 60_000, *p, *p, *p, *p, Some(*v)))
            .collect()
    }

    fn closes(values: &[f64]) -> Vec<Ohlcv> {
        values
            .iter()
            .enumerate()
            .map(|(i, c)| Ohlcv::new(i as i64 * 60_000, *c, *c, *c, *c, None))
            .collect()
    }

    fn fit_bars(values: &[f64]) -> Vec<FitBar> {
        values
            .iter()
            .enumerate()
            .map(|(i, c)| FitBar {
                time: i as f64 * MIN,
                close: *c,
            })
            .collect()
    }

    const KEYS: [&str; 3] = [
        RegressionChannel::TYPE,
        AnchoredVwap::TYPE,
        FixedRangeVolumeProfile::TYPE,
    ];

    #[test]
    fn metas_match_the_registry() {
        let all = metas();
        assert_eq!(all.len(), 3);
        let reg = meta("regressionchannel");
        assert_eq!(reg.group, "channels");
        assert_eq!(reg.default_style.line_color, NEUTRAL);
        assert_eq!(reg.default_style.line_width, 1.0);
        let vwap = meta("anchoredvwap");
        assert_eq!(vwap.group, "measure");
        assert_eq!(vwap.default_style.line_color, INFO);
        assert_eq!(vwap.default_style.line_width, 2.0);
        let frvp = meta("fixedrangevp");
        assert_eq!(frvp.group, "measure");
        assert_eq!(frvp.default_style.line_color, BULLISH);
        for m in &all {
            assert!(!m.covers_series);
            assert!(m.icon.starts_with("<svg"));
        }
    }

    #[test]
    fn types_report_keys_and_anchor_schemas() {
        for (key, min, roles) in [
            ("regressionchannel", 2, vec!["start", "end"]),
            ("anchoredvwap", 1, vec!["anchor"]),
            ("fixedrangevp", 2, vec!["start", "end"]),
        ] {
            let d = create(key, Vec::new());
            assert_eq!(d.type_key(), key);
            let schema = d.anchor_schema();
            assert_eq!((schema.min, schema.max), (min, min));
            let got: Vec<&str> = schema.slots.iter().map(|s| s.role.as_str()).collect();
            assert_eq!(got, roles);
            assert!(schema.slots.iter().all(|s| s.free == FreeAxis::X));
        }
    }

    #[test]
    fn degenerate_anchor_counts_never_panic() {
        let plain = LinearProjector::sample();
        let with_data = LinearProjector::sample().with_bars(flat_bars(&[(10.0, 1.0)]));
        for key in KEYS {
            for anchors in [Vec::new(), vec![pt(0.0, 10.0)]] {
                let d = create(key, anchors);
                for proj in [&plain, &with_data] {
                    let _ = d.hit_test(5.0, 5.0, proj, 4.0);
                    let _ = d.handle_points(proj);
                    let _ = d.hit_handle(5.0, 5.0, proj, 4.0);
                    let _ = d.bounds(proj);
                    let _ = d.price_range();
                    let _ = d.time_extent();
                }
                let _ = d.schema();
                let _ = d.write_props();
                let _ = d.serialize();
                let _ = d.is_complete();
            }
        }
    }

    #[test]
    fn serialization_round_trips() {
        for key in KEYS {
            let m = meta(key);
            let d = create(key, vec![pt(0.0, 10.0), pt(4.0 * MIN, 20.0)]);
            let doc = d.serialize();
            assert_eq!(doc.type_key, key);
            let again = (m.create)(&DrawingInit::from(doc.clone()));
            assert_eq!(again.serialize(), doc);
        }
    }

    // ── regression ──

    #[test]
    fn perfect_line_has_zero_deviation_and_full_r2() {
        let f = compute_regression_fit(&fit_bars(&[10.0, 12.0, 14.0, 16.0, 18.0])).unwrap();
        assert!(approx(f.r2, 1.0));
        assert!(approx(f.dev, 0.0));
        assert!(approx(f.mid0, 10.0));
        assert!(approx(f.mid1, 18.0));
        assert_eq!((f.n, f.t0, f.t1), (5, 0.0, 4.0 * MIN));
    }

    #[test]
    fn flat_series_has_zero_slope_and_zero_r2() {
        let f = compute_regression_fit(&fit_bars(&[50.0; 4])).unwrap();
        assert!(approx(f.mid0, 50.0) && approx(f.mid1, 50.0));
        assert!(approx(f.r2, 0.0) && approx(f.dev, 0.0));
    }

    #[test]
    fn scatter_has_partial_r2_and_positive_band() {
        let f = compute_regression_fit(&fit_bars(&[10.0, 13.0, 12.0, 17.0, 18.0])).unwrap();
        assert!(f.r2 > 0.0 && f.r2 < 1.0);
        assert!(f.dev > 0.0);
    }

    #[test]
    fn fewer_than_two_bars_cannot_be_fit() {
        assert!(compute_regression_fit(&fit_bars(&[10.0])).is_none());
        assert!(compute_regression_fit(&[]).is_none());
    }

    fn regression_with_data() -> (RegressionChannel, LinearProjector) {
        let proj = LinearProjector::sample().with_bars(closes(&[10.0, 12.0, 14.0, 16.0, 18.0]));
        // anchor prices are arbitrary: the fit ignores them
        let init =
            DrawingInit::on_pane("price").anchors(vec![pt(0.0, 999.0), pt(4.0 * MIN, -999.0)]);
        (RegressionChannel::new(&init), proj)
    }

    #[test]
    fn regression_geometry_comes_from_the_fit() {
        let (d, proj) = regression_with_data();
        let f = d.fit(&proj).unwrap();
        assert!(approx(f.mid0, 10.0) && approx(f.mid1, 18.0));
        let l = d.layout(&proj).unwrap();
        assert!(approx(l.x0, 0.0) && approx(l.x1, 4.0));
        assert!(approx(l.mid_y0, 760.0) && approx(l.mid_y1, 728.0));
        assert!(l.show_r2 && approx(l.r2, 1.0));
        let handles = d.handle_points(&proj);
        assert_eq!(handles.len(), 2);
        assert!(approx(handles[0].1, 760.0) && approx(handles[1].1, 728.0));
        // the cached span reflects the fitted band, not the anchor prices
        let range = d.price_range().unwrap();
        assert!(approx(range.min, 10.0) && approx(range.max, 18.0));
        let b = d.bounds(&proj).unwrap();
        assert!(approx(b.x, 0.0) && approx(b.w, 4.0) && approx(b.y, 728.0) && approx(b.h, 32.0));
        // on the midline at x=2 (y=744), and far outside
        assert!(d.hit_test(2.0, 744.0, &proj, 3.0));
        assert!(!d.hit_test(500.0, 100.0, &proj, 3.0));
        assert_eq!(d.hit_handle(0.5, 760.0, &proj, 3.0), Some(0));
        assert_eq!(d.hit_handle(2.0, 744.0, &proj, 3.0), None);
        assert_eq!(d.band(&proj).unwrap().len(), 4);
    }

    #[test]
    fn regression_without_bars_falls_back_to_anchors() {
        let proj = LinearProjector::sample();
        let d = RegressionChannel::new(
            &DrawingInit::on_pane("price").anchors(vec![pt(0.0, 20.0), pt(5.0 * MIN, 40.0)]),
        );
        assert!(d.fit(&proj).is_none() && d.layout(&proj).is_none());
        assert_eq!(d.handle_points(&proj), vec![(0.0, 720.0), (5.0, 640.0)]);
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 20.0,
                max: 40.0
            })
        );
        assert!(!d.hit_test(1.0, 1.0, &proj, 3.0));
        assert!(d.bounds(&proj).is_none());
    }

    #[test]
    fn regression_settings_round_trip_through_props() {
        let (mut d, _) = regression_with_data();
        assert_eq!(d.reg.mid_color, "#787b86");
        assert_eq!(d.reg.upper_color, "#089981");
        assert_eq!(d.reg.lower_color, "#f23645");
        assert_eq!(d.reg.upper_fill, "#08998126");
        assert!(d.reg.show_r2);
        let mut patch = Props::new();
        patch.insert("reg.midColor".into(), json!("#123456"));
        patch.insert("reg.upperStyle".into(), json!("dashed"));
        patch.insert("reg.lowerFill".into(), json!("#00000080"));
        patch.insert("reg.showR2".into(), json!(false));
        patch.insert("style.lineWidth".into(), json!(3));
        d.apply_settings(&patch);
        assert_eq!(d.base().style.line_width, 3.0);
        let doc = d.serialize();
        let props = doc.props.clone().unwrap();
        assert_eq!(props["midColor"], "#123456");
        assert_eq!(props["upperStyle"], "dashed");
        assert_eq!(props["lowerFill"], "#00000080");
        assert_eq!(props["showR2"], false);
        let round = RegressionChannel::new(&DrawingInit::from(doc));
        assert_eq!(round.reg, d.reg);
    }

    #[test]
    fn regression_read_props_ignores_wrong_types() {
        let mut props = Props::new();
        props.insert("midColor".into(), json!(5));
        props.insert("midStyle".into(), json!("wavy"));
        props.insert("showR2".into(), json!("yes"));
        props.insert("upperColor".into(), json!("#abcdef"));
        let d = RegressionChannel::new(&DrawingInit::on_pane("price").props(props));
        assert_eq!(d.reg.mid_color, NEUTRAL);
        assert_eq!(d.reg.mid_style, LineStyle::Solid);
        assert!(d.reg.show_r2);
        assert_eq!(d.reg.upper_color, "#abcdef");
    }

    #[test]
    fn regression_schema_lists_the_reg_paths() {
        let d = create("regressionchannel", Vec::new());
        let paths: Vec<String> = d.schema().fields.into_iter().map(|f| f.path).collect();
        assert_eq!(paths.len(), 9);
        assert_eq!(paths[0], "reg.midColor");
        assert_eq!(paths[8], "reg.showR2");
    }

    // ── vwap ──

    fn vbar(time: f64, p: f64, volume: f64) -> VwapBar {
        VwapBar {
            time,
            high: p,
            low: p,
            close: p,
            volume,
        }
    }

    #[test]
    fn vwap_weights_by_volume() {
        let pts = compute_vwap(&[vbar(0.0, 10.0, 1.0), vbar(MIN, 20.0, 3.0)], 1.0).unwrap();
        assert!(approx(pts[0].mid, 10.0));
        assert!(approx(pts[1].mid, 17.5));
        // first sample has no dispersion
        assert!(approx(pts[0].upper, 10.0) && approx(pts[0].lower, 10.0));
        // variance = (100*1 + 400*3)/4 - 17.5^2 = 325 - 306.25 = 18.75
        assert!(approx(pts[1].upper - pts[1].mid, 18.75_f64.sqrt()));
        assert!(approx(pts[1].mid - pts[1].lower, 18.75_f64.sqrt()));
    }

    #[test]
    fn vwap_multiplier_scales_linearly_and_zero_volume_is_equal_weight() {
        let rows: Vec<VwapBar> = [10.0, 20.0, 12.0, 18.0]
            .iter()
            .enumerate()
            .map(|(i, p)| vbar(i as f64, *p, 1.0))
            .collect();
        let one = compute_vwap(&rows, 1.0).unwrap();
        let two = compute_vwap(&rows, 2.0).unwrap();
        let half1 = one[3].upper - one[3].mid;
        assert!(half1 > 0.0);
        assert!(approx(two[3].upper - two[3].mid, half1 * 2.0));
        let flat = compute_vwap(&[vbar(0.0, 10.0, 0.0), vbar(MIN, 20.0, 0.0)], 1.0).unwrap();
        assert!(approx(flat[1].mid, 15.0));
        assert!(compute_vwap(&[], 1.0).is_none());
    }

    fn vwap_at(anchor_time: f64) -> (AnchoredVwap, LinearProjector) {
        let proj = LinearProjector::sample().with_bars(flat_bars(&[
            (10.0, 1.0),
            (20.0, 1.0),
            (30.0, 1.0),
        ]));
        let d =
            AnchoredVwap::new(&DrawingInit::on_pane("price").anchors(vec![pt(anchor_time, 999.0)]));
        (d, proj)
    }

    #[test]
    fn vwap_accumulates_from_the_anchor_forward() {
        let (d, proj) = vwap_at(0.0);
        assert_eq!(d.time_extent(), None);
        assert_eq!(d.price_range(), None);
        let pts = d.series(&proj).unwrap();
        assert_eq!(pts.len(), 3);
        assert!(approx(pts[2].mid, 20.0));
        assert_eq!(d.handle_points(&proj), vec![(0.0, 760.0)]);
        assert_eq!(
            d.time_extent(),
            Some(Span {
                min: 0.0,
                max: 2.0 * MIN
            })
        );
        let r = d.price_range().unwrap();
        assert!(r.min <= 10.0 && r.max >= 20.0);
        let l = d.layout(&proj).unwrap();
        assert_eq!(l.mid.len(), 3);
        assert_eq!(AnchoredVwap::band_polygon(&l).len(), 6);
        assert!(d.bounds(&proj).is_some());
        // on the first sample, and far away
        assert!(d.hit_test(0.0, 760.0, &proj, 3.0));
        assert!(!d.hit_test(900.0, 5.0, &proj, 3.0));
        assert_eq!(d.hit_handle(0.0, 760.0, &proj, 3.0), Some(0));

        let (later, proj) = vwap_at(MIN);
        let pts = later.series(&proj).unwrap();
        assert_eq!(pts.len(), 2);
        assert!(approx(pts[0].mid, 20.0) && approx(pts[1].mid, 25.0));
    }

    #[test]
    fn vwap_without_bars_falls_back_to_the_anchor() {
        let proj = LinearProjector::sample();
        let d = AnchoredVwap::new(&DrawingInit::on_pane("price").anchors(vec![pt(0.0, 20.0)]));
        assert!(d.series(&proj).is_none() && d.layout(&proj).is_none());
        assert_eq!(d.handle_points(&proj), vec![(0.0, 720.0)]);
        assert_eq!(d.price_range(), None);
    }

    #[test]
    fn vwap_defaults_and_settings() {
        let (mut d, _) = vwap_at(0.0);
        assert_eq!(d.vwap.mid_color, "#5b9cf6");
        assert_eq!(d.vwap.upper_color, "#5b9cf600");
        assert_eq!(d.vwap.lower_color, "#5b9cf600");
        assert_eq!(d.vwap.band_fill, "#5b9cf633");
        assert_eq!(d.vwap.multiplier, 1.0);
        let mut patch = Props::new();
        patch.insert("vwap.midColor".into(), json!("#123456"));
        patch.insert("vwap.multiplier".into(), json!(2.5));
        patch.insert("vwap.bandFill".into(), json!("#00000080"));
        d.apply_settings(&patch);
        let doc = d.serialize();
        let props = doc.props.clone().unwrap();
        assert_eq!(props["midColor"], "#123456");
        assert_eq!(props["multiplier"], 2.5);
        let round = AnchoredVwap::new(&DrawingInit::from(doc));
        assert_eq!(round.vwap, d.vwap);
        let fields = d.schema().fields;
        assert_eq!(fields[2].path, "vwap.multiplier");
        assert_eq!(
            (fields[2].min, fields[2].max, fields[2].step),
            (Some(0.5), Some(5.0), Some(0.5))
        );
    }

    // ── fixed-range volume profile ──

    fn fbar(time: f64, open: f64, high: f64, low: f64, close: f64, volume: f64) -> FrvpBar {
        FrvpBar {
            time,
            open,
            high,
            low,
            close,
            volume,
        }
    }

    fn flat_fbars(rows: &[(f64, f64)]) -> Vec<FrvpBar> {
        rows.iter()
            .enumerate()
            .map(|(i, (p, v))| fbar(i as f64 * MIN, *p, *p, *p, *p, *v))
            .collect()
    }

    #[test]
    fn profile_adds_full_volume_to_every_row_an_ohlc_price_touches() {
        let p = build_estimated_profile(&[fbar(0.0, 100.0, 110.0, 100.0, 110.0, 1000.0)], 2.0, 0.7)
            .unwrap();
        assert_eq!(p.rows.len(), 2);
        assert!(approx(p.rows[0].up, 1000.0) && approx(p.rows[1].up, 1000.0));
        assert_eq!(p.rows[0].down, 0.0);
        assert!(approx(p.row_h, 5.0));
    }

    #[test]
    fn profile_splits_by_direction() {
        let p = build_estimated_profile(
            &[
                fbar(0.0, 100.0, 100.0, 100.0, 105.0, 400.0),
                fbar(MIN, 105.0, 105.0, 100.0, 100.0, 600.0),
            ],
            10.0,
            0.7,
        )
        .unwrap();
        let up: f64 = p.rows.iter().map(|r| r.up).sum();
        let down: f64 = p.rows.iter().map(|r| r.down).sum();
        assert!(up > 0.0 && down > 0.0);
    }

    #[test]
    fn rows_of_flat_bars_sum_to_total_volume_and_the_value_area_grows_from_the_poc() {
        let data = flat_fbars(&[
            (100.0, 100.0),
            (101.0, 100.0),
            (102.0, 800.0),
            (103.0, 100.0),
            (104.0, 100.0),
        ]);
        let p = build_estimated_profile(&data, 5.0, 0.7).unwrap();
        let total: f64 = p.rows.iter().map(FrvpRow::total).sum();
        assert!(approx(total, 1200.0));
        assert_eq!(p.poc, 2);
        assert!(approx(p.max_total, 800.0));
        // equal neighbours: the lower one is absorbed first (above > below is false)
        assert_eq!((p.va_from, p.va_to), (1, 2));
        let va: f64 = p.rows[p.va_from..=p.va_to].iter().map(FrvpRow::total).sum();
        assert!(va >= total * 0.7 - 1e-6);
        // a full value area covers everything
        let all = build_estimated_profile(&data, 5.0, 1.0).unwrap();
        assert_eq!((all.va_from, all.va_to), (0, 4));
    }

    #[test]
    fn profile_without_volume_is_none() {
        assert!(build_estimated_profile(&[], 24.0, 0.7).is_none());
        assert!(build_estimated_profile(&flat_fbars(&[(100.0, 0.0)]), 24.0, 0.7).is_none());
        assert!(build_estimated_profile(&flat_fbars(&[(100.0, 5.0)]), f64::NAN, 0.7).is_some());
    }

    #[test]
    fn a_flat_window_collapses_to_one_row_and_huge_row_counts_are_capped() {
        let p = build_estimated_profile(&flat_fbars(&[(100.0, 5.0)]), 24.0, 0.7).unwrap();
        assert_eq!(p.rows.len(), 1);
        assert_eq!(p.row_h, 1.0);
        let wide = flat_fbars(&[(100.0, 5.0), (110.0, 5.0)]);
        assert_eq!(
            build_estimated_profile(&wide, 1e15, 0.7)
                .unwrap()
                .rows
                .len(),
            MAX_ROWS as usize
        );
        assert_eq!(
            build_estimated_profile(&wide, -3.0, 0.7)
                .unwrap()
                .rows
                .len(),
            1
        );
    }

    #[test]
    fn developing_levels_track_the_poc_as_the_range_fills() {
        let data = flat_fbars(&[(100.0, 100.0), (102.0, 100.0), (110.0, 800.0)]);
        let r = compute_fixed_range_profile(&data, 10.0, 0.7, true).unwrap();
        assert_eq!(r.developing_poc.len(), 3);
        assert_eq!(r.developing_va_high.len(), 3);
        assert_eq!(r.developing_va_low.len(), 3);
        assert_eq!(r.developing_poc[0].time, 0.0);
        assert_eq!(r.developing_poc[2].time, 2.0 * MIN);
        let final_poc = r.profile.rows[r.profile.poc].price + r.profile.row_h / 2.0;
        assert!(approx(r.developing_poc[2].price, final_poc));
        assert!(r.developing_poc[2].price > r.developing_poc[0].price);
        let none = compute_fixed_range_profile(&data, 10.0, 0.7, false).unwrap();
        assert!(none.developing_poc.is_empty());
    }

    fn frvp_with_data() -> (FixedRangeVolumeProfile, LinearProjector) {
        let proj = LinearProjector::sample().with_bars(flat_bars(&[
            (100.0, 100.0),
            (101.0, 100.0),
            (102.0, 800.0),
            (103.0, 100.0),
            (104.0, 100.0),
        ]));
        let mut props = Props::new();
        props.insert("rows".into(), json!(5));
        let d = FixedRangeVolumeProfile::new(
            &DrawingInit::on_pane("price")
                .anchors(vec![pt(0.0, 999.0), pt(4.0 * MIN, 999.0)])
                .props(props),
        );
        (d, proj)
    }

    #[test]
    fn frvp_layout_and_geometry() {
        let (d, proj) = frvp_with_data();
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 999.0,
                max: 999.0
            })
        );
        let l = d.layout(&proj).unwrap();
        assert_eq!((l.x0, l.x1, l.anchor_x, l.grow), (0.0, 4.0, 0.0, 1));
        assert!(approx(l.max_w, 1.4));
        assert_eq!(l.y_edges.len(), 6);
        assert!(approx(l.y_edges[0], 400.0) && approx(l.y_edges[5], 384.0));
        // POC row 2: mid price 100 + 2*0.8 + 0.4 = 102 -> y 392
        assert!(approx(l.poc_y.unwrap(), 392.0));
        assert!(approx(l.vah_y.unwrap(), (200.0 - 102.4) / 0.25));
        assert!(approx(l.val_y.unwrap(), (200.0 - 100.8) / 0.25));
        assert!(l.developing_poc.is_empty());
        let handles = d.handle_points(&proj);
        assert!(approx(handles[0].0, 0.0) && approx(handles[1].0, 4.0));
        assert!(approx(handles[0].1, 392.0));
        let b = d.bounds(&proj).unwrap();
        assert!(approx(b.x, 0.0) && approx(b.w, 4.0) && approx(b.y, 384.0) && approx(b.h, 16.0));
        let range = d.price_range().unwrap();
        assert!(approx(range.min, 100.0) && approx(range.max, 104.0));
        assert!(d.hit_test(1.0, 390.0, &proj, 2.0));
        assert!(!d.hit_test(900.0, 5.0, &proj, 2.0));
        assert_eq!(d.hit_handle(4.0, 392.0, &proj, 0.5), Some(1));
    }

    #[test]
    fn frvp_anchored_right_grows_left_and_developing_lines_are_drawn() {
        let (mut d, proj) = frvp_with_data();
        let mut props = d.write_props().unwrap();
        props.insert("anchor".into(), json!("right"));
        props.insert("showDevelopingPoc".into(), json!(true));
        d.read_props(&props);
        let l = d.layout(&proj).unwrap();
        assert_eq!((l.anchor_x, l.grow), (4.0, -1));
        assert_eq!(l.developing_poc.len(), 5);
        assert_eq!(l.developing_va_high.len(), 5);
        let b = d.bounds(&proj).unwrap();
        assert!(approx(b.x, 0.0) && approx(b.w, 4.0));
    }

    #[test]
    fn frvp_without_bars_falls_back_to_anchors() {
        let proj = LinearProjector::sample();
        let d = FixedRangeVolumeProfile::new(
            &DrawingInit::on_pane("price").anchors(vec![pt(0.0, 20.0), pt(5.0 * MIN, 40.0)]),
        );
        assert!(d.compute(&proj).is_none() && d.layout(&proj).is_none());
        assert_eq!(d.handle_points(&proj), vec![(0.0, 720.0), (5.0, 640.0)]);
        assert_eq!(
            d.price_range(),
            Some(Span {
                min: 20.0,
                max: 40.0
            })
        );
    }

    #[test]
    fn frvp_defaults_settings_and_leniency() {
        let d = create("fixedrangevp", Vec::new());
        let schema_paths: Vec<String> = d.schema().fields.into_iter().map(|f| f.path).collect();
        assert_eq!(schema_paths.len(), 23);
        assert!(schema_paths.contains(&"frvp.developingVaStyle".to_string()));
        let s = FrvpStyle::default();
        assert_eq!((s.rows, s.value_area_pct, s.width_pct), (24.0, 70.0, 35.0));
        assert_eq!(s.anchor, FrvpAnchor::Left);
        assert_eq!(s.up_color, "#08998166");
        assert_eq!(s.down_color, "#f2364566");
        assert_eq!(s.va_up_color, "#089981BF");
        assert_eq!(s.va_down_color, "#f23645BF");
        assert_eq!(s.vah_color, NEUTRAL);
        assert_eq!(s.poc_color, None);
        assert_eq!(s.developing_poc_style, LineStyle::Dotted);
        assert_eq!(s.developing_va_color, ACCENT);
        // unset POC color is absent from props; a set one round-trips
        let mut d = FixedRangeVolumeProfile::new(&DrawingInit::on_pane("price"));
        assert!(!d.write_props().unwrap().contains_key("pocColor"));
        d.apply_setting("frvp.pocColor", &json!("#ffffff"));
        d.apply_setting("frvp.rows", &json!(48));
        d.apply_setting("frvp.showDevelopingPoc", &json!(true));
        let doc = d.serialize();
        assert_eq!(doc.props.as_ref().unwrap()["pocColor"], "#ffffff");
        let round = FixedRangeVolumeProfile::new(&DrawingInit::from(doc));
        assert_eq!(round.frvp, d.frvp);
        assert_eq!(round.frvp.rows, 48.0);
        d.apply_setting("frvp.pocColor", &Value::Null);
        assert_eq!(d.frvp.poc_color, None);
        // wrong types are ignored
        let mut props = Props::new();
        props.insert("rows".into(), json!("many"));
        props.insert("anchor".into(), json!(5));
        props.insert("showPoc".into(), json!(false));
        let lenient = FixedRangeVolumeProfile::new(&DrawingInit::on_pane("price").props(props));
        assert_eq!(lenient.frvp.rows, 24.0);
        assert_eq!(lenient.frvp.anchor, FrvpAnchor::Left);
        assert!(!lenient.frvp.show_poc);
    }

    /// A projector with a series gateway: the stats tools do not read it, but a
    /// finer-timeframe answer must not disturb them.
    struct WithSeries(LinearProjector);

    impl Projector for WithSeries {
        fn x_of(&self, time: f64) -> f64 {
            self.0.x_of(time)
        }
        fn y_of(&self, price: f64, pane_id: &str) -> Option<f64> {
            self.0.y_of(price, pane_id)
        }
        fn px_to_point(&self, x: f64, y: f64, pane_id: &str) -> DrawingPoint {
            self.0.px_to_point(x, y, pane_id)
        }
        fn pane_id_at_y(&self, y: f64) -> Option<String> {
            self.0.pane_id_at_y(y)
        }
        fn bars_in_range(&self, from: f64, to: f64) -> Option<Vec<Ohlcv>> {
            self.0.bars_in_range(from, to)
        }
        fn series_in_range(
            &self,
            _timeframe: &str,
            _from: f64,
            _to: f64,
        ) -> Option<crate::drawings::series::DrawingSeriesState> {
            Some(crate::drawings::series::DrawingSeriesState::Unavailable {
                reason: crate::drawings::series::SeriesUnavailable::NoSource,
            })
        }
        fn width(&self) -> f64 {
            self.0.width()
        }
        fn height(&self) -> f64 {
            self.0.height()
        }
    }

    #[test]
    fn a_series_gateway_does_not_change_the_bar_driven_result() {
        let (d, proj) = frvp_with_data();
        let wrapped = WithSeries(proj.clone());
        assert_eq!(d.layout(&proj), d.layout(&wrapped));
    }
}
