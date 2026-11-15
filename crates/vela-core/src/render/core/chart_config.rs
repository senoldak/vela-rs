//! Comprehensive, **serializable** native-renderer style/config.
//!
//! Two shapes live here:
//!  - [`ChartStyle`]: the renderer's LIVE store of the granular cosmetic knobs
//!    that were previously hard-coded or theme-derived (grid colors, crosshair,
//!    candle border/wick, fonts, separators). A `None` color means "inherit
//!    from the theme".
//!  - [`ChartConfig`]: the FLAT, versioned JSON the user persists / templates
//!    with. It aggregates the theme cosmetics + the scene's display flags +
//!    [`ChartStyle`] into one self-contained document (the renderer's
//!    `getConfig()` resolves every inherited value to a concrete one so an
//!    exported template stands alone). Its serde form is field for field the
//!    original's JSON, because hosts persist it.
//!
//! [`merge_config`] is the single validating reducer: it deep-merges an
//! untrusted partial config onto a known-good base, dropping any malformed
//! field. This keeps `applyConfig(json)` total: it never throws on bad input
//! and never half-applies.
//!
//! The original reads a process-wide chart-type registry; here the functions
//! that need it ([`price_style_ids`], [`base_painting_of`],
//! [`has_own_candle_paint`], [`candle_override_for`], [`factory_reset_config`],
//! [`merge_config`]) take a [`ChartTypeRegistry`]. The original has no
//! `defaultChartConfig`; [`default_chart_config`] is what a fresh renderer's
//! `getConfig()` returns for a theme, which is the base the original's tests
//! (and a host seeding a template) start from. The unexported, unused
//! clamp helpers of the original (`clampRowTicks`, `clampRatio`, ...) are not
//! ported.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::chart_types::settings_row_value_keys;
use crate::chart_types::{BasePainting, ChartTypeRegistry, SettingsRowDescriptor, SettingsValue};
use crate::indicators::classics::math::js_round;
use crate::model::LineStyle;
use crate::palette::{ACCENT, BEARISH, BULLISH, CHIP_PLATE, CROSSHAIR, SERIES_LINE, WARNING};
use crate::theme::dark_theme;

use super::color::{is_js_whitespace, with_alpha};
use super::scene_graph::ScaleMode;

pub const CHART_CONFIG_VERSION: u32 = 1;

/// Baseline style defaults: the baseline chart type owns its palette and does
/// NOT follow the candle up/down colors. Above-baseline draws in
/// `BASELINE_TOP_LINE`, below-baseline in `BASELINE_BOTTOM_LINE`. Each area is
/// a two-stop gradient of the line color: a stronger `BASELINE_FILL_ALPHA`
/// wash near the line fading to a fainter `BASELINE_FILL_ALPHA_FAR` (still
/// visible, not fully clear) at the baseline. `BASELINE_LEVEL_DEFAULT` places
/// the baseline as a percentage of the visible pane range (0 = pane low, 100 =
/// pane high).
pub const BASELINE_TOP_LINE: &str = BULLISH;
pub const BASELINE_BOTTOM_LINE: &str = BEARISH;
pub const BASELINE_FILL_ALPHA: f64 = 0.25;
pub const BASELINE_FILL_ALPHA_FAR: f64 = 0.05;
pub const BASELINE_LEVEL_DEFAULT: f64 = 50.0;

/// Columns style default: each column is a large filled area, so its default
/// colors are the chart's up/down at this opacity. A color set in the columns
/// block is used as given.
pub const COLUMNS_FILL_ALPHA: f64 = 0.5;

/// Default area/baseline fill edge (fades out): the original renderer's `TRANSPARENT`.
pub const TRANSPARENT: &str = "rgba(0,0,0,0)";

/// Per-type settings bags (plugin SDK sections), keyed by type id then row key.
pub type ChartTypeBags = BTreeMap<String, Map<String, Value>>;

// ── live style store (renderer-owned; None color => inherit theme) ──

#[derive(Clone, Debug, PartialEq)]
pub struct GridLineStyle {
    pub visible: bool,
    /// `None` inherits `theme.gridColor`.
    pub color: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CrosshairStyle {
    /// `None` inherits `theme.textColor`.
    pub color: Option<String>,
    pub width: f64,
    pub style: LineStyle,
    /// Line opacity (0-1); reference charting crosshairs are translucent.
    pub opacity: f64,
    /// Axis-chip background; `None` inherits `theme.textColor`.
    pub label_background: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CandleStyle {
    /// Whether the filled candle body is drawn (off means wicks/borders only).
    pub body_visible: bool,
    pub border_visible: bool,
    /// `None` inherits the body color.
    pub border_up_color: Option<String>,
    pub border_down_color: Option<String>,
    pub wick_visible: bool,
    pub wick_up_color: Option<String>,
    pub wick_down_color: Option<String>,
}

/// Per-price-style cosmetics: each chart type carries its OWN colors and does
/// NOT inherit from another style. A `None` color is the style's "use the
/// chart's up/down default" sentinel (so an untouched style still matches the
/// candle palette and the prior rendering); set it and that style becomes
/// independent. The candle BODY colors stay the renderer's up/down colors (the
/// `candles` block adds border/wick on top); these blocks cover the non-candle
/// styles.
#[derive(Clone, Debug, PartialEq)]
pub struct BarsStyle {
    /// `None` inherits the chart up color.
    pub up_color: Option<String>,
    /// `None` inherits the chart down color.
    pub down_color: Option<String>,
}

/// The line style's color + width, shared by the line, line-with-markers and
/// step-line styles (one line look, three ways of drawing it).
#[derive(Clone, Debug, PartialEq)]
pub struct LineSeriesStyle {
    /// `None` inherits the chart up color.
    pub color: Option<String>,
    pub width: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AreaSeriesStyle {
    /// `None` inherits the chart up color.
    pub line_color: Option<String>,
    pub width: f64,
    /// Gradient top (near the line); `None` is the resolved line color.
    pub top_color: Option<String>,
    /// Gradient bottom (at the baseline); `None` is transparent.
    pub bottom_color: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BaselineSeriesStyle {
    /// `None` is the baseline up default (`BASELINE_TOP_LINE`).
    pub top_line_color: Option<String>,
    /// `None` is the baseline down default (`BASELINE_BOTTOM_LINE`).
    pub bottom_line_color: Option<String>,
    /// Top area fill near the up line; `None` is a wash of the top line color at `BASELINE_FILL_ALPHA`.
    pub top_fill_color: Option<String>,
    /// Top area fill near the baseline (the lower end of the top area); `None`
    /// is a fainter wash of the top line color at `BASELINE_FILL_ALPHA_FAR`.
    pub top_fill_color2: Option<String>,
    /// Bottom area fill near the down line; `None` is a wash of the bottom line color at `BASELINE_FILL_ALPHA`.
    pub bottom_fill_color: Option<String>,
    /// Bottom area fill near the baseline (the upper end of the bottom area);
    /// `None` is a fainter wash of the bottom line color at `BASELINE_FILL_ALPHA_FAR`.
    pub bottom_fill_color2: Option<String>,
    pub width: f64,
    /// Baseline position as a percent of the visible pane price range (0 = low, 100 = high).
    pub baseline_level: f64,
}

/// Columns style: a column per bar wears the up color when its close is at or
/// above the previous bar's close, else the down color.
#[derive(Clone, Debug, PartialEq)]
pub struct ColumnsSeriesStyle {
    /// `None` is the chart up color at [`COLUMNS_FILL_ALPHA`].
    pub up_color: Option<String>,
    /// `None` is the chart down color at [`COLUMNS_FILL_ALPHA`].
    pub down_color: Option<String>,
}

/// Session-zone shading (the `sessionZones` feature): the washes painted over
/// pre-market and post-market time bands on day-split tapes, and the single
/// extended-hours band on overnight roll tapes. Alpha belongs in the color
/// itself.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionShadeStyle {
    pub premarket_color: String,
    pub postmarket_color: String,
    pub extended_color: String,
}

/// Default pre-market wash: faint enough to sit behind candles and gridlines.
pub fn premarket_shade() -> String {
    with_alpha(WARNING, 0.08)
}

/// Default post-market wash.
pub fn postmarket_shade() -> String {
    with_alpha(ACCENT, 0.08)
}

/// The extended-hours wash (overnight roll tapes) defaults to the after-hours blue.
pub fn extended_shade() -> String {
    postmarket_shade()
}

/// Empty space kept around the data. `top` / `bottom` are the share of each
/// pane's PIXEL height the autoscaled window reserves above the highest / below
/// the lowest visible value (percent); `right` is the whitespace after the
/// newest bar, in bars, that fit, re-frame and scroll-to-latest land on.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartMargins {
    pub top: f64,
    pub bottom: f64,
    pub right: f64,
}

pub const DEFAULT_MARGINS: ChartMargins = ChartMargins {
    top: 10.0,
    bottom: 10.0,
    right: 10.0,
};

#[derive(Clone, Debug, PartialEq)]
pub struct ChartStyle {
    /// Per-chart-type settings (plugin SDK sections), keyed by type id then row key.
    pub chart_types: ChartTypeBags,
    /// Axis/label font size in CSS px (the family stays on the theme).
    pub font_size: f64,
    pub margins: ChartMargins,
    pub grid_vert: GridLineStyle,
    pub grid_horz: GridLineStyle,
    /// Axis frame lines (the right price-axis border); `None` inherits `theme.borderColor`.
    pub border_color: Option<String>,
    /// The draggable line between stacked panes; `None` inherits `theme.borderColor`.
    pub separator_color: Option<String>,
    pub crosshair: CrosshairStyle,
    pub candle: CandleStyle,
    pub bars: BarsStyle,
    pub line: LineSeriesStyle,
    pub area: AreaSeriesStyle,
    pub baseline: BaselineSeriesStyle,
    pub columns: ColumnsSeriesStyle,
    pub sessions: SessionShadeStyle,
}

impl Default for ChartStyle {
    fn default() -> Self {
        default_chart_style()
    }
}

/// A fresh live style store with every value at its "inherit / current behavior" default.
pub fn default_chart_style() -> ChartStyle {
    ChartStyle {
        chart_types: BTreeMap::new(),
        font_size: 11.0,
        margins: DEFAULT_MARGINS,
        grid_vert: GridLineStyle {
            visible: true,
            color: None,
        },
        grid_horz: GridLineStyle {
            visible: true,
            color: None,
        },
        border_color: None,
        separator_color: None,
        crosshair: CrosshairStyle {
            color: Some(CROSSHAIR.to_string()),
            width: 1.0,
            style: LineStyle::Dashed,
            opacity: 0.4,
            label_background: Some(CHIP_PLATE.to_string()),
        },
        candle: CandleStyle {
            body_visible: true,
            border_visible: false,
            border_up_color: None,
            border_down_color: None,
            wick_visible: true,
            wick_up_color: None,
            wick_down_color: None,
        },
        bars: BarsStyle {
            up_color: None,
            down_color: None,
        },
        line: LineSeriesStyle {
            color: Some(SERIES_LINE.to_string()),
            width: 2.0,
        },
        area: AreaSeriesStyle {
            line_color: Some(SERIES_LINE.to_string()),
            width: 2.0,
            top_color: Some(with_alpha(SERIES_LINE, 0.28)),
            bottom_color: Some(with_alpha(SERIES_LINE, 0.02)),
        },
        baseline: BaselineSeriesStyle {
            top_line_color: None,
            bottom_line_color: None,
            top_fill_color: None,
            top_fill_color2: None,
            bottom_fill_color: None,
            bottom_fill_color2: None,
            width: 2.0,
            baseline_level: BASELINE_LEVEL_DEFAULT,
        },
        columns: ColumnsSeriesStyle {
            up_color: None,
            down_color: None,
        },
        sessions: SessionShadeStyle {
            premarket_color: premarket_shade(),
            postmarket_color: postmarket_shade(),
            extended_color: extended_shade(),
        },
    }
}

// ── serialized config document (persistence / templating) ──

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutConfig {
    pub background: String,
    pub text_color: String,
    pub font_family: String,
    pub font_size: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GridLineConfig {
    pub visible: bool,
    pub color: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GridConfig {
    pub vert_lines: GridLineConfig,
    pub horz_lines: GridLineConfig,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrosshairConfig {
    pub color: String,
    pub width: f64,
    pub style: LineStyle,
    pub opacity: f64,
    pub label_background: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceScaleConfig {
    pub mode: ScaleMode,
    pub log: bool,
    /// Inverted price axis (high at the bottom).
    pub invert: bool,
    /// Autoscale fits the price bars only; indicators no longer widen the window.
    pub price_only: bool,
    /// Dragging an auto-scaled pane unlocks manual scaling to pan time and price together.
    pub free_pan: bool,
    pub border_color: String,
    pub labels_visible: bool,
    pub current_price_line: bool,
    pub price_label: bool,
    pub countdown: bool,
    /// Glide the forming bar (and the last-price line/label) toward each live
    /// tick instead of snapping. The duration comes from `animations.liveBar` /
    /// the `animLiveBar` feature; this is only the on/off switch the settings
    /// dialog shows.
    pub animate_last_price: bool,
}

/// Motion on/off switches (the settings dialog's Animation group). On/off only:
/// each motion's duration stays what the host configured (`animations` option /
/// renderer features); switching one back on restores that duration. `pan`
/// covers both the drag-release inertia and the programmatic scroll glide. The
/// live-bar glide's switch is `priceScale.animateLastPrice` (it predates this
/// block).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimationsConfig {
    pub zoom: bool,
    pub pan: bool,
    pub autoscale: bool,
    /// The first-paint candle reveal: takes effect on the next load.
    pub intro: bool,
}

/// Stacked-pane chrome: the draggable line between an indicator's pane and the one above it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanesConfig {
    pub separator_color: String,
}

/// Strategy trade markers (the `tradeMarkers` feature): the order-fill units on the price pane.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TradesConfig {
    pub visible: bool,
    /// The order-id/comment text line.
    pub labels: bool,
    /// The signed-quantity text line.
    pub qty: bool,
    pub long_color: String,
    pub short_color: String,
    pub exit_color: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeScaleConfig {
    pub timezone: String,
}

/// Timeline marks (the `marks` feature): the lane's master toggle + per-group
/// visibility (the Events tab's checkboxes). `groups` merges additively, like
/// `stacking.series`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarksConfig {
    pub visible: bool,
    pub groups: BTreeMap<String, bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandlesConfig {
    pub up_color: String,
    pub down_color: String,
    pub body_visible: bool,
    pub border_visible: bool,
    pub border_up_color: String,
    pub border_down_color: String,
    pub wick_visible: bool,
    pub wick_up_color: String,
    pub wick_down_color: String,
}

/// OHLC-bars style: its own up/down (independent of the candle body colors).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BarsConfig {
    pub up_color: String,
    pub down_color: String,
}

/// Line style: its own color + width (also drawn by the line-with-markers and step-line styles).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineConfig {
    pub color: String,
    pub width: f64,
}

/// Area style: its own line + gradient-fill colors + width.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AreaConfig {
    pub line_color: String,
    pub width: f64,
    pub top_color: String,
    pub bottom_color: String,
}

/// Baseline style: its own above/below line + two-stop fill colors + width + level.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineConfig {
    pub top_line_color: String,
    pub bottom_line_color: String,
    pub top_fill_color: String,
    pub top_fill_color2: String,
    pub bottom_fill_color: String,
    pub bottom_fill_color2: String,
    pub width: f64,
    pub baseline_level: f64,
}

