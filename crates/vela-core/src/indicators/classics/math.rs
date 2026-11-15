//! Pure array math for the classic indicator catalog. Every function is
//! bar-aligned: input and output arrays index 1:1 with the chart's bars, and
//! `NaN` marks "no value yet" (warm-up) or an honest gap (e.g. a volume-less
//! bar in a volume study). Windowed ops emit NaN until the window is fully
//! finite, and recursive ops (EMA/RMA) re-seed after a mid-stream gap instead
//! of carrying a stale state across it.

use std::f64::consts::PI;

use crate::model::Ohlcv;

const NAN: f64 = f64::NAN;

/// Price-source vocabulary shared by every classic with a `source` input.
pub const SOURCES: [&str; 8] = [
    "Close", "Open", "High", "Low", "HL2", "HLC3", "OHLC4", "HLCC4",
];

/// `Math.max` semantics: NaN if either operand is NaN (`f64::max` would drop it).
pub fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        NAN
    } else {
        a.max(b)
    }
}

/// `Math.min` semantics: NaN if either operand is NaN.
pub fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        NAN
    } else {
        a.min(b)
    }
}

/// `Math.round` semantics: halves round toward +∞ (`f64::round` rounds them away from zero).
pub fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// Resolve a source name to per-bar values. Unknown names fall back to Close.
pub fn source_values(bars: &[Ohlcv], source: &str) -> Vec<f64> {
    bars.iter()
        .map(|b| match source {
            "Open" => b.open,
            "High" => b.high,
            "Low" => b.low,
            "HL2" => (b.high + b.low) / 2.0,
            "HLC3" => (b.high + b.low + b.close) / 3.0,
            "OHLC4" => (b.open + b.high + b.low + b.close) / 4.0,
            "HLCC4" => (b.high + b.low + b.close + b.close) / 4.0,
            _ => b.close,
        })
        .collect()
}

/// Per-bar volume; NaN where the provider reports none (gaps, never fake zeros).
pub fn volumes(bars: &[Ohlcv]) -> Vec<f64> {
    bars.iter()
        .map(|b| b.volume.filter(|v| v.is_finite()).unwrap_or(NAN))
        .collect()
}

pub fn highs(bars: &[Ohlcv]) -> Vec<f64> {
    bars.iter().map(|b| b.high).collect()
}

pub fn lows(bars: &[Ohlcv]) -> Vec<f64> {
    bars.iter().map(|b| b.low).collect()
}

pub fn closes(bars: &[Ohlcv]) -> Vec<f64> {
    bars.iter().map(|b| b.close).collect()
}

pub fn opens(bars: &[Ohlcv]) -> Vec<f64> {
    bars.iter().map(|b| b.open).collect()
}

/// Element-wise combine of same-length arrays; any non-finite operand yields NaN.
/// The output has `a`'s length; a missing `b` entry counts as a gap.
pub fn zip(a: &[f64], b: &[f64], f: impl Fn(f64, f64) -> f64) -> Vec<f64> {
    a.iter()
        .enumerate()
        .map(|(i, &x)| {
            let y = b.get(i).copied().unwrap_or(NAN);
            if x.is_finite() && y.is_finite() {
                f(x, y)
            } else {
                NAN
            }
        })
        .collect()
}

/// Element-wise transform; a non-finite input yields NaN.
pub fn map(a: &[f64], f: impl Fn(f64) -> f64) -> Vec<f64> {
    a.iter()
        .map(|&x| if x.is_finite() { f(x) } else { NAN })
        .collect()
}

pub fn add(a: &[f64], b: &[f64]) -> Vec<f64> {
    zip(a, b, |x, y| x + y)
}

pub fn sub(a: &[f64], b: &[f64]) -> Vec<f64> {
    zip(a, b, |x, y| x - y)
}

/// Windowed fold: NaN until `len` consecutive finite values end at the index.
/// `fold` receives the window as a slice of exactly `len` values, oldest first.
fn windowed(v: &[f64], len: usize, fold: impl Fn(&[f64]) -> f64) -> Vec<f64> {
    let mut out = vec![NAN; v.len()];
    if len == 0 {
        return out;
    }
    // Track the length of the finite run ending at i so a single mid-stream NaN
    // restarts the warm-up instead of poisoning one output only.
    let mut run = 0usize;
    for i in 0..v.len() {
        run = if v[i].is_finite() { run + 1 } else { 0 };
        if run >= len {
            out[i] = fold(&v[i + 1 - len..=i]);
        }
    }
    out
}

