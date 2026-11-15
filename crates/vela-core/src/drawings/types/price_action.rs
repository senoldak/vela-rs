//! Price-action tools: a fair value gap, an order block and a liquidity
//! level, each placed on one candle, found on the candles around it and
//! followed as price comes back.
//!
//! The analysis half (the original's `priceAction.ts`) is a set of pure
//! functions over ascending OHLC bars, so a tool recomputes as bars arrive.
//! Every event is the index of the bar it happened on, or `None` when it has
//! not happened yet. The tool half (`PriceActionTool` / `PriceActionZone`
//! and the three tools) reads the chart's bars through
//! `Projector::bars_in_range`; without series access a tool degrades to its
//! anchor guide.

use std::any::Any;

use serde_json::{Value, json};

use crate::drawings::drawing::{
    AnchorSchema, AnchorSlot, Bounds, Drawing, DrawingBase, DrawingInit, Props, Span,
};
use crate::drawings::geometry::{FreeAxis, Projector};
use crate::drawings::registry::{DrawingTypeMeta, svg24};
use crate::drawings::schema::{
    FieldGroup, FieldKind, FieldOption, SettingsField, SettingsSchema, line_fields,
};
use crate::drawings::style::DrawingStyle;
use crate::i18n::msg;
use crate::indicators::classics::math::{js_max, js_min};
use crate::model::{LineStyle, Ohlcv};
use crate::palette::{BEARISH, BULLISH, WARNING};

// ── analysis (priceAction.ts) ──

/// The candle fields the analysis reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PriceActionBar {
    pub time: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

impl From<Ohlcv> for PriceActionBar {
    fn from(b: Ohlcv) -> Self {
        Self {
            time: b.time as f64,
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
        }
    }
}

/// How many candles either side of the clicked one are searched for a gap.
const GAP_REACH: i64 = 3;

/// The bar at a signed index; `None` outside the series (JS reads `undefined`).
fn bar_at(bars: &[PriceActionBar], i: i64) -> Option<&PriceActionBar> {
    usize::try_from(i).ok().and_then(|i| bars.get(i))
}

/// The bar whose open time is closest to `time` (bars ascending), or `None` with no bars.
pub fn nearest_bar_index(bars: &[PriceActionBar], time: f64) -> Option<usize> {
    if bars.is_empty() {
        return None;
    }
    let mut lo = 0;
    let mut hi = bars.len() - 1;
    while lo < hi {
        let mid = (lo + hi) >> 1;
        if bars[mid].time < time {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo > 0 && (bars[lo - 1].time - time).abs() <= (bars[lo].time - time).abs() {
        return Some(lo - 1);
    }
    Some(lo)
}

/// A gap left by one candle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gap {
    pub bull: bool,
    pub top: f64,
    pub bottom: f64,
}

/// A gap found near a clicked candle, with the candle that left it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NearestGap {
    pub index: usize,
    pub bull: bool,
    pub top: f64,
    pub bottom: f64,
}

fn gap_at_signed(bars: &[PriceActionBar], i: i64) -> Option<Gap> {
    let prev = bar_at(bars, i - 1)?;
    let next = bar_at(bars, i + 1)?;
    bar_at(bars, i)?;
    if next.low > prev.high {
        return Some(Gap {
            bull: true,
            top: next.low,
            bottom: prev.high,
        });
    }
    if next.high < prev.low {
        return Some(Gap {
            bull: false,
            top: prev.low,
            bottom: next.high,
        });
    }
    None
}

/// A gap left by the candle at `i` (the middle of three): bullish when the
/// next candle's low stays above the previous one's high, bearish when its
/// high stays below the previous low.
pub fn gap_at(bars: &[PriceActionBar], i: usize) -> Option<Gap> {
    gap_at_signed(bars, i as i64)
}

/// The gap at the candle at `i`, or at the nearest candle a few bars away that left one.
pub fn nearest_gap(bars: &[PriceActionBar], i: usize) -> Option<NearestGap> {
    let i = i as i64;
    for k in 0..=GAP_REACH {
        let candidates: &[i64] = if k == 0 { &[0] } else { &[-k, k] };
        for off in candidates {
            let j = i + off;
            if let (Some(g), Ok(index)) = (gap_at_signed(bars, j), usize::try_from(j)) {
                return Some(NearestGap {
                    index,
                    bull: g.bull,
                    top: g.top,
                    bottom: g.bottom,
                });
            }
        }
    }
    None
}

/// A fair value gap and what price has done to it since.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GapState {
    /// The middle candle of the three.
    pub index: usize,
    pub bull: bool,
    pub top: f64,
    pub bottom: f64,
    /// The gap's halfway price.
    pub mid: f64,
    /// Price first traded back into the gap.
    pub tested: Option<usize>,
    /// Price reached the gap's halfway line.
    pub half: Option<usize>,
    /// Price traded through the whole gap.
    pub filled: Option<usize>,
    /// After filling, a candle closed beyond the gap's far side: it now works the other way.
    pub inverted: Option<usize>,
}

/// A fair value gap around the candle at (or near) `i`, and what price has done to it since.
pub fn gap_state(bars: &[PriceActionBar], i: usize) -> Option<GapState> {
    let g = nearest_gap(bars, i)?;
    let NearestGap {
        index,
        bull,
        top,
        bottom,
    } = g;
    let mid = (top + bottom) / 2.0;
    let (mut tested, mut half, mut filled, mut inverted) = (None, None, None, None);
    let mut j = index + 2;
    while j < bars.len() && inverted.is_none() {
        let b = &bars[j];
        let reach = if bull { b.low } else { b.high };
        let into = |v: f64| if bull { reach <= v } else { reach >= v };
        if tested.is_none() && into(if bull { top } else { bottom }) {
            tested = Some(j);
        }
        if half.is_none() && into(mid) {
            half = Some(j);
        }
        if filled.is_none() && into(if bull { bottom } else { top }) {
            filled = Some(j);
        }
        if filled.is_some()
            && (if bull {
                b.close < bottom
            } else {
                b.close > top
            })
        {
            inverted = Some(j);
        }
        j += 1;
    }
    Some(GapState {
        index,
        bull,
        top,
        bottom,
        mid,
        tested,
        half,
        filled,
        inverted,
    })
}

/// Which part of the candle an order block covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockRange {
    /// The candle's body.
    Body,
    /// The candle's whole range, high to low.
    Wick,
}

impl BlockRange {
    fn as_str(self) -> &'static str {
        match self {
            BlockRange::Body => "body",
            BlockRange::Wick => "wick",
        }
    }
}

/// An order block and what price has done to it since.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockState {
    pub index: usize,
    /// A down-close candle marks demand (a bullish block); an up-close one supply.
    pub bull: bool,
    pub top: f64,
    pub bottom: f64,
    pub mid: f64,
    /// Price came back into the block after leaving it.
    pub tested: Option<usize>,
    /// Price reached the block's halfway line.
    pub half: Option<usize>,
    /// A candle closed through the block's far side.
    pub broken: Option<usize>,
    /// After the break, price returned to the block from the other side.
    pub retest: Option<usize>,
}

/// An order block on the candle at `i` (its body, or its whole range) and
/// what price has done to it since. Tests only count once price has first
/// left the block; a close through it breaks it either way.
pub fn block_state(bars: &[PriceActionBar], i: usize, range: BlockRange) -> Option<BlockState> {
    let c = bars.get(i)?;
    let bull = c.close < c.open;
    let (top, bottom) = match range {
        BlockRange::Body => (js_max(c.open, c.close), js_min(c.open, c.close)),
        BlockRange::Wick => (c.high, c.low),
    };
    let mid = (top + bottom) / 2.0;
    let mut left = false;
    let (mut tested, mut half, mut broken, mut retest) = (None, None, None, None);
    let mut left_after_break = false;
    let mut j = i + 1;
    while j < bars.len() && retest.is_none() {
        let b = &bars[j];
        if broken.is_none() {
            if !left {
                left = if bull { b.low > top } else { b.high < bottom };
            } else {
                if tested.is_none() && (if bull { b.low <= top } else { b.high >= bottom }) {
                    tested = Some(j);
                }
                if half.is_none() && (if bull { b.low <= mid } else { b.high >= mid }) {
                    half = Some(j);
                }
            }
            if if bull {
                b.close < bottom
            } else {
                b.close > top
            } {
                broken = Some(j);
            }
        } else if !left_after_break {
            left_after_break = if bull { b.high < bottom } else { b.low > top };
        } else if if bull { b.high >= bottom } else { b.low <= top } {
            retest = Some(j);
        }
        j += 1;
    }
    Some(BlockState {
        index: i,
        bull,
        top,
        bottom,
        mid,
        tested,
        half,
        broken,
        retest,
    })
}

