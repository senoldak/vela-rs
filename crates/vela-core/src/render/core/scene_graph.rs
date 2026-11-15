//! The retained scene the backend renders: panes, the shared price bars, the
//! mounted indicator models (rendered by pane id), the per-indicator draw
//! order, and transient crosshair state.
//!
//! The original keeps its panes, indicators and channels in JavaScript `Map`s,
//! whose iteration order is insertion order and which tie-breaks several sorts;
//! [`InsertionMap`] keeps that order. Objects the original shares by reference
//! (a pane node, a model) are owned here: read them with `get`, change them
//! with `get_mut`.
//!
//! Left for the browser crate / the renderer: a [`DrawingSlice`]'s canvas is
//! the opaque [`CanvasImage`] handle (the original's plot-sized
//! `HTMLCanvasElement`), downcast by the backend that composites it. The
//! original's cached pane order (`orderedCache`) is not kept: pane lists are a
//! handful of entries, so [`SceneGraph::ordered_panes`] sorts on each call and
//! can never go stale.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::chart_types::BasePainting;
use crate::format::ResolvedFormatters;
use crate::marks::{MarkGroup, MarksDisplayState, TimelineMark};
use crate::model::volume_layers::{VolumeLayerData, VpvrLayerData};
use crate::model::{IndicatorModel, Millis, Ohlcv, PaneAxisBand, PaneKind};
use crate::render::canvas::CanvasImage;
use crate::render::shared::trade_markers::{TradeMarkersState, default_trade_markers_state};

use super::chart_config::{
    CandlePaintOverride, ChartStyle, CrosshairOverride, default_chart_style,
};
use super::coordinate_system::{PaneBounds, PriceScale};

/// A user-defined shaded time band spanning the full plot height (all panes):
/// the generic primitive behind session highlighting (weekends,
/// pre/regular/post). Unlike the engine-emitted `Background` (per-pane, from
/// Pine `bgcolor()`), highlights are renderer-owned and set via the
/// `highlights` feature.
#[derive(Clone, Debug, PartialEq)]
pub struct HighlightArea {
    /// Inclusive start, epoch ms.
    pub from: Millis,
    /// Exclusive end, epoch ms.
    pub to: Millis,
    pub color: String,
}

/// Session time bands (the `sessionZones` feature): `[start, end)` epoch-ms
/// pairs a host derives from its market calendar. Day-split tapes populate
/// `pre`/`post`; overnight roll tapes populate the single `extended` phase
/// instead. The renderer shades them with the config's session colors
/// (`ChartStyle.sessions`), behind grid + data. No zones at all
/// (`SceneGraph::session_zones == None`) means the market has no session
/// structure (continuous venues).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionZones {
    pub pre: Vec<(Millis, Millis)>,
    pub post: Vec<(Millis, Millis)>,
    pub extended: Vec<(Millis, Millis)>,
}

/// Price-axis display mode: absolute price, percent change vs a visible
/// baseline, or values indexed to 100 at that same baseline
/// (`index = price / baseline * 100`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScaleMode {
    #[default]
    Price,
    Percent,
    Indexed,
}

impl ScaleMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ScaleMode::Price => "price",
            ScaleMode::Percent => "percent",
            ScaleMode::Indexed => "indexed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "price" => Some(ScaleMode::Price),
            "percent" => Some(ScaleMode::Percent),
            "indexed" => Some(ScaleMode::Indexed),
            _ => None,
        }
    }
}

/// A percent/indexed axis descriptor for the tick + label formatters: the
/// visible baseline (a finite, non-zero reference value) plus which flavor.
/// `indexed: false` means percent change (`+2.34%`); `indexed: true` a plain
/// number indexed to 100 at the baseline. Absent means an absolute
/// (price/volume) axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PctScale {
    pub baseline: f64,
    pub indexed: bool,
}

/// One raster layer of drawings interleaved into a pane's series stack: user
/// drawings slotted by their own z, or an indicator's Pine drawings slotted at
/// their model's z: a prepainted plot-sized canvas the backend composites
/// immediately BEFORE the series whose z key is `before_z`, so its drawings sit
/// under that series and every series above it, but over everything painted
/// earlier (grid, fills, lower series).
#[derive(Clone)]
pub struct DrawingSlice {
    /// The z key of the series this layer paints in front of the grid but behind.
    pub before_z: f64,
    /// Plot-sized, dpr-scaled canvas holding just this layer's drawings.
    pub canvas: CanvasImage,
}

impl std::fmt::Debug for DrawingSlice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DrawingSlice")
            .field("before_z", &self.before_z)
            .finish_non_exhaustive()
    }
}

/// How a pane's axis labels read (see [`PaneNode::axis_format`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneAxisFormat {
    /// A volume indicator alone in its own pane: the scale abbreviates with K/M/B suffixes.
    Volume,
    /// A pane whose owning content declares a `paneAxis` override: price ticks,
    /// horizontal gridlines and the crosshair value chip are suppressed (the
    /// content is not value-mapped).
    NoAxis,
}

impl PaneAxisFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            PaneAxisFormat::Volume => "volume",
            PaneAxisFormat::NoAxis => "none",
        }
    }
}

/// A retained pane node. Stacked top-to-bottom by `order`; `height_weight`
/// sets relative height. `bounds` (pixel extent) is recomputed on resize;
/// `scale` (price window) is recomputed per frame by autoscale. Both are
/// consumed by every layer via the
/// [`CoordinateSystem`](super::CoordinateSystem).
#[derive(Clone, Debug, PartialEq)]
pub struct PaneNode {
    pub id: String,
    pub kind: PaneKind,
    pub order: i32,
    pub height_weight: f64,
    pub bounds: PaneBounds,
    /// Rendered price window (eased toward `scale_target` while animating).
    pub scale: PriceScale,
    /// Autoscale result; `scale` snaps to it when idle, glides during zoom/fling.
    pub scale_target: PriceScale,
    /// False until `scale` has been snapped to a real target once, so a pane
    /// added mid-animation seeds its scale instead of easing from the `{0,1}`
    /// placeholder.
    pub initialized: bool,
    /// User-set price window (price-axis drag / vertical pan). When set the
    /// pane STOPS autoscaling and renders this window verbatim; cleared (back
    /// to autoscale) by a double-click reset / re-fit.
    pub manual_scale: Option<PriceScale>,
    /// Collapsed to a thin strip (legend + expand affordance): laid out at a
    /// fixed small height, keeping its weight so expanding restores its proportion.
    pub collapsed: bool,
    /// How this pane's axis labels read; `None` is the default price format.
    /// Recomputed per autoscale pass, so it clears the moment the pane's
    /// content changes.
    pub axis_format: Option<PaneAxisFormat>,
    /// Categorical axis labels for a `paneAxis`-overridden pane (band labels at
    /// fractions of the pane's height, drawn instead of price ticks).
    /// Recomputed per autoscale pass alongside `axis_format`.
    pub axis_bands: Option<Vec<PaneAxisBand>>,
    /// This STUDY pane's own axis mode: `Price` (absolute) or `Percent` (change
    /// vs its own `percent_baseline`). The PRICE pane instead follows the
    /// scene-level `scale_mode` (persisted chart setting), so every pane's
    /// scale is independent. `None` is `Price`.
    pub scale_mode: Option<ScaleMode>,
    /// This STUDY pane's own logarithmic flag (the PRICE pane follows the
    /// scene-level `log_scale`). `None` is linear.
    pub log_scale: Option<bool>,
    /// This STUDY pane's own inverted-axis flag (high at the bottom); the PRICE
    /// pane follows the scene-level `invert_scale`. `None` is normal orientation.
    pub invert: Option<bool>,
    /// Reference value for THIS pane's percent mode (its first visible value),
    /// recomputed per frame. On the price pane it is the first visible bar
    /// close; on a study pane, its master series' first visible value.
    pub percent_baseline: f64,
}

