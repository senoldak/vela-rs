//! Clean-room tick generation for the native renderer's axes.
//!
//! - [`price_ticks`]: 1/2/5 x 10^n "nice numbers" for the price axis.
//! - [`time_ticks`]: a UTC-aligned ladder for the time axis (crypto and 24/7 friendly;
//!   DST and session-aware ticks are a later refinement).
//!
//! Every label goes through the chart's formatters (`chart.i18n.format`); the original's
//! default of the built-in English formatting is the caller's `&english_format()`.
//!
//! Scales are passed as [`AxisScale`] (the `{ min, max, log? }` shape the original takes
//! structurally); a pane's scale converts into it.
// A NaN operand must fail these checks, as the original's `!(a > b)` does.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

use crate::format::{
    CompactFormatOptions, FormatSurface, PriceFormatOptions, PriceMode, ResolvedFormatters,
    TimeStampFormatOptions, TimeTickFormatOptions, TimeTickUnit,
};
use crate::indicators::classics::math::{js_max, js_min, js_round};
use crate::intl::{civil_from_days, days_from_civil};
use crate::render::core::PriceScale;
use crate::render::core::scene_graph::{PaneAxisFormat, PctScale};
use crate::util::to_fixed;

use super::tz::display_zone;

/// The part of a price scale the tick and label functions read.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AxisScale {
    pub min: f64,
    pub max: f64,
    pub log: bool,
}

impl From<&PriceScale> for AxisScale {
    fn from(scale: &PriceScale) -> Self {
        Self {
            min: scale.min,
            max: scale.max,
            log: scale.is_log(),
        }
    }
}

impl AxisScale {
    pub fn new(min: f64, max: f64) -> Self {
        Self {
            min,
            max,
            log: false,
        }
    }

    pub fn log(min: f64, max: f64) -> Self {
        Self {
            min,
            max,
            log: true,
        }
    }
}

/// An axis tick: its price and its label.
#[derive(Clone, Debug, PartialEq)]
pub struct AxisTick {
    pub price: f64,
    pub label: String,
}

/// `Math.pow(10, n)` for the integral exponents ticks use. Up to 10^22 a power is exact,
/// and its reciprocal is then the correctly rounded `10^-n`; beyond that the library
/// `pow` is the closest available match.
fn pow10(n: f64) -> f64 {
    if n.is_finite() && n.abs() <= 22.0 {
        10f64.powi(n as i32)
    } else {
        10f64.powf(n)
    }
}

/// The 1/2/5 step for a range split into about `target` intervals.
fn nice_step(min: f64, max: f64, target: f64) -> f64 {
    let raw = (max - min) / js_max(1.0, target);
    let mag = pow10(raw.log10().floor());
    let norm = raw / mag;
    let factor = if norm < 1.5 {
        1.0
    } else if norm < 3.0 {
        2.0
    } else if norm < 7.0 {
        5.0
    } else {
        10.0
    };
    factor * mag
}

/// `Number(value.toFixed(decimals))`.
fn parse_fixed(value: f64, decimals: usize) -> f64 {
    to_fixed(value, decimals).parse::<f64>().unwrap_or(f64::NAN)
}

pub fn price_ticks(min: f64, max: f64, target: f64) -> Vec<f64> {
    if !(max > min) || !min.is_finite() || !max.is_finite() {
        return Vec::new();
    }
    let step = nice_step(min, max, target);
    let decimals = js_max(0.0, -step.log10().floor() + 1.0);
    let start = (min / step).ceil() * step;
    if !(step > 0.0) || start + step == start {
        return Vec::new();
    }
    // Count the ticks up front and index them: accumulating `v += step` can stall when the
    // float spacing changes inside the range (e.g. across a power of two).
    let count = ((max - start) / step + 1e-6).floor() + 1.0;
    if !(count > 0.0) || count > 1000.0 {
        return Vec::new();
    }
    let decimals = decimals as usize;
    (0..count as usize)
        .map(|i| parse_fixed(start + i as f64 * step, decimals))
        .collect()
}

/// Decimals to show for a price, derived from the tick step magnitude.
pub fn price_decimals(min: f64, max: f64, target: f64) -> f64 {
    if !(max > min) {
        return 2.0;
    }
    let step = nice_step(min, max, target);
    js_max(0.0, js_min(8.0, -step.log10().floor() + 1.0))
}

/// Nice ticks for a logarithmic axis. Over a wide range (about a decade or more) it places
/// 1/2/5 per decade; over a narrow range (the common zoomed view) log is close to linear,
/// so it falls back to dense linear nice-numbers, positioned via the log mapping.
pub fn log_price_ticks(min: f64, max: f64, target: f64) -> Vec<f64> {
    if min <= 0.0 || !(max > min) || !min.is_finite() || !max.is_finite() {
        return Vec::new();
    }
    if max.log10() - min.log10() < 1.1 {
        return price_ticks(min, max, target);
    }
    let mut out = Vec::new();
    let start_exp = min.log10().floor() as i64;
    let end_exp = max.log10().ceil() as i64;
    for e in start_exp..=end_exp {
        for m in [1.0, 2.0, 5.0] {
            let v = m * pow10(e as f64);
            if v >= min && v <= max {
                out.push(v);
            }
        }
    }
    out
}

/// Decimals to show for a single price value (for log axes where the step varies).
pub fn value_decimals(v: f64) -> f64 {
    let a = v.abs();
    if a >= 100.0 {
        0.0
    } else if a >= 1.0 {
        2.0
    } else if a >= 0.01 {
        4.0
    } else {
        6.0
    }
}