/// Rolling sum over `len` bars.
pub fn sum(v: &[f64], len: usize) -> Vec<f64> {
    windowed(v, len, |w| w.iter().sum())
}

/// Simple moving average.
pub fn sma(v: &[f64], len: usize) -> Vec<f64> {
    windowed(v, len, |w| w.iter().sum::<f64>() / len as f64)
}

/// Linearly weighted moving average (weights 1..len, newest heaviest).
pub fn wma(v: &[f64], len: usize) -> Vec<f64> {
    let norm = (len * (len + 1)) as f64 / 2.0;
    windowed(v, len, |w| {
        w.iter()
            .enumerate()
            .map(|(k, x)| x * (k + 1) as f64)
            .sum::<f64>()
            / norm
    })
}

/// Volume-weighted moving average.
pub fn vwma(src: &[f64], vol: &[f64], len: usize) -> Vec<f64> {
    let pv = zip(src, vol, |p, q| p * q);
    zip(&sum(&pv, len), &sum(vol, len), |a, b| {
        if b > 0.0 { a / b } else { NAN }
    })
}

/// Population standard deviation over `len` bars.
pub fn stdev(v: &[f64], len: usize) -> Vec<f64> {
    windowed(v, len, |w| {
        let mean = w.iter().sum::<f64>() / len as f64;
        let sq: f64 = w.iter().map(|x| (x - mean) * (x - mean)).sum();
        (sq / len as f64).sqrt()
    })
}

/// Mean absolute deviation over `len` bars (the CCI denominator).
pub fn mean_dev(v: &[f64], len: usize) -> Vec<f64> {
    windowed(v, len, |w| {
        let mean = w.iter().sum::<f64>() / len as f64;
        w.iter().map(|x| (x - mean).abs()).sum::<f64>() / len as f64
    })
}

pub fn highest(v: &[f64], len: usize) -> Vec<f64> {
    windowed(v, len, |w| {
        w.iter().copied().fold(f64::NEG_INFINITY, f64::max)
    })
}

pub fn lowest(v: &[f64], len: usize) -> Vec<f64> {
    windowed(v, len, |w| w.iter().copied().fold(f64::INFINITY, f64::min))
}

/// Bars since the window's highest value (0 = the current bar). Ties pick the newest.
pub fn bars_since_highest(v: &[f64], len: usize) -> Vec<f64> {
    windowed(v, len, |w| {
        let mut m = f64::NEG_INFINITY;
        let mut at = w.len() - 1;
        for (k, &x) in w.iter().enumerate() {
            if x >= m {
                m = x;
                at = k;
            }
        }
        (w.len() - 1 - at) as f64
    })
}

/// Bars since the window's lowest value (0 = the current bar). Ties pick the newest.
pub fn bars_since_lowest(v: &[f64], len: usize) -> Vec<f64> {
    windowed(v, len, |w| {
        let mut m = f64::INFINITY;
        let mut at = w.len() - 1;
        for (k, &x) in w.iter().enumerate() {
            if x <= m {
                m = x;
                at = k;
            }
        }
        (w.len() - 1 - at) as f64
    })
}

/// Recursive average with seed = SMA of the first clean window; re-seeds after a gap.
fn recursive(v: &[f64], len: usize, alpha: f64) -> Vec<f64> {
    let mut out = vec![NAN; v.len()];
    let mut run = 0usize;
    let mut prev = NAN;
    for i in 0..v.len() {
        let x = v[i];
        if !x.is_finite() {
            run = 0;
            prev = NAN;
            continue;
        }
        run += 1;
        if prev.is_finite() {
            prev = alpha * x + (1.0 - alpha) * prev;
            out[i] = prev;
        } else if run >= len {
            // A zero length averages an empty window: 0/0, which stays NaN.
            prev = v[i + 1 - len..=i].iter().sum::<f64>() / len as f64;
            out[i] = prev;
        }
    }
    out
}

/// Exponential moving average (alpha = 2 / (len + 1)).
pub fn ema(v: &[f64], len: usize) -> Vec<f64> {
    recursive(v, len, 2.0 / (len as f64 + 1.0))
}

/// Smoothed moving average / Wilder's average (alpha = 1 / len).
pub fn rma(v: &[f64], len: usize) -> Vec<f64> {
    recursive(v, len, 1.0 / len as f64)
}