impl PaneNode {
    fn new(id: &str, kind: PaneKind, order: i32, height_weight: Option<f64>) -> Self {
        Self {
            id: id.to_string(),
            kind,
            order,
            height_weight: height_weight.unwrap_or(match kind {
                PaneKind::Price => 3.0,
                PaneKind::Study => 1.0,
            }),
            bounds: PaneBounds {
                top: 0.0,
                height: 0.0,
            },
            scale: PriceScale::new(0.0, 1.0),
            scale_target: PriceScale::new(0.0, 1.0),
            initialized: false,
            manual_scale: None,
            collapsed: false,
            axis_format: None,
            axis_bands: None,
            scale_mode: None,
            log_scale: None,
            invert: None,
            percent_baseline: 0.0,
        }
    }
}

/// A merged (own-scale) indicator's private price window inside a shared pane.
#[derive(Clone, Debug, PartialEq)]
pub struct IndicatorScale {
    pub scale: PriceScale,
    pub scale_target: PriceScale,
    pub initialized: bool,
    pub manual_scale: Option<PriceScale>,
}

/// A map that iterates in insertion order, like a JavaScript `Map`. Setting an
/// existing key replaces its value in place (the key keeps its position).
/// Lookups are linear: the scene's maps hold a handful of entries.
#[derive(Clone, Debug)]
pub struct InsertionMap<V> {
    entries: Vec<(String, V)>,
}

impl<V> Default for InsertionMap<V> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<V> InsertionMap<V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn position(&self, key: &str) -> Option<usize> {
        self.entries.iter().position(|(k, _)| k == key)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.position(key).is_some()
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.position(key).map(|i| &self.entries[i].1)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.position(key).map(|i| &mut self.entries[i].1)
    }

    /// Set `key`, returning the value it replaced.
    pub fn insert(&mut self, key: impl Into<String>, value: V) -> Option<V> {
        let key = key.into();
        match self.position(&key) {
            Some(i) => Some(std::mem::replace(&mut self.entries[i].1, value)),
            None => {
                self.entries.push((key, value));
                None
            }
        }
    }

    /// The value at `key`, creating it first when absent; the flag says whether it was created.
    pub fn get_or_insert_with(&mut self, key: &str, create: impl FnOnce() -> V) -> (&mut V, bool) {
        let (i, created) = match self.position(key) {
            Some(i) => (i, false),
            None => {
                self.entries.push((key.to_string(), create()));
                (self.entries.len() - 1, true)
            }
        };
        (&mut self.entries[i].1, created)
    }

    pub fn remove(&mut self, key: &str) -> Option<V> {
        self.position(key).map(|i| self.entries.remove(i).1)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(k, _)| k.as_str())
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.iter().map(|(_, v)| v)
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.entries.iter_mut().map(|(_, v)| v)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&str, &mut V)> {
        self.entries.iter_mut().map(|(k, v)| (k.as_str(), v))
    }
}

impl<V> FromIterator<(String, V)> for InsertionMap<V> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(iter: I) -> Self {
        let mut map = Self::new();
        for (k, v) in iter {
            map.insert(k, v);
        }
        map
    }
}

/// An `{ id, z }` entry of the indicator draw order.
#[derive(Clone, Debug, PartialEq)]
pub struct ZEntry {
    pub id: String,
    pub z: f64,
}

/// A content-less click's brief filled flash: the cluster key and the
/// frame-clock time it ends.
#[derive(Clone, Debug, PartialEq)]
pub struct MarksFlash {
    pub key: String,
    pub until: f64,
}

/// The crosshair's pixel position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CrosshairPoint {
    pub x: f64,
    pub y: f64,
}

