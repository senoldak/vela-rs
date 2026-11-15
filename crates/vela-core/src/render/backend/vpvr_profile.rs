//! Volume-by-price profile binning for the visible-range volume profile (VPVR).

use crate::indicators::classics::math::{js_max, js_min, js_round};
use crate::model::Ohlcv;

/// Most rows a profile is built with, so a nonsensical row count cannot exhaust memory.
const MAX_ROWS: f64 = 1_000_000.0;

/// One profile row: `[price, price + row_h)` with the up/down volume that landed in it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VpvrRow {
    /// The row's LOWER price bound.
    pub price: f64,
    pub up: f64,
    pub down: f64,
}

impl VpvrRow {
    fn total(&self) -> f64 {
        self.up + self.down
    }
}

/// The bucketed visible-range volume-by-price profile.
#[derive(Clone, Debug, PartialEq)]
pub struct VpvrProfile {
    /// Ascending by price; length = requested row count (a degenerate flat range yields
    /// 1).
    pub rows: Vec<VpvrRow>,
    /// Price height of one row.
    pub row_h: f64,
    /// Price floor of the profile (`rows[0].price`).
    pub min: f64,
    /// Largest single-row total volume (normalization; > 0).
    pub max_total: f64,
    /// Index of the point-of-control (highest total-volume) row.
    pub poc: usize,
    /// Value-area row span, inclusive `[va_from, va_to]`.
    pub va_from: usize,
    pub va_to: usize,
}

/// A bar that carries volume and a finite price range. (The original would throw on a
/// non-finite range; such a bar is skipped here.)
fn usable(b: &Ohlcv) -> Option<f64> {
    match b.volume {
        Some(v) if v > 0.0 && b.low.is_finite() && b.high.is_finite() => Some(v),
        _ => None,
    }
}

fn clamp_index(k: f64, n: usize) -> usize {
    if k < 0.0 || k.is_nan() {
        0
    } else if k >= n as f64 {
        n - 1
    } else {
        k as usize
    }
}