/// Columns style: its own up/down colors.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnsConfig {
    pub up_color: String,
    pub down_color: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesConfig {
    pub style: String,
    pub baseline: Option<f64>,
    /// Spacing multiplier for non-connecting styles (candles/bars/columns/HA/plugin
    /// types): scales the center-to-center pitch (and the crosshair step) without
    /// changing body width. 1 = default.
    pub spacing: f64,
}

/// Session-zone shading: the washes painted over the session bands a host
/// pushes through the `sessionZones` feature: pre/post-market on day-split
/// tapes, the single extended-hours phase on overnight roll tapes (no bands
/// means the colors are dormant).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionsConfig {
    pub premarket_color: String,
    pub postmarket_color: String,
    pub extended_color: String,
}

/// Draw-order keys: what paints in front of what. The candles' own key plus
/// one per indicator id; user drawings persist their keys in the drawings
/// document, in the same space, so a saved chart keeps a drawing under the
/// candles or between two indicators.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackingConfig {
    pub candles: f64,
    pub series: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartConfig {
    pub version: u32,
    pub layout: LayoutConfig,
    pub grid: GridConfig,
    pub crosshair: CrosshairConfig,
    pub price_scale: PriceScaleConfig,
    pub animations: AnimationsConfig,
    pub panes: PanesConfig,
    /// Whitespace around the data (the Canvas tab's Margins group): top/bottom
    /// in percent of pane height, right in bars. See [`ChartMargins`].
    pub margins: ChartMargins,
    pub trades: TradesConfig,
    pub time_scale: TimeScaleConfig,
    pub marks: MarksConfig,
    /// Per-chart-type settings (plugin SDK sections), keyed by type id then row key.
    pub chart_types: ChartTypeBags,
    pub candles: CandlesConfig,
    pub bars: BarsConfig,
    pub line: LineConfig,
    pub area: AreaConfig,
    pub baseline: BaselineConfig,
    pub columns: ColumnsConfig,
    pub series: SeriesConfig,
    pub sessions: SessionsConfig,
    pub stacking: StackingConfig,
}

/// What a fresh renderer's `getConfig()` returns on `theme` (the original's
/// default options: the standard candle colors, every motion on but the live-bar
/// glide, nothing stored yet). Theme keys the theme lacks fall back to the
/// built-in dark theme's.
pub fn default_chart_config(theme: &Value) -> ChartConfig {
    let fallback = dark_theme();
    let t = |key: &str| -> String {
        theme
            .get(key)
            .and_then(Value::as_str)
            .or_else(|| fallback.get(key).and_then(Value::as_str))
            .unwrap_or_default()
            .to_string()
    };
    let s = default_chart_style();
    let up = BULLISH.to_string();
    let down = BEARISH.to_string();
    let grid = t("gridColor");
    let border = t("borderColor");
    let text = t("textColor");
    let wash = columns_default_colors(&CandleColors {
        up_color: up.clone(),
        down_color: down.clone(),
    });
    let top_line = BASELINE_TOP_LINE.to_string();
    let bottom_line = BASELINE_BOTTOM_LINE.to_string();
    let line_color = s.area.line_color.clone().unwrap_or_else(|| up.clone());
    ChartConfig {
        version: CHART_CONFIG_VERSION,
        layout: LayoutConfig {
            background: t("background"),
            text_color: text.clone(),
            font_family: t("fontFamily"),
            font_size: s.font_size,
        },
        grid: GridConfig {
            vert_lines: GridLineConfig {
                visible: s.grid_vert.visible,
                color: grid.clone(),
            },
            horz_lines: GridLineConfig {
                visible: s.grid_horz.visible,
                color: grid,
            },
        },
        crosshair: CrosshairConfig {
            color: s.crosshair.color.clone().unwrap_or_else(|| text.clone()),
            width: s.crosshair.width,
            style: s.crosshair.style,
            opacity: s.crosshair.opacity,
            label_background: s.crosshair.label_background.clone().unwrap_or(text),
        },
        price_scale: PriceScaleConfig {
            mode: ScaleMode::Price,
            log: false,
            invert: false,
            price_only: false,
            free_pan: false,
            border_color: border.clone(),
            labels_visible: true,
            current_price_line: true,
            price_label: true,
            countdown: true,
            animate_last_price: false,
        },
        animations: AnimationsConfig {
            zoom: true,
            pan: true,
            autoscale: true,
            intro: true,
        },
        panes: PanesConfig {
            separator_color: border,
        },
        margins: s.margins,
        trades: TradesConfig {
            visible: true,
            labels: true,
            qty: true,
            long_color: crate::palette::TRADE_LONG.to_string(),
            short_color: crate::palette::TRADE_SHORT.to_string(),
            exit_color: crate::palette::TRADE_EXIT.to_string(),
        },
        time_scale: TimeScaleConfig {
            timezone: "UTC".to_string(),
        },
        marks: MarksConfig {
            visible: true,
            groups: BTreeMap::new(),
        },
        chart_types: BTreeMap::new(),
        candles: CandlesConfig {
            up_color: up.clone(),
            down_color: down.clone(),
            body_visible: s.candle.body_visible,
            border_visible: s.candle.border_visible,
            border_up_color: up.clone(),
            border_down_color: down.clone(),
            wick_visible: s.candle.wick_visible,
            wick_up_color: up.clone(),
            wick_down_color: down.clone(),
        },
        bars: BarsConfig {
            up_color: up.clone(),
            down_color: down,
        },
        line: LineConfig {
            color: s.line.color.unwrap_or_else(|| up.clone()),
            width: s.line.width,
        },
        area: AreaConfig {
            line_color: line_color.clone(),
            width: s.area.width,
            top_color: s.area.top_color.unwrap_or(line_color),
            bottom_color: s
                .area
                .bottom_color
                .unwrap_or_else(|| TRANSPARENT.to_string()),
        },
        baseline: BaselineConfig {
            top_fill_color: with_alpha(&top_line, BASELINE_FILL_ALPHA),
            top_fill_color2: with_alpha(&top_line, BASELINE_FILL_ALPHA_FAR),
            bottom_fill_color: with_alpha(&bottom_line, BASELINE_FILL_ALPHA),
            bottom_fill_color2: with_alpha(&bottom_line, BASELINE_FILL_ALPHA_FAR),
            top_line_color: top_line,
            bottom_line_color: bottom_line,
            width: s.baseline.width,
            baseline_level: s.baseline.baseline_level,
        },
        columns: ColumnsConfig {
            up_color: wash.up_color,
            down_color: wash.down_color,
        },
        series: SeriesConfig {
            style: "candles".to_string(),
            baseline: None,
            spacing: 1.0,
        },
        sessions: SessionsConfig {
            premarket_color: s.sessions.premarket_color,
            postmarket_color: s.sessions.postmarket_color,
            extended_color: s.sessions.extended_color,
        },
        stacking: StackingConfig {
            candles: 0.0,
            series: BTreeMap::new(),
        },
    }
}

// ── price styles ──