/// The retained scene the backend renders. The original renders directly from
/// the indicator models each frame (immediate-mode, culled to the visible
/// range); the per-node diff is a later optimization.
pub struct SceneGraph {
    pub panes: InsertionMap<PaneNode>,
    pub indicators: InsertionMap<IndicatorModel>,
    pub bars: Vec<Ohlcv>,
    /// Volume-layer config pushed by the volume native indicator (`None`: layer off). Ephemeral.
    pub volume_layer: Option<VolumeLayerData>,
    /// Generic native-data channels for SDK renderer layers (`setNativeData(id, ...)`). Ephemeral.
    pub native_data: InsertionMap<Value>,
    /// Loading ranges per channel (`setNativeData(id + '-pending', ...)`). Ephemeral.
    pub native_pending: InsertionMap<Vec<(f64, f64)>>,
    /// VPVR-layer config pushed by the VPVR native indicator (`None`: layer off). Ephemeral.
    pub vpvr_layer: Option<VpvrLayerData>,
    pub crosshair: Option<CrosshairPoint>,
    /// Runtime override of the crosshair's lines and style (the `crosshairOverride` feature). Ephemeral.
    pub crosshair_override: Option<CrosshairOverride>,
    /// How the base price series is drawn on the price pane (candles by default).
    pub price_style: String,
    /// Price-series base painting for the ACTIVE style (see `ChartTypeDefinition::base_painting`).
    pub base_painting: BasePainting,
    /// The ACTIVE style's own candle cosmetics (`chartTypes.<id>.candle*`) when
    /// it is a candle-based plugin type; `None` paints with the shared
    /// `style.candle` block.
    pub candle_override: Option<CandlePaintOverride>,
    /// Explicit baseline reference price for the `baseline` style; when `None`
    /// the baseline follows `style.baseline.baseline_level` as a percent of the
    /// visible pane range (resolved per frame via
    /// [`baseline_price_for`](Self::baseline_price_for)).
    pub baseline_value: Option<f64>,
    /// Draw the dashed horizontal line at the latest price (price pane).
    /// Independent of the axis label chip (`show_price_label`): either can show
    /// without the other.
    pub show_price_line: bool,
    /// Draw the last-price label chip on the price axis. Independent of the line.
    pub show_price_label: bool,
    /// Draw the countdown-to-bar-close chip on the price axis. When the price
    /// label is also shown, the two merge into one stacked block (countdown
    /// under the label); when either shows alone it is centered on the latest
    /// price level.
    pub show_countdown: bool,
    /// Logarithmic price scale on the price pane.
    pub log_scale: bool,
    /// Inverted price axis on the price pane (high at the bottom). Study panes carry their own.
    pub invert_scale: bool,
    /// The price pane's autoscale fits the bars alone: indicator plots, their
    /// price lines and Pine drawings paint where they fall instead of widening the window.
    pub scale_price_only: bool,
    /// Exchange tick size for the active symbol (e.g. 0.01), when known. Drives
    /// the price-axis decimals: the instrument's true precision instead of the
    /// zoom-derived formula (the fallback until symbol metadata loads).
    pub price_mintick: Option<f64>,
    /// Price-axis mode on the price pane. Gridlines, axis labels and crosshair
    /// chip all follow it.
    pub scale_mode: ScaleMode,
    /// Reference price for percent mode (first visible bar's close); recomputed per frame.
    pub percent_baseline: f64,
    /// IANA time zone for the time axis + crosshair/data-window stamps (`"UTC"` default).
    pub timezone: String,
    /// The chart's formatters (`chart.i18n.format`) for axis labels, chips and
    /// readouts; `None` is the built-in English formatting (a renderer driven
    /// without a chart).
    pub format: Option<ResolvedFormatters>,
    /// Draw the background gridlines (price + time). Master toggle (`gridlines`
    /// feature); per-axis visibility + colors live in `style.grid_vert`/`grid_horz`.
    pub show_grid: bool,
    /// Comprehensive cosmetic config: grid colors, crosshair, candle
    /// border/wick, fonts, separators. Every draw layer reads its knobs from
    /// here, falling back to the theme for any value left at its inherit default.
    pub style: ChartStyle,
    /// Draw the price/time axis tick labels.
    pub show_axis_labels: bool,
    /// Strategy trade-marker display (the `tradeMarkers` feature): master
    /// toggle, the two text lines, and the palette. Trade markers always paint
    /// on the price pane.
    pub trade_markers: TradeMarkersState,
    /// Timeline-mark display (the `marks` feature): the lane's master toggle + per-group visibility.
    pub marks: MarksDisplayState,
    /// The host's timeline marks + group definitions (`setTimelineMarks`),
    /// painted on the lane above the time axis.
    pub timeline_marks: Vec<TimelineMark>,
    pub mark_groups: Vec<MarkGroup>,
    /// The mark stack (bar index) fanned out by hover or tap, if any.
    pub marks_expanded_stack: Option<i64>,
    /// The lane glyph (cluster key) under the pointer (it pulses) and when the
    /// hover began (frame-clock ms).
    pub marks_hover_key: Option<String>,
    pub marks_hover_since: f64,
    /// The cluster whose popup is open: its glyph paints filled ("active").
    pub marks_active_key: Option<String>,
    pub marks_flash: Option<MarksFlash>,
    /// Renderer-owned shaded time bands (session highlighting), behind grid + data.
    pub highlights: Vec<HighlightArea>,
    /// Pre/post-market bands pushed by the host (`sessionZones` feature); `None`: no sessions.
    pub session_zones: Option<SessionZones>,
    /// Draw-order key of the price candles, relative to indicator series z (see
    /// `series_z`). Indicators with z below this draw BEHIND the candles;
    /// at/above draw in front. Default 0 with indicators mounting at z < 0, so
    /// the price reads on top of every overlay, and user drawings (z >= 1 by
    /// default) on top of the price.
    pub candle_z: f64,
    /// Hide the base price series (candles/bars/line/area) without removing it:
    /// overlay indicators keep drawing and the pane autoscales to them.
    /// Toggled from the object tree.
    pub candles_hidden: bool,
    /// Per-indicator foreground draw-order key (series layer), keyed by
    /// indicator id. Higher = drawn later (in front). Assigned on mount to the
    /// current BOTTOM of the stack, so each indicator arrives behind the
    /// candles (and behind older indicators); `set_indicator_z` /
    /// `bring_indicator_to_front` / `send_indicator_to_back` change it.
    series_z: InsertionMap<f64>,
    /// Per-pane raster layers of drawings interleaved into the series stack
    /// (each indicator's Pine drawings at its model's z, plus in-stack user
    /// drawings): each a prepainted canvas the backend composites just before
    /// the series carrying `before_z`. Rebuilt by the renderer per data frame.
    pub drawing_slices: HashMap<String, Vec<DrawingSlice>>,
    /// Per-model index offset: the chart bar index of the model's
    /// `anchor_time`: its index-aligned payloads (dense series arrays,
    /// `bar_index` drawings) count from that bar. Only nonzero for models
    /// computed over a SUFFIX of the bars (whole-chart models, the norm, aren't
    /// stored). Recomputed by the renderer on set_bars + mount/patch.
    anchor_offsets: HashMap<String, i64>,
    /// Per-indicator private price windows (merged indicators drawn on their
    /// own scale column). Populated per frame for models flagged `own_scale`;
    /// absent means the model shares its pane's master scale.
    pub indicator_scales: InsertionMap<IndicatorScale>,
}

impl Default for SceneGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl SceneGraph {
    pub fn new() -> Self {
        Self {
            panes: InsertionMap::new(),
            indicators: InsertionMap::new(),
            bars: Vec::new(),
            volume_layer: None,
            native_data: InsertionMap::new(),
            native_pending: InsertionMap::new(),
            vpvr_layer: None,
            crosshair: None,
            crosshair_override: None,
            price_style: "candles".to_string(),
            base_painting: BasePainting::Candles,
            candle_override: None,
            baseline_value: None,
            show_price_line: true,
            show_price_label: true,
            show_countdown: true,
            log_scale: false,
            invert_scale: false,
            scale_price_only: false,
            price_mintick: None,
            scale_mode: ScaleMode::Price,
            percent_baseline: 0.0,
            timezone: "UTC".to_string(),
            format: None,
            show_grid: true,
            style: default_chart_style(),
            show_axis_labels: true,
            trade_markers: default_trade_markers_state(),
            marks: MarksDisplayState::default(),
            timeline_marks: Vec::new(),
            mark_groups: Vec::new(),
            marks_expanded_stack: None,
            marks_hover_key: None,
            marks_hover_since: 0.0,
            marks_active_key: None,
            marks_flash: None,
            highlights: Vec::new(),
            session_zones: None,
            candle_z: 0.0,
            candles_hidden: false,
            series_z: InsertionMap::new(),
            drawing_slices: HashMap::new(),
            anchor_offsets: HashMap::new(),
            indicator_scales: InsertionMap::new(),
        }
    }

    /// Panes sorted top-to-bottom by `order` (equal orders keep insertion order).
    pub fn ordered_panes(&self) -> Vec<&PaneNode> {
        let mut out: Vec<&PaneNode> = self.panes.values().collect();
        out.sort_by_key(|p| p.order);
        out
    }

    /// The pane ids in [`ordered_panes`](Self::ordered_panes) order, for callers that then mutate panes.
    pub fn ordered_pane_ids(&self) -> Vec<String> {
        self.ordered_panes().iter().map(|p| p.id.clone()).collect()
    }

    /// The session zones resolved into colored bands (pre/post-market or
    /// extended-hours washes from the config's session colors): consumed by the
    /// same painting path as [`highlights`](Self::highlights).
    pub fn session_highlight_bands(&self) -> Vec<HighlightArea> {
        let Some(zones) = &self.session_zones else {
            return Vec::new();
        };
        let sessions = &self.style.sessions;
        let mut out = Vec::new();
        for (from, to) in &zones.pre {
            out.push(HighlightArea {
                from: *from,
                to: *to,
                color: sessions.premarket_color.clone(),
            });
        }
        for (from, to) in &zones.post {
            out.push(HighlightArea {
                from: *from,
                to: *to,
                color: sessions.postmarket_color.clone(),
            });
        }
        for (from, to) in &zones.extended {
            out.push(HighlightArea {
                from: *from,
                to: *to,
                color: sessions.extended_color.clone(),
            });
        }
        out
    }

