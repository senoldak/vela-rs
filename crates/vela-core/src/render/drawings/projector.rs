//! The renderer's implementation of the model's [`Projector`] over the native
//! [`CoordinateSystem`]. Every drawing resolves its data-space anchors to media pixels
//! through this one transform, so it stays aligned with the candles and chrome (which
//! share the same `coords`). Built per paint (cheap), so it always reflects the current
//! viewport and pane scales.
//!
//! The original's `ProjectorBar` (OHLCV with an optional volume, time in ms) is
//! [`Ohlcv`] here.

use crate::drawings::{DrawingPoint, DrawingSeriesState, PaneRect, Projector};
use crate::model::Ohlcv;
use crate::render::core::{CoordinateSystem, PaneBounds, PriceScale};

/// A pane's live price window and pixel extent (the subset the projector needs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneView {
    pub scale: PriceScale,
    pub bounds: PaneBounds,
    /// Collapsed to a legend-only strip: drawings on it are hidden (treated as
    /// zero-height).
    pub collapsed: bool,
}

/// Looks a pane up by id.
pub type PaneOf<'a> = &'a dyn Fn(&str) -> Option<PaneView>;
/// Which pane owns a pixel y.
pub type PaneIdAtY<'a> = &'a dyn Fn(f64) -> Option<String>;
/// Bars whose time falls within `[from, to]`.
pub type BarsInRange<'a> = &'a dyn Fn(f64, f64) -> Vec<Ohlcv>;
/// A finer timeframe's bars for `[from, to]`: `(timeframe, from, to)`.
pub type SeriesInRange<'a> = &'a dyn Fn(&str, f64, f64) -> DrawingSeriesState;

/// A [`Projector`] over the native coordinate system. See [`create_projector`].
pub struct NativeProjector<'a> {
    coords: &'a CoordinateSystem,
    pane_of: PaneOf<'a>,
    pane_id_at_y: PaneIdAtY<'a>,
    bars_in_range: Option<BarsInRange<'a>>,
    series_in_range: Option<SeriesInRange<'a>>,
}

/// Build a [`Projector`] over `coords`. `bars_in_range` and `series_in_range` are the
/// optional series access statistical drawings use; without them those drawings degrade
/// to an anchor-only rendering.
pub fn create_projector<'a>(
    coords: &'a CoordinateSystem,
    pane_of: PaneOf<'a>,
    pane_id_at_y: PaneIdAtY<'a>,
    bars_in_range: Option<BarsInRange<'a>>,
    series_in_range: Option<SeriesInRange<'a>>,
) -> NativeProjector<'a> {
    NativeProjector {
        coords,
        pane_of,
        pane_id_at_y,
        bars_in_range,
        series_in_range,
    }
}