/// `n`-bar difference (the original's default is `n = 1`).
pub fn change(v: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![NAN; v.len()];
    for i in n..v.len() {
        let a = v[i];
        let b = v[i - n];
        if a.is_finite() && b.is_finite() {
            out[i] = a - b;
        }
    }
    out
}

/// Percentage rate of change over `len` bars.
pub fn roc(v: &[f64], len: usize) -> Vec<f64> {
    let mut out = vec![NAN; v.len()];
    for i in len..v.len() {
        let a = v[i];
        let b = v[i - len];
        if a.is_finite() && b.is_finite() && b != 0.0 {
            out[i] = (100.0 * (a - b)) / b;
        }
    }
    out
}

/// Shift values `n` bars forward in time (positive n plots past values on later bars).
pub fn shift(v: &[f64], n: i64) -> Vec<f64> {
    let len = v.len() as i64;
    (0..len)
        .map(|i| {
            let j = i - n;
            if j >= 0 && j < len {
                v[j as usize]
            } else {
                NAN
            }
        })
        .collect()
}

/// True range per bar (uses the previous close where one exists).
pub fn true_range(bars: &[Ohlcv]) -> Vec<f64> {
    bars.iter()
        .enumerate()
        .map(|(i, b)| {
            if i == 0 {
                return b.high - b.low;
            }
            let pc = bars[i - 1].close;
            js_max(
                js_max(b.high - b.low, (b.high - pc).abs()),
                (b.low - pc).abs(),
            )
        })
        .collect()
}

/// Average true range (Wilder's smoothing of the true range).
pub fn atr(bars: &[Ohlcv], len: usize) -> Vec<f64> {
    rma(&true_range(bars), len)
}

/// Relative strength index over `len` bars.
pub fn rsi(v: &[f64], len: usize) -> Vec<f64> {
    let d = change(v, 1);
    let up = map(&d, |x| x.max(0.0));
    let dn = map(&d, |x| (-x).max(0.0));
    zip(&rma(&up, len), &rma(&dn, len), |u, w| {
        if w == 0.0 {
            100.0
        } else {
            100.0 - 100.0 / (1.0 + u / w)
        }
    })
}

/// Raw stochastic %K: where the value sits inside its `len`-bar high/low range.
pub fn stoch(v: &[f64], hi: &[f64], lo: &[f64], len: usize) -> Vec<f64> {
    let hh = highest(hi, len);
    let ll = lowest(lo, len);
    let mut out = vec![NAN; v.len()];
    for i in 0..v.len() {
        let x = v[i];
        let h = hh.get(i).copied().unwrap_or(NAN);
        let l = ll.get(i).copied().unwrap_or(NAN);
        if !x.is_finite() || !h.is_finite() || !l.is_finite() {
            continue;
        }
        out[i] = if h == l {
            50.0
        } else {
            (100.0 * (x - l)) / (h - l)
        };
    }
    out
}

/// Least-squares linear regression value at each bar. `offset` steps back
/// along the fitted line: 0 is the fit's endpoint (the current bar),
/// `len - 1` its start.
pub fn linreg(v: &[f64], len: usize, offset: f64) -> Vec<f64> {
    // Closed form over x = 0..len-1 with y newest-last: slope/intercept from the
    // standard normal equations, evaluated at x = len - 1 - offset.
    let n = len as f64;
    let sx = ((n - 1.0) * n) / 2.0;
    let sxx = ((n - 1.0) * n * (2.0 * n - 1.0)) / 6.0;
    let denom = n * sxx - sx * sx;
    windowed(v, len, |w| {
        let mut sy = 0.0;
        let mut sxy = 0.0;
        for (k, &y) in w.iter().enumerate() {
            sy += y;
            sxy += k as f64 * y;
        }
        let slope = if denom == 0.0 {
            0.0
        } else {
            (n * sxy - sx * sy) / denom
        };
        let intercept = (sy - slope * sx) / n;
        intercept + slope * (n - 1.0 - offset)
    })
}

/// Hull moving average: WMA of `2·WMA(len/2) − WMA(len)` over √len bars. The
/// half length TRUNCATES: rounding it up leaves a residual lag the
/// construction is meant to cancel exactly.
pub fn hma(v: &[f64], len: usize) -> Vec<f64> {
    let n = len.max(2);
    let raw = sub(&map(&wma(v, (n / 2).max(1)), |x| 2.0 * x), &wma(v, n));
    wma(&raw, (js_round((n as f64).sqrt()) as usize).max(1))
}