/// The library's own price styles, in picker order: [`price_style_ids`] appends
/// the SDK-registered chart types after these, so UIs can group the two families.
pub const BUILTIN_PRICE_STYLES: [&str; 9] = [
    "candles",
    "bars",
    "line",
    "linemarkers",
    "stepline",
    "area",
    "baseline",
    "columns",
    "heikinashi",
];

/// Base price-series painting for a style: built-ins paint themselves; a plugin
/// type may declare `BasePainting::None` to suppress candles under its layer.
pub fn base_painting_of(style: &str, registry: &ChartTypeRegistry) -> BasePainting {
    registry
        .get(style)
        .map_or(BasePainting::Candles, |t| t.base_painting())
}

/// Candle cosmetics a candle-based PLUGIN style stores in its own per-type bag
/// (`chartTypes.<id>.candle*`: reserved keys, edited by the Symbol tab's Candles
/// group while that style is active). Per-key: `None` inherits the shared
/// `candles` block, so an untouched plugin style paints exactly like before,
/// and edits never leak back into the candles/heikin-ashi styles.
#[derive(Clone, Debug, PartialEq)]
pub struct CandlePaintOverride {
    pub up_color: Option<String>,
    pub down_color: Option<String>,
    pub body_visible: Option<bool>,
    pub border_visible: Option<bool>,
    pub border_up_color: Option<String>,
    pub border_down_color: Option<String>,
    pub wick_visible: Option<bool>,
    pub wick_up_color: Option<String>,
    pub wick_down_color: Option<String>,
}

/// Whether a style carries its OWN candle cosmetics: a registered plugin type
/// whose base painting is candles. Built-ins (heikin-ashi included) share the
/// `candles` block instead.
pub fn has_own_candle_paint(style: &str, registry: &ChartTypeRegistry) -> bool {
    if BUILTIN_PRICE_STYLES.contains(&style) {
        return false;
    }
    registry
        .get(style)
        .is_some_and(|t| t.base_painting() == BasePainting::Candles)
}

/// The reserved per-type keys behind [`CandlePaintOverride`]: the Symbol tab's
/// Candles group writes them for a candle-based plugin style; `null` = inherit.
pub const CANDLE_OVERRIDE_KEYS: [&str; 9] = [
    "candleUpColor",
    "candleDownColor",
    "candleBodyVisible",
    "candleBorderVisible",
    "candleBorderUpColor",
    "candleBorderDownColor",
    "candleWickVisible",
    "candleWickUpColor",
    "candleWickDownColor",
];

/// The candle override for a style, read from the per-type bags: `None` for
/// built-ins and for `BasePainting::None` types (nothing of theirs is candle-painted).
pub fn candle_override_for(
    style: &str,
    bags: &ChartTypeBags,
    registry: &ChartTypeRegistry,
) -> Option<CandlePaintOverride> {
    if !has_own_candle_paint(style, registry) {
        return None;
    }
    let empty = Map::new();
    let bag = bags.get(style).unwrap_or(&empty);
    let color = |key: &str| color_of(bag.get(key)).map(str::to_string);
    let flag = |key: &str| bag.get(key).and_then(Value::as_bool);
    Some(CandlePaintOverride {
        up_color: color("candleUpColor"),
        down_color: color("candleDownColor"),
        body_visible: flag("candleBodyVisible"),
        border_visible: flag("candleBorderVisible"),
        border_up_color: color("candleBorderUpColor"),
        border_down_color: color("candleBorderDownColor"),
        wick_visible: flag("candleWickVisible"),
        wick_up_color: color("candleWickUpColor"),
        wick_down_color: color("candleWickDownColor"),
    })
}

/// The candle cosmetics to PAINT (see [`effective_candle_paint`]).
#[derive(Clone, Debug, PartialEq)]
pub struct EffectiveCandlePaint {
    pub up: String,
    pub down: String,
    pub candle: CandleStyle,
}

/// The candle cosmetics to PAINT: the shared block overridden per-key by the
/// active style's override; body colors default to the theme's up/down (shared
/// by both backends, so their candle paths stay pixel-identical).
pub fn effective_candle_paint(
    base: &CandleStyle,
    over: Option<&CandlePaintOverride>,
    theme_up: &str,
    theme_down: &str,
) -> EffectiveCandlePaint {
    let Some(o) = over else {
        return EffectiveCandlePaint {
            up: theme_up.to_string(),
            down: theme_down.to_string(),
            candle: base.clone(),
        };
    };
    let pick =
        |own: &Option<String>, shared: &Option<String>| own.clone().or_else(|| shared.clone());
    EffectiveCandlePaint {
        up: o.up_color.clone().unwrap_or_else(|| theme_up.to_string()),
        down: o
            .down_color
            .clone()
            .unwrap_or_else(|| theme_down.to_string()),
        candle: CandleStyle {
            body_visible: o.body_visible.unwrap_or(base.body_visible),
            border_visible: o.border_visible.unwrap_or(base.border_visible),
            border_up_color: pick(&o.border_up_color, &base.border_up_color),
            border_down_color: pick(&o.border_down_color, &base.border_down_color),
            wick_visible: o.wick_visible.unwrap_or(base.wick_visible),
            wick_up_color: pick(&o.wick_up_color, &base.wick_up_color),
            wick_down_color: pick(&o.wick_down_color, &base.wick_down_color),
        },
    }
}

/// Every valid price-style id RIGHT NOW: the built-ins plus SDK-registered chart types.
pub fn price_style_ids(registry: &ChartTypeRegistry) -> Vec<String> {
    let mut out: Vec<String> = BUILTIN_PRICE_STYLES.iter().map(|s| s.to_string()).collect();
    for t in registry.all() {
        if !out.contains(&t.id) {
            out.push(t.id.clone());
        }
    }
    out
}

// ── crosshair override ──

/// A runtime override of the local crosshair (the `crosshairOverride` feature):
/// for an interaction that asks the user to pick something on the chart and
/// wants the cursor to say so. Each field replaces the configured crosshair
/// style while set; `horizontal` / `vertical: false` drop that line together
/// with its axis chip. Never part of the saved config.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrosshairOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertical: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub horizontal: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<LineStyle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    /// Veil the plot right of the vertical line's bar (every pane, up to the
    /// price scale), e.g. to hide what comes after a time being picked.
    /// `opacity` defaults to 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shade_right: Option<ShadeRight>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShadeRight {
    pub color: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
}

/// Keep the well-formed fields of a `crosshairOverride` value; anything else
/// (or nothing left) gives `None`.
pub fn sanitize_crosshair_override(value: &Value) -> Option<CrosshairOverride> {
    let v = value.as_object()?;
    let mut out = CrosshairOverride::default();
    if let Some(b) = v.get("vertical").and_then(Value::as_bool) {
        out.vertical = Some(b);
    }
    if let Some(b) = v.get("horizontal").and_then(Value::as_bool) {
        out.horizontal = Some(b);
    }
    if let Some(c) = color_of(v.get("color")) {
        out.color = Some(c.to_string());
    }
    if let Some(n) = num_of(v.get("width")) {
        out.width = Some(n.clamp(0.5, 8.0));
    }
    if let Some(s) = line_style_of(v.get("style")) {
        out.style = Some(s);
    }
    if let Some(n) = num_of(v.get("opacity")) {
        out.opacity = Some(clamp_opacity(n));
    }
    if let Some(shade) = v.get("shadeRight").and_then(Value::as_object)
        && let Some(color) = color_of(shade.get("color"))
    {
        out.shade_right = Some(ShadeRight {
            color: color.to_string(),
            opacity: num_of(shade.get("opacity")).map(clamp_opacity),
        });
    }
    (out != CrosshairOverride::default()).then_some(out)
}

// ── validators (loose but safe: a chart consumes arbitrary CSS color strings) ──

/// A non-blank string (`typeof v === 'string' && v.trim().length > 0`).
fn color_of(v: Option<&Value>) -> Option<&str> {
    match v {
        Some(Value::String(s)) if !s.chars().all(is_js_whitespace) => Some(s),
        _ => None,
    }
}

/// A finite number (JSON numbers always are).
fn num_of(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|n| n.is_finite())
}

fn line_style_of(v: Option<&Value>) -> Option<LineStyle> {
    match v.and_then(Value::as_str)? {
        "solid" => Some(LineStyle::Solid),
        "dashed" => Some(LineStyle::Dashed),
        "dotted" => Some(LineStyle::Dotted),
        _ => None,
    }
}

fn scale_mode_of(v: Option<&Value>) -> Option<ScaleMode> {
    ScaleMode::parse(v.and_then(Value::as_str)?)
}

/// An object's entries. An array reads as an object keyed by index (as in
/// `Object.entries`), anything else as empty.
fn as_object(v: Option<&Value>) -> Map<String, Value> {
    match v {
        Some(Value::Object(m)) => m.clone(),
        Some(Value::Array(a)) => a
            .iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v.clone()))
            .collect(),
        _ => Map::new(),
    }
}

fn clamp_opacity(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

fn clamp_width(v: f64) -> f64 {
    v.clamp(1.0, 10.0)
}

fn clamp_level(v: f64) -> f64 {
    v.clamp(0.0, 100.0)
}

/// Bar-spacing multiplier: 1 = default, values below 1 tighten and above 1 widen the pitch.
/// Floored just above 0 (a 0 pitch would collapse every bar onto one pixel).
fn clamp_spacing(v: f64) -> f64 {
    v.clamp(0.1, 10.0)
}

/// Vertical margin percent: 0-40 each, so the data always keeps at least a fifth of the pane.
fn clamp_margin_pct(v: f64) -> f64 {
    v.clamp(0.0, 40.0)
}

/// Right margin in bars: a whole number, 0-200.
fn clamp_margin_bars(v: f64) -> f64 {
    js_round(v.clamp(0.0, 200.0))
}

// ── theme following ──

/// A theme's candle colors: the defaults of every series color that inherits them.
#[derive(Clone, Debug, PartialEq)]
pub struct CandleColors {
    pub up_color: String,
    pub down_color: String,
}

/// The colors a theme switch carries over: a color still on `prev`'s up or down
/// color is the theme's default, so it becomes `next`'s; any other color was
/// chosen and stays. For an inheriting (`None`) color, map the closure over the
/// option: it stays `None`.
pub fn theme_color_follower<'a>(
    prev: &'a CandleColors,
    next: &'a CandleColors,
) -> impl Fn(&str) -> String + 'a {
    move |color| {
        if color == prev.up_color {
            next.up_color.clone()
        } else if color == prev.down_color {
            next.down_color.clone()
        } else {
            color.to_string()
        }
    }
}

/// The columns' default colors for a theme's candle colors (see [`COLUMNS_FILL_ALPHA`]).
pub fn columns_default_colors(theme: &CandleColors) -> CandleColors {
    CandleColors {
        up_color: with_alpha(&theme.up_color, COLUMNS_FILL_ALPHA),
        down_color: with_alpha(&theme.down_color, COLUMNS_FILL_ALPHA),
    }
}

/// [`theme_color_follower`] for the columns block: its defaults are washes of
/// the theme's colors, so a wash of `prev`'s becomes the same wash of `next`'s,
/// on top of the plain follow.
pub fn columns_color_follower<'a>(
    prev: &'a CandleColors,
    next: &'a CandleColors,
) -> impl Fn(&str) -> String + 'a {
    let follow = theme_color_follower(prev, next);
    let wash_prev = columns_default_colors(prev);
    let wash_next = columns_default_colors(next);
    move |color| {
        let followed = follow(color);
        if followed == wash_prev.up_color {
            wash_next.up_color.clone()
        } else if followed == wash_prev.down_color {
            wash_next.down_color.clone()
        } else {
            followed
        }
    }
}