/// Which side of a candle liquidity rests on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiquiditySide {
    High,
    Low,
}

/// A liquidity level and when price took it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiquidityState {
    pub index: usize,
    /// Resting above a high (true) or below a low.
    pub high: bool,
    pub price: f64,
    /// A candle traded through the level and closed back inside it.
    pub swept: Option<usize>,
    /// A candle closed beyond the level.
    pub broken: Option<usize>,
}

/// Liquidity resting beyond the high or low of the candle at `i`, and when price took it.
pub fn liquidity_state(
    bars: &[PriceActionBar],
    i: usize,
    side: LiquiditySide,
) -> Option<LiquidityState> {
    let c = bars.get(i)?;
    let high = side == LiquiditySide::High;
    let price = if high { c.high } else { c.low };
    let (mut swept, mut broken) = (None, None);
    let mut j = i + 1;
    while j < bars.len() && broken.is_none() {
        let b = &bars[j];
        if if high {
            b.close > price
        } else {
            b.close < price
        } {
            broken = Some(j);
        } else if swept.is_none() && (if high { b.high > price } else { b.low < price }) {
            swept = Some(j);
        }
        j += 1;
    }
    Some(LiquidityState {
        index: i,
        high,
        price,
        swept,
        broken,
    })
}

// ── shared tool base (PriceActionTool / PriceActionZone) ──

/// A point to paint a marker at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Marker {
    pub x: f64,
    pub y: f64,
}

/// The box a zone continues as once price flips it.
#[derive(Clone, Debug, PartialEq)]
pub struct ZoneFlipBox {
    pub x1: f64,
    pub x2: f64,
    pub fill: String,
}

/// A zone in pixels, ready to paint: the box, its halfway line, where it
/// stopped, and the box it continues as once price flips it.
#[derive(Clone, Debug, PartialEq)]
pub struct PriceZoneGeometry {
    pub x1: f64,
    pub x2: f64,
    pub y_top: f64,
    pub y_bottom: f64,
    /// Fill (its alpha is the zone's opacity); the outline and halfway line take its hue, opaque.
    pub fill: String,
    pub border: bool,
    pub mid_y: Option<f64>,
    /// The zone has run its course (tested, filled, broken...) and reads quieter.
    pub faded: bool,
    /// A dot on the zone's edge, at the bar where it stopped.
    pub marker: Option<Marker>,
    /// The zone carried on the other way (an inverse gap, a breaker block).
    pub flip: Option<ZoneFlipBox>,
    pub handle: (f64, f64),
}

/// How far right a zone or level runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoneExtend {
    Tested,
    Half,
    Filled,
    Broken,
    /// On to the latest candle.
    Always,
}

impl ZoneExtend {
    pub fn as_str(self) -> &'static str {
        match self {
            ZoneExtend::Tested => "tested",
            ZoneExtend::Half => "half",
            ZoneExtend::Filled => "filled",
            ZoneExtend::Broken => "broken",
            ZoneExtend::Always => "always",
        }
    }

    fn parse(s: &str, allowed: &[ZoneExtend]) -> Option<Self> {
        allowed.iter().copied().find(|e| e.as_str() == s)
    }
}

/// The settings every zone tool has (round-trip through `props`).
#[derive(Clone, Debug, PartialEq)]
pub struct ZoneSettings {
    /// How far right the zone runs: to an event (`Tested`, `Half`, the tool's
    /// own far-side event) or `Always` on to the latest candle.
    pub extend: ZoneExtend,
    /// Fill of a bullish / bearish zone; its alpha is the opacity.
    pub up_color: String,
    pub down_color: String,
    /// The dashed line at the zone's halfway price.
    pub midline: bool,
    pub border: bool,
    /// Quieter once the zone has run its course.
    pub fade: bool,
}

impl ZoneSettings {
    /// Read the shared keys off a props bag; anything missing or mistyped takes `d`'s value.
    fn read(props: &Props, d: &ZoneSettings, allowed: &[ZoneExtend]) -> ZoneSettings {
        ZoneSettings {
            extend: props
                .get("extend")
                .and_then(Value::as_str)
                .and_then(|s| ZoneExtend::parse(s, allowed))
                .unwrap_or(d.extend),
            up_color: read_string(props, "upColor", &d.up_color),
            down_color: read_string(props, "downColor", &d.down_color),
            midline: read_bool(props, "midline", d.midline),
            border: read_bool(props, "border", d.border),
            fade: read_bool(props, "fade", d.fade),
        }
    }

    fn write(&self, props: &mut Props) {
        props.insert("extend".into(), json!(self.extend.as_str()));
        props.insert("upColor".into(), json!(self.up_color));
        props.insert("downColor".into(), json!(self.down_color));
        props.insert("midline".into(), json!(self.midline));
        props.insert("border".into(), json!(self.border));
        props.insert("fade".into(), json!(self.fade));
    }
}

fn read_bool(props: &Props, key: &str, default: bool) -> bool {
    props.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn read_string(props: &Props, key: &str, default: &str) -> String {
    props
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_string()
}

/// A zone as a tool reads it off the candles: its span, and the bars it
/// starts, stops and flips at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoneFormation {
    /// The candle the zone is placed on (its handle).
    pub index: usize,
    pub bull: bool,
    pub top: f64,
    pub bottom: f64,
    pub mid: f64,
    pub start: usize,
    /// `None` runs the zone on to the latest candle.
    pub end: Option<usize>,
    /// The box it continues as the other way.
    pub flip: Option<ZoneFlip>,
}

/// Where a flipped zone starts and stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZoneFlip {
    pub from: usize,
    /// `None` runs on to the latest candle.
    pub to: Option<usize>,
}

/// The chart's candles, and the one nearest a tool's anchor.
#[derive(Clone, Debug, PartialEq)]
pub struct Candles {
    pub bars: Vec<PriceActionBar>,
    pub at: usize,
}

/// The chart's candles and the one nearest the anchor; `None` without an
/// anchor, without series access, or on an empty series.
pub fn candles(base: &DrawingBase, proj: &dyn Projector) -> Option<Candles> {
    let a = base.anchors.first()?;
    let bars: Vec<PriceActionBar> = proj
        .bars_in_range(f64::NEG_INFINITY, f64::INFINITY)?
        .into_iter()
        .map(PriceActionBar::from)
        .collect();
    let at = nearest_bar_index(&bars, a.time)?;
    Some(Candles { bars, at })
}

/// Half a candle's width at bar `i`, in pixels.
pub fn half_bar(proj: &dyn Projector, bars: &[PriceActionBar], i: usize) -> f64 {
    let a = bars.get(i.saturating_sub(1));
    let bi = bars.len().saturating_sub(1).min(i.max(1));
    let b = bars.get(bi);
    match (a, b) {
        (Some(a), Some(b)) if i.saturating_sub(1) != bi => {
            js_max(1.0, (proj.x_of(b.time) - proj.x_of(a.time)).abs() / 2.0)
        }
        _ => 3.0,
    }
}

/// Pixel x for the right edge of a zone that stops at bar `end`, or runs on to the chart's edge.
pub fn right_edge(
    proj: &dyn Projector,
    bars: &[PriceActionBar],
    end: Option<usize>,
) -> Option<f64> {
    match end {
        None => Some(js_max(proj.width(), proj.x_of(bars.last()?.time))),
        Some(end) => Some(proj.x_of(bars.get(end)?.time) + half_bar(proj, bars, end)),
    }
}

/// The anchor itself, for a tool with nothing on the candles to hang its handle on.
pub fn anchor_handle(base: &DrawingBase, proj: &dyn Projector) -> Vec<(f64, f64)> {
    base.anchor_px(0, proj).into_iter().collect()
}