    pub fn indicators_for_pane(&self, pane_id: &str) -> Vec<&IndicatorModel> {
        self.indicators
            .values()
            .filter(|m| m.pane_id.as_deref() == Some(pane_id))
            .collect()
    }

    /// Merged (own-scale) indicators on a pane, ordered by z: one axis column each.
    pub fn own_scale_indicators_for_pane(&self, pane_id: &str) -> Vec<&IndicatorModel> {
        self.ordered_indicators_for_pane(pane_id)
            .into_iter()
            .filter(|m| m.own_scale)
            .collect()
    }

    /// Ensure a merged indicator has a private scale slot (seeded from the pane if given).
    pub fn ensure_indicator_scale(
        &mut self,
        id: &str,
        seed: Option<PriceScale>,
    ) -> &mut IndicatorScale {
        let (slot, _) = self.indicator_scales.get_or_insert_with(id, || {
            let base = seed.unwrap_or(PriceScale::new(0.0, 1.0));
            IndicatorScale {
                scale: base,
                scale_target: base,
                initialized: false,
                manual_scale: None,
            }
        });
        slot
    }

    pub fn drop_indicator_scale(&mut self, id: &str) {
        self.indicator_scales.remove(id);
    }

    /// The price window a model renders on: its own scale when merged
    /// (`own_scale`), else the pane's.
    pub fn scale_for(&self, model: &IndicatorModel, pane: &PaneNode) -> PriceScale {
        if model.own_scale
            && let Some(s) = self.indicator_scales.get(&model.id)
        {
            return s.scale;
        }
        pane.scale
    }

    /// Apply a new top-to-bottom pane order (ids not present are ignored).
    pub fn order_panes(&mut self, ordered_ids: &[&str]) {
        for (i, id) in ordered_ids.iter().enumerate() {
            if let Some(pane) = self.panes.get_mut(id) {
                pane.order = i as i32;
            }
        }
    }

