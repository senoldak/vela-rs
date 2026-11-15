//! Series access for data-driven drawings: the ranged, any-timeframe sibling
//! of [`super::Projector::bars_in_range`]. Painting stays synchronous: a read
//! either serves bars from cache or reports `Loading` while a fetch runs in
//! the background.

use crate::model::Ohlcv;

/// Why a series request cannot be served. Terminal for the request: no fetch was started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SeriesUnavailable {
    /// The market has no ranged feed (inline data).
    NoSource,
    /// No offered timeframe is below the chart's (the chart is already at the finest).
    NoneLower,
    /// The explicit pick does not subdivide the chart's timeframe (lower picks exist).
    NotLower,
    /// The range would need more bars than the gateway's cap.
    TooWide,
}

/// The synchronous answer to a series read. `Ready` and `Loading` carry the
/// RESOLVED timeframe (an `auto` request comes back concrete) and its bar
/// duration so consumers never re-derive timeframe math.
#[derive(Clone, Debug, PartialEq)]
pub enum DrawingSeriesState {
    /// May carry best-effort PARTIAL bars from previously fetched overlapping
    /// windows, so a resize keeps painting what it already has.
    Loading {
        timeframe: String,
        bar_ms: f64,
        bars: Option<Vec<Ohlcv>>,
    },
    Ready {
        bars: Vec<Ohlcv>,
        timeframe: String,
        bar_ms: f64,
    },
    Unavailable {
        reason: SeriesUnavailable,
    },
}

/// The core-owned gateway a renderer hands drawings through
/// [`super::Projector::series_in_range`]. `timeframe` accepts a canonical
/// value (`"5"`, `"60"`, `"D"`) or `"auto"`.
///
/// The original's `onUpdate` subscription (repaint when a fetch lands) belongs
/// to the host and is not part of this trait.
pub trait DrawingSeriesGateway {
    /// Synchronous cache read for the CHART's OWN symbol; kicks a background fetch on a miss.
    fn series_in_range(&self, timeframe: &str, from: f64, to: f64) -> DrawingSeriesState;
}