/// Bucket the visible bars' volume by price. Each bar's volume is distributed over the
/// rows its high-low range overlaps, proportionally to the overlap, and split up/down by
/// the bar's close direction, so total volume is conserved exactly. The value area is
/// grown from the POC by repeatedly absorbing the larger adjacent row until it covers
/// `value_area_frac` of the total volume. Pure: the renderer memoizes calls on the
/// visible range.
///
/// Returns `None` when the window holds no volume (or no bars). The window `[i0, i1]` is
/// clamped to the slice.
pub fn build_vpvr_profile(
    bars: &[Ohlcv],
    i0: i64,
    i1: i64,
    row_count: f64,
    value_area_frac: f64,
) -> Option<VpvrProfile> {
    let from = i0.max(0);
    let to = i1.min(bars.len() as i64 - 1);
    if from > to {
        return None;
    }
    let window = &bars[from as usize..=to as usize];

    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for b in window {
        if usable(b).is_none() {
            continue;
        }
        if b.low < min {
            min = b.low;
        }
        if b.high > max {
            max = b.high;
        }
    }
    if !min.is_finite() {
        return None; // no visible volume at all
    }

    // A flat window (every volumed bar at one price) still profiles, as a single row.
    let n = if max > min {
        let rounded = js_round(row_count);
        if rounded.is_finite() {
            js_max(1.0, rounded).min(MAX_ROWS) as usize
        } else {
            1
        }
    } else {
        1
    };
    let row_h = if max > min {
        (max - min) / n as f64
    } else {
        1.0
    };
    let mut rows: Vec<VpvrRow> = (0..n)
        .map(|k| VpvrRow {
            price: min + k as f64 * row_h,
            up: 0.0,
            down: 0.0,
        })
        .collect();

    for b in window {
        let Some(v) = usable(b) else { continue };
        let up = b.close >= b.open;
        let add = |row: &mut VpvrRow, amount: f64| {
            if up {
                row.up += amount;
            } else {
                row.down += amount;
            }
        };
        let span = b.high - b.low;
        if span <= 0.0 {
            // Point bar: all volume in the row containing its price.
            let k = clamp_index(f64::floor((b.low - min) / row_h), n);
            add(&mut rows[k], v);
            continue;
        }
        // Distribute over overlapped rows, proportional to overlap.
        let k_first = clamp_index(f64::floor((b.low - min) / row_h), n);
        let k_last = clamp_index(f64::floor((b.high - min) / row_h - 1e-9), n);
        for (k, row) in rows.iter_mut().enumerate().take(k_last + 1).skip(k_first) {
            let row_bot = min + k as f64 * row_h;
            let overlap = js_min(b.high, row_bot + row_h) - js_max(b.low, row_bot);
            if overlap > 0.0 {
                add(row, (v * overlap) / span);
            }
        }
    }

    let mut total = 0.0;
    let mut max_total = 0.0;
    let mut poc = 0;
    for (k, row) in rows.iter().enumerate() {
        let t = row.total();
        total += t;
        if t > max_total {
            max_total = t;
            poc = k;
        }
    }
    if max_total <= 0.0 {
        return None;
    }

    // Grow the value area from the POC: absorb whichever adjacent row is larger until the
    // area covers the target volume fraction.
    let target = total * js_min(1.0, js_max(0.0, value_area_frac));
    let mut va_from = poc;
    let mut va_to = poc;
    let mut acc = max_total;
    while acc < target && (va_from > 0 || va_to < n - 1) {
        let below = if va_from > 0 {
            rows[va_from - 1].total()
        } else {
            -1.0
        };
        let above = if va_to < n - 1 {
            rows[va_to + 1].total()
        } else {
            -1.0
        };
        if above > below {
            va_to += 1;
            acc += above;
        } else {
            va_from -= 1;
            acc += below;
        }
    }

    Some(VpvrProfile {
        rows,
        row_h,
        min,
        max_total,
        poc,
        va_from,
        va_to,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar_at(low: f64, high: f64, volume: f64, up: bool, time: i64) -> Ohlcv {
        let (open, close) = if up { (low, high) } else { (high, low) };
        Ohlcv::new(time, open, high, low, close, Some(volume))
    }

    fn bar(low: f64, high: f64, volume: f64) -> Ohlcv {
        bar_at(low, high, volume, true, 0)
    }

    fn down(low: f64, high: f64, volume: f64) -> Ohlcv {
        bar_at(low, high, volume, false, 0)
    }

    fn total(p: &VpvrProfile) -> f64 {
        p.rows.iter().map(|r| r.up + r.down).sum()
    }

    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-6, "{a} != {b}");
    }

    #[test]
    fn conserves_total_volume_exactly_across_the_rows() {
        let bars = [
            bar(100.0, 110.0, 500.0),
            down(103.0, 118.0, 750.0),
            bar(95.0, 105.0, 250.0),
        ];
        let p = build_vpvr_profile(&bars, 0, 2, 24.0, 0.7).unwrap();
        near(total(&p), 1500.0);
    }

    #[test]
    fn splits_up_down_volume_by_bar_direction() {
        let bars = [bar(100.0, 110.0, 600.0), down(100.0, 110.0, 600.0)];
        let p = build_vpvr_profile(&bars, 0, 1, 10.0, 0.7).unwrap();
        for r in &p.rows {
            near(r.up, r.down);
        }
        near(p.rows.iter().map(|r| r.up).sum(), 600.0);
    }

    #[test]
    fn puts_the_poc_where_the_most_volume_overlaps() {
        // Two bars overlap only in [104, 106]: the middle rows carry double volume.
        let bars = [bar(100.0, 106.0, 600.0), bar(104.0, 110.0, 600.0)];
        let p = build_vpvr_profile(&bars, 0, 1, 10.0, 0.7).unwrap();
        near(p.row_h, 1.0);
        let poc_price = p.rows[p.poc].price;
        assert!(poc_price >= 104.0 - 1e-9);
        assert!(poc_price + p.row_h <= 106.0 + 1e-9);
    }

    #[test]
    fn grows_the_value_area_around_the_poc_to_cover_the_requested_fraction() {
        let bars = [bar(100.0, 106.0, 600.0), bar(104.0, 110.0, 600.0)];
        let p = build_vpvr_profile(&bars, 0, 1, 10.0, 0.7).unwrap();
        let va: f64 = (p.va_from..=p.va_to)
            .map(|k| p.rows[k].up + p.rows[k].down)
            .sum();
        assert!(va >= total(&p) * 0.7 - 1e-6);
        assert!(p.va_from <= p.poc);
        assert!(p.va_to >= p.poc);
        // Full-coverage request spans every row; zero-coverage collapses to the POC alone.
        let full = build_vpvr_profile(&bars, 0, 1, 10.0, 1.0).unwrap();
        assert_eq!((full.va_from, full.va_to), (0, full.rows.len() - 1));
        let none = build_vpvr_profile(&bars, 0, 1, 10.0, 0.0).unwrap();
        assert_eq!((none.va_from, none.va_to), (none.poc, none.poc));
    }

    #[test]
    fn handles_a_flat_window_as_a_single_row_and_skips_volumeless_bars() {
        let flat = Ohlcv::new(0, 100.0, 100.0, 100.0, 100.0, Some(400.0));
        let no_vol = Ohlcv::new(1, 90.0, 120.0, 80.0, 95.0, None);
        let p = build_vpvr_profile(&[flat, no_vol], 0, 1, 24.0, 0.7).unwrap();
        assert_eq!(p.rows.len(), 1);
        near(p.rows[0].up, 400.0);
        near(p.max_total, 400.0);
    }

    #[test]
    fn returns_none_when_the_window_holds_no_volume_or_no_bars() {
        let no_vol = Ohlcv::new(0, 90.0, 120.0, 80.0, 95.0, None);
        assert!(build_vpvr_profile(&[no_vol], 0, 0, 24.0, 0.7).is_none());
        assert!(build_vpvr_profile(&[], 0, 0, 24.0, 0.7).is_none());
        // A window outside the array.
        assert!(build_vpvr_profile(&[bar(100.0, 110.0, 100.0)], 5, 9, 24.0, 0.7).is_none());
    }

    #[test]
    fn a_point_bar_lands_all_volume_in_its_containing_row() {
        let point = Ohlcv::new(1, 105.0, 105.0, 105.0, 105.0, Some(5000.0));
        let bars = [bar(100.0, 110.0, 1000.0), point];
        let p = build_vpvr_profile(&bars, 0, 1, 10.0, 0.7).unwrap();
        let k = p
            .rows
            .iter()
            .position(|r| 105.0 >= r.price && 105.0 < r.price + p.row_h)
            .unwrap();
        assert_eq!(p.poc, k);
        assert!(p.rows[k].up >= 5000.0);
    }

    #[test]
    fn a_negative_window_start_is_clamped() {
        let bars = [bar(100.0, 110.0, 100.0)];
        assert!(build_vpvr_profile(&bars, -5, 3, 10.0, 0.7).is_some());
    }

    #[test]
    fn zero_or_nan_row_counts_still_profile() {
        let bars = [bar(100.0, 110.0, 100.0)];
        assert_eq!(
            build_vpvr_profile(&bars, 0, 0, 0.0, 0.7)
                .unwrap()
                .rows
                .len(),
            1
        );
        assert_eq!(
            build_vpvr_profile(&bars, 0, 0, f64::NAN, 0.7)
                .unwrap()
                .rows
                .len(),
            1
        );
        assert_eq!(
            build_vpvr_profile(&bars, 0, 0, 2.4, 0.7)
                .unwrap()
                .rows
                .len(),
            2
        );
    }

    #[test]
    fn bars_with_non_finite_prices_are_skipped() {
        let bad = Ohlcv::new(0, f64::NAN, f64::NAN, f64::NAN, f64::NAN, Some(10.0));
        let inf = Ohlcv::new(1, 1.0, f64::INFINITY, 1.0, 2.0, Some(10.0));
        let bars = [bad, inf, bar(100.0, 110.0, 100.0)];
        let p = build_vpvr_profile(&bars, 0, 2, 10.0, 0.7).unwrap();
        near(total(&p), 100.0);
    }
}