/// The anchor of a price-action tool: one candle, free along time only.
fn candle_anchor_schema(free: FreeAxis) -> AnchorSchema {
    AnchorSchema {
        min: 1,
        max: 1,
        slots: vec![AnchorSlot::new("candle", free)],
    }
}

/// A zone's pixel layout, or `None` when the candles don't form one.
pub fn zone_geometry(
    base: &DrawingBase,
    proj: &dyn Projector,
    s: &ZoneSettings,
    formation: impl FnOnce(&[PriceActionBar], usize) -> Option<ZoneFormation>,
) -> Option<PriceZoneGeometry> {
    let c = candles(base, proj)?;
    let f = formation(&c.bars, c.at)?;
    let yt = base.y_of(f.top, proj)?;
    let yb = base.y_of(f.bottom, proj)?;
    let bars = &c.bars;
    let left = |i: usize| Some(proj.x_of(bars.get(i)?.time) - half_bar(proj, bars, i));
    let y_top = js_min(yt, yb);
    let y_bottom = js_max(yt, yb);
    let mid_y = base.y_of(f.mid, proj).unwrap_or((y_top + y_bottom) / 2.0);
    let flip = match f.flip {
        Some(fl) => Some(ZoneFlipBox {
            x1: left(fl.from)?,
            x2: right_edge(proj, bars, fl.to)?,
            fill: if f.bull {
                s.down_color.clone()
            } else {
                s.up_color.clone()
            },
        }),
        None => None,
    };
    // The dot sits on the edge price reached: the near side, the halfway line or the far side.
    let edge = match s.extend {
        ZoneExtend::Tested => {
            if f.bull {
                f.top
            } else {
                f.bottom
            }
        }
        ZoneExtend::Half => f.mid,
        _ => {
            if f.bull {
                f.bottom
            } else {
                f.top
            }
        }
    };
    let edge_y = base.y_of(edge, proj);
    let marker = match (f.end, &flip, edge_y) {
        (Some(end), None, Some(y)) => Some(Marker {
            x: proj.x_of(bars.get(end)?.time),
            y,
        }),
        _ => None,
    };
    Some(PriceZoneGeometry {
        x1: left(f.start)?,
        x2: right_edge(proj, bars, f.end)?,
        y_top,
        y_bottom,
        fill: if f.bull {
            s.up_color.clone()
        } else {
            s.down_color.clone()
        },
        border: s.border,
        mid_y: s.midline.then_some(mid_y),
        faded: f.end.is_some() && s.fade && flip.is_none(),
        marker,
        flip,
        handle: (proj.x_of(bars.get(f.index)?.time), mid_y),
    })
}

/// Body hit-test of a zone tool. With no zone under the anchor, the
/// painter's guide at the anchor still selects it.
pub fn zone_hit_test(
    base: &DrawingBase,
    zone: Option<&PriceZoneGeometry>,
    px: f64,
    py: f64,
    proj: &dyn Projector,
    tol: f64,
) -> bool {
    let Some(z) = zone else {
        return base
            .anchors
            .first()
            .is_some_and(|a| (px - proj.x_of(a.time)).abs() <= tol);
    };
    let in_y = py >= z.y_top - tol && py <= z.y_bottom + tol;
    if in_y && px >= z.x1 - tol && px <= z.x2 + tol {
        return true;
    }
    z.flip
        .as_ref()
        .is_some_and(|f| in_y && px >= f.x1 - tol && px <= f.x2 + tol)
}

/// The one handle of a zone tool: on the zone, else at the anchor.
pub fn zone_handle_points(
    base: &DrawingBase,
    zone: Option<&PriceZoneGeometry>,
    proj: &dyn Projector,
) -> Vec<(f64, f64)> {
    match zone {
        Some(z) => vec![z.handle],
        None => anchor_handle(base, proj),
    }
}

/// The selection box of a zone tool.
pub fn zone_bounds(zone: Option<&PriceZoneGeometry>, proj: &dyn Projector) -> Option<Bounds> {
    let z = zone?;
    let right = js_min(proj.width(), z.flip.as_ref().map_or(z.x2, |f| f.x2));
    Some(Bounds {
        x: z.x1,
        y: z.y_top,
        w: js_max(1.0, right - z.x1),
        h: js_max(1.0, z.y_bottom - z.y_top),
    })
}

/// Apply a `<prefix>.<key>` setting by re-reading the tool's props with that
/// key replaced. A value of the wrong type leaves the key at its default.
/// Returns whether `path` belonged to `prefix`.
fn apply_nested_setting<D: Drawing + ?Sized>(
    d: &mut D,
    prefix: &str,
    path: &str,
    value: &Value,
) -> bool {
    let Some(key) = path
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('.'))
    else {
        return false;
    };
    if !key.contains('.') {
        let mut props = d.write_props().unwrap_or_default();
        props.insert(key.to_string(), value.clone());
        d.read_props(&props);
    }
    true
}

fn option(value: &'static str, label: crate::i18n::MessageLike) -> FieldOption {
    FieldOption::new(value, label)
}

fn strings(values: &[&str]) -> Vec<Value> {
    values.iter().map(|v| json!(v)).collect()
}

// ── fair value gap ──

/// A fair value gap's settings (round-trip through `props`).
#[derive(Clone, Debug, PartialEq)]
pub struct GapSettings {
    /// `extend` is one of tested / half / filled / always.
    pub zone: ZoneSettings,
    /// Once filled, a close through the far side carries the gap on the other way.
    pub inverse: bool,
}

const GAP_EXTENDS: [ZoneExtend; 4] = [
    ZoneExtend::Tested,
    ZoneExtend::Half,
    ZoneExtend::Filled,
    ZoneExtend::Always,
];

impl Default for GapSettings {
    fn default() -> Self {
        Self {
            zone: ZoneSettings {
                extend: ZoneExtend::Filled,
                up_color: format!("{BULLISH}33"),
                down_color: format!("{BEARISH}33"),
                midline: true,
                border: false,
                fade: true,
            },
            inverse: false,
        }
    }
}

/// A fair value gap: three candles where the third leaves a gap with the
/// first, an imbalance price often returns to. Placed with one click on the
/// middle candle (or near it: the nearest gap is used), it runs right until
/// price tests it, reaches its halfway line or fills it, then marks where.
/// Found and followed on the candles, so it updates as new ones arrive.
#[derive(Clone, Debug)]
pub struct FairValueGap {
    base: DrawingBase,
    pub gap: GapSettings,
}

impl FairValueGap {
    pub const TYPE: &'static str = "fairvaluegap";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            gap: GapSettings::default(),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }

    fn formation(&self, bars: &[PriceActionBar], at: usize) -> Option<ZoneFormation> {
        let s = gap_state(bars, at)?;
        let ZoneSettings { extend, .. } = self.gap.zone;
        let flip = match (self.gap.inverse && extend == ZoneExtend::Filled, s.inverted) {
            (true, Some(from)) => Some(ZoneFlip { from, to: None }),
            _ => None,
        };
        let end = match extend {
            ZoneExtend::Tested => s.tested,
            ZoneExtend::Half => s.half,
            ZoneExtend::Filled => s.filled,
            ZoneExtend::Broken | ZoneExtend::Always => None,
        };
        Some(ZoneFormation {
            index: s.index,
            bull: s.bull,
            top: s.top,
            bottom: s.bottom,
            mid: s.mid,
            start: s.index.saturating_sub(1),
            end,
            flip,
        })
    }

    /// The zone's pixel layout, or `None` when the candles don't form one.
    pub fn zone(&self, proj: &dyn Projector) -> Option<PriceZoneGeometry> {
        zone_geometry(&self.base, proj, &self.gap.zone, |bars, at| {
            self.formation(bars, at)
        })
    }
}