/// Decimal places needed to render a tick size exactly (0.01 -> 2, 0.5 -> 1, 1 -> 0).
/// Probes increasing precision until the value round-trips, so it is robust to float
/// noise and to non-power-of-10 ticks (0.25, 0.5). Capped at 8.
pub fn tick_decimals(tick: f64) -> f64 {
    if !(tick > 0.0) || !tick.is_finite() {
        return 2.0;
    }
    for d in 0..=8usize {
        if (parse_fixed(tick, d) - tick).abs() <= tick * 1e-6 {
            return d as f64;
        }
    }
    8.0
}

/// Decimals for a price-axis label, by precedence:
///  1. the exchange symbol's tick size (`mintick`) when known: the instrument's TRUE
///     precision, so it does not drift with zoom;
///  2. otherwise the zoom-derived "nice step" formula (the only source before symbol
///     metadata loads, or for offline and non-exchange data).
///
/// A result of 0 is floored to 2 so a price never renders as a bare integer.
pub fn axis_decimals(scale: &AxisScale, height_px: f64, mintick: Option<f64>) -> f64 {
    let d = match mintick {
        Some(m) if m > 0.0 => tick_decimals(m),
        _ => price_decimals(scale.min, scale.max, tick_count(height_px)),
    };
    if d == 0.0 { 2.0 } else { d }
}

/// Price-axis label density: about one tick per 50px of pane height (like LwC).
pub fn tick_count(pane_height_px: f64) -> f64 {
    js_max(2.0, js_min(16.0, js_round(pane_height_px / 50.0)))
}

/// Price ticks for a pane: log ticks if logarithmic, else height-adaptive nice numbers.
pub fn pane_ticks(scale: &AxisScale, height_px: f64) -> Vec<f64> {
    if scale.log {
        log_price_ticks(scale.min, scale.max, tick_count(height_px))
    } else {
        price_ticks(scale.min, scale.max, tick_count(height_px))
    }
}

/// A price as a percent change vs baseline.
fn to_pct(price: f64, baseline: f64) -> f64 {
    (price / baseline - 1.0) * 100.0
}

/// A price as a value indexed to 100 at the baseline (`baseline` reads exactly 100).
fn to_index(price: f64, baseline: f64) -> f64 {
    (price / baseline) * 100.0
}

/// Format a percent change with a sign (`+2.34%` / `-1.20%`).
pub fn format_pct(pct: f64, fmt: &ResolvedFormatters, surface: FormatSurface) -> String {
    fmt.price(
        pct,
        &PriceFormatOptions {
            surface: Some(surface),
            mode: Some(PriceMode::Percent),
            ..PriceFormatOptions::new(2.0)
        },
    )
}

/// Format an indexed-to-100 value (plain number, two decimals, e.g. `103.45`).
pub fn format_index(idx: f64, fmt: &ResolvedFormatters, surface: FormatSurface) -> String {
    fmt.price(
        idx,
        &PriceFormatOptions {
            surface: Some(surface),
            mode: Some(PriceMode::Indexed),
            ..PriceFormatOptions::new(2.0)
        },
    )
}

/// Abbreviate a magnitude with a K/M/B suffix (e.g. `1.2M`), for volume axes, where a raw
/// `1234567.00` reads poorly. Small values keep up to two decimals, dropping trailing
/// zeros; a bare `0` stays `0`.
pub fn format_compact_value(v: f64, fmt: &ResolvedFormatters, surface: FormatSurface) -> String {
    fmt.compact(
        v,
        &CompactFormatOptions {
            surface: Some(surface),
            decimals: Some(2.0),
            trim_zeros: Some(true),
        },
    )
}

/// Axis ticks for a pane as `{ price, label }`: the single source both the gridlines
/// (which need the pixel position from `price`) and the axis labels consume, so they
/// always agree. In percent / indexed mode (`pct` set) the ticks are nice ROUND percents
/// or index values mapped back to price; otherwise nice round prices. Linear only for
/// percent/indexed (the affine price/percent map keeps the geometry identical to a linear
/// price axis). [`PaneAxisFormat::Volume`] abbreviates the labels (K/M/B), used for a
/// volume-only pane. [`PaneAxisFormat::NoAxis`] yields no ticks at all: an unscaled pane
/// (content not value-mapped) draws neither labels nor the gridlines that would ride them.
pub fn pane_axis_ticks(
    scale: &AxisScale,
    height_px: f64,
    pct: Option<PctScale>,
    mintick: Option<f64>,
    format: Option<PaneAxisFormat>,
    fmt: &ResolvedFormatters,
) -> Vec<AxisTick> {
    if format == Some(PaneAxisFormat::NoAxis) {
        return Vec::new();
    }
    if let Some(PctScale { baseline, indexed }) = pct {
        let lo = js_min(scale.min, scale.max);
        let hi = js_max(scale.min, scale.max);
        let count = tick_count(height_px);
        return if indexed {
            let (i_lo, i_hi) = (to_index(lo, baseline), to_index(hi, baseline));
            price_ticks(i_lo, i_hi, count)
                .into_iter()
                .map(|idx| AxisTick {
                    price: (baseline * idx) / 100.0,
                    label: format_index(idx, fmt, FormatSurface::PriceAxis),
                })
                .collect()
        } else {
            let (p_lo, p_hi) = (to_pct(lo, baseline), to_pct(hi, baseline));
            price_ticks(p_lo, p_hi, count)
                .into_iter()
                .map(|p| AxisTick {
                    price: baseline * (1.0 + p / 100.0),
                    label: format_pct(p, fmt, FormatSurface::PriceAxis),
                })
                .collect()
        };
    }
    if format == Some(PaneAxisFormat::Volume) {
        return pane_ticks(scale, height_px)
            .into_iter()
            .map(|price| AxisTick {
                price,
                label: format_compact_value(price, fmt, FormatSurface::PriceAxis),
            })
            .collect();
    }
    pane_ticks(scale, height_px)
        .into_iter()
        .map(|price| AxisTick {
            price,
            label: format_price_label(
                scale,
                height_px,
                price,
                mintick,
                fmt,
                FormatSurface::PriceAxis,
            ),
        })
        .collect()
}

