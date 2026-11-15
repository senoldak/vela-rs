//! A renderer-free [`Projector`].

use crate::model::Ohlcv;

use super::geometry::{DrawingPoint, PaneRect, Projector};

/// A [`Projector`] with a fixed linear scale on one pane: `x` grows with
/// time, `y` grows as price falls (screen coordinates). It backs tests and
/// tools that need drawing geometry without a renderer.
#[derive(Clone, Debug, PartialEq)]
pub struct LinearProjector {
    pub pane_id: String,
    /// The time at pixel x = 0.
    pub time_origin: f64,
    pub ms_per_px: f64,
    /// The price at pixel y = 0 (the top edge).
    pub price_top: f64,
    pub price_per_px: f64,
    pub width: f64,
    pub height: f64,
    /// Bars served through `bars_in_range`; `None` = no series access.
    pub bars: Option<Vec<Ohlcv>>,
}

impl LinearProjector {
    /// A 1000×600 plot on pane `price`: one pixel per minute from time 0,
    /// price 200 at the top falling 0.25 per pixel.
    pub fn sample() -> Self {
        Self {
            pane_id: "price".to_string(),
            time_origin: 0.0,
            ms_per_px: 60_000.0,
            price_top: 200.0,
            price_per_px: 0.25,
            width: 1000.0,
            height: 600.0,
            bars: None,
        }
    }

    pub fn with_bars(mut self, bars: Vec<Ohlcv>) -> Self {
        self.bars = Some(bars);
        self
    }
}

impl Projector for LinearProjector {
    fn x_of(&self, time: f64) -> f64 {
        (time - self.time_origin) / self.ms_per_px
    }

    fn y_of(&self, price: f64, pane_id: &str) -> Option<f64> {
        (pane_id == self.pane_id).then(|| (self.price_top - price) / self.price_per_px)
    }

    fn px_to_point(&self, x: f64, y: f64, _pane_id: &str) -> DrawingPoint {
        DrawingPoint {
            time: self.time_origin + x * self.ms_per_px,
            price: self.price_top - y * self.price_per_px,
        }
    }

    fn pane_id_at_y(&self, y: f64) -> Option<String> {
        (y >= 0.0 && y <= self.height).then(|| self.pane_id.clone())
    }

    fn pane_rect(&self, pane_id: &str) -> Option<PaneRect> {
        (pane_id == self.pane_id).then_some(PaneRect {
            top: 0.0,
            height: self.height,
        })
    }

    fn bars_between(&self, t1: f64, t2: f64) -> Option<f64> {
        Some(crate::indicators::classics::math::js_round(
            (t2 - t1) / self.ms_per_px,
        ))
    }

    fn bars_in_range(&self, from: f64, to: f64) -> Option<Vec<Ohlcv>> {
        let bars = self.bars.as_ref()?;
        Some(
            bars.iter()
                .filter(|b| (b.time as f64) >= from && (b.time as f64) <= to)
                .copied()
                .collect(),
        )
    }

    fn width(&self) -> f64 {
        self.width
    }

    fn height(&self) -> f64 {
        self.height
    }
}