/// `config` with its series colors moved from `prev`'s candle colors to
/// `next`'s (see [`theme_color_follower`]).
pub fn follow_theme_colors(
    config: &ChartConfig,
    prev: &CandleColors,
    next: &CandleColors,
) -> ChartConfig {
    let follow = theme_color_follower(prev, next);
    let follow_columns = columns_color_follower(prev, next);
    let mut out = config.clone();
    let c = &config.candles;
    out.candles = CandlesConfig {
        up_color: follow(&c.up_color),
        down_color: follow(&c.down_color),
        border_up_color: follow(&c.border_up_color),
        border_down_color: follow(&c.border_down_color),
        wick_up_color: follow(&c.wick_up_color),
        wick_down_color: follow(&c.wick_down_color),
        ..c.clone()
    };
    out.bars = BarsConfig {
        up_color: follow(&config.bars.up_color),
        down_color: follow(&config.bars.down_color),
    };
    out.line.color = follow(&config.line.color);
    out.area.line_color = follow(&config.area.line_color);
    out.area.top_color = follow(&config.area.top_color);
    out.columns = ColumnsConfig {
        up_color: follow_columns(&config.columns.up_color),
        down_color: follow_columns(&config.columns.down_color),
    };
    out
}

// ── reset + merge ──

fn settings_value_json(v: &SettingsValue) -> Value {
    match v {
        SettingsValue::Bool(b) => Value::Bool(*b),
        SettingsValue::Text(t) => Value::String(t.clone()),
        SettingsValue::Number(n) => {
            serde_json::Number::from_f64(*n).map_or(Value::Null, Value::Number)
        }
    }
}

/// The document "Reset defaults" applies. [`merge_config`] is deliberately
/// ADDITIVE for `chartTypes` (a patch only touches the type ids it names), so
/// the factory snapshot alone cannot undo SDK settings edited after mount: the
/// reset document must name every registered type at its registry-declared row
/// defaults, covering EVERY key the section can store (instances, subsections,
/// range min/max, toggle swatches). The registry is read at call time (types
/// may register after mount), and values the snapshot itself pinned win over
/// the defvals: they ARE the first-run state.
///
/// The reset restores SETTINGS, not the view: the price style is what the user
/// picked to look at (topbar / style menu), and the snapshot only pins
/// whichever style was active at mount. The caller passes the LIVE style so
/// the document keeps the chart type the user is on while every one of its
/// settings goes back to default; with `None` the snapshot's own style applies
/// (template semantics).
///
/// A section's `instances` list counts as declared only when non-empty (the
/// Rust section cannot tell an absent list from an empty one).
pub fn factory_reset_config(
    factory: &ChartConfig,
    price_style: Option<&str>,
    registry: &ChartTypeRegistry,
) -> ChartConfig {
    let mut bag: ChartTypeBags = BTreeMap::new();
    for t in registry.all() {
        let Some(section) = &t.settings else {
            continue;
        };
        let mut defaults = Map::new();
        // Registry-enumerated: every key a row stores (toggle, inline controls, range
        // bounds), with no kind-specific walk that can miss a key the dialog seeds.
        let add_rows = |rows: &[SettingsRowDescriptor], defaults: &mut Map<String, Value>| {
            for r in rows {
                for k in settings_row_value_keys(r) {
                    defaults.insert(k.key, settings_value_json(&k.defval));
                }
            }
        };
        // An absent enable key means OFF: seed it explicitly so an instance the user
        // turned on cannot survive the additive merge; a row that declares the same key
        // (a subsection's master toggle) overrides it with its own defval via add_rows.
        for inst in &section.instances {
            if let Some(key) = &inst.enable_key {
                defaults.insert(key.clone(), Value::Bool(false));
            }
            add_rows(&inst.rows, &mut defaults);
        }
        for sub in &section.subsections {
            if let Some(key) = &sub.enable_key {
                defaults.insert(key.clone(), Value::Bool(false));
            }
            add_rows(&sub.rows, &mut defaults);
        }
        if section.instances.is_empty() {
            add_rows(&section.rows, &mut defaults);
        }
        bag.insert(t.id.clone(), defaults);
    }
    // The Symbol tab's candle rows of a candle-based plugin style are not registry
    // rows: they live on reserved `candle*` keys of the same bag, `null` meaning
    // "inherit the shared candles block". Name them too, or a hidden/recolored candle
    // set under a plugin style survives the reset.
    for t in registry.all() {
        if !has_own_candle_paint(&t.id, registry) {
            continue;
        }
        let defaults = bag.entry(t.id.clone()).or_default();
        for key in CANDLE_OVERRIDE_KEYS {
            defaults.insert(key.to_string(), Value::Null);
        }
    }
    for (type_id, vals) in &factory.chart_types {
        let slot = bag.entry(type_id.clone()).or_default();
        for (k, v) in vals {
            slot.insert(k.clone(), v.clone());
        }
    }
    let mut out = factory.clone();
    out.chart_types = bag;
    out.series.style = price_style.unwrap_or(&factory.series.style).to_string();
    out
}

