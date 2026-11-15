use crate::i18n::MessageLike;

use super::ohlcv::{Millis, Ohlcv};

/// Value-series kinds drawn as a connected/point series.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineLikeKind {
    #[default]
    Line,
    Area,
    Step,
    Histogram,
    Columns,
    Circles,
    Cross,
}

impl LineLikeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LineLikeKind::Line => "line",
            LineLikeKind::Area => "area",
            LineLikeKind::Step => "step",
            LineLikeKind::Histogram => "histogram",
            LineLikeKind::Columns => "columns",
            LineLikeKind::Circles => "circles",
            LineLikeKind::Cross => "cross",
        }
    }
}

/// Whether an OHLC series draws candles or bars.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CandleKind {
    Candle,
    Bar,
}

/// All renderable series kinds. `fill`, `background` and `hline` are
/// intentionally NOT series kinds: they are overlays on a pane (see `scene`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SeriesKind {
    LineLike(LineLikeKind),
    Candle,
    Bar,
    Markers,
}

impl SeriesKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SeriesKind::LineLike(k) => k.as_str(),
            SeriesKind::Candle => "candle",
            SeriesKind::Bar => "bar",
            SeriesKind::Markers => "markers",
        }
    }
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum LineStyle {
    #[default]
    Solid,
    Dashed,
    Dotted,
}

/// A single point of a value series. `value: None` marks a gap (whitespace).
#[derive(Clone, Debug, PartialEq)]
pub struct SeriesPoint {
    pub time: Millis,
    pub value: Option<f64>,
    /// Per-point color override.
    pub color: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LineLikeStyle {
    pub color: String,
    pub width: f64,
    pub line_style: LineStyle,
    /// Baseline for histogram/area; ignored by the line family.
    pub base: Option<f64>,
}

/// Candle colors; every field optional (an override of the theme's own).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CandleStyle {
    pub up: Option<String>,
    pub down: Option<String>,
    pub wick_up: Option<String>,
    pub wick_down: Option<String>,
    pub border_up: Option<String>,
    pub border_down: Option<String>,
}

/// Per-bar plotcandle/plotbar override (body / wick / border colors).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CandleBarColor {
    pub color: Option<String>,
    pub wick_color: Option<String>,
    pub border_color: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MarkerPosition {
    AboveBar,
    BelowBar,
    InBar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MarkerSize {
    Tiny,
    Small,
    Normal,
    Large,
    Huge,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MarkerPoint {
    pub time: Millis,
    pub position: MarkerPosition,
    /// Neutral shape token (e.g. `arrowUp`, `circle`, `square`); mapped per renderer.
    pub shape: String,
    pub color: String,
    pub text: Option<String>,
    pub size: Option<MarkerSize>,
}

/// A chart surface a series can show on (see [`SeriesDisplay`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SeriesSurface {
    Pane,
    PriceScale,
    Legend,
    DataWindow,
}

/// Where a series shows, surface by surface. Every flag defaults to shown
/// (`None`); `Some(false)` takes the series off that one surface while it
/// keeps anchoring fills.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeriesDisplay {
    pub pane: Option<bool>,
    pub price_scale: Option<bool>,
    pub legend: Option<bool>,
    pub data_window: Option<bool>,
}

impl SeriesDisplay {
    /// Shown nowhere a user reads it: off the pane, the legend and the data window.
    pub fn hidden() -> Self {
        Self {
            pane: Some(false),
            price_scale: None,
            legend: Some(false),
            data_window: Some(false),
        }
    }

    fn flag(&self, surface: SeriesSurface) -> Option<bool> {
        match surface {
            SeriesSurface::Pane => self.pane,
            SeriesSurface::PriceScale => self.price_scale,
            SeriesSurface::Legend => self.legend,
            SeriesSurface::DataWindow => self.data_window,
        }
    }
}

