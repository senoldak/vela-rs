//! Number formatting shared by the widget chrome (statusline, data readouts), pure.

use crate::intl::{Intl, NumberFormatOptions, default_intl};
use crate::util::to_fixed;

/// Sensible decimal count for a price magnitude (crypto-friendly).
pub fn decimals_for(reference: f64) -> usize {
    let a = reference.abs();
    if a >= 1.0 {
        2
    } else if a >= 0.01 {
        4
    } else {
        6
    }
}

/// English, grouped price (`toLocaleString("en-US", ...)`); `"\u{2014}"` for `None` or a
/// non-finite value.
///
/// Deprecated in the original (English only): format through the chart's translator.
pub fn fmt_price(n: Option<f64>, dp: Option<usize>) -> String {
    let Some(n) = n.filter(|n| n.is_finite()) else {
        return "\u{2014}".to_string();
    };
    let d = dp.unwrap_or_else(|| decimals_for(n)) as u32;
    let options = NumberFormatOptions {
        minimum_fraction_digits: Some(d),
        maximum_fraction_digits: Some(d),
        ..NumberFormatOptions::default()
    };
    let intl: std::sync::Arc<dyn Intl> = default_intl();
    intl.format_number("en-US", &options, n)
        .unwrap_or_else(|| to_fixed(n, d as usize))
}

fn open_close(open: Option<f64>, close: Option<f64>) -> Option<(f64, f64)> {
    match (open, close) {
        (Some(o), Some(c)) if o.is_finite() && c.is_finite() && o != 0.0 => Some((o, c)),
        _ => None,
    }
}

/// Signed change + percent (`+123.45 (+1.23%)`) from open to close; `""` when unavailable.
pub fn fmt_change(open: Option<f64>, close: Option<f64>) -> String {
    let Some((open, close)) = open_close(open, close) else {
        return String::new();
    };
    let diff = close - open;
    let pct = (diff / open) * 100.0;
    let sign = if diff >= 0.0 { "+" } else { "" };
    format!(
        "{sign}{} ({sign}{}%)",
        fmt_price(Some(diff), Some(decimals_for(close))),
        to_fixed(pct, 2)
    )
}

/// Signed percent change alone (`+1.23%`) from open to close; `""` when unavailable.
pub fn fmt_change_pct(open: Option<f64>, close: Option<f64>) -> String {
    let Some((open, close)) = open_close(open, close) else {
        return String::new();
    };
    let pct = ((close - open) / open) * 100.0;
    format!("{}{}%", if pct >= 0.0 { "+" } else { "" }, to_fixed(pct, 2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_follow_the_price_magnitude() {
        assert_eq!(decimals_for(65_000.0), 2);
        assert_eq!(decimals_for(1.0), 2);
        assert_eq!(decimals_for(0.5), 4);
        assert_eq!(decimals_for(0.01), 4);
        assert_eq!(decimals_for(0.009), 6);
        assert_eq!(decimals_for(-3.0), 2);
    }

    #[test]
    fn fmt_price_groups_and_rounds() {
        assert_eq!(fmt_price(Some(1234.5), None), "1,234.50");
        assert_eq!(fmt_price(Some(0.5), None), "0.5000");
        assert_eq!(fmt_price(Some(-1234.567), Some(1)), "-1,234.6");
        assert_eq!(fmt_price(Some(7.0), Some(0)), "7");
    }

    #[test]
    fn fmt_price_dashes_unusable_values() {
        assert_eq!(fmt_price(None, None), "\u{2014}");
        assert_eq!(fmt_price(Some(f64::NAN), None), "\u{2014}");
        assert_eq!(fmt_price(Some(f64::INFINITY), Some(2)), "\u{2014}");
    }

    #[test]
    fn fmt_change_signs_the_move_and_the_percent() {
        assert_eq!(fmt_change(Some(100.0), Some(101.5)), "+1.50 (+1.50%)");
        assert_eq!(fmt_change(Some(100.0), Some(98.0)), "-2.00 (-2.00%)");
        assert_eq!(fmt_change(Some(0.0), Some(98.0)), "");
        assert_eq!(fmt_change(None, Some(98.0)), "");
        assert_eq!(fmt_change(Some(f64::NAN), Some(98.0)), "");
    }

    #[test]
    fn fmt_change_pct_formats_the_signed_percent_delta_alone() {
        assert_eq!(fmt_change_pct(Some(100.0), Some(101.5)), "+1.50%");
        assert_eq!(fmt_change_pct(Some(100.0), Some(98.0)), "-2.00%");
        assert_eq!(fmt_change_pct(Some(0.0), Some(98.0)), "");
        assert_eq!(fmt_change_pct(None, Some(98.0)), "");
    }
}