/// Format a single price for an axis chip in the pane's mode (percent / indexed vs
/// absolute; a volume pane abbreviates with K/M/B; an unscaled pane yields `""`, so
/// callers skip the chip).
#[allow(clippy::too_many_arguments)]
pub fn format_axis_value(
    scale: &AxisScale,
    height_px: f64,
    value: f64,
    pct: Option<PctScale>,
    mintick: Option<f64>,
    format: Option<PaneAxisFormat>,
    fmt: &ResolvedFormatters,
    surface: FormatSurface,
) -> String {
    if format == Some(PaneAxisFormat::NoAxis) {
        return String::new();
    }
    if let Some(p) = pct {
        return if p.indexed {
            format_index(to_index(value, p.baseline), fmt, surface)
        } else {
            format_pct(to_pct(value, p.baseline), fmt, surface)
        };
    }
    if format == Some(PaneAxisFormat::Volume) {
        return format_compact_value(value, fmt, surface);
    }
    format_price_label(scale, height_px, value, mintick, fmt, surface)
}

/// Format a price for an axis/chip. Decimals come from [`axis_decimals`] (exchange tick
/// size when known, else the zoom-derived formula, floored at 2), EXCEPT on wide
/// (multi-decade) log scales which use per-value decimals so tiny and huge prices both
/// read well. Shared by the price axis, the current-price line and the crosshair so they
/// always agree.
pub fn format_price_label(
    scale: &AxisScale,
    height_px: f64,
    value: f64,
    mintick: Option<f64>,
    fmt: &ResolvedFormatters,
    surface: FormatSurface,
) -> String {
    let wide_log = scale.log && scale.max.log10() - scale.min.log10() >= 1.1;
    let decimals = if wide_log {
        let d = value_decimals(value);
        if d == 0.0 { 2.0 } else { d }
    } else {
        axis_decimals(scale, height_px, mintick)
    };
    fmt.price(
        value,
        &PriceFormatOptions {
            surface: Some(surface),
            mintick,
            mode: Some(PriceMode::Price),
            ..PriceFormatOptions::new(decimals)
        },
    )
}

/// One time-axis tick.
#[derive(Clone, Debug, PartialEq)]
pub struct TimeTick {
    pub time: f64,
    pub label: String,
    pub major: bool,
}

const SEC: f64 = 1000.0;
const MIN: f64 = 60.0 * SEC;
const HOUR: f64 = 60.0 * MIN;
const DAY: f64 = 24.0 * HOUR;
const WEEK: f64 = 7.0 * DAY;
const MONTH: f64 = 30.0 * DAY;
const YEAR: f64 = 365.0 * DAY;

const STEP_LADDER: [f64; 19] = [
    SEC,
    5.0 * SEC,
    15.0 * SEC,
    30.0 * SEC,
    MIN,
    5.0 * MIN,
    15.0 * MIN,
    30.0 * MIN,
    HOUR,
    2.0 * HOUR,
    4.0 * HOUR,
    6.0 * HOUR,
    12.0 * HOUR,
    DAY,
    2.0 * DAY,
    WEEK,
    MONTH,
    3.0 * MONTH,
    YEAR,
];

/// The crosshair's time-axis chip: `Sun 30 Aug '26 19:00`. Always the full calendar date
/// (weekday, day, month, two-digit year) so the stamp is unambiguous however far the view
/// is scrolled; the wall-clock time is appended only when bars are intraday: on a daily
/// or coarser bar the hh:mm would just echo the bar's open and add noise. A daily or
/// coarser bar reads its UTC date in every zone (see [`display_zone`]).
pub fn format_time_stamp(
    ms: f64,
    time_zone: &str,
    bar_interval_ms: f64,
    fmt: &ResolvedFormatters,
    surface: FormatSurface,
) -> String {
    fmt.time_stamp(
        ms,
        &TimeStampFormatOptions {
            surface: Some(surface),
            with_time: Some(bar_interval_ms < DAY),
            time_zone: Some(display_zone(time_zone, bar_interval_ms).to_string()),
        },
    )
}

fn pick_step(target_ms: f64) -> f64 {
    STEP_LADDER
        .iter()
        .copied()
        .find(|step| *step >= target_ms)
        .unwrap_or(YEAR)
}

/// The UTC hour, minute and day of month of a time value (`getUTCHours`, `getUTCMinutes`,
/// `getUTCDate`).
fn utc_hour_minute_date(ms: f64) -> (i64, i64, u32) {
    let ms = ms as i64;
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (_, _, day) = civil_from_days(days);
    (rem / 3_600_000, rem / 60_000 % 60, day)
}

