//! Pure price-window transforms for manual vertical scaling (price-axis drag +
//! vertical price pan). Kept separate from the renderer so the math is
//! testable without a DOM. Both operate in log space when the window is
//! logarithmic so the gesture feels uniform on screen (the pane renders
//! through `ln`).

use super::coordinate_system::PriceScale;

/// Scale a window's SPAN by `factor` about its center (drag down means
/// `factor > 1`, zoom out).
pub fn rescale_around(start: &PriceScale, factor: f64) -> PriceScale {
    if start.log_usable() {
        let lmin = start.min.ln();
        let lmax = start.max.ln();
        let c = (lmin + lmax) / 2.0;
        let h = ((lmax - lmin) / 2.0) * factor;
        return PriceScale {
            min: (c - h).exp(),
            max: (c + h).exp(),
            log: Some(true),
            invert: None,
        };
    }
    let c = (start.min + start.max) / 2.0;
    let h = ((start.max - start.min) / 2.0) * factor;
    PriceScale {
        min: c - h,
        max: c + h,
        log: start.log,
        invert: None,
    }
}

/// Shift a window by `dy` pixels of pane height (down means both bounds move
/// UP in price).
pub fn shift_scale(start: &PriceScale, dy: f64, height: f64) -> PriceScale {
    if height <= 0.0 {
        return *start;
    }
    if start.log_usable() {
        let lmin = start.min.ln();
        let lmax = start.max.ln();
        let d = ((lmax - lmin) * dy) / height;
        return PriceScale {
            min: (lmin + d).exp(),
            max: (lmax + d).exp(),
            log: Some(true),
            invert: None,
        };
    }
    let d = ((start.max - start.min) * dy) / height;
    PriceScale {
        min: start.min + d,
        max: start.max + d,
        log: start.log,
        invert: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-6, "{a} vs {b}");
    }

    #[test]
    fn rescale_around_keeps_the_center_fixed_and_scales_the_span_by_the_factor() {
        let out = rescale_around(&PriceScale::new(100.0, 200.0), 2.0);
        close((out.min + out.max) / 2.0, 150.0);
        close(out.max - out.min, 200.0);
    }

    #[test]
    fn rescale_around_with_a_factor_below_one_compresses_the_span() {
        let out = rescale_around(&PriceScale::new(0.0, 100.0), 0.5);
        close((out.min + out.max) / 2.0, 50.0);
        close(out.max - out.min, 50.0);
    }

    #[test]
    fn rescale_around_works_in_log_space_about_the_geometric_center() {
        let start = PriceScale {
            log: Some(true),
            ..PriceScale::new(10.0, 1000.0)
        };
        let out = rescale_around(&start, 2.0);
        assert_eq!(out.log, Some(true));
        close((out.min * out.max).sqrt(), 100.0);
        // log-span doubled: original 2 decades, now 4
        close(out.max.log10() - out.min.log10(), 4.0);
    }

    #[test]
    fn rescale_around_a_log_window_that_cannot_be_logged_stays_linear() {
        let start = PriceScale {
            log: Some(true),
            ..PriceScale::new(-10.0, 10.0)
        };
        let out = rescale_around(&start, 2.0);
        assert_eq!((out.min, out.max, out.log), (-20.0, 20.0, Some(true)));
    }

    #[test]
    fn shift_scale_moves_both_bounds_up_in_price_when_dragged_down() {
        // span 100 over 100px: 1 price per px
        let out = shift_scale(&PriceScale::new(100.0, 200.0), 50.0, 100.0);
        close(out.min, 150.0);
        close(out.max, 250.0);
    }

    #[test]
    fn shift_scale_preserves_the_span() {
        let start = PriceScale::new(100.0, 200.0);
        let out = shift_scale(&start, -30.0, 100.0);
        close(out.max - out.min, start.max - start.min);
        close(out.min, 70.0);
    }

    #[test]
    fn shift_scale_on_zero_height_is_a_no_op_copy() {
        let out = shift_scale(&PriceScale::new(1.0, 2.0), 10.0, 0.0);
        assert_eq!(out, PriceScale::new(1.0, 2.0));
    }

    #[test]
    fn shift_scale_in_log_space_shifts_by_a_constant_ratio() {
        let start = PriceScale {
            log: Some(true),
            ..PriceScale::new(10.0, 1000.0)
        };
        let out = shift_scale(&start, 50.0, 100.0);
        close(out.min, 100.0);
        close(out.max, 10_000.0);
        assert_eq!(out.log, Some(true));
    }

    #[test]
    fn a_flat_window_stays_flat_and_nan_input_propagates() {
        let out = rescale_around(&PriceScale::new(5.0, 5.0), 3.0);
        assert_eq!((out.min, out.max), (5.0, 5.0));
        assert!(
            shift_scale(&PriceScale::new(1.0, 2.0), f64::NAN, 10.0)
                .min
                .is_nan()
        );
    }
}
