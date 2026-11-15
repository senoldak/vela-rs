//! Geometry seam for user drawings. A drawing stores its anchors in DATA space
//! ([`DrawingPoint`]); every pixel it needs is resolved on demand through a
//! [`Projector`] the renderer supplies. The model never stores pixels, so a
//! drawing survives reload, pan/zoom, bar-prepend and timezone changes.

use crate::model::Ohlcv;

use super::series::DrawingSeriesState;

/// A drawing anchor in DATA space (epoch-ms time + data-space price).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DrawingPoint {
    /// Epoch ms; resolved to a fractional logical bar index by the time scale.
    pub time: f64,
    /// Data-space price (not normalized, not pane-relative).
    pub price: f64,
}

/// Axes a handle is free to move along.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FreeAxis {
    Both,
    X,
    Y,
    None,
}

/// Pixel render-geometry for multi-line drawings (channels, pitchforks): the
/// stroked line segments plus an optional fill polygon.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SegmentGeometry {
    /// Line segments to stroke, each `[x1, y1, x2, y2]` in media px.
    pub segments: Vec<[f64; 4]>,
    /// A polygon to fill between lines, or `None` when there's no fill.
    pub fill: Option<Vec<(f64, f64)>>,
}

/// Magnet (snap-to-candle) strength. `Off` never snaps; `Strong` always snaps
/// the anchor to the nearest bar/OHLC; `Weak` snaps only when the candle point
/// is within a small pixel radius of the cursor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SnapMode {
    #[default]
    Off,
    Weak,
    Strong,
}

/// A pane's vertical pixel extent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneRect {
    pub top: f64,
    /// 0 while the pane is hidden.
    pub height: f64,
}

/// The renderer-supplied data→pixel transform. Defined in core so the model
/// depends only on the interface.
pub trait Projector {
    /// Pixel x for an epoch-ms time (extrapolates past either edge).
    fn x_of(&self, time: f64) -> f64;
    /// Pixel y for a price on a pane; `None` when the pane is gone.
    fn y_of(&self, price: f64, pane_id: &str) -> Option<f64>;
    /// Inverse: pixel → data point on a pane.
    fn px_to_point(&self, x: f64, y: f64, pane_id: &str) -> DrawingPoint;
    /// Which pane owns a pixel y, or `None` outside every pane.
    fn pane_id_at_y(&self, y: f64) -> Option<String>;
    /// A pane's vertical pixel extent; `None` when the pane is gone or the
    /// renderer does not report it.
    fn pane_rect(&self, _pane_id: &str) -> Option<PaneRect> {
        None
    }
    /// Approximate whole bars between two times; `None` when unsupported.
    fn bars_between(&self, _t1: f64, _t2: f64) -> Option<f64> {
        None
    }
    /// Bars whose time falls within `[from, to]` (inclusive), ascending.
    /// `None` when the renderer has no series access; statistical drawings
    /// then degrade to an anchor-only fallback.
    fn bars_in_range(&self, _from: f64, _to: f64) -> Option<Vec<Ohlcv>> {
        None
    }
    /// Bars of a FINER timeframe than the chart's, for the same symbol: a
    /// cache read that reports `Loading` while a background fetch runs.
    /// `None` when the renderer has no series gateway; such drawings then
    /// degrade to an anchors-only rendering.
    fn series_in_range(
        &self,
        _timeframe: &str,
        _from: f64,
        _to: f64,
    ) -> Option<DrawingSeriesState> {
        None
    }
    /// Plot width in media px (excludes the right price-axis strip).
    fn width(&self) -> f64;
    /// Plot height in media px (excludes the bottom time-axis strip).
    fn height(&self) -> f64;
}