/// Arnaud Legoux moving average: a Gaussian window whose peak `offset` slides
/// toward the newest bar.
pub fn alma(v: &[f64], len: usize, offset: f64, sigma: f64) -> Vec<f64> {
    let m = offset * (len as f64 - 1.0);
    let s = len as f64 / sigma.max(1e-9);
    let weights: Vec<f64> = (0..len)
        .map(|i| {
            let d = i as f64 - m;
            (-(d * d) / (2.0 * s * s)).exp()
        })
        .collect();
    let norm: f64 = weights.iter().sum();
    windowed(v, len, |w| {
        let acc: f64 = w.iter().zip(&weights).map(|(x, wt)| x * wt).sum();
        if norm == 0.0 { NAN } else { acc / norm }
    })
}

/// Double exponential moving average: `2·EMA − EMA(EMA)`.
pub fn dema(v: &[f64], len: usize) -> Vec<f64> {
    let e1 = ema(v, len);
    sub(&map(&e1, |x| 2.0 * x), &ema(&e1, len))
}

/// Triple exponential moving average: `3·EMA − 3·EMA² + EMA³`.
pub fn tema(v: &[f64], len: usize) -> Vec<f64> {
    let e1 = ema(v, len);
    let e2 = ema(&e1, len);
    let e3 = ema(&e2, len);
    zip(
        &zip(&map(&e1, |x| 3.0 * x), &map(&e2, |x| 3.0 * x), |a, b| a - b),
        &e3,
        |a, b| a + b,
    )
}

/// Default fast/slow smoothing constants of [`kama`].
pub const KAMA_FAST_ALPHA: f64 = 2.0 / 3.0;
pub const KAMA_SLOW_ALPHA: f64 = 2.0 / 31.0;

/// Kaufman's adaptive moving average: an EMA whose alpha tracks the efficiency ratio.
pub fn kama(v: &[f64], len: usize, fast_alpha: f64, slow_alpha: f64) -> Vec<f64> {
    let direction = map(&change(v, len), f64::abs);
    let volatility = sum(&map(&change(v, 1), f64::abs), len);
    let mut out = vec![NAN; v.len()];
    let mut prev = NAN;
    for i in 0..v.len() {
        let x = v[i];
        if !x.is_finite() {
            continue;
        }
        let d = direction[i];
        let w = volatility[i];
        if !d.is_finite() || !w.is_finite() {
            prev = if prev.is_finite() { prev } else { x };
            out[i] = prev;
            continue;
        }
        let er = if w == 0.0 { 0.0 } else { d / w };
        let sc = (er * (fast_alpha - slow_alpha) + slow_alpha).powi(2);
        prev = if prev.is_finite() {
            prev + sc * (x - prev)
        } else {
            x
        };
        out[i] = prev;
    }
    out
}

/// McGinley dynamic: an average whose step shrinks as price outruns it. It
/// seeds on the first value rather than a warmed-up average: the
/// `(price/prev)⁴` denominator explodes when the line starts far below price,
/// and a lagging seed never catches up.
pub fn mcginley(v: &[f64], len: usize) -> Vec<f64> {
    let mut out = vec![NAN; v.len()];
    let mut prev = NAN;
    for i in 0..v.len() {
        let x = v[i];
        if !x.is_finite() {
            continue;
        }
        if !prev.is_finite() {
            prev = x;
        } else {
            let ratio = if prev == 0.0 { 0.0 } else { x / prev };
            let denom = len as f64 * ratio.powi(4);
            prev += if denom == 0.0 {
                0.0
            } else {
                (x - prev) / denom
            };
        }
        out[i] = prev;
    }
    out
}

/// Default coefficients of [`hamming`].
pub const HAMMING_A0: f64 = 0.54;
pub const HAMMING_A1: f64 = 0.46;