/// Deep-merge an untrusted partial `patch` onto a known-good `base`, validating
/// every field and silently dropping malformed ones. Pure (returns a fresh
/// config, mutates nothing): the single reducer behind `applyConfig(json)` and
/// the import path.
pub fn merge_config(
    base: &ChartConfig,
    patch: &Value,
    registry: &ChartTypeRegistry,
) -> ChartConfig {
    let p = as_object(Some(patch));
    let sub = |key: &str| as_object(p.get(key));
    let ct_patch = sub("chartTypes");
    let mut chart_types_bag = base.chart_types.clone();
    for (type_id, vals) in &ct_patch {
        let slot = chart_types_bag.entry(type_id.clone()).or_default();
        for (k, v) in as_object(Some(vals)) {
            slot.insert(k, v);
        }
    }
    let layout = sub("layout");
    let grid = sub("grid");
    let gv = as_object(grid.get("vertLines"));
    let gh = as_object(grid.get("horzLines"));
    let cross = sub("crosshair");
    let ps = sub("priceScale");
    let anim = sub("animations");
    let panes = sub("panes");
    let margins = sub("margins");
    let trades = sub("trades");
    let ts = sub("timeScale");
    let marks = sub("marks");
    let mark_groups = as_object(marks.get("groups"));
    let candles = sub("candles");
    let bars = sub("bars");
    let line = sub("line");
    let area = sub("area");
    let baseline = sub("baseline");
    let columns = sub("columns");
    let series = sub("series");
    let sessions = sub("sessions");
    let stacking = sub("stacking");
    let stack_series = as_object(stacking.get("series"));

    let color = |m: &Map<String, Value>, key: &str, base: &str| -> String {
        color_of(m.get(key)).unwrap_or(base).to_string()
    };
    let flag = |m: &Map<String, Value>, key: &str, base: bool| -> bool {
        m.get(key).and_then(Value::as_bool).unwrap_or(base)
    };
    let number = |m: &Map<String, Value>, key: &str, base: f64, f: fn(f64) -> f64| -> f64 {
        num_of(m.get(key)).map_or(base, f)
    };

    let mut groups = base.marks.groups.clone();
    for (id, v) in &mark_groups {
        if let Some(b) = v.as_bool() {
            groups.insert(id.clone(), b);
        }
    }
    let mut stack = base.stacking.series.clone();
    for (id, z) in &stack_series {
        if let Some(z) = num_of(Some(z)) {
            stack.insert(id.clone(), z);
        }
    }
    let price_style_valid = |v: Option<&Value>| -> Option<String> {
        let s = v?.as_str()?;
        price_style_ids(registry)
            .iter()
            .any(|id| id == s)
            .then(|| s.to_string())
    };

    ChartConfig {
        version: CHART_CONFIG_VERSION,
        chart_types: chart_types_bag,
        layout: LayoutConfig {
            background: color(&layout, "background", &base.layout.background),
            text_color: color(&layout, "textColor", &base.layout.text_color),
            font_family: color(&layout, "fontFamily", &base.layout.font_family),
            font_size: number(&layout, "fontSize", base.layout.font_size, |v| {
                v.clamp(6.0, 32.0)
            }),
        },
        grid: GridConfig {
            vert_lines: GridLineConfig {
                visible: flag(&gv, "visible", base.grid.vert_lines.visible),
                color: color(&gv, "color", &base.grid.vert_lines.color),
            },
            horz_lines: GridLineConfig {
                visible: flag(&gh, "visible", base.grid.horz_lines.visible),
                color: color(&gh, "color", &base.grid.horz_lines.color),
            },
        },
        crosshair: CrosshairConfig {
            color: color(&cross, "color", &base.crosshair.color),
            width: number(&cross, "width", base.crosshair.width, |v| v.clamp(0.5, 8.0)),
            style: line_style_of(cross.get("style")).unwrap_or(base.crosshair.style),
            opacity: number(&cross, "opacity", base.crosshair.opacity, clamp_opacity),
            label_background: color(&cross, "labelBackground", &base.crosshair.label_background),
        },
        price_scale: PriceScaleConfig {
            mode: scale_mode_of(ps.get("mode")).unwrap_or(base.price_scale.mode),
            log: flag(&ps, "log", base.price_scale.log),
            invert: flag(&ps, "invert", base.price_scale.invert),
            price_only: flag(&ps, "priceOnly", base.price_scale.price_only),
            free_pan: flag(&ps, "freePan", base.price_scale.free_pan),
            border_color: color(&ps, "borderColor", &base.price_scale.border_color),
            labels_visible: flag(&ps, "labelsVisible", base.price_scale.labels_visible),
            current_price_line: flag(&ps, "currentPriceLine", base.price_scale.current_price_line),
            price_label: flag(&ps, "priceLabel", base.price_scale.price_label),
            countdown: flag(&ps, "countdown", base.price_scale.countdown),
            animate_last_price: flag(&ps, "animateLastPrice", base.price_scale.animate_last_price),
        },
        animations: AnimationsConfig {
            zoom: flag(&anim, "zoom", base.animations.zoom),
            pan: flag(&anim, "pan", base.animations.pan),
            autoscale: flag(&anim, "autoscale", base.animations.autoscale),
            intro: flag(&anim, "intro", base.animations.intro),
        },
        panes: PanesConfig {
            separator_color: color(&panes, "separatorColor", &base.panes.separator_color),
        },
        margins: ChartMargins {
            top: number(&margins, "top", base.margins.top, clamp_margin_pct),
            bottom: number(&margins, "bottom", base.margins.bottom, clamp_margin_pct),
            right: number(&margins, "right", base.margins.right, clamp_margin_bars),
        },
        trades: TradesConfig {
            visible: flag(&trades, "visible", base.trades.visible),
            labels: flag(&trades, "labels", base.trades.labels),
            qty: flag(&trades, "qty", base.trades.qty),
            long_color: color(&trades, "longColor", &base.trades.long_color),
            short_color: color(&trades, "shortColor", &base.trades.short_color),
            exit_color: color(&trades, "exitColor", &base.trades.exit_color),
        },
        time_scale: TimeScaleConfig {
            timezone: match ts.get("timezone") {
                Some(Value::String(s)) if !s.is_empty() => s.clone(),
                _ => base.time_scale.timezone.clone(),
            },
        },
        marks: MarksConfig {
            visible: flag(&marks, "visible", base.marks.visible),
            // Additive like `stacking.series`: a patch names only the groups it carries, so a
            // choice stored for a group the host has not registered yet survives verbatim.
            groups,
        },
        candles: CandlesConfig {
            up_color: color(&candles, "upColor", &base.candles.up_color),
            down_color: color(&candles, "downColor", &base.candles.down_color),
            body_visible: flag(&candles, "bodyVisible", base.candles.body_visible),
            border_visible: flag(&candles, "borderVisible", base.candles.border_visible),
            border_up_color: color(&candles, "borderUpColor", &base.candles.border_up_color),
            border_down_color: color(&candles, "borderDownColor", &base.candles.border_down_color),
            wick_visible: flag(&candles, "wickVisible", base.candles.wick_visible),
            wick_up_color: color(&candles, "wickUpColor", &base.candles.wick_up_color),
            wick_down_color: color(&candles, "wickDownColor", &base.candles.wick_down_color),
        },
        bars: BarsConfig {
            up_color: color(&bars, "upColor", &base.bars.up_color),
            down_color: color(&bars, "downColor", &base.bars.down_color),
        },
        line: LineConfig {
            color: color(&line, "color", &base.line.color),
            width: number(&line, "width", base.line.width, clamp_width),
        },
        area: AreaConfig {
            line_color: color(&area, "lineColor", &base.area.line_color),
            width: number(&area, "width", base.area.width, clamp_width),
            top_color: color(&area, "topColor", &base.area.top_color),
            bottom_color: color(&area, "bottomColor", &base.area.bottom_color),
        },
        baseline: BaselineConfig {
            top_line_color: color(&baseline, "topLineColor", &base.baseline.top_line_color),
            bottom_line_color: color(
                &baseline,
                "bottomLineColor",
                &base.baseline.bottom_line_color,
            ),
            top_fill_color: color(&baseline, "topFillColor", &base.baseline.top_fill_color),
            top_fill_color2: color(&baseline, "topFillColor2", &base.baseline.top_fill_color2),
            bottom_fill_color: color(
                &baseline,
                "bottomFillColor",
                &base.baseline.bottom_fill_color,
            ),
            bottom_fill_color2: color(
                &baseline,
                "bottomFillColor2",
                &base.baseline.bottom_fill_color2,
            ),
            width: number(&baseline, "width", base.baseline.width, clamp_width),
            baseline_level: number(
                &baseline,
                "baselineLevel",
                base.baseline.baseline_level,
                clamp_level,
            ),
        },
        columns: ColumnsConfig {
            up_color: color(&columns, "upColor", &base.columns.up_color),
            down_color: color(&columns, "downColor", &base.columns.down_color),
        },
        series: SeriesConfig {
            style: price_style_valid(series.get("style"))
                .unwrap_or_else(|| base.series.style.clone()),
            baseline: match series.get("baseline") {
                Some(Value::Null) => None,
                other => num_of(other).or(base.series.baseline),
            },
            spacing: number(&series, "spacing", base.series.spacing, clamp_spacing),
        },
        sessions: SessionsConfig {
            premarket_color: color(&sessions, "premarketColor", &base.sessions.premarket_color),
            postmarket_color: color(
                &sessions,
                "postmarketColor",
                &base.sessions.postmarket_color,
            ),
            extended_color: color(&sessions, "extendedColor", &base.sessions.extended_color),
        },
        stacking: StackingConfig {
            candles: num_of(stacking.get("candles")).unwrap_or(base.stacking.candles),
            // Additive like `chartTypes`: a patch names only the ids it carries; the rest keep
            // their current keys (ids belong to the session's indicators, not the template).
            series: stack,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chart_types::{
        ChartTypeDefinition, ChartTypeSettingsInstance, ChartTypeSettingsSection,
        ChartTypeSettingsSubsection, SettingsInlineControl, SettingsRowInlineNumber,
        SettingsRowSwatch, SettingsRowToggle, SettingsRowWidth, SettingsSelectOption,
    };
    use crate::i18n::MessageLike;
    use serde_json::json;

    fn base_config() -> ChartConfig {
        default_chart_config(&dark_theme())
    }

    fn merge(base: &ChartConfig, patch: Value) -> ChartConfig {
        merge_config(base, &patch, &ChartTypeRegistry::builtin())
    }

    fn label(s: &str) -> MessageLike {
        MessageLike::Text(s.to_string().into())
    }

    // ── mergeConfig ──

    #[test]
    fn returns_the_base_unchanged_for_an_empty_or_non_object_patch() {
        let base = base_config();
        assert_eq!(merge(&base, json!({})), base);
        assert_eq!(merge(&base, Value::Null), base);
        assert_eq!(merge(&base, json!(42)), base);
        assert_eq!(merge(&base, json!("text")), base);
        assert_eq!(merge(&base, json!([1, 2])), base);
    }

    #[test]
    fn applies_only_the_named_fields_of_a_partial_patch() {
        let base = base_config();
        let out = merge(&base, json!({ "candles": { "upColor": "#123456" } }));
        assert_eq!(out.candles.up_color, "#123456");
        assert_eq!(out.candles.down_color, base.candles.down_color);
        assert_eq!(out.layout, base.layout);
    }

    #[test]
    fn drops_malformed_fields_and_keeps_the_base_value() {
        let base = base_config();
        let out = merge(
            &base,
            json!({
                "layout": { "background": 123, "fontSize": "big" },
                "crosshair": { "style": "zigzag", "width": -5 },
                "priceScale": { "mode": "nope", "log": "yes" },
                "series": { "style": "renko" },
            }),
        );
        assert_eq!(out.layout.background, base.layout.background);
        assert_eq!(out.layout.font_size, base.layout.font_size);
        assert_eq!(out.crosshair.style, base.crosshair.style);
        assert_eq!(out.price_scale.mode, base.price_scale.mode);
        assert_eq!(out.price_scale.log, base.price_scale.log);
        assert_eq!(out.series.style, base.series.style);
        // a valid-but-clamped value still applies
        assert_eq!(
            merge(&base, json!({ "crosshair": { "width": 4 } }))
                .crosshair
                .width,
            4.0
        );
        // a negative width clamps to the floor
        assert_eq!(
            merge(&base, json!({ "crosshair": { "width": -5 } }))
                .crosshair
                .width,
            0.5
        );
    }

    #[test]
    fn blank_strings_are_not_colors() {
        let base = base_config();
        let out = merge(
            &base,
            json!({ "layout": { "background": "   ", "textColor": "\u{a0}\u{feff}" }, "line": { "color": "" } }),
        );
        assert_eq!(out.layout.background, base.layout.background);
        assert_eq!(out.layout.text_color, base.layout.text_color);
        assert_eq!(out.line.color, base.line.color);
    }

    #[test]
    fn clamps_opacity_to_0_1_and_font_size_to_a_sane_range() {
        let base = base_config();
        assert_eq!(
            merge(&base, json!({ "crosshair": { "opacity": 5 } }))
                .crosshair
                .opacity,
            1.0
        );
        assert_eq!(
            merge(&base, json!({ "crosshair": { "opacity": -1 } }))
                .crosshair
                .opacity,
            0.0
        );
        assert_eq!(
            merge(&base, json!({ "layout": { "fontSize": 999 } }))
                .layout
                .font_size,
            32.0
        );
        assert_eq!(
            merge(&base, json!({ "layout": { "fontSize": 1 } }))
                .layout
                .font_size,
            6.0
        );
    }

    #[test]
    fn series_baseline_accepts_a_number_or_explicit_null() {
        let base = base_config();
        assert_eq!(
            merge(&base, json!({ "series": { "baseline": 100 } }))
                .series
                .baseline,
            Some(100.0)
        );
        assert_eq!(
            merge(&base, json!({ "series": { "baseline": null } }))
                .series
                .baseline,
            None
        );
        assert_eq!(
            merge(&base, json!({ "series": { "baseline": "x" } }))
                .series
                .baseline,
            base.series.baseline
        );
        let mut with_level = base.clone();
        with_level.series.baseline = Some(7.0);
        assert_eq!(
            merge(&with_level, json!({ "series": { "baseline": "x" } }))
                .series
                .baseline,
            Some(7.0)
        );
        assert_eq!(
            merge(&with_level, json!({ "series": {} })).series.baseline,
            Some(7.0)
        );
    }

    #[test]
    fn series_spacing_defaults_to_1_clamps_to_0_1_10_and_drops_non_numbers() {
        let base = base_config();
        assert_eq!(base.series.spacing, 1.0);
        let spacing = |v: Value| {
            merge(&base, json!({ "series": { "spacing": v } }))
                .series
                .spacing
        };
        assert_eq!(spacing(json!(2.5)), 2.5);
        assert_eq!(spacing(json!(0.5)), 0.5);
        assert_eq!(spacing(json!(0)), 0.1);
        assert_eq!(spacing(json!(-3)), 0.1);
        assert_eq!(spacing(json!(999)), 10.0);
        assert_eq!(spacing(json!("x")), base.series.spacing);
    }

    #[test]
    fn trades_apply_valid_fields_and_drop_malformed_ones() {
        let base = base_config();
        assert_eq!(
            base.trades,
            TradesConfig {
                visible: true,
                labels: true,
                qty: true,
                long_color: "#2962ff".into(),
                short_color: "#f23645".into(),
                exit_color: "#d500f9".into(),
            }
        );
        let out = merge(
            &base,
            json!({ "trades": { "qty": false, "exitColor": "#111111", "visible": "yes", "longColor": 7 } }),
        );
        assert!(!out.trades.qty);
        assert_eq!(out.trades.exit_color, "#111111");
        assert_eq!(out.trades.visible, base.trades.visible);
        assert_eq!(out.trades.long_color, base.trades.long_color);
        assert_eq!(out.trades.labels, base.trades.labels);
    }

    #[test]
    fn merges_the_pane_separator_color_and_drops_a_malformed_one() {
        let base = base_config();
        assert_eq!(
            merge(&base, json!({ "panes": { "separatorColor": "#abcdef" } }))
                .panes
                .separator_color,
            "#abcdef"
        );
        assert_eq!(
            merge(&base, json!({ "panes": { "separatorColor": 42 } }))
                .panes
                .separator_color,
            base.panes.separator_color
        );
    }

    #[test]
    fn margins_defaults_clamping_and_malformed_values() {
        let base = base_config();
        assert_eq!(
            base.margins,
            ChartMargins {
                top: 10.0,
                bottom: 10.0,
                right: 10.0
            }
        );
        let out = merge(
            &base,
            json!({ "margins": { "top": 8, "bottom": 99, "right": 12.4 } }),
        );
        assert_eq!(
            out.margins,
            ChartMargins {
                top: 8.0,
                bottom: 40.0,
                right: 12.0
            }
        );
        assert_eq!(
            merge(&base, json!({ "margins": { "right": -3 } }))
                .margins
                .right,
            0.0
        );
        assert_eq!(
            merge(&base, json!({ "margins": { "right": 999 } }))
                .margins
                .right,
            200.0
        );
        assert_eq!(
            merge(&base, json!({ "margins": { "right": 12.5 } }))
                .margins
                .right,
            13.0
        );
        assert_eq!(
            merge(&base, json!({ "margins": { "top": "x", "right": null } })).margins,
            base.margins
        );
    }

    #[test]
    fn always_pins_the_current_version() {
        let base = base_config();
        assert_eq!(
            merge(&base, json!({ "version": 999 })).version,
            CHART_CONFIG_VERSION
        );
    }

    #[test]
    fn merges_stacking_additively() {
        let mut base = base_config();
        base.stacking = StackingConfig {
            candles: 0.0,
            series: BTreeMap::from([("ind-1".to_string(), -1.0), ("ind-2".to_string(), -2.0)]),
        };
        let out = merge(
            &base,
            json!({ "stacking": { "candles": 3, "series": { "ind-2": 5, "ind-3": "junk" } } }),
        );
        assert_eq!(
            out.stacking,
            StackingConfig {
                candles: 3.0,
                series: BTreeMap::from([("ind-1".to_string(), -1.0), ("ind-2".to_string(), 5.0)]),
            }
        );
        assert_eq!(merge(&base, json!({})).stacking, base.stacking);
    }

    #[test]
    fn merges_mark_groups_additively_and_drops_non_booleans() {
        let mut base = base_config();
        base.marks.groups.insert("dividends".into(), true);
        let out = merge(
            &base,
            json!({ "marks": { "visible": false, "groups": { "earnings": false, "dividends": "no", "x": 1 } } }),
        );
        assert!(!out.marks.visible);
        assert_eq!(
            out.marks.groups,
            BTreeMap::from([
                ("dividends".to_string(), true),
                ("earnings".to_string(), false)
            ])
        );
    }

    #[test]
    fn the_time_zone_must_be_a_non_empty_string() {
        let base = base_config();
        let zone = |v: Value| {
            merge(&base, json!({ "timeScale": { "timezone": v } }))
                .time_scale
                .timezone
        };
        assert_eq!(zone(json!("America/New_York")), "America/New_York");
        assert_eq!(zone(json!("")), "UTC");
        assert_eq!(zone(json!(5)), "UTC");
    }

    #[test]
    fn scale_modes_validate() {
        let base = base_config();
        let mode = |v: Value| {
            merge(&base, json!({ "priceScale": { "mode": v } }))
                .price_scale
                .mode
        };
        assert_eq!(mode(json!("percent")), ScaleMode::Percent);
        assert_eq!(mode(json!("indexed")), ScaleMode::Indexed);
        assert_eq!(mode(json!("PERCENT")), ScaleMode::Price);
        assert_eq!(mode(json!(null)), ScaleMode::Price);
    }

    #[test]
    fn price_style_accepts_built_ins_and_registered_types_only() {
        let base = base_config();
        let mut registry = ChartTypeRegistry::new();
        registry.register(ChartTypeDefinition::new("renko"));
        let style = |reg: &ChartTypeRegistry, v: &str| {
            merge_config(&base, &json!({ "series": { "style": v } }), reg)
                .series
                .style
        };
        assert_eq!(style(&registry, "renko"), "renko");
        assert_eq!(style(&registry, "area"), "area");
        assert_eq!(style(&ChartTypeRegistry::new(), "renko"), "candles");
        assert_eq!(style(&ChartTypeRegistry::new(), "heikinashi"), "heikinashi");
    }

    #[test]
    fn an_array_chart_types_patch_reads_like_object_entries_by_index() {
        let base = base_config();
        let out = merge(&base, json!({ "chartTypes": ["a"] }));
        assert_eq!(out.chart_types.get("0"), Some(&Map::new()));
    }

    #[test]
    fn animations_validates_the_block() {
        let base = base_config();
        assert_eq!(
            base.animations,
            AnimationsConfig {
                zoom: true,
                pan: true,
                autoscale: true,
                intro: true
            }
        );
        let out = merge(
            &base,
            json!({ "animations": { "zoom": false, "pan": "no", "autoscale": 0, "extra": true } }),
        );
        assert_eq!(
            out.animations,
            AnimationsConfig {
                zoom: false,
                pan: true,
                autoscale: true,
                intro: true
            }
        );
        assert_eq!(merge(&base, json!({})).animations, base.animations);
        assert_eq!(
            merge(&base, json!({ "animations": "off" })).animations,
            base.animations
        );
    }

    #[test]
    fn per_style_fields_merge_and_ignore_malformed_ones_and_widths_clamp() {
        let base = base_config();
        let out = merge(
            &base,
            json!({
                "area": { "lineColor": "#010203", "topColor": 7, "width": 99 },
                "baseline": { "topLineColor": "#040506", "width": -3 },
            }),
        );
        assert_eq!(out.area.line_color, "#010203");
        assert_eq!(out.area.top_color, base.area.top_color);
        assert_eq!(out.area.width, 10.0);
        assert_eq!(out.baseline.top_line_color, "#040506");
        assert_eq!(out.baseline.width, 1.0);
    }

    #[test]
    fn accepts_rgba_fill_colors() {
        let base = base_config();
        let out = merge(
            &base,
            json!({ "area": { "topColor": "rgba(13,152,198,0.35)", "bottomColor": "rgba(13,152,198,0)" } }),
        );
        assert_eq!(out.area.top_color, "rgba(13,152,198,0.35)");
        assert_eq!(out.area.bottom_color, "rgba(13,152,198,0)");
    }

    #[test]
    fn keeps_the_second_baseline_fill_stops_and_clamps_the_level() {
        let base = base_config();
        let out = merge(
            &base,
            json!({ "baseline": { "topFillColor2": "rgba(1,2,3,0.4)", "bottomFillColor2": "rgba(4,5,6,0.4)", "baselineLevel": 250 } }),
        );
        assert_eq!(out.baseline.top_fill_color2, "rgba(1,2,3,0.4)");
        assert_eq!(out.baseline.bottom_fill_color2, "rgba(4,5,6,0.4)");
        assert_eq!(out.baseline.baseline_level, 100.0);
        assert_eq!(
            merge(&out, json!({ "baseline": { "baselineLevel": -10 } }))
                .baseline
                .baseline_level,
            0.0
        );
    }

    #[test]
    fn styling_one_chart_type_does_not_bleed_into_the_others() {
        let out = merge(
            &base_config(),
            json!({ "line": { "color": "#ff00ff", "width": 4 } }),
        );
        assert_eq!(
            out.line,
            LineConfig {
                color: "#ff00ff".into(),
                width: 4.0
            }
        );
        assert_eq!(out.candles.up_color, "#089981");
        assert_eq!(out.bars.up_color, "#089981");
        assert_eq!(out.area.line_color, "#3b82f6");
        assert_eq!(out.baseline.top_line_color, "#089981");
    }

    #[test]
    fn merge_is_pure() {
        let base = base_config();
        let snapshot = base.clone();
        let _ = merge(
            &base,
            json!({ "candles": { "upColor": "#000000" }, "chartTypes": { "x": { "a": 1 } } }),
        );
        assert_eq!(base, snapshot);
    }

    // ── defaults ──

    #[test]
    fn default_chart_style_reproduces_current_rendering() {
        let s = default_chart_style();
        assert_eq!(s.font_size, 11.0);
        assert_eq!(
            s.grid_vert,
            GridLineStyle {
                visible: true,
                color: None
            }
        );
        assert_eq!(
            s.grid_horz,
            GridLineStyle {
                visible: true,
                color: None
            }
        );
        // Axis border + pane separator inherit the theme border, so a light theme
        // resolves light chrome lines instead of the dark constants.
        assert_eq!(s.border_color, None);
        assert_eq!(s.separator_color, None);
        assert_eq!(
            s.crosshair,
            CrosshairStyle {
                color: Some("#9aa0ad".into()),
                width: 1.0,
                style: LineStyle::Dashed,
                opacity: 0.4,
                label_background: Some("#595959".into()),
            }
        );
        assert!(!s.candle.border_visible);
        assert!(s.candle.wick_visible);
        assert_eq!(s.sessions.premarket_color, "rgba(255,152,0,0.08)");
        assert_eq!(s.sessions.postmarket_color, "rgba(41,98,255,0.08)");
        assert_eq!(s.sessions.extended_color, s.sessions.postmarket_color);
    }

    #[test]
    fn the_default_config_resolves_to_concrete_values_from_the_dark_theme() {
        let cfg = base_config();
        assert_eq!(cfg.version, CHART_CONFIG_VERSION);
        assert_eq!(
            cfg.layout,
            LayoutConfig {
                background: "#151619".into(),
                text_color: "#b2b5be".into(),
                font_family: "sans-serif".into(),
                font_size: 11.0,
            }
        );
        assert_eq!(
            cfg.grid.vert_lines,
            GridLineConfig {
                visible: true,
                color: "#20222c".into()
            }
        );
        assert_eq!(
            cfg.grid.horz_lines,
            GridLineConfig {
                visible: true,
                color: "#20222c".into()
            }
        );
        assert_eq!(
            cfg.crosshair,
            CrosshairConfig {
                color: "#9aa0ad".into(),
                width: 1.0,
                style: LineStyle::Dashed,
                opacity: 0.4,
                label_background: "#595959".into(),
            }
        );
        assert_eq!(
            cfg.price_scale,
            PriceScaleConfig {
                mode: ScaleMode::Price,
                log: false,
                invert: false,
                price_only: false,
                free_pan: false,
                border_color: "#2a2b30".into(),
                labels_visible: true,
                current_price_line: true,
                price_label: true,
                countdown: true,
                animate_last_price: false,
            }
        );
        assert_eq!(cfg.panes.separator_color, "#2a2b30");
        assert_eq!(cfg.time_scale.timezone, "UTC");
        assert_eq!(cfg.candles.up_color, "#089981");
        assert_eq!(cfg.candles.down_color, "#f23645");
        assert_eq!(cfg.candles.border_up_color, "#089981");
        assert_eq!(cfg.candles.wick_down_color, "#f23645");
        assert_eq!(
            cfg.series,
            SeriesConfig {
                style: "candles".into(),
                baseline: None,
                spacing: 1.0
            }
        );
        assert_eq!(cfg.chart_types, BTreeMap::new());
    }

    #[test]
    fn per_style_defaults_are_the_candle_palette_and_the_series_blue() {
        let cfg = base_config();
        assert_eq!(
            cfg.bars,
            BarsConfig {
                up_color: "#089981".into(),
                down_color: "#f23645".into()
            }
        );
        assert_eq!(
            cfg.line,
            LineConfig {
                color: "#3b82f6".into(),
                width: 2.0
            }
        );
        assert_eq!(
            cfg.area,
            AreaConfig {
                line_color: "#3b82f6".into(),
                width: 2.0,
                top_color: "rgba(59,130,246,0.28)".into(),
                bottom_color: "rgba(59,130,246,0.02)".into(),
            }
        );
        assert_eq!(
            cfg.baseline,
            BaselineConfig {
                top_line_color: "#089981".into(),
                bottom_line_color: "#f23645".into(),
                top_fill_color: "rgba(8,153,129,0.25)".into(),
                top_fill_color2: "rgba(8,153,129,0.05)".into(),
                bottom_fill_color: "rgba(242,54,69,0.25)".into(),
                bottom_fill_color2: "rgba(242,54,69,0.05)".into(),
                width: 2.0,
                baseline_level: 50.0,
            }
        );
        assert_eq!(
            cfg.columns,
            ColumnsConfig {
                up_color: "rgba(8,153,129,0.5)".into(),
                down_color: "rgba(242,54,69,0.5)".into(),
            }
        );
    }

    #[test]
    fn the_config_json_matches_the_original_field_for_field() {
        let cfg = base_config();
        let json = serde_json::to_value(&cfg).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "version": 1,
                "chartTypes": {},
                "layout": { "background": "#151619", "textColor": "#b2b5be", "fontFamily": "sans-serif", "fontSize": 11.0 },
                "grid": {
                    "vertLines": { "visible": true, "color": "#20222c" },
                    "horzLines": { "visible": true, "color": "#20222c" },
                },
                "crosshair": { "color": "#9aa0ad", "width": 1.0, "style": "dashed", "opacity": 0.4, "labelBackground": "#595959" },
                "priceScale": {
                    "mode": "price", "log": false, "invert": false, "priceOnly": false, "freePan": false,
                    "borderColor": "#2a2b30", "labelsVisible": true, "currentPriceLine": true,
                    "priceLabel": true, "countdown": true, "animateLastPrice": false,
                },
                "animations": { "zoom": true, "pan": true, "autoscale": true, "intro": true },
                "panes": { "separatorColor": "#2a2b30" },
                "margins": { "top": 10.0, "bottom": 10.0, "right": 10.0 },
                "trades": {
                    "visible": true, "labels": true, "qty": true,
                    "longColor": "#2962ff", "shortColor": "#f23645", "exitColor": "#d500f9",
                },
                "timeScale": { "timezone": "UTC" },
                "marks": { "visible": true, "groups": {} },
                "candles": {
                    "upColor": "#089981", "downColor": "#f23645", "bodyVisible": true, "borderVisible": false,
                    "borderUpColor": "#089981", "borderDownColor": "#f23645", "wickVisible": true,
                    "wickUpColor": "#089981", "wickDownColor": "#f23645",
                },
                "bars": { "upColor": "#089981", "downColor": "#f23645" },
                "line": { "color": "#3b82f6", "width": 2.0 },
                "area": { "lineColor": "#3b82f6", "width": 2.0, "topColor": "rgba(59,130,246,0.28)", "bottomColor": "rgba(59,130,246,0.02)" },
                "baseline": {
                    "topLineColor": "#089981", "bottomLineColor": "#f23645",
                    "topFillColor": "rgba(8,153,129,0.25)", "topFillColor2": "rgba(8,153,129,0.05)",
                    "bottomFillColor": "rgba(242,54,69,0.25)", "bottomFillColor2": "rgba(242,54,69,0.05)",
                    "width": 2.0, "baselineLevel": 50.0,
                },
                "columns": { "upColor": "rgba(8,153,129,0.5)", "downColor": "rgba(242,54,69,0.5)" },
                "series": { "style": "candles", "baseline": null, "spacing": 1.0 },
                "sessions": {
                    "premarketColor": "rgba(255,152,0,0.08)",
                    "postmarketColor": "rgba(41,98,255,0.08)",
                    "extendedColor": "rgba(41,98,255,0.08)",
                },
                "stacking": { "candles": 0.0, "series": {} },
            })
        );
    }

    #[test]
    fn the_config_round_trips_through_json_text() {
        let cfg = base_config();
        let text = serde_json::to_string(&cfg).unwrap();
        let back: ChartConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(back, cfg);
        // and merging its own JSON onto itself changes nothing
        assert_eq!(merge(&cfg, serde_json::to_value(&cfg).unwrap()), cfg);
    }

    #[test]
    fn a_light_theme_resolves_its_own_chrome() {
        let cfg = default_chart_config(&crate::theme::light_theme());
        assert_eq!(cfg.layout.background, "#ffffff");
        assert_eq!(cfg.grid.vert_lines.color, "#cccccc");
        assert_eq!(cfg.price_scale.border_color, "#d4dae3");
        assert_eq!(cfg.panes.separator_color, "#d4dae3");
    }

    // ── theme following ──

    #[test]
    fn candle_colors_follow_the_theme_and_other_colors_stay() {
        let factory = base_config();
        let prev = CandleColors {
            up_color: BULLISH.into(),
            down_color: BEARISH.into(),
        };
        let next = CandleColors {
            up_color: "#00aaff".into(),
            down_color: "#ff00aa".into(),
        };
        let doc = follow_theme_colors(&factory, &prev, &next);
        assert_eq!(doc.candles.up_color, "#00aaff");
        assert_eq!(doc.candles.down_color, "#ff00aa");
        assert_eq!(doc.candles.wick_up_color, "#00aaff");
        assert_eq!(doc.candles.border_down_color, "#ff00aa");
        assert_eq!(doc.bars.up_color, "#00aaff");
        assert_eq!(doc.line.color, factory.line.color);
        assert_eq!(doc.layout, factory.layout);
        // the columns wash follows too
        assert_eq!(doc.columns.up_color, "rgba(0,170,255,0.5)");
        assert_eq!(doc.columns.down_color, "rgba(255,0,170,0.5)");
        // the baseline owns its palette
        assert_eq!(doc.baseline, factory.baseline);
    }

    #[test]
    fn a_chosen_color_survives_a_theme_switch() {
        let mut factory = base_config();
        factory.candles.up_color = "#123456".into();
        let prev = CandleColors {
            up_color: BULLISH.into(),
            down_color: BEARISH.into(),
        };
        let next = CandleColors {
            up_color: "#00aaff".into(),
            down_color: "#ff00aa".into(),
        };
        let doc = follow_theme_colors(&factory, &prev, &next);
        assert_eq!(doc.candles.up_color, "#123456");
        let follow = theme_color_follower(&prev, &next);
        assert_eq!(follow(BEARISH), "#ff00aa");
        assert_eq!(follow("#abcdef"), "#abcdef");
        // an inheriting color stays inheriting
        let inherited: Option<&str> = None;
        assert_eq!(inherited.map(&follow), None);
    }

    // ── crosshair override ──

    #[test]
    fn sanitizes_a_crosshair_override() {
        let out = sanitize_crosshair_override(&json!({
            "vertical": false, "horizontal": "x", "color": "#fff", "width": 99, "style": "dotted",
            "opacity": 3, "shadeRight": { "color": "#000", "opacity": -1 }, "junk": 1,
        }))
        .unwrap();
        assert_eq!(out.vertical, Some(false));
        assert_eq!(out.horizontal, None);
        assert_eq!(out.color.as_deref(), Some("#fff"));
        assert_eq!(out.width, Some(8.0));
        assert_eq!(out.style, Some(LineStyle::Dotted));
        assert_eq!(out.opacity, Some(1.0));
        assert_eq!(
            out.shade_right,
            Some(ShadeRight {
                color: "#000".into(),
                opacity: Some(0.0)
            })
        );
        let bare =
            sanitize_crosshair_override(&json!({ "shadeRight": { "color": "#000" } })).unwrap();
        assert_eq!(
            bare.shade_right,
            Some(ShadeRight {
                color: "#000".into(),
                opacity: None
            })
        );
    }

    #[test]
    fn a_crosshair_override_with_nothing_valid_is_none() {
        for v in [
            json!(null),
            json!(3),
            json!("x"),
            json!({}),
            json!({ "width": "x", "color": " " }),
            json!({ "shadeRight": { "opacity": 1 } }),
        ] {
            assert_eq!(sanitize_crosshair_override(&v), None, "{v}");
        }
    }

    // ── candle paint ──

    fn candle_type(id: &str, painting: Option<BasePainting>) -> ChartTypeDefinition {
        let mut def = ChartTypeDefinition::new(id);
        def.base_painting = painting;
        def
    }

    #[test]
    fn base_painting_and_own_candle_paint_follow_the_registry() {
        let mut reg = ChartTypeRegistry::builtin();
        reg.register(candle_type("plain", None));
        reg.register(candle_type("hidden", Some(BasePainting::None)));
        assert_eq!(base_painting_of("candles", &reg), BasePainting::Candles);
        assert_eq!(base_painting_of("unknown", &reg), BasePainting::Candles);
        assert_eq!(base_painting_of("hidden", &reg), BasePainting::None);
        assert!(has_own_candle_paint("plain", &reg));
        assert!(!has_own_candle_paint("hidden", &reg));
        assert!(!has_own_candle_paint("heikinashi", &reg));
        assert!(!has_own_candle_paint("candles", &reg));
        assert!(!has_own_candle_paint("unknown", &reg));
    }

    #[test]
    fn candle_override_reads_the_reserved_keys_of_the_type_bag() {
        let mut reg = ChartTypeRegistry::new();
        reg.register(candle_type("plain", None));
        let mut bags = ChartTypeBags::new();
        let bag = json!({ "candleUpColor": "#123456", "candleBodyVisible": false, "candleWickUpColor": " ", "candleBorderVisible": "no" });
        bags.insert("plain".into(), bag.as_object().unwrap().clone());
        let over = candle_override_for("plain", &bags, &reg).unwrap();
        assert_eq!(over.up_color.as_deref(), Some("#123456"));
        assert_eq!(over.body_visible, Some(false));
        assert_eq!(over.wick_up_color, None);
        assert_eq!(over.border_visible, None);
        assert_eq!(candle_override_for("candles", &bags, &reg), None);
        // no bag stored: every key inherits
        let none = candle_override_for("plain", &ChartTypeBags::new(), &reg).unwrap();
        assert_eq!(none.up_color, None);
        assert_eq!(none.body_visible, None);
    }

    #[test]
    fn the_effective_candle_paint_overrides_per_key() {
        let base = default_chart_style().candle;
        let plain = effective_candle_paint(&base, None, "#0f0", "#f00");
        assert_eq!((plain.up.as_str(), plain.down.as_str()), ("#0f0", "#f00"));
        assert_eq!(plain.candle, base);
        let over = CandlePaintOverride {
            up_color: Some("#111".into()),
            down_color: None,
            body_visible: Some(false),
            border_visible: None,
            border_up_color: Some("#222".into()),
            border_down_color: None,
            wick_visible: None,
            wick_up_color: None,
            wick_down_color: None,
        };
        let out = effective_candle_paint(&base, Some(&over), "#0f0", "#f00");
        assert_eq!((out.up.as_str(), out.down.as_str()), ("#111", "#f00"));
        assert!(!out.candle.body_visible);
        assert_eq!(out.candle.border_visible, base.border_visible);
        assert_eq!(out.candle.border_up_color.as_deref(), Some("#222"));
        assert_eq!(out.candle.border_down_color, base.border_down_color);
    }

    #[test]
    fn price_style_ids_lists_built_ins_then_registered_types_without_duplicates() {
        let mut reg = ChartTypeRegistry::builtin();
        reg.register(ChartTypeDefinition::new("renko"));
        let ids = price_style_ids(&reg);
        assert_eq!(&ids[..9], &BUILTIN_PRICE_STYLES);
        assert_eq!(ids.len(), 10);
        assert_eq!(ids[9], "renko");
    }

    // ── factory reset ──

    fn sdk_type(
        id: &str,
        section: ChartTypeSettingsSection,
        painting: Option<BasePainting>,
    ) -> ChartTypeDefinition {
        let mut def = ChartTypeDefinition::new(id);
        def.settings = Some(section);
        def.base_painting = painting;
        def
    }

    fn number_row(key: &str, defval: f64) -> SettingsRowDescriptor {
        SettingsRowDescriptor::Number {
            key: key.into(),
            label: label(key),
            defval,
            min: None,
            max: None,
            step: None,
            when: None,
        }
    }

    fn toggle_row(key: &str, defval: bool) -> SettingsRowDescriptor {
        SettingsRowDescriptor::Toggle {
            key: key.into(),
            label: label(key),
            defval,
            number: None,
            colors: vec![],
            width: None,
            when: None,
        }
    }

    fn sdk_flat() -> ChartTypeDefinition {
        sdk_type(
            "sdktype",
            ChartTypeSettingsSection {
                title: label("SDK Type"),
                rows: vec![number_row("rows", 10.0), toggle_row("shade", true)],
                ..Default::default()
            },
            Some(BasePainting::None),
        )
    }

    fn with_types(types: Vec<ChartTypeDefinition>) -> ChartTypeRegistry {
        let mut reg = ChartTypeRegistry::new();
        for t in types {
            reg.register(t);
        }
        reg
    }

    fn bag(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    fn apply_stored(
        cfg: &ChartConfig,
        reg: &ChartTypeRegistry,
        type_id: &str,
        vals: Value,
    ) -> ChartConfig {
        merge_config(cfg, &json!({ "chartTypes": { type_id: vals } }), reg)
    }

    #[test]
    fn the_factory_snapshot_alone_cannot_undo_sdk_settings() {
        let reg = with_types(vec![sdk_flat()]);
        let factory = base_config();
        assert!(factory.chart_types.is_empty());
        let edited = apply_stored(
            &factory,
            &reg,
            "sdktype",
            json!({ "rows": 25, "shade": false }),
        );
        // the raw snapshot names no type ids, so the values survive
        let after = merge_config(&edited, &serde_json::to_value(&factory).unwrap(), &reg);
        assert_eq!(
            after.chart_types["sdktype"],
            bag(json!({ "rows": 25, "shade": false }))
        );
    }

    #[test]
    fn the_reset_document_names_every_registered_type_at_its_row_defaults() {
        let reg = with_types(vec![sdk_flat()]);
        let factory = base_config();
        let edited = apply_stored(
            &factory,
            &reg,
            "sdktype",
            json!({ "rows": 25, "shade": false }),
        );
        let reset = factory_reset_config(&factory, None, &reg);
        let after = merge_config(&edited, &serde_json::to_value(&reset).unwrap(), &reg);
        assert_eq!(
            after.chart_types["sdktype"],
            bag(json!({ "rows": 10.0, "shade": true }))
        );
    }

    #[test]
    fn the_reset_keeps_the_live_price_style() {
        let reg = with_types(vec![sdk_flat()]);
        let factory = base_config();
        assert_eq!(factory.series.style, "candles");
        assert_eq!(
            factory_reset_config(&factory, Some("sdktype"), &reg)
                .series
                .style,
            "sdktype"
        );
        // without the live style the snapshot's own applies
        assert_eq!(
            factory_reset_config(&factory, None, &reg).series.style,
            "candles"
        );
    }

    #[test]
    fn the_reset_clears_a_candle_based_plugin_styles_own_candle_rows() {
        // basePainting defaults to candles, so the type owns the reserved candle* keys.
        let section = ChartTypeSettingsSection {
            title: label("SDK Type"),
            rows: vec![number_row("rows", 10.0)],
            ..Default::default()
        };
        let reg = with_types(vec![sdk_type("sdktype", section, None)]);
        let factory = base_config();
        let edited = apply_stored(
            &factory,
            &reg,
            "sdktype",
            json!({ "candleBodyVisible": false, "candleWickVisible": false, "candleBorderVisible": false, "candleUpColor": "#123456" }),
        );
        let reset = factory_reset_config(&factory, Some("sdktype"), &reg);
        let after = merge_config(&edited, &serde_json::to_value(&reset).unwrap(), &reg);
        let b = &after.chart_types["sdktype"];
        for key in [
            "candleBodyVisible",
            "candleWickVisible",
            "candleBorderVisible",
            "candleUpColor",
        ] {
            assert_eq!(b[key], Value::Null, "{key}");
        }
        assert_eq!(b["rows"], json!(10.0));
        // a null override paints the shared candles block again
        assert_eq!(
            candle_override_for("sdktype", &after.chart_types, &reg)
                .unwrap()
                .body_visible,
            None
        );
    }

    #[test]
    fn values_the_snapshot_itself_pinned_win_over_the_defaults() {
        let reg = with_types(vec![sdk_flat()]);
        let factory = apply_stored(&base_config(), &reg, "sdktype", json!({ "rows": 40 }));
        let edited = apply_stored(
            &factory,
            &reg,
            "sdktype",
            json!({ "rows": 99, "shade": false }),
        );
        let reset = factory_reset_config(&factory, None, &reg);
        let after = merge_config(&edited, &serde_json::to_value(&reset).unwrap(), &reg);
        assert_eq!(
            after.chart_types["sdktype"],
            bag(json!({ "rows": 40, "shade": true }))
        );
    }

    #[test]
    fn a_type_registered_after_the_snapshot_is_covered() {
        let factory = base_config();
        let reg = with_types(vec![sdk_flat()]); // registered after the snapshot was taken
        let edited = apply_stored(&factory, &reg, "sdktype", json!({ "rows": 25 }));
        let reset = factory_reset_config(&factory, None, &reg);
        let after = merge_config(&edited, &serde_json::to_value(&reset).unwrap(), &reg);
        assert_eq!(
            after.chart_types["sdktype"],
            bag(json!({ "rows": 10.0, "shade": true }))
        );
    }

    #[test]
    fn the_reset_covers_structured_sections() {
        let section = ChartTypeSettingsSection {
            title: label("SDK Type"),
            instances: vec![
                ChartTypeSettingsInstance {
                    label: label("One"),
                    enable_key: None,
                    rows: vec![number_row("size", 5.0)],
                },
                ChartTypeSettingsInstance {
                    label: label("Two"),
                    enable_key: Some("twoEnabled".into()),
                    rows: vec![
                        SettingsRowDescriptor::Toggle {
                            key: "shade".into(),
                            label: label("Shade"),
                            defval: false,
                            number: None,
                            colors: vec![SettingsRowSwatch {
                                key: "shadeColor".into(),
                                label: label("Shade color"),
                                defval: "#111111".into(),
                                when: None,
                            }],
                            width: None,
                            when: None,
                        },
                        SettingsRowDescriptor::Range {
                            label: label("Filter"),
                            min_key: "filterMin".into(),
                            max_key: "filterMax".into(),
                            defval: 0.0,
                            min: None,
                            max: None,
                            step: None,
                            placeholder: None,
                            when: None,
                        },
                    ],
                },
            ],
            subsections: vec![ChartTypeSettingsSubsection {
                title: label("Extra"),
                enable_key: Some("extraOn".into()),
                rows: vec![
                    toggle_row("extraOn", true),
                    SettingsRowDescriptor::Heading {
                        label: label("Group"),
                        when: None,
                    },
                    SettingsRowDescriptor::Select {
                        key: "flavor".into(),
                        label: label("Flavor"),
                        options: vec![
                            SettingsSelectOption::Plain("a".into()),
                            SettingsSelectOption::Plain("b".into()),
                        ],
                        defval: "a".into(),
                        when: None,
                    },
                ],
            }],
            ..Default::default()
        };
        let reg = with_types(vec![sdk_type("sdktype", section, Some(BasePainting::None))]);
        let factory = base_config();
        let edited = apply_stored(
            &factory,
            &reg,
            "sdktype",
            json!({ "size": 9, "twoEnabled": true, "shade": true, "shadeColor": "#ff0000", "filterMin": 3, "filterMax": 8, "extraOn": false, "flavor": "b" }),
        );
        let reset = factory_reset_config(&factory, None, &reg);
        let after = merge_config(&edited, &serde_json::to_value(&reset).unwrap(), &reg);
        assert_eq!(
            after.chart_types["sdktype"],
            bag(json!({
                "size": 5.0,
                "twoEnabled": false, // enable key without a row: instance presence resets to off
                "shade": false,
                "shadeColor": "#111111",
                "filterMin": 0.0,
                "filterMax": 0.0,
                "extraOn": true, // enable key WITH a row: the row's defval wins over the off seed
                "flavor": "a",
            }))
        );
    }

    #[test]
    fn the_reset_covers_a_toggle_rows_inline_controls_and_composite_rows() {
        let section = ChartTypeSettingsSection {
            title: label("SDK Type"),
            rows: vec![
                SettingsRowDescriptor::Toggle {
                    key: "poc".into(),
                    label: label("POC"),
                    defval: true,
                    number: Some(SettingsRowInlineNumber {
                        key: "pocPct".into(),
                        label: label("Percent"),
                        defval: 70.0,
                        min: None,
                        max: None,
                        step: None,
                    }),
                    colors: vec![],
                    width: Some(SettingsRowWidth {
                        key: "pocWidth".into(),
                        label: label("Line width"),
                        defval: 2.0,
                    }),
                    when: None,
                },
                SettingsRowDescriptor::Row {
                    label: label("Mixed"),
                    toggle: Some(SettingsRowToggle {
                        key: "mixOn".into(),
                        defval: false,
                    }),
                    controls: vec![
                        SettingsInlineControl::Select {
                            key: "mixMode".into(),
                            label: label("Mode"),
                            options: vec![
                                SettingsSelectOption::Plain("a".into()),
                                SettingsSelectOption::Plain("b".into()),
                            ],
                            defval: "a".into(),
                            when: None,
                        },
                        SettingsInlineControl::Color {
                            key: "mixInk".into(),
                            label: label("Ink"),
                            defval: "#111111".into(),
                            when: None,
                        },
                    ],
                    when: None,
                },
            ],
            ..Default::default()
        };
        let reg = with_types(vec![sdk_type("sdktype", section, Some(BasePainting::None))]);
        let factory = base_config();
        let edited = apply_stored(
            &factory,
            &reg,
            "sdktype",
            json!({ "poc": false, "pocPct": 40, "pocWidth": 5, "mixOn": true, "mixMode": "b", "mixInk": "#ff0000" }),
        );
        let reset = factory_reset_config(&factory, None, &reg);
        let after = merge_config(&edited, &serde_json::to_value(&reset).unwrap(), &reg);
        assert_eq!(
            after.chart_types["sdktype"],
            bag(json!({
                "poc": true, "pocPct": 70.0, "pocWidth": 2.0,
                "mixOn": false, "mixMode": "a", "mixInk": "#111111",
            }))
        );
    }

    #[test]
    fn a_type_without_a_settings_section_is_left_out_of_the_reset() {
        let reg = with_types(vec![ChartTypeDefinition::new("plain")]);
        let reset = factory_reset_config(&base_config(), None, &reg);
        // candle-based, so only the reserved candle keys are named
        assert_eq!(reset.chart_types["plain"].len(), CANDLE_OVERRIDE_KEYS.len());
        let none = with_types(vec![candle_type("hidden", Some(BasePainting::None))]);
        assert!(
            factory_reset_config(&base_config(), None, &none)
                .chart_types
                .is_empty()
        );
    }
}
