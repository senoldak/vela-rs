use crate::i18n::MessageLike;

use super::drawings::{
    DrawingBox, DrawingLabel, DrawingLine, DrawingLinefill, DrawingPolyline, DrawingTable,
};
use super::ohlcv::{Millis, Ohlcv};
use super::series::{LineStyle, SeriesSpec};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaneKind {
    Price,
    Study,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pane {
    pub id: String,
    pub kind: PaneKind,
    /// Display order, top-to-bottom; the price pane is conventionally order 0.
    pub order: i32,
    /// Relative height weight among panes (the renderer normalizes).
    pub height_weight: Option<f64>,
    pub title: Option<String>,
}

/// One bar's vertical gradient stop for a gradient fill (color@price).
#[derive(Clone, Debug, PartialEq)]
pub struct FillGradientStop {
    pub top_value: f64,
    pub bottom_value: f64,
    pub top_color: String,
    pub bottom_color: String,
}

/// A band fill between two value series.
#[derive(Clone, Debug, PartialEq)]
pub struct Fill {
    pub id: String,
    pub pane_id: String,
    /// RESOLVED series ids.
    pub from_series_id: String,
    pub to_series_id: String,
    /// Flat band color (no per-bar variation).
    pub color: Option<String>,
    /// Per-bar solid color, aligned to the anchor points by index.
    pub colors: Option<Vec<Option<String>>>,
    /// Per-bar vertical gradient, aligned by index.
    pub gradient: Option<Vec<Option<FillGradientStop>>>,
    /// Render on the price pane regardless of the indicator's pane.
    pub overlay: bool,
}

/// A vertical background tint over a time span.
#[derive(Clone, Debug, PartialEq)]
pub struct Background {
    pub id: String,
    pub pane_id: String,
    /// Inclusive start, epoch ms.
    pub from: Millis,
    /// Exclusive end, epoch ms.
    pub to: Millis,
    pub color: String,
    pub overlay: bool,
}

/// A horizontal price line.
#[derive(Clone, Debug, PartialEq)]
pub struct PriceLine {
    pub id: String,
    pub pane_id: String,
    pub price: f64,
    pub color: Option<String>,
    pub line_style: Option<LineStyle>,
    pub width: Option<f64>,
    pub title: Option<String>,
    /// `title` in the chart's language. `None` ⇒ `title`.
    pub display_title: Option<MessageLike>,
}

/// The renderer-neutral, full description of what to draw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    pub bars: Vec<Ohlcv>,
    pub panes: Vec<Pane>,
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
}