/// Hamming-window weighted moving average (a raised cosine taper over the window).
pub fn hamming(v: &[f64], len: usize, a0: f64, a1: f64) -> Vec<f64> {
    if len <= 1 {
        return v
            .iter()
            .map(|&x| if x.is_finite() { x } else { NAN })
            .collect();
    }
    let weights: Vec<f64> = (0..len)
        .map(|i| a0 - a1 * ((2.0 * PI * i as f64) / (len as f64 - 1.0)).cos())
        .collect();
    let norm: f64 = weights.iter().sum();
    // Weight 0 belongs to the NEWEST bar of the window: the reference walks
    // from the newest bar backwards over a window symmetric about its centre.
    windowed(v, len, |w| {
        let acc: f64 = w.iter().rev().zip(&weights).map(|(x, wt)| x * wt).sum();
        if norm == 0.0 { NAN } else { acc / norm }
    })
}

/// Chande momentum oscillator: the signed gain/loss balance over `len` bars, in −100..100.
pub fn cmo(v: &[f64], len: usize) -> Vec<f64> {
    let d = change(v, 1);
    let up = sum(&map(&d, |x| x.max(0.0)), len);
    let dn = sum(&map(&d, |x| (-x).max(0.0)), len);
    zip(&up, &dn, |u, w| {
        if u + w == 0.0 {
            0.0
        } else {
            (100.0 * (u - w)) / (u + w)
        }
    })
}

/// Pearson correlation of two series over `len` bars.
pub fn correlation(a: &[f64], b: &[f64], len: usize) -> Vec<f64> {
    let mut out = vec![NAN; a.len()];
    if len == 0 {
        // An empty window has zero variance: the original yields 0 everywhere.
        return vec![0.0; a.len()];
    }
    for i in (len - 1)..a.len() {
        let wa = &a[i + 1 - len..=i];
        let Some(wb) = b.get(i + 1 - len..=i) else {
            continue;
        };
        if wa.iter().chain(wb).any(|x| !x.is_finite()) {
            continue;
        }
        let ma = wa.iter().sum::<f64>() / len as f64;
        let mb = wb.iter().sum::<f64>() / len as f64;
        let mut cov = 0.0;
        let mut va = 0.0;
        let mut vb = 0.0;
        for (x, y) in wa.iter().zip(wb) {
            let da = x - ma;
            let db = y - mb;
            cov += da * db;
            va += da * da;
            vb += db * db;
        }
        let denom = (va * vb).sqrt();
        out[i] = if denom == 0.0 { 0.0 } else { cov / denom };
    }
    out
}

/// Bar ordinals (0, 1, 2, …): the x axis of a regression or correlation against time.
pub fn bar_index(length: usize) -> Vec<f64> {
    (0..length).map(|i| i as f64).collect()
}

/// Symmetrically weighted moving average over 4 bars (weights 1,2,2,1 / 6).
pub fn swma(v: &[f64]) -> Vec<f64> {
    let mut out = vec![NAN; v.len()];
    for i in 3..v.len() {
        let (a, b, c, d) = (v[i - 3], v[i - 2], v[i - 1], v[i]);
        if a.is_finite() && b.is_finite() && c.is_finite() && d.is_finite() {
            out[i] = (a + 2.0 * b + 2.0 * c + d) / 6.0;
        }
    }
    out
}

/// Percent of the trailing `len` values the current value is at or above (0..100).
pub fn percent_rank(v: &[f64], len: usize) -> Vec<f64> {
    let mut out = vec![NAN; v.len()];
    for i in len..v.len() {
        let x = v[i];
        if !x.is_finite() {
            continue;
        }
        let window = &v[i - len..i];
        if window.iter().any(|y| !y.is_finite()) {
            continue;
        }
        let below = window.iter().filter(|&&y| y <= x).count();
        out[i] = (100.0 * below as f64) / len as f64;
    }
    out
}

/// Cumulative running sum; a NaN term is skipped (the accumulation holds).
pub fn cum_sum(v: &[f64]) -> Vec<f64> {
    let mut out = vec![NAN; v.len()];
    let mut acc = 0.0;
    let mut seen = false;
    for i in 0..v.len() {
        let x = v[i];
        if x.is_finite() {
            acc += x;
            seen = true;
        }
        if seen {
            out[i] = acc;
        }
    }
    out
}