impl Drawing for FairValueGap {
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
        candle_anchor_schema(FreeAxis::X)
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        zone_hit_test(&self.base, self.zone(proj).as_ref(), px, py, proj, tol)
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        zone_handle_points(&self.base, self.zone(proj).as_ref(), proj)
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        zone_bounds(self.zone(proj).as_ref(), proj)
    }

    fn price_range(&self) -> Option<Span> {
        None
    }

    fn schema(&self) -> SettingsSchema {
        let extend_options = vec![
            option(
                "tested",
                msg("drawing.fairvaluegap.option.tested", "Until tested"),
            ),
            option("half", msg("drawing.fairvaluegap.option.half", "Until 50%")),
            option(
                "filled",
                msg("drawing.fairvaluegap.option.filled", "Until filled"),
            ),
            option(
                "always",
                msg("drawing.fairvaluegap.option.always", "Always"),
            ),
        ];
        SettingsSchema::new(vec![
            SettingsField::new(
                "gap.extend",
                msg("drawing.fairvaluegap.gapExtend", "Extend right"),
                FieldKind::Select,
            )
            .options(extend_options)
            .group(FieldGroup::Behavior),
            SettingsField::new(
                "gap.upColor",
                msg("drawing.fairvaluegap.gapUpColor", "Bullish"),
                FieldKind::Color,
            )
            .group(FieldGroup::Fill),
            SettingsField::new(
                "gap.downColor",
                msg("drawing.fairvaluegap.gapDownColor", "Bearish"),
                FieldKind::Color,
            )
            .group(FieldGroup::Fill),
            SettingsField::new(
                "gap.midline",
                msg("drawing.fairvaluegap.gapMidline", "Midline"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Line),
            SettingsField::new(
                "gap.border",
                msg("drawing.fairvaluegap.gapBorder", "Border"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Line),
            SettingsField::new(
                "gap.fade",
                msg("drawing.fairvaluegap.gapFade", "Fade at end"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Behavior)
            .when("gap.extend", strings(&["tested", "half", "filled"])),
            SettingsField::new(
                "gap.inverse",
                msg("drawing.fairvaluegap.gapInverse", "Flip on close"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Behavior)
            .when("gap.extend", strings(&["filled"])),
        ])
    }

    fn write_props(&self) -> Option<Props> {
        let mut props = Props::new();
        self.gap.zone.write(&mut props);
        props.insert("inverse".into(), json!(self.gap.inverse));
        Some(props)
    }

    fn read_props(&mut self, props: &Props) {
        let d = GapSettings::default();
        self.gap = GapSettings {
            zone: ZoneSettings::read(props, &d.zone, &GAP_EXTENDS),
            inverse: read_bool(props, "inverse", d.inverse),
        };
    }

    fn apply_setting(&mut self, path: &str, value: &Value) {
        if !apply_nested_setting(self, "gap", path, value) {
            crate::drawings::drawing::apply_setting_default(self, path, value);
        }
    }
}

// ── order block ──

/// An order block's settings (round-trip through `props`).
#[derive(Clone, Debug, PartialEq)]
pub struct BlockSettings {
    /// `extend` is one of tested / half / broken / always.
    pub zone: ZoneSettings,
    /// The candle's body, or its whole range from high to low.
    pub range: BlockRange,
    /// Once broken, the block carries on the other way until price returns to it.
    pub breaker: bool,
}

const BLOCK_EXTENDS: [ZoneExtend; 4] = [
    ZoneExtend::Tested,
    ZoneExtend::Half,
    ZoneExtend::Broken,
    ZoneExtend::Always,
];

impl Default for BlockSettings {
    fn default() -> Self {
        Self {
            zone: ZoneSettings {
                extend: ZoneExtend::Broken,
                up_color: format!("{BULLISH}29"),
                down_color: format!("{BEARISH}29"),
                midline: true,
                border: true,
                fade: true,
            },
            range: BlockRange::Body,
            breaker: true,
        }
    }
}

/// An order block: the last opposite candle before a move, a zone price
/// often comes back to. Placed with one click on the candle (a down-close
/// candle marks demand, an up-close one supply), it runs right until price
/// tests it, reaches its halfway line or closes through it. A broken block
/// can carry on as a breaker, the other way, until price returns to it.
#[derive(Clone, Debug)]
pub struct OrderBlock {
    base: DrawingBase,
    pub block: BlockSettings,
}

impl OrderBlock {
    pub const TYPE: &'static str = "orderblock";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            block: BlockSettings::default(),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }

    fn formation(&self, bars: &[PriceActionBar], at: usize) -> Option<ZoneFormation> {
        let s = block_state(bars, at, self.block.range)?;
        let extend = self.block.zone.extend;
        let flip = match (self.block.breaker && extend == ZoneExtend::Broken, s.broken) {
            (true, Some(from)) => Some(ZoneFlip { from, to: s.retest }),
            _ => None,
        };
        let end = match extend {
            ZoneExtend::Tested => s.tested,
            ZoneExtend::Half => s.half,
            ZoneExtend::Broken => s.broken,
            ZoneExtend::Filled | ZoneExtend::Always => None,
        };
        Some(ZoneFormation {
            index: s.index,
            bull: s.bull,
            top: s.top,
            bottom: s.bottom,
            mid: s.mid,
            start: s.index,
            end,
            flip,
        })
    }

    /// The zone's pixel layout, or `None` when the candles don't form one.
    pub fn zone(&self, proj: &dyn Projector) -> Option<PriceZoneGeometry> {
        zone_geometry(&self.base, proj, &self.block.zone, |bars, at| {
            self.formation(bars, at)
        })
    }
}

impl Drawing for OrderBlock {
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
        candle_anchor_schema(FreeAxis::X)
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        zone_hit_test(&self.base, self.zone(proj).as_ref(), px, py, proj, tol)
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        zone_handle_points(&self.base, self.zone(proj).as_ref(), proj)
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        zone_bounds(self.zone(proj).as_ref(), proj)
    }

    fn price_range(&self) -> Option<Span> {
        None
    }

    fn schema(&self) -> SettingsSchema {
        let range_options = vec![
            option("body", msg("drawing.orderblock.option.body", "Body")),
            option("wick", msg("drawing.orderblock.option.wick", "Full range")),
        ];
        let extend_options = vec![
            option(
                "tested",
                msg("drawing.orderblock.option.tested", "Until tested"),
            ),
            option("half", msg("drawing.orderblock.option.half", "Until 50%")),
            option(
                "broken",
                msg("drawing.orderblock.option.broken", "Until broken"),
            ),
            option("always", msg("drawing.orderblock.option.always", "Always")),
        ];
        SettingsSchema::new(vec![
            SettingsField::new(
                "block.range",
                msg("drawing.orderblock.blockRange", "Zone"),
                FieldKind::Select,
            )
            .options(range_options)
            .group(FieldGroup::Behavior),
            SettingsField::new(
                "block.extend",
                msg("drawing.orderblock.blockExtend", "Extend right"),
                FieldKind::Select,
            )
            .options(extend_options)
            .group(FieldGroup::Behavior),
            SettingsField::new(
                "block.upColor",
                msg("drawing.orderblock.blockUpColor", "Bullish"),
                FieldKind::Color,
            )
            .group(FieldGroup::Fill),
            SettingsField::new(
                "block.downColor",
                msg("drawing.orderblock.blockDownColor", "Bearish"),
                FieldKind::Color,
            )
            .group(FieldGroup::Fill),
            SettingsField::new(
                "block.midline",
                msg("drawing.orderblock.blockMidline", "Midline"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Line),
            SettingsField::new(
                "block.border",
                msg("drawing.orderblock.blockBorder", "Border"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Line),
            SettingsField::new(
                "block.fade",
                msg("drawing.orderblock.blockFade", "Fade at end"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Behavior)
            .when("block.extend", strings(&["tested", "half", "broken"])),
            SettingsField::new(
                "block.breaker",
                msg("drawing.orderblock.blockBreaker", "Breaker"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Behavior)
            .when("block.extend", strings(&["broken"])),
        ])
    }

    fn write_props(&self) -> Option<Props> {
        let mut props = Props::new();
        self.block.zone.write(&mut props);
        props.insert("range".into(), json!(self.block.range.as_str()));
        props.insert("breaker".into(), json!(self.block.breaker));
        Some(props)
    }

    fn read_props(&mut self, props: &Props) {
        let d = BlockSettings::default();
        self.block = BlockSettings {
            zone: ZoneSettings::read(props, &d.zone, &BLOCK_EXTENDS),
            range: match props.get("range").and_then(Value::as_str) {
                Some("body") => BlockRange::Body,
                Some("wick") => BlockRange::Wick,
                _ => d.range,
            },
            breaker: read_bool(props, "breaker", d.breaker),
        };
    }

    fn apply_setting(&mut self, path: &str, value: &Value) {
        if !apply_nested_setting(self, "block", path, value) {
            crate::drawings::drawing::apply_setting_default(self, path, value);
        }
    }
}

// ── liquidity ──

/// How far right a liquidity level runs. A close beyond it takes it too, so
/// "until swept" also stops at a break.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiquidityExtend {
    Swept,
    Broken,
    Always,
}

impl LiquidityExtend {
    fn as_str(self) -> &'static str {
        match self {
            LiquidityExtend::Swept => "swept",
            LiquidityExtend::Broken => "broken",
            LiquidityExtend::Always => "always",
        }
    }
}

/// A liquidity level's settings (round-trip through `props`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiquiditySettings {
    pub extend: LiquidityExtend,
    /// Mark the candle that swept the level.
    pub mark_sweep: bool,
}

impl Default for LiquiditySettings {
    fn default() -> Self {
        Self {
            extend: LiquidityExtend::Swept,
            mark_sweep: true,
        }
    }
}

/// A liquidity level in pixels, ready to paint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiquidityGeometry {
    pub x1: f64,
    pub x2: f64,
    pub y: f64,
    /// Where the level was swept: just past the sweeping candle's wick.
    pub sweep: Option<Marker>,
    pub handle: (f64, f64),
}

/// Liquidity resting above a candle's high (buy-side) or below its low
/// (sell-side): the stops price tends to run before turning. Placed with one
/// click on the candle, nearer its high or its low; the level runs right
/// until price sweeps it or closes beyond it, and the sweep is marked.
#[derive(Clone, Debug)]
pub struct Liquidity {
    base: DrawingBase,
    pub liq: LiquiditySettings,
}

impl Liquidity {
    pub const TYPE: &'static str = "liquidity";

    pub fn new(init: &DrawingInit) -> Self {
        let mut d = Self {
            base: DrawingBase::new(init),
            liq: LiquiditySettings::default(),
        };
        if let Some(props) = &init.props {
            d.read_props(props);
        }
        d
    }

    /// The level on the candles: the side nearer the anchor's price, and what price did to it.
    pub fn state(&self, proj: &dyn Projector) -> Option<LiquidityState> {
        let c = candles(&self.base, proj)?;
        let a = self.base.anchors.first()?;
        let bar = c.bars.get(c.at)?;
        let side = if a.price >= (bar.high + bar.low) / 2.0 {
            LiquiditySide::High
        } else {
            LiquiditySide::Low
        };
        liquidity_state(&c.bars, c.at, side)
    }

    /// The level's pixel layout, or `None` when the candles hold none.
    pub fn line(&self, proj: &dyn Projector) -> Option<LiquidityGeometry> {
        let c = candles(&self.base, proj)?;
        let s = self.state(proj)?;
        let y = self.base.y_of(s.price, proj)?;
        let bars = &c.bars;
        let LiquiditySettings { extend, mark_sweep } = self.liq;
        let end = match extend {
            LiquidityExtend::Always => None,
            LiquidityExtend::Broken => s.broken,
            LiquidityExtend::Swept => s.swept.or(s.broken),
        };
        let x1 = proj.x_of(bars.get(s.index)?.time);
        let mut sweep = None;
        if let Some(swept) = s
            .swept
            .filter(|swept| mark_sweep && end.is_none_or(|e| *swept <= e))
        {
            let b = bars.get(swept)?;
            let wick = self.base.y_of(if s.high { b.high } else { b.low }, proj);
            if let Some(wick) = wick {
                sweep = Some(Marker {
                    x: proj.x_of(b.time),
                    y: wick + if s.high { -9.0 } else { 9.0 },
                });
            }
        }
        Some(LiquidityGeometry {
            x1,
            x2: right_edge(proj, bars, end)?,
            y,
            sweep,
            handle: (x1, y),
        })
    }
}

impl Drawing for Liquidity {
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
        // The anchor's price picks the side: nearer the candle's high or its low.
        candle_anchor_schema(FreeAxis::Both)
    }

    fn hit_test(&self, px: f64, py: f64, proj: &dyn Projector, tol: f64) -> bool {
        self.line(proj)
            .is_some_and(|l| px >= l.x1 - tol && px <= l.x2 + tol && (py - l.y).abs() <= tol)
    }

    fn handle_points(&self, proj: &dyn Projector) -> Vec<(f64, f64)> {
        match self.line(proj) {
            Some(l) => vec![l.handle],
            None => anchor_handle(&self.base, proj),
        }
    }

    fn bounds(&self, proj: &dyn Projector) -> Option<Bounds> {
        let l = self.line(proj)?;
        Some(Bounds {
            x: l.x1,
            y: l.y - 4.0,
            w: js_max(1.0, js_min(proj.width(), l.x2) - l.x1),
            h: 8.0,
        })
    }

    fn price_range(&self) -> Option<Span> {
        None
    }

    fn schema(&self) -> SettingsSchema {
        let extend_options = vec![
            option(
                "swept",
                msg("drawing.liquidity.option.swept", "Until swept"),
            ),
            option(
                "broken",
                msg("drawing.liquidity.option.broken", "Until broken"),
            ),
            option("always", msg("drawing.liquidity.option.always", "Always")),
        ];
        let mut fields = line_fields();
        fields.push(
            SettingsField::new(
                "liq.extend",
                msg("drawing.liquidity.liqExtend", "Extend right"),
                FieldKind::Select,
            )
            .options(extend_options)
            .group(FieldGroup::Behavior),
        );
        fields.push(
            SettingsField::new(
                "liq.markSweep",
                msg("drawing.liquidity.liqMarkSweep", "Mark sweep"),
                FieldKind::Boolean,
            )
            .group(FieldGroup::Behavior),
        );
        SettingsSchema::new(fields)
    }

    fn write_props(&self) -> Option<Props> {
        let mut props = Props::new();
        props.insert("extend".into(), json!(self.liq.extend.as_str()));
        props.insert("markSweep".into(), json!(self.liq.mark_sweep));
        Some(props)
    }

    fn read_props(&mut self, props: &Props) {
        let d = LiquiditySettings::default();
        self.liq = LiquiditySettings {
            extend: match props.get("extend").and_then(Value::as_str) {
                Some("swept") => LiquidityExtend::Swept,
                Some("broken") => LiquidityExtend::Broken,
                Some("always") => LiquidityExtend::Always,
                _ => d.extend,
            },
            mark_sweep: read_bool(props, "markSweep", d.mark_sweep),
        };
    }

    fn apply_setting(&mut self, path: &str, value: &Value) {
        if !apply_nested_setting(self, "liq", path, value) {
            crate::drawings::drawing::apply_setting_default(self, path, value);
        }
    }
}

/// The registry entries of this family.
pub fn metas() -> Vec<DrawingTypeMeta> {
    vec![
        DrawingTypeMeta::new(
            FairValueGap::TYPE,
            "priceaction",
            msg("drawing.fairvaluegap.label", "Fair Value Gap"),
            svg24(
                r#"<path d="M6 15v6M12 3v18M18 2v6"/><rect x="3.5" y="9.5" width="17" height="5" rx="1" fill="currentColor" fill-opacity="0.28" stroke-width="1.4"/>"#,
            ),
            DrawingStyle::line(BULLISH, 1.0, LineStyle::Solid),
            |init| Box::new(FairValueGap::new(init)),
        )
        .placement_hint(msg(
            "drawing.fairvaluegap.placementHint",
            "Click the middle candle of a gap",
        )),
        DrawingTypeMeta::new(
            OrderBlock::TYPE,
            "priceaction",
            msg("drawing.orderblock.label", "Order Block"),
            svg24(
                r#"<rect x="4" y="7.5" width="5" height="8" rx="1"/><path d="M6.5 4v3.5M6.5 15.5V19"/><rect x="9" y="7.5" width="11.5" height="8" rx="1" fill="currentColor" fill-opacity="0.22" stroke-width="1.4"/>"#,
            ),
            DrawingStyle::line(BULLISH, 1.0, LineStyle::Solid),
            |init| Box::new(OrderBlock::new(init)),
        )
        .placement_hint(msg(
            "drawing.orderblock.placementHint",
            "Click the candle that forms the block",
        )),
        DrawingTypeMeta::new(
            Liquidity::TYPE,
            "priceaction",
            msg("drawing.liquidity.label", "Liquidity"),
            svg24(
                r#"<path d="M3 8h12" stroke-dasharray="2.5 2.5"/><path d="m16.5 4.5 4 4m0-4-4 4"/><path d="M3.5 20 8 13.5l4 3.5 5-8"/>"#,
            ),
            DrawingStyle::line(WARNING, 1.0, LineStyle::Solid),
            |init| Box::new(Liquidity::new(init)),
        )
        .placement_hint(msg(
            "drawing.liquidity.placementHint",
            "Click near a candle's high or low",
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawings::geometry::DrawingPoint;
    use crate::drawings::projector::LinearProjector;

    const MIN5: f64 = 300_000.0;

    /// Candles from [open, high, low, close] rows, five minutes apart.
    fn rows(rows: &[[f64; 4]]) -> Vec<PriceActionBar> {
        rows.iter()
            .enumerate()
            .map(|(i, r)| PriceActionBar {
                time: i as f64 * MIN5,
                open: r[0],
                high: r[1],
                low: r[2],
                close: r[3],
            })
            .collect()
    }

    fn ohlcv(bars: &[PriceActionBar]) -> Vec<Ohlcv> {
        bars.iter()
            .map(|b| Ohlcv::new(b.time as i64, b.open, b.high, b.low, b.close, None))
            .collect()
    }

    /// x = time / 5 min * 10 px, y = 300 - price * 2, 400 x 300.
    fn proj(bars: &[PriceActionBar]) -> LinearProjector {
        let mut p = LinearProjector::sample();
        p.ms_per_px = MIN5 / 10.0;
        p.price_top = 150.0;
        p.price_per_px = 0.5;
        p.width = 400.0;
        p.height = 300.0;
        p.with_bars(ohlcv(bars))
    }

    fn patch(pairs: &[(&str, Value)]) -> Props {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn at(time: f64, price: f64) -> DrawingPoint {
        DrawingPoint { time, price }
    }

    // A bullish gap at bar 2: bar 1's high (102) stays below bar 3's low (104).
    fn gap_up() -> Vec<PriceActionBar> {
        rows(&[
            [100.0, 101.0, 99.0, 100.5],
            [100.5, 102.0, 100.0, 101.8],
            [101.8, 106.0, 100.8, 105.5],
            [105.5, 107.0, 104.0, 106.5],
            [106.5, 107.5, 104.5, 107.0],
            [107.0, 107.2, 103.5, 104.0],
            [104.0, 106.0, 102.8, 103.0],
            [103.0, 104.0, 101.5, 101.8],
            [101.8, 103.0, 101.0, 101.2],
        ])
    }

    // A down-close candle at bar 1 (a bullish block: body 98-100), then price leaves, returns and breaks.
    fn block() -> Vec<PriceActionBar> {
        rows(&[
            [99.0, 100.0, 98.5, 99.5],
            [100.0, 100.5, 97.5, 98.0],
            [98.0, 103.0, 97.8, 102.5],
            [102.5, 105.0, 101.0, 104.5],
            [104.5, 105.0, 99.5, 100.5],
            [100.5, 101.0, 98.8, 99.0],
            [99.0, 99.2, 96.0, 96.5],
            [96.5, 97.5, 95.0, 97.0],
            [97.0, 98.5, 96.5, 98.2],
        ])
    }

    // A swing high at bar 2 (high 105), swept by bar 5's wick, closed above by bar 7.
    fn sweep() -> Vec<PriceActionBar> {
        rows(&[
            [100.0, 101.0, 99.0, 100.5],
            [100.5, 103.0, 100.0, 102.5],
            [102.5, 105.0, 102.0, 103.0],
            [103.0, 103.5, 100.0, 100.5],
            [100.5, 102.0, 99.5, 101.5],
            [101.5, 105.6, 101.0, 104.0],
            [104.0, 104.8, 102.0, 103.0],
            [103.0, 106.0, 102.5, 105.5],
        ])
    }

    fn make(key: &str, anchors: Vec<DrawingPoint>, props: Option<Props>) -> Box<dyn Drawing> {
        let m = metas()
            .into_iter()
            .find(|m| m.type_key == key)
            .expect("registered");
        let mut init = DrawingInit::on_pane("price").anchors(anchors);
        init.style = Some(m.default_style.to_partial());
        init.props = props;
        (m.create)(&init)
    }

    fn fvg(anchors: Vec<DrawingPoint>, props: Option<Props>) -> FairValueGap {
        FairValueGap::new(&DrawingInit {
            props,
            ..DrawingInit::on_pane("price").anchors(anchors)
        })
    }

    fn ob(anchors: Vec<DrawingPoint>, props: Option<Props>) -> OrderBlock {
        OrderBlock::new(&DrawingInit {
            props,
            ..DrawingInit::on_pane("price").anchors(anchors)
        })
    }

    fn liq(anchors: Vec<DrawingPoint>) -> Liquidity {
        Liquidity::new(&DrawingInit::on_pane("price").anchors(anchors))
    }

    fn exercise(d: &dyn Drawing, proj: &LinearProjector) {
        let _ = d.hit_test(5.0, 5.0, proj, 4.0);
        let _ = d.handle_points(proj);
        let _ = d.hit_handle(5.0, 5.0, proj, 4.0);
        let _ = d.bounds(proj);
        let _ = d.price_range();
        let _ = d.time_extent();
        let _ = d.schema();
        let _ = d.write_props();
        let _ = d.serialize();
    }

    #[test]
    fn registry_entries_and_survival_without_anchors_or_bars() {
        assert_eq!(metas().len(), 3);
        let with_bars = proj(&gap_up());
        let no_bars = LinearProjector::sample();
        for key in ["fairvaluegap", "orderblock", "liquidity"] {
            let m = metas().into_iter().find(|m| m.type_key == key).unwrap();
            assert_eq!(m.group, "priceaction");
            assert!(m.placement_hint.is_some());
            assert!(!m.covers_series);
            assert_eq!(m.default_style.line_width, 1.0);
            for anchors in [vec![], vec![at(2.0 * MIN5, 103.0)]] {
                let d = make(key, anchors, None);
                assert_eq!(d.type_key(), key);
                let schema = d.anchor_schema();
                assert_eq!((schema.min, schema.max), (1, 1));
                assert_eq!(schema.slots[0].role, "candle");
                exercise(d.as_ref(), &with_bars);
                exercise(d.as_ref(), &no_bars);
            }
            let d = make(key, vec![at(2.0 * MIN5, 103.0)], None);
            let ser = d.serialize();
            assert!(ser.props.is_some());
            let again = (m.create)(&DrawingInit::from(ser.clone()));
            assert_eq!(again.serialize(), ser);
        }
        let key = |k: &str| metas().into_iter().find(|m| m.type_key == k).unwrap();
        assert_eq!(key("fairvaluegap").default_style.line_color, BULLISH);
        assert_eq!(key("orderblock").default_style.line_color, BULLISH);
        assert_eq!(key("liquidity").default_style.line_color, WARNING);
        assert_eq!(
            make("fairvaluegap", vec![], None).anchor_schema().slots[0].free,
            FreeAxis::X
        );
        assert_eq!(
            make("liquidity", vec![], None).anchor_schema().slots[0].free,
            FreeAxis::Both
        );
    }

    #[test]
    fn without_series_access_a_tool_falls_back_to_its_anchor() {
        let p = LinearProjector::sample();
        let d = fvg(vec![at(600_000.0, 150.0)], None);
        assert_eq!(d.zone(&p), None);
        // x = 10, y = 200
        assert_eq!(d.handle_points(&p), [(10.0, 200.0)]);
        assert!(d.hit_test(11.0, 0.0, &p, 2.0));
        assert!(!d.hit_test(20.0, 0.0, &p, 2.0));
        assert_eq!(d.bounds(&p), None);
        let l = liq(vec![at(600_000.0, 150.0)]);
        assert_eq!(l.line(&p), None);
        assert_eq!(l.handle_points(&p), [(10.0, 200.0)]);
        assert!(!l.hit_test(10.0, 200.0, &p, 5.0));
        // an empty series is no series
        let empty = LinearProjector::sample().with_bars(vec![]);
        assert_eq!(d.zone(&empty), None);
    }

    #[test]
    fn a_gap_is_the_space_between_the_first_high_and_third_low() {
        let g = gap_up();
        assert_eq!(
            gap_at(&g, 2),
            Some(Gap {
                bull: true,
                top: 104.0,
                bottom: 102.0
            })
        );
        assert_eq!(gap_at(&g, 1), None);
        assert_eq!(gap_at(&g, 0), None);
        assert_eq!(gap_at(&g, 8), None);
        assert_eq!(gap_at(&g, 99), None);
        let bear = rows(&[
            [100.0, 101.0, 99.0, 99.5],
            [99.5, 99.8, 96.0, 96.5],
            [96.5, 97.0, 94.0, 95.0],
        ]);
        assert_eq!(
            gap_at(&bear, 1),
            Some(Gap {
                bull: false,
                top: 99.0,
                bottom: 97.0
            })
        );
    }

    #[test]
    fn a_click_a_candle_or_two_off_finds_the_nearest_gap() {
        let g = gap_up();
        assert_eq!(nearest_gap(&g, 4).map(|n| n.index), Some(2));
        assert_eq!(nearest_gap(&g, 0).map(|n| n.index), Some(2));
        assert_eq!(nearest_gap(&g, 8), None);
        assert_eq!(nearest_gap(&[], 0), None);
    }

    #[test]
    fn a_click_between_candles_lands_on_the_nearest_one() {
        let g = gap_up();
        assert_eq!(nearest_bar_index(&g, 2.0 * MIN5 + MIN5 * 0.4), Some(2));
        assert_eq!(nearest_bar_index(&g, 2.0 * MIN5 + MIN5 * 0.6), Some(3));
        assert_eq!(nearest_bar_index(&g, -1e9), Some(0));
        assert_eq!(nearest_bar_index(&g, 1e12), Some(g.len() - 1));
        assert_eq!(nearest_bar_index(&g, f64::NAN), Some(0));
        assert_eq!(nearest_bar_index(&[], 0.0), None);
    }

    #[test]
    fn a_gap_records_what_price_did_to_it() {
        let s = gap_state(&gap_up(), 2).unwrap();
        assert_eq!(
            [s.tested, s.half, s.filled, s.inverted],
            [Some(5), Some(6), Some(7), Some(7)]
        );
        assert_eq!(s.mid, 103.0);
        assert_eq!(gap_state(&gap_up(), 8), None);
    }

    #[test]
    fn each_extension_stops_the_gap_at_its_own_event() {
        let bars = gap_up();
        let p = proj(&bars);
        let mut d = fvg(vec![at(2.0 * MIN5, 103.0)], None);
        let mut right = |extend: &str| {
            d.apply_settings(&patch(&[("gap.extend", json!(extend))]));
            d.zone(&p).unwrap().x2
        };
        assert_eq!(
            [right("tested"), right("half"), right("filled")],
            [55.0, 65.0, 75.0]
        );
        assert_eq!(right("always"), 400.0);
    }

    #[test]
    fn a_gap_on_the_chart_spans_from_the_first_candle_to_the_fill() {
        let bars = gap_up();
        let p = proj(&bars);
        let d = fvg(vec![at(2.0 * MIN5, 103.0)], None);
        let z = d.zone(&p).unwrap();
        assert_eq!(z.x1, 5.0);
        assert_eq!(z.x2, 75.0);
        assert_eq!(
            (z.y_top, z.y_bottom),
            (300.0 - 104.0 * 2.0, 300.0 - 102.0 * 2.0)
        );
        assert_eq!(z.mid_y, Some(300.0 - 103.0 * 2.0));
        assert_eq!(
            z.marker,
            Some(Marker {
                x: 70.0,
                y: 300.0 - 102.0 * 2.0
            })
        );
        assert!(z.faded);
        assert!(z.flip.is_none());
        assert!(!z.border);
        assert_eq!(z.fill, format!("{BULLISH}33"));
        assert_eq!(z.handle, (20.0, 94.0));
        assert!(d.hit_test(40.0, 300.0 - 103.0 * 2.0, &p, 2.0));
        assert!(!d.hit_test(90.0, 300.0 - 103.0 * 2.0, &p, 2.0));
        assert_eq!(d.handle_points(&p), [(20.0, 94.0)]);
        assert_eq!(
            d.bounds(&p),
            Some(Bounds {
                x: 5.0,
                y: 92.0,
                w: 70.0,
                h: 4.0
            })
        );
        assert_eq!(d.price_range(), None);
    }

    #[test]
    fn a_gap_set_to_flip_carries_on_the_other_way() {
        let bars = gap_up();
        let p = proj(&bars);
        let d = fvg(
            vec![at(2.0 * MIN5, 103.0)],
            Some(patch(&[("inverse", json!(true))])),
        );
        let z = d.zone(&p).unwrap();
        let flip = z.flip.expect("flipped");
        assert_eq!(flip.x1, 65.0);
        assert_eq!(flip.x2, 400.0);
        assert_eq!(flip.fill, d.gap.zone.down_color);
        assert!(!z.faded);
        assert_eq!(z.marker, None);
        // the flipped box is hit-testable and bounds run to the plot edge
        assert!(d.hit_test(200.0, 94.0, &p, 2.0));
        assert_eq!(d.bounds(&p).map(|b| b.x + b.w), Some(400.0));
    }

    #[test]
    fn with_no_gap_near_the_candle_the_guide_still_selects_it() {
        let bars = gap_up();
        let p = proj(&bars);
        let d = fvg(vec![at(8.0 * MIN5, 101.0)], None);
        assert_eq!(d.zone(&p), None);
        assert!(d.hit_test(80.0, 50.0, &p, 2.0));
        assert!(!d.hit_test(60.0, 50.0, &p, 2.0));
        assert_eq!(d.bounds(&p), None);
    }

    #[test]
    fn a_down_close_candle_marks_demand() {
        let b = block();
        let s = block_state(&b, 1, BlockRange::Body).unwrap();
        assert!(s.bull);
        assert_eq!((s.top, s.bottom), (100.0, 98.0));
        let w = block_state(&b, 1, BlockRange::Wick).unwrap();
        assert_eq!((w.top, w.bottom), (100.5, 97.5));
        assert!(!block_state(&b, 0, BlockRange::Body).unwrap().bull);
        assert_eq!(block_state(&b, 99, BlockRange::Body), None);
    }

    #[test]
    fn a_block_is_tested_half_filled_broken_and_retested() {
        let s = block_state(&block(), 1, BlockRange::Body).unwrap();
        assert_eq!(
            [s.tested, s.half, s.broken, s.retest],
            [Some(4), Some(5), Some(6), Some(8)]
        );
    }

    #[test]
    fn a_block_closed_through_before_price_left_it_is_broken_all_the_same() {
        let failed = rows(&[
            [99.0, 100.0, 98.5, 99.5],
            [100.0, 100.5, 97.5, 98.0],
            [98.0, 99.0, 95.0, 95.5],
        ]);
        let s = block_state(&failed, 1, BlockRange::Body).unwrap();
        assert_eq!((s.tested, s.broken), (None, Some(2)));
    }

    #[test]
    fn a_broken_block_carries_on_as_a_breaker() {
        let bars = block();
        let p = proj(&bars);
        let mut d = ob(vec![at(MIN5, 99.0)], None);
        let z = d.zone(&p).unwrap();
        assert_eq!(z.x2, 65.0);
        let flip = z.flip.clone().expect("breaker");
        assert_eq!((flip.x1, flip.x2), (55.0, 85.0));
        assert_eq!(flip.fill, d.block.zone.down_color);
        assert!(z.border);
        assert_eq!(z.fill, format!("{BULLISH}29"));
        d.apply_settings(&patch(&[("block.breaker", json!(false))]));
        assert_eq!(d.zone(&p).unwrap().flip, None);
        // without a breaker the zone fades and marks where it broke
        let z = d.zone(&p).unwrap();
        assert!(z.faded);
        assert_eq!(
            z.marker,
            Some(Marker {
                x: 60.0,
                y: 300.0 - 98.0 * 2.0
            })
        );
    }

    #[test]
    fn the_block_range_setting_widens_the_zone() {
        let bars = block();
        let p = proj(&bars);
        let mut d = ob(vec![at(MIN5, 99.0)], None);
        let body = d.zone(&p).unwrap();
        d.apply_settings(&patch(&[("block.range", json!("wick"))]));
        let wick = d.zone(&p).unwrap();
        assert!(wick.y_bottom - wick.y_top > body.y_bottom - body.y_top);
        assert_eq!(wick.y_top, 300.0 - 100.5 * 2.0);
    }

    #[test]
    fn liquidity_is_swept_by_a_wick_and_broken_by_a_close() {
        let s = liquidity_state(&sweep(), 2, LiquiditySide::High).unwrap();
        assert_eq!((s.price, s.swept, s.broken), (105.0, Some(5), Some(7)));
        let low = liquidity_state(&sweep(), 4, LiquiditySide::Low).unwrap();
        assert_eq!(low.price, 99.5);
        assert!(!low.high);
        assert_eq!(liquidity_state(&sweep(), 40, LiquiditySide::Low), None);
    }

    #[test]
    fn a_close_beyond_breaks_liquidity_without_a_sweep() {
        let breakout = rows(&[
            [100.0, 101.0, 99.0, 100.5],
            [100.5, 105.0, 100.0, 104.0],
            [104.0, 108.0, 103.5, 107.5],
        ]);
        let s = liquidity_state(&breakout, 1, LiquiditySide::High).unwrap();
        assert_eq!((s.swept, s.broken), (None, Some(2)));
        let p = proj(&breakout);
        let l = liq(vec![at(MIN5, 104.5)]).line(&p).unwrap();
        assert_eq!(l.x2, 25.0);
        assert_eq!(l.sweep, None);
    }

    #[test]
    fn the_click_height_picks_the_buy_or_sell_side() {
        let bars = sweep();
        let p = proj(&bars);
        let hi = liq(vec![at(2.0 * MIN5, 104.5)]).state(&p).unwrap();
        let lo = liq(vec![at(2.0 * MIN5, 102.2)]).state(&p).unwrap();
        assert!(hi.high);
        assert_eq!(hi.price, 105.0);
        assert!(!lo.high);
        assert_eq!(lo.price, 102.0);
    }

    #[test]
    fn the_level_runs_to_the_sweep_which_is_marked_past_the_wick() {
        let bars = sweep();
        let p = proj(&bars);
        let d = liq(vec![at(2.0 * MIN5, 104.5)]);
        let l = d.line(&p).unwrap();
        assert_eq!((l.x1, l.x2, l.y), (20.0, 55.0, 300.0 - 105.0 * 2.0));
        let s = l.sweep.unwrap();
        assert_eq!(s.x, 50.0);
        assert!((s.y - (300.0 - 105.6 * 2.0 - 9.0)).abs() < 1e-9);
        assert_eq!(l.handle, (20.0, 90.0));
        assert!(d.hit_test(40.0, l.y, &p, 3.0));
        assert!(!d.hit_test(40.0, l.y + 20.0, &p, 3.0));
        assert_eq!(
            d.bounds(&p),
            Some(Bounds {
                x: 20.0,
                y: 86.0,
                w: 35.0,
                h: 8.0
            })
        );
        assert_eq!(d.price_range(), None);
    }

    #[test]
    fn liquidity_extensions_and_sweep_marking() {
        let bars = sweep();
        let p = proj(&bars);
        let mut d = liq(vec![at(2.0 * MIN5, 104.5)]);
        d.apply_settings(&patch(&[("liq.extend", json!("broken"))]));
        assert_eq!(d.line(&p).unwrap().x2, 75.0);
        assert!(d.line(&p).unwrap().sweep.is_some());
        d.apply_settings(&patch(&[("liq.extend", json!("always"))]));
        assert_eq!(d.line(&p).unwrap().x2, 400.0);
        d.apply_settings(&patch(&[("liq.markSweep", json!(false))]));
        assert_eq!(d.line(&p).unwrap().sweep, None);
        // a sweep past the stop is not marked
        d.apply_settings(&patch(&[
            ("liq.markSweep", json!(true)),
            ("liq.extend", json!("swept")),
        ]));
        let sell = liq(vec![at(4.0 * MIN5, 99.6)]);
        let l = sell.line(&p).unwrap();
        assert_eq!(l.y, 300.0 - 99.5 * 2.0);
        assert_eq!(l.sweep, None);
    }

    #[test]
    fn settings_survive_a_save_and_a_restore() {
        let mut d = fvg(vec![at(0.0, 1.0)], None);
        d.apply_settings(&patch(&[
            ("gap.extend", json!("half")),
            ("gap.border", json!(true)),
            ("gap.upColor", json!("#00ff0040")),
        ]));
        let back = FairValueGap::new(&DrawingInit::from(d.serialize()));
        assert_eq!(back.gap.zone.extend, ZoneExtend::Half);
        assert!(back.gap.zone.border);
        assert_eq!(back.gap.zone.up_color, "#00ff0040");
        assert_eq!(
            d.write_props()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [
                "border",
                "downColor",
                "extend",
                "fade",
                "inverse",
                "midline",
                "upColor"
            ]
        );
    }

    #[test]
    fn unknown_or_mistyped_props_fall_back_to_the_defaults() {
        let d = ob(
            vec![at(0.0, 1.0)],
            Some(patch(&[
                ("extend", json!("nope")),
                ("range", json!(7)),
                ("border", json!("yes")),
                ("upColor", json!(3)),
            ])),
        );
        assert_eq!(d.block, BlockSettings::default());
        // `filled` is a gap option, not a block one
        let d = ob(vec![], Some(patch(&[("extend", json!("filled"))])));
        assert_eq!(d.block.zone.extend, ZoneExtend::Broken);
        let g = fvg(vec![], Some(patch(&[("extend", json!("broken"))])));
        assert_eq!(g.gap.zone.extend, ZoneExtend::Filled);
        let l = Liquidity::new(&DrawingInit::on_pane("price").props(patch(&[
            ("extend", json!("filled")),
            ("markSweep", json!(false)),
        ])));
        assert_eq!(
            l.liq,
            LiquiditySettings {
                extend: LiquidityExtend::Swept,
                mark_sweep: false
            }
        );
    }

    #[test]
    fn settings_outside_the_tool_prefix_use_the_shared_rules() {
        let mut d = fvg(vec![], None);
        d.apply_settings(&patch(&[
            ("locked", json!(true)),
            ("style.lineColor", json!("#123456")),
            ("gap.deep.key", json!(1)),
        ]));
        assert!(d.base().locked);
        assert_eq!(d.base().style.line_color, "#123456");
        assert_eq!(d.gap, GapSettings::default());
    }

    #[test]
    fn options_that_matter_only_in_some_modes_say_which() {
        let schema = fvg(vec![], None).schema();
        let inverse = schema
            .fields
            .iter()
            .find(|f| f.path == "gap.inverse")
            .unwrap();
        let when = inverse.when.as_ref().unwrap();
        assert_eq!(when.path, "gap.extend");
        assert_eq!(when.any_of, [json!("filled")]);
        let fade = schema.fields.iter().find(|f| f.path == "gap.fade").unwrap();
        assert_eq!(fade.when.as_ref().unwrap().any_of.len(), 3);
        let block = ob(vec![], None).schema();
        assert_eq!(block.fields.len(), 8);
        let breaker = block
            .fields
            .iter()
            .find(|f| f.path == "block.breaker")
            .unwrap();
        assert_eq!(breaker.when.as_ref().unwrap().any_of, [json!("broken")]);
        let liquidity = liq(vec![]).schema();
        let paths: Vec<_> = liquidity.fields.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "style.lineColor",
                "style.lineWidth",
                "style.lineStyle",
                "liq.extend",
                "liq.markSweep"
            ]
        );
    }

    #[test]
    fn half_a_candle_is_three_px_with_a_single_bar() {
        let bars = rows(&[[1.0, 2.0, 0.5, 1.5]]);
        let p = proj(&bars);
        assert_eq!(half_bar(&p, &bars, 0), 3.0);
        assert_eq!(half_bar(&p, &[], 0), 3.0);
        let two = rows(&[[1.0, 2.0, 0.5, 1.5], [1.0, 2.0, 0.5, 1.5]]);
        assert_eq!(half_bar(&p, &two, 0), 5.0);
        assert_eq!(half_bar(&p, &two, 1), 5.0);
        assert_eq!(right_edge(&p, &two, None), Some(400.0));
        assert_eq!(right_edge(&p, &two, Some(9)), None);
    }
}
