//! Day, week and month bars are stamped at 00:00 UTC of their trading date: a date, not
//! an instant. A surface that shows a coarse bar's date reads it in UTC (no time of day)
//! whatever the display zone, while intraday bars read in the display zone. Ported from
//! `coarse-bar-dates.test.ts`: the zone choice itself (`displayZone`) and the axis, grid,
//! crosshair and data-window cases belong to the renderer, so these tests call the
//! formatters the way those surfaces do.
//!
//! Zones come from the `FakeIntl` test double (New York, Chicago and Tokyo as the fixed
//! offsets they have on 22 and 23 October 2026).

mod i18n_common;

use i18n_common::{english, fake, translator};
use vela_core::format::{TimeStampFormatOptions, zone_offset_ms};
use vela_core::i18n::{LocalizationOptions, Translator};
use vela_core::timezones::{EXCHANGE_TIMEZONE, resolve_timezone};

const HOUR: f64 = 3_600_000.0;
const FRIDAY_23_OCT: f64 = 1_792_713_600_000.0;

fn stamp(ms: f64, zone: &str, with_time: bool) -> String {
    let t = translator(&fake(), LocalizationOptions::default());
    t.format().time_stamp(
        ms,
        &TimeStampFormatOptions {
            time_zone: Some(zone.to_string()),
            with_time: Some(with_time),
            ..Default::default()
        },
    )
}

#[test]
fn a_daily_bars_utc_date_reads_the_same_in_every_utc_spelling() {
    for zone in ["UTC", "Etc/UTC"] {
        assert_eq!(
            stamp(FRIDAY_23_OCT, zone, false),
            "Fri 23 Oct '26",
            "{zone}"
        );
    }
}

#[test]
fn a_weekly_bar_shows_on_its_monday_and_a_monthly_bar_on_its_first() {
    for zone in ["UTC", "Etc/UTC"] {
        assert_eq!(
            stamp(1_792_368_000_000.0, zone, false),
            "Mon 19 Oct '26",
            "{zone}"
        );
        assert_eq!(
            stamp(1_790_812_800_000.0, zone, false),
            "Thu 1 Oct '26",
            "{zone}"
        );
    }
}

#[test]
fn a_date_without_time_still_shifts_into_a_zone_that_is_asked_for() {
    // This is why a surface must pick UTC for coarse bars: asking for New York moves the date.
    assert_eq!(
        stamp(FRIDAY_23_OCT, "America/New_York", false),
        "Thu 22 Oct '26"
    );
}

#[test]
fn intraday_bars_keep_converting_into_the_display_zone() {
    let chicago = resolve_timezone(EXCHANGE_TIMEZONE, Some("America/Chicago"));
    assert_eq!(
        stamp(FRIDAY_23_OCT + 3.0 * HOUR, "America/New_York", true),
        "Thu 22 Oct '26 23:00"
    );
    assert_eq!(
        stamp(FRIDAY_23_OCT, "America/New_York", true),
        "Thu 22 Oct '26 20:00"
    );
    assert_eq!(
        stamp(FRIDAY_23_OCT + 3.0 * HOUR, &chicago, true),
        "Thu 22 Oct '26 22:00"
    );
    assert_eq!(
        stamp(FRIDAY_23_OCT, "Asia/Tokyo", true),
        "Fri 23 Oct '26 09:00"
    );
}

#[test]
fn the_built_in_backend_reads_every_zone_it_does_not_know_as_utc() {
    let t = translator(&english(), LocalizationOptions::default());
    let at = FRIDAY_23_OCT + 3.0 * HOUR;
    let options = TimeStampFormatOptions {
        time_zone: Some("America/New_York".into()),
        ..Default::default()
    };
    assert_eq!(t.format().time_stamp(at, &options), "Fri 23 Oct '26 03:00");
}

#[test]
fn a_zone_offset_is_the_walls_distance_from_utc() {
    let intl = fake();
    let at = FRIDAY_23_OCT + 3.0 * HOUR;
    assert_eq!(
        zone_offset_ms(intl.as_ref(), at, "America/New_York"),
        -4.0 * HOUR
    );
    assert_eq!(
        zone_offset_ms(intl.as_ref(), at, "Asia/Kolkata"),
        5.5 * HOUR
    );
    assert_eq!(
        zone_offset_ms(intl.as_ref(), at, "Pacific/Marquesas"),
        -9.5 * HOUR
    );
    // UTC, an empty zone, an unknown one and an invalid instant all read 0.
    for zone in ["UTC", "", "Etc/UTC", "Not/AZone"] {
        assert_eq!(zone_offset_ms(intl.as_ref(), at, zone), 0.0, "{zone:?}");
    }
    assert_eq!(
        zone_offset_ms(intl.as_ref(), f64::NAN, "America/New_York"),
        0.0
    );
    assert_eq!(zone_offset_ms(intl.as_ref(), 9e15, "America/New_York"), 0.0);
}

#[test]
fn a_zone_offset_keeps_the_sub_millisecond_part_of_the_instant_like_the_original() {
    let intl = fake();
    // `new Date(ms)` truncates, but the offset subtracts the original number.
    let offset = zone_offset_ms(intl.as_ref(), FRIDAY_23_OCT + 0.25, "America/New_York");
    assert_eq!(offset, -4.0 * HOUR - 0.25);
}