/// Fixed-value array aligned to a length.
pub fn fill(len: usize, value: f64) -> Vec<f64> {
    vec![value; len]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-10, "{a} != {b}");
    }

    #[test]
    fn sma_warms_up_over_the_window_then_averages() {
        let out = sma(&[1.0, 2.0, 3.0, 4.0, 5.0], 3);
        assert!(out[0].is_nan());
        assert!(out[1].is_nan());
        assert_eq!(&out[2..], &[2.0, 3.0, 4.0]);
    }

    #[test]
    fn ema_seeds_with_the_sma_of_the_first_window_and_recurses() {
        let out = ema(&[1.0, 2.0, 3.0, 4.0], 2);
        assert!(out[0].is_nan());
        close(out[1], 1.5);
        close(out[2], 2.5);
        close(out[3], 3.5);
    }

    #[test]
    fn rma_uses_the_wilder_alpha() {
        let out = rma(&[1.0, 2.0, 3.0, 4.0], 2);
        close(out[1], 1.5);
        close(out[2], 2.25);
        close(out[3], 3.125);
    }

    #[test]
    fn wma_weights_the_newest_values_heaviest() {
        close(wma(&[1.0, 2.0, 3.0], 3)[2], 14.0 / 6.0);
    }

    #[test]
    fn stdev_is_the_population_deviation() {
        close(stdev(&[2.0, 4.0], 2)[1], 1.0);
    }

    #[test]
    fn rsi_saturates_at_100_on_a_monotonic_rise() {
        let out = rsi(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 3);
        assert!((out[5] - 100.0).abs() < 1e-6);
    }

    #[test]
    fn linreg_reproduces_a_perfect_line_exactly() {
        let out = linreg(&[1.0, 2.0, 3.0, 4.0], 3, 0.0);
        close(out[2], 3.0);
        close(out[3], 4.0);
    }

    #[test]
    fn highest_and_lowest_track_the_window_extremes() {
        assert_eq!(&highest(&[1.0, 3.0, 2.0], 2)[1..], &[3.0, 3.0]);
        assert_eq!(&lowest(&[1.0, 3.0, 2.0], 2)[1..], &[1.0, 2.0]);
    }

    #[test]
    fn percent_rank_counts_trailing_values_at_or_below_the_current_one() {
        close(percent_rank(&[1.0, 2.0, 3.0, 4.0], 3)[3], 100.0);
    }

    #[test]
    fn swma_applies_the_1_2_2_1_kernel() {
        close(swma(&[1.0, 2.0, 3.0, 4.0])[3], 2.5);
    }

    #[test]
    fn cum_sum_holds_the_accumulation_across_a_nan_term() {
        assert_eq!(cum_sum(&[1.0, NAN, 2.0]), vec![1.0, 1.0, 3.0]);
    }

    #[test]
    fn windowed_ops_restart_their_warm_up_after_a_mid_stream_gap() {
        let out = sma(&[1.0, 2.0, NAN, 3.0, 4.0], 2);
        close(out[1], 1.5);
        assert!(out[2].is_nan());
        assert!(out[3].is_nan());
        close(out[4], 3.5);
    }

    #[test]
    fn change_propagates_gaps() {
        let out = change(&[1.0, NAN, 3.0], 1);
        assert!(out[1].is_nan());
        assert!(out[2].is_nan());
    }

    #[test]
    fn bars_since_extreme_counts_back_from_the_current_bar() {
        assert_eq!(bars_since_highest(&[5.0, 1.0, 2.0], 3)[2], 2.0);
        assert_eq!(bars_since_lowest(&[5.0, 1.0, 2.0], 3)[2], 1.0);
        // Ties pick the newest bar.
        assert_eq!(bars_since_highest(&[3.0, 3.0], 2)[1], 0.0);
    }

    #[test]
    fn shift_moves_values_forward_and_back() {
        assert_eq!(shift(&[1.0, 2.0, 3.0], 1)[1..], [1.0, 2.0]);
        assert!(shift(&[1.0, 2.0, 3.0], 1)[0].is_nan());
        assert_eq!(shift(&[1.0, 2.0, 3.0], -1)[..2], [2.0, 3.0]);
    }

    #[test]
    fn js_helpers_match_javascript() {
        assert!(js_max(1.0, NAN).is_nan());
        assert!(js_min(NAN, 1.0).is_nan());
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
    }

    #[test]
    fn hamming_with_a_unit_window_passes_values_through() {
        assert_eq!(
            hamming(&[1.0, 2.0], 1, HAMMING_A0, HAMMING_A1),
            vec![1.0, 2.0]
        );
        // A symmetric window over a constant series returns the constant.
        close(hamming(&[4.0, 4.0, 4.0], 3, HAMMING_A0, HAMMING_A1)[2], 4.0);
    }
}
