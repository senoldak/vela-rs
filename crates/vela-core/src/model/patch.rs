use super::drawings::{
    DrawingBox, DrawingLabel, DrawingLine, DrawingLinefill, DrawingPolyline, DrawingTable,
};
use super::ohlcv::{Millis, Ohlcv};
use super::series::{CandleBarColor, MarkerPoint, SeriesPoint, SeriesSpec};
use super::trades::TradeExecution;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirtyRange {
    pub from: Millis,
    pub to: Millis,
}

/// The changed tail of one series.
#[derive(Clone, Debug, PartialEq)]
pub enum SeriesValues {
    Points(Vec<SeriesPoint>),
    Bars {
        bars: Vec<Ohlcv>,
        /// Per-bar colors, index-aligned to `bars`. `None` keeps the current
        /// ones; `Some(vec![])` clears them.
        bar_colors: Option<Vec<Option<CandleBarColor>>>,
    },
    Markers(Vec<MarkerPoint>),
}

/// Per-series changed tail in a value patch.
#[derive(Clone, Debug, PartialEq)]
pub struct SeriesValueDelta {
    pub series_id: String,
    pub values: SeriesValues,
}

/// How a value patch states the emitting run's anchor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnchorUpdate {
    /// Not stated: keep the model's current anchor.
    #[default]
    Keep,
    /// The run spanned the WHOLE chart: clear any previous anchor.
    Clear,
    /// The run started at this bar time.
    Set(Millis),
}

/// Value-only update to existing series: legal as an in-place renderer update.
#[derive(Clone, Debug, PartialEq)]
pub struct ValuePatch {
    pub indicator_id: String,
    pub dirty: DirtyRange,
    pub anchor_time: AnchorUpdate,
    pub series: Vec<SeriesValueDelta>,
    /// Full drawing snapshots for this tick; `None` ≡ unchanged.
    pub lines: Option<Vec<DrawingLine>>,
    pub boxes: Option<Vec<DrawingBox>>,
    pub labels: Option<Vec<DrawingLabel>>,
    pub polylines: Option<Vec<DrawingPolyline>>,
    pub linefills: Option<Vec<DrawingLinefill>>,
    pub tables: Option<Vec<DrawingTable>>,
    /// Trade executions follow the same full-snapshot-per-tick pattern.
    pub trades: Option<Vec<TradeExecution>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SchemaChangeReason {
    Kind,
    Pane,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SchemaChange {
    pub series_id: String,
    pub reason: SchemaChangeReason,
}

/// Structural change: series added/removed/kind-changed, or panes changed.
/// Forces a remount of the affected series.
#[derive(Clone, Debug, PartialEq)]
pub struct SchemaPatch {
    pub indicator_id: String,
    pub added: Vec<SeriesSpec>,
    pub removed: Vec<String>,
    pub changed: Vec<SchemaChange>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ScenePatch {
    Value(ValuePatch),
    Schema(SchemaPatch),
}