/// UTC-aligned tick ladder for the time axis. `offset_ms` shifts alignment and labels into
/// a chosen time zone (0 reads UTC): ticks land on local midnights and hours and the
/// returned `time` is the REAL epoch ms (zoned alignment undone) so the X mapping stays
/// correct.
pub fn time_ticks(
    from_ms: f64,
    to_ms: f64,
    target: f64,
    offset_ms: f64,
    fmt: &ResolvedFormatters,
) -> Vec<TimeTick> {
    let span = to_ms - from_ms;
    if !(span > 0.0) || !from_ms.is_finite() || !to_ms.is_finite() || !offset_ms.is_finite() {
        return Vec::new();
    }
    let step = pick_step(span / js_max(1.0, target));
    // Work in zoned space so `first` and the steps align to local wall-clock, then undo the
    // shift on each tick's `time` for pixel placement on the real (UTC) axis.
    let z_from = from_ms + offset_ms;
    let z_to = to_ms + offset_ms;
    if step >= MONTH {
        let months = if step >= YEAR {
            12
        } else if step >= 3.0 * MONTH {
            3
        } else {
            1
        };
        return calendar_ticks(z_from, z_to, months, offset_ms, fmt);
    }
    let first = (z_from / step).ceil() * step;
    let clock_unit = if step < MIN {
        TimeTickUnit::Second
    } else if step < HOUR {
        TimeTickUnit::Minute
    } else {
        TimeTickUnit::Hour
    };
    let mut out = Vec::new();
    let mut zt = first;
    while zt <= z_to {
        let t = zt - offset_ms;
        let (hours, minutes, date) = utc_hour_minute_date(zt);
        let (unit, major) = if step < DAY {
            let major = hours == 0 && minutes == 0;
            (if major { TimeTickUnit::Day } else { clock_unit }, major)
        } else if step < YEAR {
            let major = date == 1;
            (
                if major {
                    TimeTickUnit::Month
                } else {
                    TimeTickUnit::Day
                },
                major,
            )
        } else {
            (TimeTickUnit::Year, true)
        };
        out.push(TimeTick {
            time: t,
            label: tick_label(fmt, t, unit, major, offset_ms),
            major,
        });
        zt += step;
    }
    out
}

fn tick_label(
    fmt: &ResolvedFormatters,
    time: f64,
    unit: TimeTickUnit,
    major: bool,
    offset_ms: f64,
) -> String {
    fmt.time_tick(
        time,
        &TimeTickFormatOptions {
            surface: Some(FormatSurface::TimeAxis),
            offset_ms: Some(offset_ms),
            ..TimeTickFormatOptions::new(unit, major)
        },
    )
}

/// The original's `Date.UTC(Math.floor(idx / 12), idx % 12, 1)` for a month index counted
/// from year 0, with both of its JavaScript quirks, so a view reaching before year 100
/// ticks where the original's does: `%` keeps the sign of `idx` (a negative month, which
/// `Date.UTC` then carries into the year before), and `Date.UTC` reads the years 0 to 99
/// as 1900 to 1999.
fn first_of_month_ms(idx: i64) -> f64 {
    let (mut year, month) = (idx.div_euclid(12), idx % 12);
    if (0..=99).contains(&year) {
        year += 1900;
    }
    let months = year * 12 + month;
    days_from_civil(months.div_euclid(12), months.rem_euclid(12) as u32 + 1, 1) as f64 * DAY
}

