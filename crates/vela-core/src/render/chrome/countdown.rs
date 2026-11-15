// A NaN operand must fail these checks, as the original's `!(a > b)` does.
#![allow(clippy::neg_cmp_op_on_partial_ord)]
use crate::format::{DurationFormatOptions, DurationStyle, FormatSurface, ResolvedFormatters};

/// The countdown-to-bar-close chip's text for a bar opened at `bar_open` on a `bar_ms`
/// cadence, as seen at wall-clock `now`; `None` once the bar has closed (the chip
/// disappears until the next bar arrives instead of sitting at `00:00`).
///
/// The remaining time is rounded UP so the chip agrees with a clock that shows whole
/// seconds: at `hh:mm:54.500` the clock reads `:54`, which implies 6 s to the minute, and
/// the chip reads `00:06`. Flooring would read `00:05` for that whole second.
pub fn countdown_text(
    bar_open: f64,
    bar_ms: f64,
    now: f64,
    fmt: &ResolvedFormatters,
) -> Option<String> {
    if !(bar_ms > 0.0) {
        return None;
    }
    let remaining = bar_open + bar_ms - now;
    if remaining <= 0.0 {
        return None;
    }
    Some(format_countdown(remaining, fmt))
}

/// `MM:SS` (or `H:MM:SS` past an hour) for the ms remaining until the bar closes, rounded
/// up.
pub fn format_countdown(ms: f64, fmt: &ResolvedFormatters) -> String {
    fmt.duration(
        ms,
        &DurationFormatOptions {
            surface: Some(FormatSurface::LastPrice),
            style: Some(DurationStyle::Countdown),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::english_format;
    use crate::intl::days_from_civil;

    const MIN: f64 = 60_000.0;

    /// A 1-minute bar opening at 14:44:00.
    fn t0() -> f64 {
        days_from_civil(2026, 9, 9) as f64 * 86_400_000.0 + (14.0 * 60.0 + 44.0) * MIN
    }

    fn text(bar_ms: f64, now: f64) -> Option<String> {
        countdown_text(t0(), bar_ms, now, &english_format())
    }

    #[test]
    fn agrees_with_a_whole_second_clock_for_the_entire_second() {
        // The clock reads :54 for all of [54.000, 55.000): 6 s to the minute.
        assert_eq!(text(MIN, t0() + 54_000.0).as_deref(), Some("00:06"));
        assert_eq!(text(MIN, t0() + 54_500.0).as_deref(), Some("00:06"));
        assert_eq!(text(MIN, t0() + 54_999.0).as_deref(), Some("00:06"));
        assert_eq!(text(MIN, t0() + 55_000.0).as_deref(), Some("00:05"));
    }

    #[test]
    fn reads_0001_through_the_last_second_and_disappears_once_the_bar_has_closed() {
        assert_eq!(text(MIN, t0() + 59_000.0).as_deref(), Some("00:01"));
        assert_eq!(text(MIN, t0() + 59_999.0).as_deref(), Some("00:01"));
        assert_eq!(text(MIN, t0() + MIN), None);
        assert_eq!(text(MIN, t0() + MIN + 4_000.0), None);
    }

    #[test]
    fn shows_the_full_interval_at_the_bar_open() {
        assert_eq!(text(MIN, t0()).as_deref(), Some("01:00"));
        assert_eq!(text(4.0 * 60.0 * MIN, t0()).as_deref(), Some("4:00:00"));
    }

    #[test]
    fn is_hidden_without_a_known_bar_cadence() {
        assert_eq!(text(0.0, t0() + 1000.0), None);
        assert_eq!(text(f64::NAN, t0() + 1000.0), None);
        assert_eq!(text(-MIN, t0()), None);
    }

    #[test]
    fn format_countdown_rounds_up_and_pads() {
        let fmt = english_format();
        assert_eq!(format_countdown(5_500.0, &fmt), "00:06");
        assert_eq!(format_countdown(6_000.0, &fmt), "00:06");
        assert_eq!(format_countdown(1.0, &fmt), "00:01");
        assert_eq!(format_countdown(0.0, &fmt), "00:00");
        assert_eq!(format_countdown(-1.0, &fmt), "00:00");
        assert_eq!(format_countdown(3_600_000.0 + 65_000.0, &fmt), "1:01:05");
    }
}