/// The kind-specific payload of a series.
#[derive(Clone, Debug, PartialEq)]
pub enum SeriesData {
    /// One entry per bar the indicator ran over, index-aligned to the bars.
    /// A gap is a `value: None` entry.
    LineLike {
        kind: LineLikeKind,
        points: Vec<SeriesPoint>,
        style: LineLikeStyle,
    },
    /// One entry per bar, index-aligned like `points`. A bar with no candle is
    /// an entry with NaN prices; never compact the array to drop it.
    Candle {
        kind: CandleKind,
        bars: Vec<Ohlcv>,
        style: Option<CandleStyle>,
        /// Per-bar colors, aligned to `bars` by index (`None` ≡ use defaults).
        bar_colors: Option<Vec<Option<CandleBarColor>>>,
    },
    Markers {
        markers: Vec<MarkerPoint>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct SeriesSpec {
    /// Content-addressed, stable across re-runs of identical source.
    pub id: String,
    /// The series' name: its identity for engines and hosts.
    pub title: String,
    /// What the legend, the data window and the settings show instead of `title`.
    pub display_title: Option<MessageLike>,
    /// Pane this series belongs to; resolved by the orchestrator.
    pub pane_id: String,
    /// Declared draw-order intent; the renderer owns final z-ordering.
    pub z_order: Option<f64>,
    /// `Some(false)` hides the series from every surface. The shorthand form:
    /// when `display` is set it decides instead, surface by surface.
    pub visible: Option<bool>,
    /// Per-surface visibility; `None` ⇒ every surface `visible` allows.
    pub display: Option<SeriesDisplay>,
    /// Render on the price pane regardless of the indicator's pane.
    pub overlay: bool,
    pub data: SeriesData,
}

impl SeriesSpec {
    pub fn kind(&self) -> SeriesKind {
        match &self.data {
            SeriesData::LineLike { kind, .. } => SeriesKind::LineLike(*kind),
            SeriesData::Candle {
                kind: CandleKind::Candle,
                ..
            } => SeriesKind::Candle,
            SeriesData::Candle {
                kind: CandleKind::Bar,
                ..
            } => SeriesKind::Bar,
            SeriesData::Markers { .. } => SeriesKind::Markers,
        }
    }

    /// True for value series carrying `points`.
    pub fn is_line_like(&self) -> bool {
        matches!(self.data, SeriesData::LineLike { .. })
    }

    /// The points of a line-like series; `None` for candles and markers.
    pub fn points(&self) -> Option<&[SeriesPoint]> {
        match &self.data {
            SeriesData::LineLike { points, .. } => Some(points),
            _ => None,
        }
    }

    /// The style of a line-like series; `None` for candles and markers.
    pub fn line_style(&self) -> Option<&LineLikeStyle> {
        match &self.data {
            SeriesData::LineLike { style, .. } => Some(style),
            _ => None,
        }
    }

    /// Whether the series shows on `surface`: its `display` flag when it
    /// carries one, else its `visible` shorthand.
    pub fn shown_on(&self, surface: SeriesSurface) -> bool {
        series_shown_on(self.visible, self.display.as_ref(), surface)
    }

    /// Whether the series takes part in its pane's autoscale.
    pub fn in_scale(&self) -> bool {
        series_in_scale(self.visible, self.display.as_ref())
    }
}

/// Whether a series shows on `surface`: its `display` flag when it carries
/// one, else its `visible` shorthand.
pub fn series_shown_on(
    visible: Option<bool>,
    display: Option<&SeriesDisplay>,
    surface: SeriesSurface,
) -> bool {
    match display {
        Some(d) => d.flag(surface) != Some(false),
        None => visible != Some(false),
    }
}

/// Whether a series takes part in its pane's autoscale: it is painted there,
/// or its value sits on the price scale. A series that only reports a value
/// (legend, data window) must not stretch the scale.
pub fn series_in_scale(visible: Option<bool>, display: Option<&SeriesDisplay>) -> bool {
    series_shown_on(visible, display, SeriesSurface::Pane)
        || series_shown_on(visible, display, SeriesSurface::PriceScale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_overrides_the_visible_shorthand() {
        let d = SeriesDisplay {
            pane: Some(false),
            ..Default::default()
        };
        assert!(!series_shown_on(Some(true), Some(&d), SeriesSurface::Pane));
        assert!(series_shown_on(
            Some(false),
            Some(&d),
            SeriesSurface::Legend
        ));
        assert!(!series_shown_on(Some(false), None, SeriesSurface::Legend));
        assert!(series_shown_on(None, None, SeriesSurface::Pane));
    }

    #[test]
    fn a_series_off_pane_and_scale_is_out_of_the_autoscale() {
        let off = SeriesDisplay {
            pane: Some(false),
            price_scale: Some(false),
            ..Default::default()
        };
        assert!(!series_in_scale(None, Some(&off)));
        assert!(series_in_scale(None, Some(&SeriesDisplay::hidden())));
    }
}
