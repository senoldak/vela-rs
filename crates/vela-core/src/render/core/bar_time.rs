//! Pure bar-time to logical-index helpers for the native renderer's
//! coordinate system.

/// Median spacing between consecutive bar times: robust to gaps and missing
/// bars. 0 when there are fewer than two bars or no forward step.
pub fn median_interval(times: &[i64]) -> f64 {
    if times.len() < 2 {
        return 0.0;
    }
    let mut diffs: Vec<i64> = times
        .windows(2)
        .map(|w| w[1].saturating_sub(w[0]))
        .filter(|d| *d > 0)
        .collect();
    if diffs.is_empty() {
        return 0.0;
    }
    diffs.sort_unstable();
    diffs[diffs.len() / 2] as f64
}

/// Map a bar time (epoch ms) to a fractional logical bar index. Interpolates
/// between known bars and extrapolates before/after the dataset using
/// `interval` so extended/future drawing anchors still resolve.
pub fn bar_time_to_logical(ms: f64, times: &[i64], interval: f64) -> f64 {
    let n = times.len();
    let (Some(&first), Some(&last)) = (times.first(), times.last()) else {
        return 0.0;
    };
    let (first, last) = (first as f64, last as f64);
    if ms <= first {
        return if interval > 0.0 {
            (ms - first) / interval
        } else {
            0.0
        };
    }
    if ms >= last {
        return (n - 1) as f64
            + if interval > 0.0 {
                (ms - last) / interval
            } else {
                0.0
            };
    }
    // Binary search for the bracketing bars, then linear-interpolate.
    let mut lo = 0usize;
    let mut hi = n - 1;
    while hi - lo > 1 {
        let mid = (lo + hi) >> 1;
        if times[mid] as f64 <= ms {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let span = (times[hi] - times[lo]) as f64;
    if span > 0.0 {
        lo as f64 + (ms - times[lo] as f64) / span
    } else {
        lo as f64
    }
}

/// Inverse of [`bar_time_to_logical`]: fractional logical index to epoch ms.
pub fn logical_to_bar_time(logical: f64, times: &[i64], interval: f64) -> f64 {
    let n = times.len();
    let (Some(&first), Some(&last)) = (times.first(), times.last()) else {
        return 0.0;
    };
    if logical <= 0.0 {
        return first as f64
            + if interval > 0.0 {
                logical * interval
            } else {
                0.0
            };
    }
    if logical >= (n - 1) as f64 {
        return last as f64
            + if interval > 0.0 {
                (logical - (n - 1) as f64) * interval
            } else {
                0.0
            };
    }
    if logical.is_nan() {
        return f64::NAN;
    }
    let lo = logical.floor();
    let frac = logical - lo;
    let lo = lo as usize;
    match (times.get(lo), times.get(lo + 1)) {
        (Some(&a), Some(&b)) => a as f64 + (b - a) as f64 * frac,
        _ => f64::NAN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_interval_ignores_gaps_and_non_forward_steps() {
        assert_eq!(median_interval(&[]), 0.0);
        assert_eq!(median_interval(&[5]), 0.0);
        assert_eq!(median_interval(&[1000, 2000, 3000, 4000]), 1000.0);
        // a weekend gap and a duplicate time do not move the median
        assert_eq!(
            median_interval(&[0, 1000, 2000, 2000, 3000, 90_000]),
            1000.0
        );
        assert_eq!(median_interval(&[3000, 2000, 1000]), 0.0);
    }

    #[test]
    fn time_maps_between_bars_and_past_the_ends() {
        let t = [1000, 2000, 4000];
        assert_eq!(bar_time_to_logical(1000.0, &t, 1000.0), 0.0);
        assert_eq!(bar_time_to_logical(3000.0, &t, 1000.0), 1.5);
        assert_eq!(bar_time_to_logical(4000.0, &t, 1000.0), 2.0);
        assert_eq!(bar_time_to_logical(6000.0, &t, 1000.0), 4.0);
        assert_eq!(bar_time_to_logical(0.0, &t, 1000.0), -1.0);
    }

    #[test]
    fn an_unknown_interval_pins_the_extrapolation_to_the_ends() {
        let t = [1000, 2000];
        assert_eq!(bar_time_to_logical(-50.0, &t, 0.0), 0.0);
        assert_eq!(bar_time_to_logical(9000.0, &t, 0.0), 1.0);
        assert_eq!(logical_to_bar_time(-3.0, &t, 0.0), 1000.0);
        assert_eq!(logical_to_bar_time(7.0, &t, 0.0), 2000.0);
    }

    #[test]
    fn empty_and_single_bar_series_do_not_panic() {
        assert_eq!(bar_time_to_logical(5.0, &[], 1000.0), 0.0);
        assert_eq!(logical_to_bar_time(5.0, &[], 1000.0), 0.0);
        assert_eq!(bar_time_to_logical(5.0, &[10], 0.0), 0.0);
        assert_eq!(bar_time_to_logical(15.0, &[10], 5.0), 1.0);
        assert_eq!(logical_to_bar_time(0.0, &[10], 5.0), 10.0);
    }

    #[test]
    fn logical_maps_back_to_time() {
        let t = [1000, 2000, 4000];
        assert_eq!(logical_to_bar_time(1.5, &t, 1000.0), 3000.0);
        assert_eq!(logical_to_bar_time(2.0, &t, 1000.0), 4000.0);
        assert_eq!(logical_to_bar_time(-0.5, &t, 1000.0), 500.0);
        assert!(logical_to_bar_time(f64::NAN, &t, 1000.0).is_nan());
    }

    #[test]
    fn duplicate_times_resolve_to_the_last_duplicate() {
        let t = [1000, 2000, 2000, 3000];
        assert_eq!(bar_time_to_logical(2000.0, &t, 1000.0), 2.0);
    }

    #[test]
    fn a_nan_time_is_nan_between_bars() {
        assert!(bar_time_to_logical(f64::NAN, &[1000, 2000, 3000], 1000.0).is_nan());
    }
}