/// Month, quarter and year ticks on real calendar boundaries (the first of the month, in
/// zoned space). Fixed 30/365-day steps drift off the calendar by a day per leap year,
/// which puts each year label weeks before the January it names.
fn calendar_ticks(
    z_from: f64,
    z_to: f64,
    months: i64,
    offset_ms: f64,
    fmt: &ResolvedFormatters,
) -> Vec<TimeTick> {
    if z_from.abs() > 8.64e15 || z_to.abs() > 8.64e15 {
        return Vec::new();
    }
    let (year, month, _) = civil_from_days((z_from.floor() as i64).div_euclid(86_400_000));
    let mut idx = year * 12 + i64::from(month) - 1;
    if first_of_month_ms(idx) < z_from {
        idx += 1;
    }
    idx = (idx as f64 / months as f64).ceil() as i64 * months;
    let unit = if months == 12 {
        TimeTickUnit::Year
    } else {
        TimeTickUnit::Month
    };
    let mut out = Vec::new();
    let mut zt = first_of_month_ms(idx);
    while zt <= z_to {
        let time = zt - offset_ms;
        out.push(TimeTick {
            time,
            label: tick_label(fmt, time, unit, true, offset_ms),
            major: true,
        });
        idx += months;
        zt = first_of_month_ms(idx);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::english_format;

    const H: f64 = 3_600_000.0;

    fn utc(y: i64, m0: u32, d: u32, h: u32, min: u32) -> f64 {
        days_from_civil(y, m0 + 1, d) as f64 * DAY + f64::from(h) * HOUR + f64::from(min) * MIN
    }

    fn price(scale: AxisScale, h: f64, v: f64, mintick: Option<f64>) -> String {
        format_price_label(
            &scale,
            h,
            v,
            mintick,
            &english_format(),
            FormatSurface::PriceAxis,
        )
    }

    fn axis_value(
        scale: AxisScale,
        v: f64,
        pct: Option<PctScale>,
        mintick: Option<f64>,
        format: Option<PaneAxisFormat>,
    ) -> String {
        format_axis_value(
            &scale,
            600.0,
            v,
            pct,
            mintick,
            format,
            &english_format(),
            FormatSurface::PriceAxis,
        )
    }

    fn pct(indexed: bool) -> Option<PctScale> {
        Some(PctScale {
            baseline: 100.0,
            indexed,
        })
    }

    // native ticks: logPriceTicks

    #[test]
    fn log_ticks_place_1_2_5_per_decade_over_a_wide_range() {
        let ticks = log_price_ticks(1.0, 1000.0, 6.0);
        for want in [1.0, 2.0, 5.0, 10.0, 100.0, 1000.0] {
            assert!(ticks.contains(&want), "{want} missing from {ticks:?}");
        }
        assert!(ticks.windows(2).all(|w| w[1] > w[0]));
    }

    #[test]
    fn log_ticks_fall_back_to_dense_linear_numbers_over_a_sub_decade_range() {
        let ticks = log_price_ticks(73.0, 179.0, 12.0);
        assert!(ticks.len() >= 4);
        assert_eq!(ticks, price_ticks(73.0, 179.0, 12.0));
    }

    #[test]
    fn log_ticks_are_empty_for_a_non_positive_or_degenerate_range() {
        assert!(log_price_ticks(0.0, 100.0, 6.0).is_empty());
        assert!(log_price_ticks(-5.0, 5.0, 6.0).is_empty());
        assert!(log_price_ticks(100.0, 100.0, 6.0).is_empty());
        assert!(log_price_ticks(1.0, f64::INFINITY, 6.0).is_empty());
        assert!(log_price_ticks(f64::NAN, 10.0, 6.0).is_empty());
    }

    // native ticks: priceTicks

    #[test]
    fn no_ticks_when_the_range_is_a_sub_ulp_sliver_the_step_cannot_cross() {
        assert!(price_ticks(24.43, 24.430000000000003, 6.0).is_empty());
    }

    #[test]
    fn slivers_that_straddle_a_power_of_two_terminate() {
        let ranges = [
            (31.99999999999999, 32.0),
            (1.999999999999998, 2.0),
            (3.999999999999996, 4.0),
            (1023.999999999999, 1024.0),
        ];
        for (below, p) in ranges {
            let ticks = price_ticks(below, p, 6.0);
            assert!(ticks.len() <= 1000);
            for t in ticks {
                assert!(t >= below - 1e-9 && t <= p + 1e-9);
            }
        }
    }

    #[test]
    fn a_normal_range_gives_the_regular_ladder() {
        assert_eq!(
            price_ticks(0.0, 10.0, 6.0),
            vec![0.0, 2.0, 4.0, 6.0, 8.0, 10.0]
        );
    }

    #[test]
    fn invalid_ranges_give_no_ticks() {
        assert!(price_ticks(5.0, 5.0, 6.0).is_empty());
        assert!(price_ticks(10.0, 0.0, 6.0).is_empty());
        assert!(price_ticks(f64::NAN, 10.0, 6.0).is_empty());
        assert!(price_ticks(0.0, f64::INFINITY, 6.0).is_empty());
    }

    #[test]
    fn a_flat_or_tiny_range_still_terminates() {
        assert!(price_ticks(100.0, 100.0 + 1e-12, 6.0).len() <= 1000);
        // A huge tick count is capped at 1000 and answers none.
        assert!(price_ticks(0.0, 1e12, 100_000.0).is_empty());
    }

    #[test]
    fn price_decimals_follow_the_step() {
        assert_eq!(price_decimals(0.0, 10.0, 6.0), 1.0);
        assert_eq!(price_decimals(58000.0, 67000.0, 6.0), 0.0);
        assert_eq!(price_decimals(5.0, 5.0, 6.0), 2.0);
        assert_eq!(price_decimals(0.0, 1e-12, 6.0), 8.0);
    }

    // native ticks: valueDecimals, tickDecimals, axisDecimals

    #[test]
    fn value_decimals_shrink_as_magnitude_grows() {
        assert_eq!(value_decimals(250.0), 0.0);
        assert_eq!(value_decimals(12.5), 2.0);
        assert_eq!(value_decimals(0.05), 4.0);
        assert_eq!(value_decimals(0.0001), 6.0);
    }

    #[test]
    fn tick_decimals_count_the_decimals_a_tick_size_needs() {
        assert_eq!(tick_decimals(1.0), 0.0);
        assert_eq!(tick_decimals(0.1), 1.0);
        assert_eq!(tick_decimals(0.01), 2.0);
        assert_eq!(tick_decimals(0.001), 3.0);
        assert_eq!(tick_decimals(0.00001), 5.0);
        assert_eq!(tick_decimals(0.5), 1.0);
        assert_eq!(tick_decimals(0.25), 2.0);
        assert_eq!(tick_decimals(0.05), 2.0);
    }

    #[test]
    fn tick_decimals_fall_back_to_2_for_a_missing_or_invalid_tick() {
        assert_eq!(tick_decimals(0.0), 2.0);
        assert_eq!(tick_decimals(-1.0), 2.0);
        assert_eq!(tick_decimals(f64::NAN), 2.0);
        assert_eq!(tick_decimals(f64::INFINITY), 2.0);
        assert_eq!(tick_decimals(1e-12), 8.0);
    }

    #[test]
    fn axis_decimals_use_the_tick_size_when_known() {
        let wide = AxisScale::new(58000.0, 67000.0);
        assert_eq!(axis_decimals(&wide, 600.0, Some(0.01)), 2.0);
        assert_eq!(axis_decimals(&wide, 600.0, Some(0.001)), 3.0);
        assert_eq!(
            axis_decimals(&AxisScale::new(0.5, 0.9), 600.0, Some(0.00001)),
            5.0
        );
    }

    #[test]
    fn axis_decimals_fall_back_to_the_zoom_formula() {
        let wide = AxisScale::new(58000.0, 67000.0);
        assert_eq!(axis_decimals(&wide, 600.0, None), 2.0); // formula gives 0, floored to 2
        let narrow = AxisScale::new(100.0, 101.0);
        assert_eq!(
            axis_decimals(&narrow, 600.0, Some(0.0)),
            axis_decimals(&narrow, 600.0, None)
        );
    }

    #[test]
    fn axis_decimals_floor_a_zero_result_at_2() {
        let wide = AxisScale::new(58000.0, 67000.0);
        assert_eq!(axis_decimals(&wide, 600.0, Some(1.0)), 2.0);
        assert_eq!(axis_decimals(&wide, 600.0, None), 2.0);
    }

    #[test]
    fn tick_count_is_one_per_fifty_pixels_within_2_to_16() {
        assert_eq!(tick_count(300.0), 6.0);
        assert_eq!(tick_count(0.0), 2.0);
        assert_eq!(tick_count(5000.0), 16.0);
    }

    // native ticks: formatPriceLabel, formatAxisValue

    #[test]
    fn labels_render_the_exchange_precision_when_a_tick_size_is_supplied() {
        assert_eq!(
            price(AxisScale::new(58000.0, 67000.0), 600.0, 58732.0, Some(0.01)),
            "58732.00"
        );
        assert_eq!(
            price(AxisScale::new(58000.0, 67000.0), 600.0, 58732.0, None),
            "58732.00"
        );
        assert_eq!(
            axis_value(AxisScale::new(0.5, 0.9), 0.6234, None, Some(0.0001), None),
            "0.6234"
        );
    }

    #[test]
    fn percent_mode_ignores_the_tick_size() {
        assert_eq!(
            axis_value(
                AxisScale::new(100.0, 110.0),
                105.0,
                pct(false),
                Some(0.01),
                None
            ),
            "+5.00%"
        );
    }

    #[test]
    fn indexed_mode_ignores_the_tick_size() {
        assert_eq!(
            axis_value(
                AxisScale::new(100.0, 110.0),
                105.0,
                pct(true),
                Some(0.01),
                None
            ),
            "105.00"
        );
    }

    #[test]
    fn a_wide_log_scale_uses_per_value_decimals() {
        let scale = AxisScale::log(0.001, 1000.0);
        assert_eq!(price(scale, 600.0, 250.0, None), "250.00");
        assert_eq!(price(scale, 600.0, 0.05, None), "0.0500");
        assert_eq!(price(scale, 600.0, 0.0001, None), "0.000100");
    }

    // native ticks: formatTimeStamp

    #[test]
    fn the_time_stamp_reads_weekday_day_month_year_and_time_on_intraday_bars() {
        let fmt = english_format();
        let s = FormatSurface::Crosshair;
        assert_eq!(
            format_time_stamp(utc(2026, 7, 30, 19, 0), "UTC", H, &fmt, s),
            "Sun 30 Aug '26 19:00"
        );
        assert_eq!(
            format_time_stamp(utc(2031, 0, 3, 9, 5), "UTC", 60_000.0, &fmt, s),
            "Fri 3 Jan '31 09:05"
        );
    }

    #[test]
    fn the_time_stamp_drops_the_time_on_daily_and_coarser_bars() {
        let fmt = english_format();
        let s = FormatSurface::Crosshair;
        let t = utc(2026, 7, 30, 19, 0);
        assert_eq!(format_time_stamp(t, "UTC", DAY, &fmt, s), "Sun 30 Aug '26");
        assert_eq!(
            format_time_stamp(t, "UTC", 7.0 * DAY, &fmt, s),
            "Sun 30 Aug '26"
        );
        assert_eq!(
            format_time_stamp(t, "UTC", 12.0 * H, &fmt, s),
            "Sun 30 Aug '26 19:00"
        );
    }

    #[test]
    fn the_time_stamp_zero_pads_the_year_and_follows_the_zone() {
        let fmt = english_format();
        let s = FormatSurface::Crosshair;
        let t = utc(2005, 11, 31, 23, 30);
        assert_eq!(
            format_time_stamp(t, "UTC", H, &fmt, s),
            "Sat 31 Dec '05 23:30"
        );
        // 23:30 UTC on Dec 31 is already Jan 1 in Tokyo (UTC+9): date, weekday and year
        // roll. A fixed-offset id stands in for `Asia/Tokyo`, which the built-in backend
        // has no rules for.
        assert_eq!(
            format_time_stamp(t, "+09:00", H, &fmt, s),
            "Sun 1 Jan '06 08:30"
        );
    }

    #[test]
    fn an_unknown_bar_interval_is_treated_as_intraday() {
        let t = utc(2026, 7, 30, 19, 0);
        assert_eq!(
            format_time_stamp(t, "UTC", 0.0, &english_format(), FormatSurface::Crosshair),
            "Sun 30 Aug '26 19:00"
        );
    }

    // coarse-bar-dates

    const WEEK_MS: f64 = 7.0 * DAY;
    const MONTH_MS: f64 = 30.0 * DAY;
    // Intraday zones are fixed-offset ids (see above).
    const ZONES: [&str; 5] = ["Etc/GMT+4", "+09:00", "UTC", "Etc/UTC", "Etc/GMT+5"];

    #[test]
    fn a_daily_bar_reads_its_utc_date_in_every_display_zone() {
        let friday = utc(2026, 9, 23, 0, 0);
        for zone in ZONES {
            assert_eq!(
                format_time_stamp(
                    friday,
                    zone,
                    DAY,
                    &english_format(),
                    FormatSurface::Crosshair
                ),
                "Fri 23 Oct '26",
                "{zone}"
            );
        }
    }

    #[test]
    fn custom_day_week_and_month_counts_follow_the_same_rule() {
        let friday = utc(2026, 9, 23, 0, 0);
        for interval in [2.0 * DAY, 3.0 * DAY, 2.0 * WEEK_MS, 3.0 * MONTH_MS] {
            assert_eq!(
                format_time_stamp(
                    friday,
                    "Etc/GMT+4",
                    interval,
                    &english_format(),
                    FormatSurface::Crosshair
                ),
                "Fri 23 Oct '26"
            );
        }
    }

    #[test]
    fn weekly_and_monthly_bars_keep_their_date_in_every_zone() {
        let fmt = english_format();
        let s = FormatSurface::Crosshair;
        for zone in ZONES {
            assert_eq!(
                format_time_stamp(utc(2026, 9, 19, 0, 0), zone, WEEK_MS, &fmt, s),
                "Mon 19 Oct '26"
            );
            assert_eq!(
                format_time_stamp(utc(2026, 9, 1, 0, 0), zone, MONTH_MS, &fmt, s),
                "Thu 1 Oct '26"
            );
        }
    }

    #[test]
    fn intraday_bars_still_convert_into_the_display_zone() {
        let fmt = english_format();
        let s = FormatSurface::Crosshair;
        assert_eq!(
            format_time_stamp(utc(2026, 9, 23, 3, 0), "Etc/GMT+4", H, &fmt, s),
            "Thu 22 Oct '26 23:00"
        );
        assert_eq!(
            format_time_stamp(utc(2026, 9, 23, 0, 0), "Etc/GMT+4", 12.0 * H, &fmt, s),
            "Thu 22 Oct '26 20:00"
        );
        assert_eq!(
            format_time_stamp(utc(2026, 9, 23, 3, 0), "Etc/GMT+5", H, &fmt, s),
            "Thu 22 Oct '26 22:00"
        );
    }

    #[test]
    fn day_ticks_of_a_daily_chart_sit_at_the_bars_utc_midnights() {
        // Display zone New York: a daily chart reads UTC, so the offset is 0.
        let from = utc(2026, 9, 19, 0, 0);
        let to = utc(2026, 9, 28, 0, 0);
        let ticks = time_ticks(from, to, 9.0, 0.0, &english_format());
        for d in [19u32, 20, 21, 22, 23, 26, 27] {
            let tick = ticks.iter().find(|t| t.label == format!("Oct {d}"));
            assert_eq!(tick.map(|t| t.time), Some(utc(2026, 9, d, 0, 0)), "Oct {d}");
        }
    }

    // native-axis-bundle: percent ticks

    #[test]
    fn format_pct_signs_and_fixes_to_two_decimals() {
        let fmt = english_format();
        let s = FormatSurface::PriceAxis;
        assert_eq!(format_pct(2.345, &fmt, s), "+2.35%");
        assert_eq!(format_pct(-1.2, &fmt, s), "-1.20%");
        assert_eq!(format_pct(0.0, &fmt, s), "+0.00%");
    }

    #[test]
    fn format_axis_value_is_absolute_without_a_descriptor() {
        let scale = AxisScale::new(90.0, 110.0);
        assert_eq!(axis_value(scale, 100.0, None, None, None), "100.0");
        assert_eq!(axis_value(scale, 110.0, pct(false), None, None), "+10.00%");
        assert_eq!(axis_value(scale, 100.0, pct(false), None, None), "+0.00%");
        assert_eq!(axis_value(scale, 110.0, pct(true), None, None), "110.00");
        assert_eq!(axis_value(scale, 100.0, pct(true), None, None), "100.00");
    }

    #[test]
    fn price_mode_ticks_are_round_prices_and_percent_ticks_stay_in_range() {
        let fmt = english_format();
        let scale = AxisScale::new(90.0, 110.0);
        let price = pane_axis_ticks(&scale, 300.0, None, None, None, &fmt);
        assert!(!price.is_empty());
        for t in &price {
            assert!(!t.label.contains('%'));
            assert!((t.label.parse::<f64>().unwrap() - t.price).abs() < 1e-9);
        }
        let pct = pane_axis_ticks(&scale, 300.0, pct(false), None, None, &fmt);
        assert!(!pct.is_empty());
        for t in &pct {
            assert!(t.label.ends_with('%'));
            assert!(t.price >= 90.0 - 1e-9 && t.price <= 110.0 + 1e-9);
        }
    }

    #[test]
    fn percent_ticks_map_back_to_price_through_the_baseline() {
        let ticks = pane_axis_ticks(
            &AxisScale::new(95.0, 105.0),
            300.0,
            pct(false),
            None,
            None,
            &english_format(),
        );
        let t = ticks.iter().find(|t| t.label == "+0.00%").unwrap();
        assert!((t.price - 100.0).abs() < 1e-9);
    }

    #[test]
    fn an_unscaled_pane_yields_no_ticks_and_an_empty_chip_label() {
        let fmt = english_format();
        let scale = AxisScale::new(90.0, 110.0);
        let none = Some(PaneAxisFormat::NoAxis);
        assert!(pane_axis_ticks(&scale, 300.0, None, None, none, &fmt).is_empty());
        // `none` wins over a percent descriptor.
        assert!(pane_axis_ticks(&scale, 300.0, pct(false), None, none, &fmt).is_empty());
        assert_eq!(axis_value(scale, 100.0, None, None, none), "");
    }

    #[test]
    fn indexed_ticks_are_plain_numbers_centered_on_100_at_the_baseline() {
        let ticks = pane_axis_ticks(
            &AxisScale::new(95.0, 105.0),
            300.0,
            pct(true),
            None,
            None,
            &english_format(),
        );
        assert!(!ticks.is_empty());
        for t in &ticks {
            assert!(!t.label.contains('%'));
            assert!(t.price >= 95.0 - 1e-9 && t.price <= 105.0 + 1e-9);
            assert!((t.price - t.label.parse::<f64>().unwrap()).abs() < 1e-9);
        }
        let at100 = ticks.iter().find(|t| t.label == "100.00").unwrap();
        assert!((at100.price - 100.0).abs() < 1e-9);
    }

    #[test]
    fn volume_ticks_are_abbreviated() {
        let ticks = pane_axis_ticks(
            &AxisScale::new(0.0, 3_000_000.0),
            300.0,
            None,
            None,
            Some(PaneAxisFormat::Volume),
            &english_format(),
        );
        assert!(ticks.iter().any(|t| t.label.ends_with('M')));
    }

    // native-axis-bundle: time ticks

    #[test]
    fn a_zone_offset_shifts_the_real_tick_time_so_labels_align_to_local_midnight() {
        let fmt = english_format();
        let from = utc(2024, 0, 1, 0, 0);
        let to = utc(2024, 0, 4, 0, 0);
        let is_day = |l: &str| {
            l.len() >= 5 && l.as_bytes()[3] == b' ' && l.as_bytes()[0].is_ascii_uppercase()
        };
        let ny = time_ticks(from, to, 4.0, -5.0 * H, &fmt);
        let ny_day = ny.iter().find(|t| is_day(&t.label)).unwrap();
        assert_eq!(ny_day.time.rem_euclid(DAY), 5.0 * H);
        let utc_ticks = time_ticks(from, to, 4.0, 0.0, &fmt);
        let utc_day = utc_ticks.iter().find(|t| is_day(&t.label)).unwrap();
        assert_eq!(utc_day.time.rem_euclid(DAY), 0.0);
    }

    #[test]
    fn year_ticks_land_on_january_1_and_carry_that_year() {
        let ticks = time_ticks(
            utc(2016, 0, 4, 0, 0),
            utc(2026, 8, 28, 0, 0),
            8.0,
            0.0,
            &english_format(),
        );
        let labels: Vec<&str> = ticks.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "2017", "2018", "2019", "2020", "2021", "2022", "2023", "2024", "2025", "2026"
            ]
        );
        for t in &ticks {
            let year: i64 = t.label.parse().unwrap();
            assert_eq!(t.time, utc(year, 0, 1, 0, 0));
            assert!(t.major);
        }
    }

    #[test]
    fn quarter_ticks_land_on_the_first_of_jan_apr_jul_oct() {
        let ticks = time_ticks(
            utc(2025, 10, 15, 0, 0),
            utc(2026, 8, 28, 0, 0),
            8.0,
            0.0,
            &english_format(),
        );
        assert!(!ticks.is_empty());
        for t in ticks {
            let (_, month, day) = civil_from_days((t.time / DAY) as i64);
            assert_eq!(day, 1);
            assert_eq!((month - 1) % 3, 0);
        }
    }

    #[test]
    fn month_ticks_land_on_the_first_of_each_month() {
        let ticks = time_ticks(
            utc(2026, 0, 10, 0, 0),
            utc(2026, 6, 20, 0, 0),
            8.0,
            0.0,
            &english_format(),
        );
        let labels: Vec<&str> = ticks.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, ["Feb", "Mar", "Apr", "May", "Jun", "Jul"]);
    }

    #[test]
    fn calendar_ticks_follow_the_zone_offset() {
        let ticks = time_ticks(
            utc(2016, 0, 4, 0, 0),
            utc(2026, 8, 28, 0, 0),
            8.0,
            -5.0 * H,
            &english_format(),
        );
        assert_eq!(ticks[0].label, "2017");
        assert_eq!(ticks[0].time, utc(2017, 0, 1, 0, 0) + 5.0 * H);
    }

    #[test]
    fn degenerate_time_ranges_give_no_ticks() {
        let fmt = english_format();
        assert!(time_ticks(10.0, 10.0, 8.0, 0.0, &fmt).is_empty());
        assert!(time_ticks(10.0, 5.0, 8.0, 0.0, &fmt).is_empty());
        assert!(time_ticks(f64::NAN, 5.0, 8.0, 0.0, &fmt).is_empty());
        assert!(time_ticks(0.0, f64::INFINITY, 8.0, 0.0, &fmt).is_empty());
    }

    #[test]
    fn midnight_ticks_are_major_on_intraday_ladders() {
        let fmt = english_format();
        let from = utc(2024, 0, 1, 0, 0);
        let ticks = time_ticks(from, from + 40_000.0, 8.0, 0.0, &fmt);
        assert!(ticks.len() >= 4);
        assert!(ticks[0].major);
        let hours = time_ticks(from - 6.0 * H, from + 6.0 * H, 6.0, 0.0, &fmt);
        assert_eq!(hours.iter().filter(|t| t.major).count(), 1);
    }
}