impl Projector for NativeProjector<'_> {
    fn x_of(&self, time: f64) -> f64 {
        self.coords.time_to_x(time)
    }

    fn y_of(&self, price: f64, pane_id: &str) -> Option<f64> {
        let p = (self.pane_of)(pane_id)?;
        Some(self.coords.price_to_y(price, &p.scale, &p.bounds))
    }

    fn px_to_point(&self, x: f64, y: f64, pane_id: &str) -> DrawingPoint {
        let p = (self.pane_of)(pane_id);
        let time = self.coords.logical_to_time(self.coords.x_to_logical(x));
        let price = p.map_or(0.0, |p| self.coords.y_to_price(y, &p.scale, &p.bounds));
        DrawingPoint { time, price }
    }

    fn pane_id_at_y(&self, y: f64) -> Option<String> {
        (self.pane_id_at_y)(y)
    }

    fn pane_rect(&self, pane_id: &str) -> Option<PaneRect> {
        let p = (self.pane_of)(pane_id)?;
        Some(PaneRect {
            top: p.bounds.top,
            height: if p.collapsed { 0.0 } else { p.bounds.height },
        })
    }

    /// The unrounded distance in bars (the ruler and the drawings round it themselves).
    fn bars_between(&self, t1: f64, t2: f64) -> Option<f64> {
        Some((self.coords.time_to_logical(t2) - self.coords.time_to_logical(t1)).abs())
    }

    fn bars_in_range(&self, from: f64, to: f64) -> Option<Vec<Ohlcv>> {
        self.bars_in_range.map(|f| f(from, to))
    }

    fn series_in_range(&self, timeframe: &str, from: f64, to: f64) -> Option<DrawingSeriesState> {
        self.series_in_range.map(|f| f(timeframe, from, to))
    }

    fn width(&self) -> f64 {
        self.coords.width()
    }

    fn height(&self) -> f64 {
        self.coords.height()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::core::ViewportState;

    fn coords() -> CoordinateSystem {
        let mut coords = CoordinateSystem::new();
        coords.set_size(800.0, 400.0, 1.0);
        coords.set_bars(&[1000, 2000, 3000, 4000, 5000]);
        coords.set_viewport(ViewportState {
            bar_spacing: 12.0,
            right_offset: 3.0,
        });
        coords
    }

    fn price_pane() -> Option<PaneView> {
        Some(PaneView {
            scale: PriceScale::new(0.0, 100.0),
            bounds: PaneBounds {
                top: 0.0,
                height: 400.0,
            },
            collapsed: false,
        })
    }

    #[test]
    fn px_to_point_then_x_of_y_of_recovers_the_pixel() {
        let coords = coords();
        let pane_of = |_: &str| price_pane();
        let at = |_: f64| Some("price".to_string());
        let proj = create_projector(&coords, &pane_of, &at, None, None);
        let p = proj.px_to_point(420.0, 150.0, "price");
        assert!((proj.x_of(p.time) - 420.0).abs() < 1e-5);
        assert!((proj.y_of(p.price, "price").unwrap() - 150.0).abs() < 1e-5);
    }

    #[test]
    fn a_missing_pane_has_no_y_no_rect_and_price_zero() {
        let coords = coords();
        let pane_of = |_: &str| None;
        let at = |_: f64| None;
        let proj = create_projector(&coords, &pane_of, &at, None, None);
        assert_eq!(proj.y_of(50.0, "gone"), None);
        assert_eq!(proj.pane_rect("gone"), None);
        assert_eq!(proj.px_to_point(10.0, 10.0, "gone").price, 0.0);
        assert_eq!(proj.pane_id_at_y(10.0), None);
    }

    #[test]
    fn a_collapsed_pane_reports_zero_height() {
        let coords = coords();
        let pane_of = |_: &str| {
            price_pane().map(|p| PaneView {
                collapsed: true,
                ..p
            })
        };
        let at = |_: f64| None;
        let proj = create_projector(&coords, &pane_of, &at, None, None);
        assert_eq!(
            proj.pane_rect("price"),
            Some(PaneRect {
                top: 0.0,
                height: 0.0
            })
        );
    }

    #[test]
    fn bars_between_is_the_unrounded_absolute_logical_distance() {
        let coords = coords();
        let pane_of = |_: &str| price_pane();
        let at = |_: f64| None;
        let proj = create_projector(&coords, &pane_of, &at, None, None);
        assert_eq!(proj.bars_between(1000.0, 3500.0), Some(2.5));
        assert_eq!(proj.bars_between(3500.0, 1000.0), Some(2.5));
    }

    #[test]
    fn series_access_exists_only_when_supplied() {
        let coords = coords();
        let pane_of = |_: &str| price_pane();
        let at = |_: f64| None;
        let bars = |from: f64, to: f64| vec![Ohlcv::new(from as i64, 1.0, 2.0, 0.5, 1.5, Some(to))];
        let proj = create_projector(&coords, &pane_of, &at, None, None);
        assert!(proj.bars_in_range(0.0, 1.0).is_none());
        assert!(proj.series_in_range("1m", 0.0, 1.0).is_none());
        let proj = create_projector(&coords, &pane_of, &at, Some(&bars), None);
        assert_eq!(proj.bars_in_range(10.0, 20.0).unwrap()[0].time, 10);
    }

    #[test]
    fn width_and_height_are_the_plot_size() {
        let coords = coords();
        let pane_of = |_: &str| price_pane();
        let at = |_: f64| None;
        let proj = create_projector(&coords, &pane_of, &at, None, None);
        assert_eq!((proj.width(), proj.height()), (800.0, 400.0));
    }
}
