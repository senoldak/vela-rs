//! Minimal IANA-timezone support for the time axis and stamps. The renderer works in
//! epoch milliseconds (UTC) everywhere; to render labels in a chosen zone it computes
//! that zone's UTC offset for a given instant and shifts the milliseconds before reading
//! UTC fields. A single offset (sampled at the visible range's midpoint) is reused across
//! the range: exact away from DST boundaries, with at most a one-hour seam across a
//! transition.

use crate::format::zone_offset_ms;
use crate::intl::Intl;

const DAY_MS: f64 = 86_400_000.0;

/// The zone a bar time is read in. Bars of a day or longer (D, W, M and their multiples)
/// are stamped at 00:00 UTC of their trading date: a date, not an instant. Shifting one
/// into a zone behind UTC would move it onto the previous day, so they read in UTC
/// whatever the display zone. Intraday bars are real instants and read in `time_zone`.
pub fn display_zone(time_zone: &str, bar_interval_ms: f64) -> &str {
    if bar_interval_ms >= DAY_MS {
        "UTC"
    } else {
        time_zone
    }
}

/// The zone's offset from UTC at `ms`, in milliseconds (New York in winter: -5 h).
/// Returns 0 for UTC, an empty zone or an unknown one, so callers degrade to plain UTC.
/// The formatter behind it stays pinned to en-US whatever the chart's locale: it PARSES
/// the zone's wall-clock parts back into numbers, so localized digits or calendars would
/// break the arithmetic.
pub fn tz_offset_ms(intl: &dyn Intl, ms: f64, time_zone: &str) -> f64 {
    zone_offset_ms(intl, ms, time_zone)
}

/// `ms` shifted so its UTC fields read the wall-clock time in `time_zone`
/// (`zonedDate(...).getTime()` of the original, whose `getUTC*` accessors are then read
/// with the UTC-field helpers).
pub fn zoned_ms(intl: &dyn Intl, ms: f64, time_zone: &str) -> f64 {
    ms + tz_offset_ms(intl, ms, time_zone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intl::default_intl;

    const HOUR: f64 = 3_600_000.0;
    const DAY: f64 = 24.0 * HOUR;
    const WEEK: f64 = 7.0 * DAY;
    const MONTH: f64 = 30.0 * DAY;

    fn utc(y: i32, m0: u32, d: u32, h: u32) -> f64 {
        use crate::intl::days_from_civil;
        (days_from_civil(i64::from(y), m0 + 1, d) as f64) * DAY + f64::from(h) * HOUR
    }

    #[test]
    fn bars_of_a_day_or_longer_read_in_utc_and_intraday_bars_in_the_display_zone() {
        assert_eq!(display_zone("America/New_York", DAY), "UTC");
        assert_eq!(display_zone("America/New_York", 2.0 * DAY), "UTC");
        assert_eq!(display_zone("Asia/Tokyo", WEEK), "UTC");
        assert_eq!(display_zone("America/Chicago", 3.0 * MONTH), "UTC");
        assert_eq!(
            display_zone("America/New_York", 12.0 * HOUR),
            "America/New_York"
        );
        assert_eq!(display_zone("America/New_York", HOUR), "America/New_York");
    }

    #[test]
    fn the_display_zone_is_kept_while_the_bar_interval_is_unknown() {
        assert_eq!(display_zone("America/New_York", 0.0), "America/New_York");
        assert_eq!(
            display_zone("America/New_York", f64::NAN),
            "America/New_York"
        );
    }

    #[test]
    fn utc_offset_is_zero() {
        let intl = default_intl();
        assert_eq!(tz_offset_ms(&*intl, utc(2024, 0, 15, 12), "UTC"), 0.0);
        assert_eq!(tz_offset_ms(&*intl, utc(2024, 0, 15, 12), ""), 0.0);
    }

    #[test]
    fn unknown_zone_degrades_to_utc() {
        let intl = default_intl();
        assert_eq!(tz_offset_ms(&*intl, utc(2024, 0, 15, 12), "Not/AZone"), 0.0);
    }

    #[test]
    fn fixed_offset_zones_shift_by_their_offset() {
        // The built-in backend knows no IANA region rules (New York reads as UTC), only
        // `Etc/GMT±N` and offset ids; those exercise the same arithmetic.
        let intl = default_intl();
        assert_eq!(
            tz_offset_ms(&*intl, utc(2024, 0, 15, 12), "Etc/GMT+5"),
            -5.0 * HOUR
        );
        assert_eq!(
            tz_offset_ms(&*intl, utc(2024, 6, 15, 12), "+09:00"),
            9.0 * HOUR
        );
        assert_eq!(
            zoned_ms(&*intl, utc(2024, 0, 15, 12), "Etc/GMT+5"),
            utc(2024, 0, 15, 7)
        );
    }

    #[test]
    fn zoned_ms_with_utc_is_the_identity() {
        let intl = default_intl();
        let t = utc(2024, 0, 15, 12);
        assert_eq!(zoned_ms(&*intl, t, "UTC"), t);
    }
}