    /// Indicators on a pane sorted by foreground z (ascending). The sort is
    /// stable, so equal-z models keep their insertion order (the default).
    pub fn ordered_indicators_for_pane(&self, pane_id: &str) -> Vec<&IndicatorModel> {
        let mut out = self.indicators_for_pane(pane_id);
        out.sort_by(|a, b| {
            self.z_of(&a.id)
                .partial_cmp(&self.z_of(&b.id))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out
    }

    /// The foreground draw-order key of an indicator (0 when never assigned).
    pub fn z_of(&self, id: &str) -> f64 {
        self.series_z.get(id).copied().unwrap_or(0.0)
    }

    /// The model's index offset: chart bar index its index-aligned payloads
    /// count from (0 = whole-chart).
    pub fn offset_of(&self, id: &str) -> i64 {
        self.anchor_offsets.get(id).copied().unwrap_or(0)
    }

    /// Offsets are SIGNED. Positive: the model starts after the chart's first
    /// bar (it ran over a suffix): readers skip its leading chart bars.
    /// Negative: the model starts BEFORE it (the chart's head moved forward
    /// under a mounted model): readers skip the model's own leading points,
    /// `points[i - off]` reaching further in. Storing only the positive case
    /// silently pinned such a model at index 0, i.e. drew it shifted.
    pub fn set_anchor_offset(&mut self, id: &str, offset: i64) {
        if offset != 0 {
            self.anchor_offsets.insert(id.to_string(), offset);
        } else {
            self.anchor_offsets.remove(id);
        }
    }

    pub fn forget_anchor_offset(&mut self, id: &str) {
        self.anchor_offsets.remove(id);
    }

    /// Resolve the baseline reference price for the given pane window: the
    /// explicit `baseline_value` when set, else `style.baseline.baseline_level`
    /// as the price that sits at that fraction of the pane height. Interpolated
    /// in the same space the pane renders in (log when `scale.log`, else
    /// linear) so `level%` always lands at `level%` of the height: matching
    /// `CoordinateSystem::y_to_price`.
    pub fn baseline_price_for(&self, scale: &PriceScale) -> f64 {
        if let Some(v) = self.baseline_value {
            return v;
        }
        let t = self.style.baseline.baseline_level / 100.0;
        if scale.log_usable() {
            let lo = scale.min.ln();
            return (lo + t * (scale.max.ln() - lo)).exp();
        }
        scale.min + (scale.max - scale.min) * t
    }

    /// Assign a default z on mount: the current bottom of the stack, so a new
    /// indicator paints behind the candles and behind every indicator already
    /// there: the price stays the top of the pile until the user restacks it.
    /// No-op if the indicator already has one.
    pub fn assign_indicator_z(&mut self, id: &str) {
        if !self.series_z.contains_key(id) {
            let z = self.bottom_z() - 1.0;
            self.series_z.insert(id, z);
        }
    }

    /// Mount-time default for a LAYER-BACKED native (it paints on a canvas
    /// stacked above the data canvas by default): top of the stack, so the
    /// recorded order tells the truth from the first frame. Keeps an existing
    /// key, so a restored stack survives the remount.
    pub fn assign_indicator_z_top(&mut self, id: &str) {
        if !self.series_z.contains_key(id) {
            let z = self.top_z() + 1.0;
            self.series_z.insert(id, z);
        }
    }

    pub fn forget_indicator_z(&mut self, id: &str) {
        self.series_z.remove(id);
    }

    pub fn set_indicator_z(&mut self, id: &str, z: f64) {
        self.series_z.insert(id, z);
    }

    /// Snapshot of the current ordering for a UI/read API: `{ id, z }` back to
    /// front, in the order the indicators paint: on a shared key, mount order
    /// (see [`ordered_indicators_for_pane`](Self::ordered_indicators_for_pane)).
    pub fn indicator_z_order(&self) -> Vec<ZEntry> {
        let mounted: HashMap<&str, usize> = self
            .indicators
            .keys()
            .enumerate()
            .map(|(i, id)| (id, i))
            .collect();
        let at = |id: &str| mounted.get(id).copied().unwrap_or(usize::MAX);
        let mut out: Vec<ZEntry> = self
            .series_z
            .iter()
            .map(|(id, z)| ZEntry {
                id: id.to_string(),
                z: *z,
            })
            .collect();
        out.sort_by(|a, b| {
            match a.z.partial_cmp(&b.z) {
                Some(std::cmp::Ordering::Equal) | None => {}
                Some(order) => return order,
            }
            at(&a.id).cmp(&at(&b.id))
        });
        out
    }

    /// The pane's series z keys (each indicator, plus the candles on the price
    /// pane), sorted ascending and de-duplicated: the boundaries a user
    /// drawing's z is slotted against.
    pub fn series_boundaries(&self, pane_id: &str) -> Vec<f64> {
        let mut keys: Vec<f64> = Vec::new();
        let mut add = |z: f64| {
            if !keys.contains(&z) {
                keys.push(z);
            }
        };
        if pane_id == "price" {
            add(self.candle_z);
        }
        for m in self.indicators_for_pane(pane_id) {
            add(self.z_of(&m.id));
        }
        keys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        keys
    }

    /// Raise an indicator above every other layer (other indicators AND the candles).
    pub fn bring_indicator_to_front(&mut self, id: &str) {
        let z = self.top_z() + 1.0;
        self.series_z.insert(id, z);
    }

    /// Drop an indicator below every other layer (other indicators AND the candles).
    pub fn send_indicator_to_back(&mut self, id: &str) {
        let z = self.bottom_z() - 1.0;
        self.series_z.insert(id, z);
    }

    fn top_z(&self) -> f64 {
        let mut max = self.candle_z;
        for z in self.series_z.values() {
            if *z > max {
                max = *z;
            }
        }
        max
    }

    fn bottom_z(&self) -> f64 {
        let mut min = self.candle_z;
        for z in self.series_z.values() {
            if *z < min {
                min = *z;
            }
        }
        min
    }

    /// Create the pane, or update an existing one in place. Without
    /// `height_weight` an existing pane keeps its weight (a user resize
    /// survives other indicators mounting onto it) and a new one gets the
    /// default: 3 for the price pane, 1 for a study pane.
    pub fn ensure_pane(
        &mut self,
        id: &str,
        kind: PaneKind,
        order: i32,
        height_weight: Option<f64>,
    ) -> &mut PaneNode {
        let (pane, created) = self
            .panes
            .get_or_insert_with(id, || PaneNode::new(id, kind, order, height_weight));
        if !created {
            pane.order = order;
            if let Some(w) = height_weight {
                pane.height_weight = w;
            }
            pane.kind = kind;
        }
        pane
    }

    pub fn remove_pane(&mut self, id: &str) {
        self.panes.remove(id);
    }
}

/// This pane's axis mode. The PRICE pane follows the scene-level `scale_mode`
/// (the persisted chart setting); every STUDY pane carries its own, so the
/// panes' scales are independent.
pub fn pane_scale_mode(scene: &SceneGraph, pane: &PaneNode) -> ScaleMode {
    match pane.kind {
        PaneKind::Price => scene.scale_mode,
        PaneKind::Study => pane.scale_mode.unwrap_or(ScaleMode::Price),
    }
}

/// This pane's logarithmic flag: the price pane from the scene, study panes their own.
pub fn pane_log_scale(scene: &SceneGraph, pane: &PaneNode) -> bool {
    match pane.kind {
        PaneKind::Price => scene.log_scale,
        PaneKind::Study => pane.log_scale.unwrap_or(false),
    }
}

/// This pane's inverted-axis flag: the price pane from the scene, study panes their own.
pub fn pane_invert(scene: &SceneGraph, pane: &PaneNode) -> bool {
    match pane.kind {
        PaneKind::Price => scene.invert_scale,
        PaneKind::Study => pane.invert.unwrap_or(false),
    }
}

/// Percent baseline for a pane, or `None` when it renders as an absolute
/// (non-percent) axis. Both percent and indexed modes share the baseline;
/// `indexed` is `false` here.
pub fn percent_baseline_for(scene: &SceneGraph, pane: &PaneNode) -> Option<f64> {
    (pane_scale_mode(scene, pane) == ScaleMode::Percent).then_some(pane.percent_baseline)
}

/// The percent/indexed descriptor for a pane's axis (baseline + flavor), or
/// `None` when the pane shows absolute values or has no usable baseline yet.
/// Percent and indexed are the same affine map onto `percent_baseline`; only
/// the label formatting differs.
pub fn percent_scale_for(scene: &SceneGraph, pane: &PaneNode) -> Option<PctScale> {
    let mode = pane_scale_mode(scene, pane);
    if mode != ScaleMode::Percent && mode != ScaleMode::Indexed {
        return None;
    }
    let baseline = pane.percent_baseline;
    if !baseline.is_finite() || baseline == 0.0 {
        return None;
    }
    Some(PctScale {
        baseline,
        indexed: mode == ScaleMode::Indexed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PaneHint;
    use crate::render::core::coordinate_system::CoordinateSystem;

    fn model(id: &str, pane_id: &str) -> IndicatorModel {
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
            pane_id: Some(pane_id.into()),
            own_scale: false,
            anchor_time: None,
            series: vec![],
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

    fn mount(scene: &mut SceneGraph, id: &str, pane_id: &str) {
        scene.indicators.insert(id, model(id, pane_id));
        scene.assign_indicator_z(id);
    }

    fn ids(models: Vec<&IndicatorModel>) -> Vec<String> {
        models.iter().map(|m| m.id.clone()).collect()
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-6, "{a} vs {b}");
    }

    // ── pane height weights ──

    #[test]
    fn gives_new_panes_the_default_weights() {
        let mut scene = SceneGraph::new();
        assert_eq!(
            scene
                .ensure_pane("price", PaneKind::Price, 0, None)
                .height_weight,
            3.0
        );
        assert_eq!(
            scene
                .ensure_pane("pane-a", PaneKind::Study, 1, None)
                .height_weight,
            1.0
        );
        assert_eq!(
            scene
                .ensure_pane("pane-b", PaneKind::Study, 2, Some(4.0))
                .height_weight,
            4.0
        );
    }

    #[test]
    fn keeps_a_pane_weight_when_the_pane_is_ensured_again_without_one() {
        let mut scene = SceneGraph::new();
        scene.ensure_pane("price", PaneKind::Price, 0, None);
        scene.ensure_pane("pane-a", PaneKind::Study, 1, None);
        // A user drag or chart.panes.setHeightWeight writes the weight on the node.
        scene.panes.get_mut("price").unwrap().height_weight = 8.0;
        scene.panes.get_mut("pane-a").unwrap().height_weight = 2.0;

        // What the renderer does whenever another indicator mounts onto these panes.
        scene.ensure_pane("price", PaneKind::Price, 0, None);
        scene.ensure_pane("pane-a", PaneKind::Study, 2, None);
        assert_eq!(scene.panes.get("price").unwrap().height_weight, 8.0);
        assert_eq!(scene.panes.get("pane-a").unwrap().height_weight, 2.0);
        assert_eq!(scene.panes.get("pane-a").unwrap().order, 2);

        // An explicit weight still replaces it.
        scene.ensure_pane("pane-a", PaneKind::Study, 2, Some(5.0));
        assert_eq!(scene.panes.get("pane-a").unwrap().height_weight, 5.0);
    }

    #[test]
    fn a_new_pane_starts_empty_and_unseeded() {
        let mut scene = SceneGraph::new();
        let pane = scene.ensure_pane("p", PaneKind::Study, 1, None);
        assert_eq!(
            pane.bounds,
            PaneBounds {
                top: 0.0,
                height: 0.0
            }
        );
        assert_eq!(pane.scale, PriceScale::new(0.0, 1.0));
        assert!(!pane.initialized);
        assert_eq!(pane.manual_scale, None);
        assert!(!pane.collapsed);
    }

    #[test]
    fn ensuring_a_pane_can_change_its_kind() {
        let mut scene = SceneGraph::new();
        scene.ensure_pane("p", PaneKind::Study, 1, None);
        let pane = scene.ensure_pane("p", PaneKind::Price, 1, None);
        assert_eq!(pane.kind, PaneKind::Price);
    }

    #[test]
    fn panes_order_by_their_order_key_and_reorder_on_demand() {
        let mut scene = SceneGraph::new();
        scene.ensure_pane("c", PaneKind::Study, 2, None);
        scene.ensure_pane("a", PaneKind::Price, 0, None);
        scene.ensure_pane("b", PaneKind::Study, 1, None);
        assert_eq!(scene.ordered_pane_ids(), strs(&["a", "b", "c"]));
        scene.order_panes(&["c", "ghost", "a"]);
        // `ghost` is ignored but still consumes its slot, like the original's forEach index
        assert_eq!(scene.panes.get("c").unwrap().order, 0);
        assert_eq!(scene.panes.get("a").unwrap().order, 2);
        assert_eq!(scene.ordered_pane_ids(), strs(&["c", "b", "a"]));
        scene.remove_pane("b");
        assert_eq!(scene.ordered_pane_ids(), strs(&["c", "a"]));
    }

    #[test]
    fn equal_pane_orders_keep_insertion_order() {
        let mut scene = SceneGraph::new();
        scene.ensure_pane("first", PaneKind::Study, 1, None);
        scene.ensure_pane("second", PaneKind::Study, 1, None);
        assert_eq!(scene.ordered_pane_ids(), strs(&["first", "second"]));
    }

    // ── z-order ──

    #[test]
    fn defaults_each_new_indicator_to_the_back_of_the_stack() {
        let mut scene = SceneGraph::new();
        mount(&mut scene, "a", "price");
        mount(&mut scene, "b", "price");
        mount(&mut scene, "c", "price");
        // Ascending z = paint order: the newest paints first (furthest back), the price on top.
        assert_eq!(
            ids(scene.ordered_indicators_for_pane("price")),
            strs(&["c", "b", "a"])
        );
        assert!(scene.z_of("a") < scene.candle_z);
        assert!(scene.z_of("b") < scene.z_of("a"));
        assert!(scene.z_of("c") < scene.z_of("b"));
    }

    #[test]
    fn bring_indicator_to_front_raises_it_above_all_others_and_the_candles() {
        let mut scene = SceneGraph::new();
        mount(&mut scene, "a", "price");
        mount(&mut scene, "b", "price");
        mount(&mut scene, "c", "price");
        scene.bring_indicator_to_front("a");
        assert_eq!(
            ids(scene.ordered_indicators_for_pane("price")),
            strs(&["c", "b", "a"])
        );
        assert!(scene.z_of("a") > scene.candle_z);
    }

    #[test]
    fn send_indicator_to_back_drops_it_below_all_others_and_the_candles() {
        let mut scene = SceneGraph::new();
        mount(&mut scene, "a", "price");
        mount(&mut scene, "b", "price");
        mount(&mut scene, "c", "price");
        scene.send_indicator_to_back("a");
        assert_eq!(
            ids(scene.ordered_indicators_for_pane("price")),
            strs(&["a", "c", "b"])
        );
        assert!(scene.z_of("a") < scene.z_of("c"));
        assert!(scene.z_of("a") < scene.candle_z);
    }

    #[test]
    fn set_indicator_z_places_candles_between_overlays() {
        let mut scene = SceneGraph::new(); // candle_z defaults to 0
        mount(&mut scene, "a", "price");
        mount(&mut scene, "b", "price");
        scene.set_indicator_z("a", -1.0);
        scene.set_indicator_z("b", 5.0);
        let ordered = scene.ordered_indicators_for_pane("price");
        assert_eq!(ordered[0].id, "a");
        assert!(scene.z_of("a") < scene.candle_z);
        assert!(scene.z_of("b") >= scene.candle_z);
    }

    #[test]
    fn ordered_indicators_for_pane_only_returns_models_on_the_requested_pane() {
        let mut scene = SceneGraph::new();
        mount(&mut scene, "a", "price");
        mount(&mut scene, "b", "pane-1");
        mount(&mut scene, "c", "price");
        assert_eq!(
            ids(scene.ordered_indicators_for_pane("price")),
            strs(&["c", "a"])
        );
        assert_eq!(
            ids(scene.ordered_indicators_for_pane("pane-1")),
            strs(&["b"])
        );
        assert!(scene.ordered_indicators_for_pane("nowhere").is_empty());
    }

    #[test]
    fn series_boundaries_lists_a_panes_z_keys_ascending() {
        let mut scene = SceneGraph::new();
        mount(&mut scene, "a", "price"); // -1
        mount(&mut scene, "b", "pane-1"); // -2
        scene.set_indicator_z("a", 4.0);
        assert_eq!(scene.series_boundaries("price"), vec![0.0, 4.0]); // candles at 0, a raised to 4
        assert_eq!(scene.series_boundaries("pane-1"), vec![-2.0]);
    }

    #[test]
    fn series_boundaries_deduplicates_shared_keys() {
        let mut scene = SceneGraph::new();
        mount(&mut scene, "a", "price");
        mount(&mut scene, "b", "price");
        scene.set_indicator_z("a", 3.0);
        scene.set_indicator_z("b", 3.0);
        assert_eq!(scene.series_boundaries("price"), vec![0.0, 3.0]);
    }

    #[test]
    fn forget_indicator_z_drops_the_order_key_on_removal() {
        let mut scene = SceneGraph::new();
        mount(&mut scene, "a", "price");
        scene.forget_indicator_z("a");
        assert!(scene.indicator_z_order().is_empty());
        assert_eq!(scene.z_of("a"), 0.0);
    }

    #[test]
    fn indicator_z_order_snapshots_id_and_z_sorted_by_z() {
        let mut scene = SceneGraph::new();
        mount(&mut scene, "a", "price");
        mount(&mut scene, "b", "price");
        scene.set_indicator_z("a", 9.0);
        scene.set_indicator_z("b", 3.0);
        assert_eq!(
            scene.indicator_z_order(),
            vec![
                ZEntry {
                    id: "b".into(),
                    z: 3.0
                },
                ZEntry {
                    id: "a".into(),
                    z: 9.0
                }
            ]
        );
    }

    #[test]
    fn indicator_z_order_breaks_ties_by_mount_order() {
        let mut scene = SceneGraph::new();
        mount(&mut scene, "m1", "price");
        mount(&mut scene, "m2", "price");
        // keys recorded in the opposite order of mounting
        scene.series_z = InsertionMap::new();
        scene.set_indicator_z("m2", 1.0);
        scene.set_indicator_z("m1", 1.0);
        scene.set_indicator_z("unmounted", 1.0);
        let order: Vec<String> = scene
            .indicator_z_order()
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(order, strs(&["m1", "m2", "unmounted"]));
    }

    #[test]
    fn assign_indicator_z_top_mounts_at_the_top_of_the_stack() {
        let mut scene = SceneGraph::new();
        scene.candle_z = 0.0;
        scene.assign_indicator_z("series-a"); // bottom: -1
        scene.assign_indicator_z_top("layer-native");
        assert!(scene.z_of("layer-native") > scene.candle_z);
        assert!(scene.z_of("layer-native") > scene.z_of("series-a"));
    }

    #[test]
    fn assign_indicator_z_top_keeps_an_existing_key() {
        let mut scene = SceneGraph::new();
        scene.set_indicator_z("layer-native", -7.0); // e.g. restored from a persisted config
        scene.assign_indicator_z_top("layer-native");
        assert_eq!(scene.z_of("layer-native"), -7.0);
        scene.assign_indicator_z("layer-native");
        assert_eq!(scene.z_of("layer-native"), -7.0);
    }

    #[test]
    fn successive_top_mounts_stack_in_add_order_newest_in_front() {
        let mut scene = SceneGraph::new();
        scene.assign_indicator_z_top("a");
        scene.assign_indicator_z_top("b");
        assert!(scene.z_of("b") > scene.z_of("a"));
    }

    // ── anchor offsets ──

    #[test]
    fn anchor_offsets_are_signed_and_zero_clears_them() {
        let mut scene = SceneGraph::new();
        assert_eq!(scene.offset_of("m"), 0);
        scene.set_anchor_offset("m", 60);
        assert_eq!(scene.offset_of("m"), 60);
        scene.set_anchor_offset("m", -60);
        assert_eq!(scene.offset_of("m"), -60);
        scene.set_anchor_offset("m", 0);
        assert_eq!(scene.offset_of("m"), 0);
        scene.set_anchor_offset("m", 5);
        scene.forget_anchor_offset("m");
        assert_eq!(scene.offset_of("m"), 0);
    }

    // ── per-pane scale mode / log / invert ──

    fn scene_with_study() -> SceneGraph {
        let mut scene = SceneGraph::new();
        scene.ensure_pane("price", PaneKind::Price, 0, Some(3.0));
        scene.ensure_pane("s1", PaneKind::Study, 1, Some(1.0));
        scene
    }

    #[test]
    fn the_price_pane_follows_the_scene_and_study_panes_are_regular_and_linear() {
        let scene = scene_with_study();
        let (price, study) = (
            scene.panes.get("price").unwrap(),
            scene.panes.get("s1").unwrap(),
        );
        assert_eq!(pane_scale_mode(&scene, price), ScaleMode::Price);
        assert_eq!(pane_scale_mode(&scene, study), ScaleMode::Price);
        assert!(!pane_log_scale(&scene, price));
        assert!(!pane_log_scale(&scene, study));
    }

    #[test]
    fn a_study_pane_switching_to_percent_or_log_does_not_touch_the_price_pane() {
        let mut scene = scene_with_study();
        {
            let study = scene.panes.get_mut("s1").unwrap();
            study.scale_mode = Some(ScaleMode::Percent);
            study.log_scale = Some(true);
        }
        let (price, study) = (
            scene.panes.get("price").unwrap(),
            scene.panes.get("s1").unwrap(),
        );
        assert_eq!(pane_scale_mode(&scene, study), ScaleMode::Percent);
        assert!(pane_log_scale(&scene, study));
        assert_eq!(pane_scale_mode(&scene, price), ScaleMode::Price);
        assert!(!pane_log_scale(&scene, price));
    }

    #[test]
    fn the_price_pane_follows_the_scene_level_setting_and_study_panes_ignore_it() {
        let mut scene = scene_with_study();
        scene.scale_mode = ScaleMode::Percent;
        scene.log_scale = true;
        let (price, study) = (
            scene.panes.get("price").unwrap(),
            scene.panes.get("s1").unwrap(),
        );
        assert_eq!(pane_scale_mode(&scene, price), ScaleMode::Percent);
        assert!(pane_log_scale(&scene, price));
        assert_eq!(pane_scale_mode(&scene, study), ScaleMode::Price);
        assert!(!pane_log_scale(&scene, study));
    }

    #[test]
    fn percent_baseline_for_gates_on_each_panes_own_mode_and_baseline() {
        let mut scene = scene_with_study();
        scene.panes.get_mut("price").unwrap().percent_baseline = 100.0;
        scene.panes.get_mut("s1").unwrap().percent_baseline = 50.0;
        // regular: no baseline (absolute axis) on either pane
        assert_eq!(
            percent_baseline_for(&scene, scene.panes.get("price").unwrap()),
            None
        );
        assert_eq!(
            percent_baseline_for(&scene, scene.panes.get("s1").unwrap()),
            None
        );
        // each pane opts into percent independently, using its own reference value
        scene.scale_mode = ScaleMode::Percent; // price pane only
        scene.panes.get_mut("s1").unwrap().scale_mode = Some(ScaleMode::Percent); // study pane only
        assert_eq!(
            percent_baseline_for(&scene, scene.panes.get("price").unwrap()),
            Some(100.0)
        );
        assert_eq!(
            percent_baseline_for(&scene, scene.panes.get("s1").unwrap()),
            Some(50.0)
        );
        // indexed has no percent baseline of its own
        scene.scale_mode = ScaleMode::Indexed;
        assert_eq!(
            percent_baseline_for(&scene, scene.panes.get("price").unwrap()),
            None
        );
    }

    #[test]
    fn percent_scale_for_describes_percent_vs_indexed_per_pane_and_gates_on_a_usable_baseline() {
        let mut scene = scene_with_study();
        scene.panes.get_mut("price").unwrap().percent_baseline = 100.0;
        scene.panes.get_mut("s1").unwrap().percent_baseline = 50.0;
        assert_eq!(
            percent_scale_for(&scene, scene.panes.get("price").unwrap()),
            None
        );
        scene.scale_mode = ScaleMode::Percent;
        assert_eq!(
            percent_scale_for(&scene, scene.panes.get("price").unwrap()),
            Some(PctScale {
                baseline: 100.0,
                indexed: false
            })
        );
        scene.panes.get_mut("s1").unwrap().scale_mode = Some(ScaleMode::Indexed);
        assert_eq!(
            percent_scale_for(&scene, scene.panes.get("s1").unwrap()),
            Some(PctScale {
                baseline: 50.0,
                indexed: true
            })
        );
        assert_eq!(
            percent_scale_for(&scene, scene.panes.get("price").unwrap()),
            Some(PctScale {
                baseline: 100.0,
                indexed: false
            })
        );
        // a zero/non-finite baseline falls back to absolute even in percent/indexed mode
        scene.panes.get_mut("s1").unwrap().percent_baseline = 0.0;
        assert_eq!(
            percent_scale_for(&scene, scene.panes.get("s1").unwrap()),
            None
        );
        scene.panes.get_mut("s1").unwrap().percent_baseline = f64::NAN;
        assert_eq!(
            percent_scale_for(&scene, scene.panes.get("s1").unwrap()),
            None
        );
        scene.panes.get_mut("s1").unwrap().percent_baseline = f64::INFINITY;
        assert_eq!(
            percent_scale_for(&scene, scene.panes.get("s1").unwrap()),
            None
        );
    }

    #[test]
    fn each_pane_flips_its_axis_independently() {
        let mut scene = scene_with_study();
        let is = |scene: &SceneGraph, id: &str| pane_invert(scene, scene.panes.get(id).unwrap());
        assert!(!is(&scene, "price"));
        assert!(!is(&scene, "s1"));
        scene.panes.get_mut("s1").unwrap().invert = Some(true);
        assert!(is(&scene, "s1"));
        assert!(!is(&scene, "price"));
        scene.invert_scale = true;
        assert!(is(&scene, "price"));
    }

    #[test]
    fn scale_mode_names_round_trip() {
        for mode in [ScaleMode::Price, ScaleMode::Percent, ScaleMode::Indexed] {
            assert_eq!(ScaleMode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(ScaleMode::parse("log"), None);
        assert_eq!(
            serde_json::to_string(&ScaleMode::Indexed).unwrap(),
            "\"indexed\""
        );
    }

    // ── baseline level ──

    fn scene_with_level(level: f64) -> SceneGraph {
        let mut scene = SceneGraph::new();
        scene.style.baseline.baseline_level = level;
        scene
    }

    const BOUNDS: PaneBounds = PaneBounds {
        top: 0.0,
        height: 400.0,
    };

    #[test]
    fn baseline_linear_scale_lands_at_the_level_percent_of_the_pane_height() {
        let scene = scene_with_level(50.0);
        let scale = PriceScale {
            log: Some(false),
            ..PriceScale::new(100.0, 200.0)
        };
        let price = scene.baseline_price_for(&scale);
        close(price, 150.0);
        close(
            CoordinateSystem::new().price_to_y(price, &scale, &BOUNDS),
            200.0,
        );
    }

    #[test]
    fn baseline_log_scale_lands_at_the_level_percent_of_the_pane_height_not_of_the_price_range() {
        let scene = scene_with_level(50.0);
        let scale = PriceScale {
            log: Some(true),
            ..PriceScale::new(100.0, 200.0)
        };
        let price = scene.baseline_price_for(&scale);
        // In log space the midpoint is the geometric mean, NOT the arithmetic 150.
        close(price, (100.0f64 * 200.0).sqrt());
        close(
            CoordinateSystem::new().price_to_y(price, &scale, &BOUNDS),
            200.0,
        );
    }

    #[test]
    fn baseline_log_scale_off_center_level_still_lands_at_that_fraction_of_the_height() {
        let scene = scene_with_level(25.0);
        let scale = PriceScale {
            log: Some(true),
            ..PriceScale::new(100.0, 200.0)
        };
        let price = scene.baseline_price_for(&scale);
        // 25% up from the bottom, 75% down from the top: y = 300 of 400.
        close(
            CoordinateSystem::new().price_to_y(price, &scale, &BOUNDS),
            300.0,
        );
    }

    #[test]
    fn an_explicit_baseline_value_overrides_the_level_on_either_scale() {
        let mut scene = scene_with_level(50.0);
        scene.baseline_value = Some(123.45);
        let linear = PriceScale {
            log: Some(false),
            ..PriceScale::new(100.0, 200.0)
        };
        let log = PriceScale {
            log: Some(true),
            ..PriceScale::new(100.0, 200.0)
        };
        assert_eq!(scene.baseline_price_for(&linear), 123.45);
        assert_eq!(scene.baseline_price_for(&log), 123.45);
    }

    #[test]
    fn a_log_flag_over_a_non_positive_window_falls_back_to_linear() {
        let scene = scene_with_level(50.0);
        let scale = PriceScale {
            log: Some(true),
            ..PriceScale::new(-10.0, 10.0)
        };
        assert_eq!(scene.baseline_price_for(&scale), 0.0);
    }

    // ── scales, ranges, zones ──

    #[test]
    fn a_merged_indicator_renders_on_its_own_scale_once_it_has_one() {
        let mut scene = SceneGraph::new();
        scene.ensure_pane("price", PaneKind::Price, 0, None);
        let mut m = model("m", "price");
        m.own_scale = true;
        scene.indicators.insert("m", m);
        let pane_scale = PriceScale::new(10.0, 20.0);
        scene.panes.get_mut("price").unwrap().scale = pane_scale;
        // no slot yet: the pane's window
        let (pane, model) = (
            scene.panes.get("price").unwrap(),
            scene.indicators.get("m").unwrap(),
        );
        assert_eq!(scene.scale_for(model, pane), pane_scale);
        // the slot is seeded from the pane once and then left alone
        scene.ensure_indicator_scale("m", Some(pane_scale)).scale = PriceScale::new(0.0, 5.0);
        let again = scene.ensure_indicator_scale("m", Some(PriceScale::new(99.0, 100.0)));
        assert_eq!(again.scale, PriceScale::new(0.0, 5.0));
        assert_eq!(again.scale_target, pane_scale);
        assert!(!again.initialized);
        let (pane, model) = (
            scene.panes.get("price").unwrap(),
            scene.indicators.get("m").unwrap(),
        );
        assert_eq!(scene.scale_for(model, pane), PriceScale::new(0.0, 5.0));
        assert_eq!(
            ids(scene.own_scale_indicators_for_pane("price")),
            strs(&["m"])
        );
        scene.drop_indicator_scale("m");
        let (pane, model) = (
            scene.panes.get("price").unwrap(),
            scene.indicators.get("m").unwrap(),
        );
        assert_eq!(scene.scale_for(model, pane), pane_scale);
    }

    #[test]
    fn an_unseeded_indicator_scale_starts_at_the_unit_window() {
        let mut scene = SceneGraph::new();
        let s = scene.ensure_indicator_scale("x", None);
        assert_eq!(s.scale, PriceScale::new(0.0, 1.0));
        assert_eq!(s.manual_scale, None);
    }

    #[test]
    fn session_zones_resolve_into_colored_bands_in_pre_post_extended_order() {
        let mut scene = SceneGraph::new();
        assert!(scene.session_highlight_bands().is_empty());
        scene.session_zones = Some(SessionZones {
            pre: vec![(1, 2), (5, 6)],
            post: vec![(3, 4)],
            extended: vec![(7, 8)],
        });
        let bands = scene.session_highlight_bands();
        let spans: Vec<(i64, i64)> = bands.iter().map(|b| (b.from, b.to)).collect();
        assert_eq!(spans, vec![(1, 2), (5, 6), (3, 4), (7, 8)]);
        assert_eq!(bands[0].color, scene.style.sessions.premarket_color);
        assert_eq!(bands[2].color, scene.style.sessions.postmarket_color);
        assert_eq!(bands[3].color, scene.style.sessions.extended_color);
    }

    #[test]
    fn a_fresh_scene_has_the_original_defaults() {
        let scene = SceneGraph::new();
        assert_eq!(scene.price_style, "candles");
        assert_eq!(scene.base_painting, BasePainting::Candles);
        assert!(scene.show_price_line && scene.show_price_label && scene.show_countdown);
        assert!(scene.show_grid && scene.show_axis_labels);
        assert!(!scene.log_scale && !scene.invert_scale && !scene.scale_price_only);
        assert_eq!(scene.scale_mode, ScaleMode::Price);
        assert_eq!(scene.timezone, "UTC");
        assert_eq!(scene.candle_z, 0.0);
        assert!(!scene.candles_hidden);
        assert!(scene.marks.visible);
        assert_eq!(scene.baseline_value, None);
        assert_eq!(scene.price_mintick, None);
    }

    #[test]
    fn insertion_map_keeps_insertion_order_and_replaces_in_place() {
        let mut m: InsertionMap<i32> = InsertionMap::new();
        m.insert("b", 1);
        m.insert("a", 2);
        assert_eq!(m.insert("b", 3), Some(1));
        assert_eq!(m.keys().collect::<Vec<_>>(), vec!["b", "a"]);
        assert_eq!(m.get("b"), Some(&3));
        assert_eq!(m.remove("b"), Some(3));
        assert_eq!(m.remove("b"), None);
        assert_eq!(m.len(), 1);
        let (v, created) = m.get_or_insert_with("c", || 9);
        assert!(created);
        *v += 1;
        let (v, created) = m.get_or_insert_with("c", || 0);
        assert!(!created);
        assert_eq!(*v, 10);
        m.clear();
        assert!(m.is_empty());
    }
}
