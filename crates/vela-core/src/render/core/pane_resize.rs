//! Pure math for dragging the separator between two vertically-adjacent panes.
//! Kept separate from the renderer so the weight redistribution is testable
//! without a DOM.
//!
//! Panes are laid out by relative `height_weight`. Moving one separator should
//! only ever resize the two panes it sits between, so their COMBINED weight is
//! redistributed across the new boundary and every other pane's weight (and
//! therefore pixel height) stays untouched.

use crate::indicators::classics::math::{js_max, js_min};

/// A snapshot of the two adjacent panes' shared pixel span when a resize drag begins.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneSplit {
    /// Pixel top of the upper pane (top of the combined span).
    pub combined_top: f64,
    /// Combined pixel height of the upper + lower pane.
    pub combined_height: f64,
    /// Combined `height_weight` of the upper + lower pane (preserved across the drag).
    pub combined_weight: f64,
    /// Pixel y of the separator (the lower pane's top) when the drag began.
    pub start_boundary_y: f64,
}

/// Smallest pixel height either pane may be dragged down to (degrades to an
/// even split when the combined span is too short to honor it on both sides).
pub const MIN_PANE_PX: f64 = 24.0;

/// The new height weights of the panes above and below the separator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplitWeights {
    pub above: f64,
    pub below: f64,
}

/// Given the drag snapshot and the TOTAL pixel drag (down grows the upper
/// pane), return the new weights. Their sum equals the original
/// `combined_weight`, so sibling panes keep their sizes; the boundary is
/// clamped so neither pane shrinks below `min_px`.
pub fn resize_split(split: &PaneSplit, dy_total: f64, min_px: f64) -> SplitWeights {
    let PaneSplit {
        combined_top,
        combined_height,
        combined_weight,
        start_boundary_y,
    } = *split;
    let margin = js_min(min_px, combined_height / 2.0);
    let lo = combined_top + margin;
    let hi = combined_top + combined_height - margin;
    let boundary = js_max(lo, js_min(hi, start_boundary_y + dy_total));
    let above_height = boundary - combined_top;
    let above = if combined_height > 0.0 {
        combined_weight * (above_height / combined_height)
    } else {
        combined_weight / 2.0
    };
    SplitWeights {
        above,
        below: combined_weight - above,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 75/25 price:study split: combined span [0, 400], weights 3 + 1.
    const SPLIT: PaneSplit = PaneSplit {
        combined_top: 0.0,
        combined_height: 400.0,
        combined_weight: 4.0,
        start_boundary_y: 300.0,
    };

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-6, "{a} vs {b}");
    }

    #[test]
    fn preserves_the_combined_weight_so_sibling_panes_keep_their_size() {
        let w = resize_split(&SPLIT, -120.0, MIN_PANE_PX);
        close(w.above + w.below, SPLIT.combined_weight);
    }

    #[test]
    fn dragging_down_grows_the_upper_pane_and_up_grows_the_lower_pane() {
        let base = resize_split(&SPLIT, 0.0, MIN_PANE_PX);
        let down = resize_split(&SPLIT, 60.0, MIN_PANE_PX);
        let up = resize_split(&SPLIT, -60.0, MIN_PANE_PX);
        assert!(down.above > base.above);
        assert!(up.above < base.above);
    }

    #[test]
    fn maps_the_new_boundary_to_weights_proportionally() {
        // Boundary 300 -> 200: the upper pane is 200/400 of the span, weight 2.
        let w = resize_split(&SPLIT, -100.0, MIN_PANE_PX);
        close(w.above, 2.0);
        close(w.below, 2.0);
    }

    #[test]
    fn clamps_so_neither_pane_shrinks_below_the_minimum_height() {
        let far_up = resize_split(&SPLIT, -10_000.0, MIN_PANE_PX);
        close(
            (far_up.above / SPLIT.combined_weight) * SPLIT.combined_height,
            MIN_PANE_PX,
        );
        let far_down = resize_split(&SPLIT, 10_000.0, MIN_PANE_PX);
        close(
            (far_down.below / SPLIT.combined_weight) * SPLIT.combined_height,
            MIN_PANE_PX,
        );
    }

    #[test]
    fn degrades_to_an_even_split_when_the_span_is_too_small_for_the_minimum_twice() {
        let tiny = PaneSplit {
            combined_top: 0.0,
            combined_height: 20.0,
            combined_weight: 2.0,
            start_boundary_y: 0.0,
        };
        let w = resize_split(&tiny, -10_000.0, MIN_PANE_PX);
        close(w.above, w.below);
    }

    #[test]
    fn a_zero_height_span_splits_the_weight_in_half() {
        let flat = PaneSplit {
            combined_top: 50.0,
            combined_height: 0.0,
            combined_weight: 6.0,
            start_boundary_y: 50.0,
        };
        let w = resize_split(&flat, 30.0, MIN_PANE_PX);
        assert_eq!((w.above, w.below), (3.0, 3.0));
    }

    #[test]
    fn a_custom_minimum_is_honored() {
        let w = resize_split(&SPLIT, -10_000.0, 100.0);
        close(w.above, 1.0);
    }
}
