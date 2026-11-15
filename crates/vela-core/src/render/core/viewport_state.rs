//! The authoritative, user-controlled pan/zoom state of the time axis.
//!
//! - `bar_spacing`: pixels between adjacent bar centers (the zoom level).
//! - `right_offset`: how many bar-widths the LAST bar sits from the right edge
//!   (the pan position). May be negative (last bar scrolled off the right).
//!
//! The chart width and bar count are NOT part of this state: the renderer owns
//! them and combines them with the viewport in the
//! [`CoordinateSystem`](super::CoordinateSystem) to produce pixel coordinates.

use crate::indicators::classics::math::{js_max, js_min};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewportState {
    pub bar_spacing: f64,
    pub right_offset: f64,
}

pub const MIN_BAR_SPACING: f64 = 0.5;

/// A high sanity ceiling for a single bar's pixel width, NOT the practical
/// zoom-in limit. The real max-zoom is governed by `MIN_VISIBLE_BARS` in the
/// renderer's viewport clamp (keep at least N candles on screen, so
/// `bar_spacing <= width / N`), which on any sub-4K display binds well before
/// this. Kept large so a deep reveal-style zoom is not pre-clamped.
pub const MAX_BAR_SPACING: f64 = 1000.0;

/// Default zoom/pan before any data is loaded.
pub fn default_viewport() -> ViewportState {
    ViewportState {
        bar_spacing: 8.0,
        right_offset: 6.0,
    }
}

pub fn clamp_bar_spacing(spacing: f64) -> f64 {
    js_max(MIN_BAR_SPACING, js_min(MAX_BAR_SPACING, spacing))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_viewport_is_eight_px_with_a_six_bar_margin() {
        assert_eq!(
            default_viewport(),
            ViewportState {
                bar_spacing: 8.0,
                right_offset: 6.0
            }
        );
    }

    #[test]
    fn spacing_clamps_to_the_zoom_bounds() {
        assert_eq!(clamp_bar_spacing(0.0), MIN_BAR_SPACING);
        assert_eq!(clamp_bar_spacing(-5.0), MIN_BAR_SPACING);
        assert_eq!(clamp_bar_spacing(1e9), MAX_BAR_SPACING);
        assert_eq!(clamp_bar_spacing(12.5), 12.5);
        assert_eq!(clamp_bar_spacing(f64::INFINITY), MAX_BAR_SPACING);
    }

    #[test]
    fn a_nan_spacing_stays_nan_like_math_max() {
        assert!(clamp_bar_spacing(f64::NAN).is_nan());
    }
}
