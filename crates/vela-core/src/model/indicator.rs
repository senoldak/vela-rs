use crate::i18n::MessageLike;

use super::drawings::{
    DrawingBox, DrawingLabel, DrawingLine, DrawingLinefill, DrawingPolyline, DrawingTable,
};
use super::inputs::{InputSchema, Inputs};
use super::ohlcv::Millis;
use super::scene::{Background, Fill, PriceLine};
use super::series::SeriesSpec;
use super::trades::TradeExecution;

/// Declaration metadata of a script indicator.
#[derive(Clone, Debug, PartialEq)]
pub struct IndicatorMeta {
    pub title: String,
    pub shorttitle: Option<String>,
    pub overlay: bool,
    pub precision: Option<u32>,
    pub format: Option<String>,
}

/// Where an indicator's plots are placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaneHint {
    Price,
    New,
}

/// One label of a categorical pane axis: `frac` is the label's center as a
/// fraction of the pane's height (0 = top, 1 = bottom).
#[derive(Clone, Debug, PartialEq)]
pub struct PaneAxisBand {
    pub frac: f64,
    pub label: String,
}

/// A pane's value-axis override: a blank axis, or a categorical axis with one
/// label per content band/row.
#[derive(Clone, Debug, PartialEq)]
pub enum PaneAxis {
    None,
    Bands(Vec<PaneAxisBand>),
}

/// Marks a NATIVE indicator (core-computed, no script engine) and its type.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeTag {
    pub indicator_type: String,
    /// The renderer-layer data channel this instance's bespoke payload lands
    /// on, when it differs from the type (an instance of a multi-instance type).
    pub channel: Option<String>,
}

/// One `barcolor` entry: a per-bar candle recolor.
#[derive(Clone, Debug, PartialEq)]
pub struct BarColor {
    pub time: Millis,
    pub color: String,
}

/// Everything one added indicator produces: the unit the orchestrator mounts
/// on the renderer. Renderer-neutral.
#[derive(Clone, Debug, PartialEq)]
pub struct IndicatorModel {
    /// Per-instance id (stable).
    pub id: String,
    /// Full display name.
    pub title: String,
    /// Compact label for the legend chip and the settings dialog. `None` ⇒ `title`.
    pub shorttitle: Option<String>,
    /// What the chart SHOWS for `title`, in its language. `None` ⇒ `title`.
    pub display_title: Option<MessageLike>,
    /// What the chart shows for `shorttitle`. `None` ⇒ `shorttitle`.
    pub display_shorttitle: Option<MessageLike>,
    pub overlay: bool,
    pub pane_hint: PaneHint,
    /// `None` ⇒ an ordinary script indicator.
    pub native: Option<NativeTag>,
    /// `Some(false)` ⇒ no legend row and no pane listing; `None` ⇒ a row.
    pub legend: Option<bool>,
    /// Value-axis override for the pane this indicator OWNS.
    pub pane_axis: Option<PaneAxis>,
    /// Resolved pane id, filled in by the orchestrator after routing.
    pub pane_id: Option<String>,
    /// Render on its OWN price scale within its pane.
    pub own_scale: bool,
    /// Chart time of the execution's FIRST bar; index-aligned payloads count
    /// from this bar. `None` ⇒ the model spans the whole chart.
    pub anchor_time: Option<Millis>,
    pub series: Vec<SeriesSpec>,
    pub fills: Vec<Fill>,
    pub backgrounds: Vec<Background>,
    pub price_lines: Vec<PriceLine>,
    pub lines: Vec<DrawingLine>,
    pub boxes: Vec<DrawingBox>,
    pub labels: Vec<DrawingLabel>,
    pub polylines: Vec<DrawingPolyline>,
    pub linefills: Vec<DrawingLinefill>,
    pub tables: Vec<DrawingTable>,
    /// Per-bar candle recolor (empty ≡ none).
    pub bar_colors: Vec<BarColor>,
    /// Strategy order executions, painted on the PRICE pane.
    pub trades: Vec<TradeExecution>,
    /// Input schema (drives a settings dialog).
    pub inputs: Vec<InputSchema>,
    /// Current input values (defaults merged with any overrides).
    pub input_values: Inputs,
    /// Declaration-props schema. Empty ≡ none.
    pub props: Vec<InputSchema>,
    /// Current prop values.
    pub prop_values: Inputs,
}
