//! Built-in date and time formatting for chart text. English keeps Vela's established
//! shapes (`Jan 5`, `14:30`, `Sun 30 Aug '26 19:00`); other languages take month and
//! weekday names and field order from the [`Intl`] backend. The calendar is always
//! Gregorian (a Thai locale would otherwise count Buddhist years) and the clock is
//! 24-hour unless the host asks otherwise.
//!
//! Where the original would throw on an invalid date (a non-English `Intl` call, or a
//! `NaN` time), the text here reads `Invalid Date`.
//!
//! [`Intl`]: crate::intl::Intl

use crate::i18n::number_text;
use crate::indicators::classics::math::{js_max, js_round};
use crate::intl::{
    DateTimeFormatOptions, HourCycle, Intl, MonthStyle, NumberUnit, NumericStyle, PartType,
    TimeZoneNameStyle, WeekdayStyle, civil_from_days, days_from_civil,
};

use super::numbers::{format_unit, localize_ascii, localize_digits, strip_bidi_marks};
use super::types::{
    ClockFormatOptions, DurationFormatOptions, DurationStyle, FormatSettings, HourCycleSetting,
    TimeStampFormatOptions, TimeTickFormatOptions, TimeTickUnit,
};

pub const EN_MONTHS_SHORT: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
pub const EN_WEEKDAYS_SHORT: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

const DAY_MS: f64 = 86_400_000.0;
const INVALID_DATE: &str = "Invalid Date";

/// `new Date(ms)`'s time value: truncated, `None` where the date is invalid.
pub(crate) fn time_clip(ms: f64) -> Option<f64> {
    (ms.is_finite() && ms.abs() <= 8.64e15).then(|| ms.trunc() + 0.0)
}

/// `Date.UTC(year, month0, day, hour, minute, second)`: two-digit years read as 19xx,
/// and `NaN` where any field is not a number or the result is out of range.
pub(crate) fn date_utc(
    year: f64,
    month0: f64,
    day: f64,
    hour: f64,
    minute: f64,
    second: f64,
) -> f64 {
    if [year, month0, day, hour, minute, second]
        .iter()
        .any(|v| !v.is_finite())
    {
        return f64::NAN;
    }
    let mut y = year.trunc();
    if (0.0..=99.0).contains(&y) {
        y += 1900.0;
    }
    let m = month0.trunc();
    let ym = y + (m / 12.0).floor();
    if ym.abs() > 400_000.0 {
        return f64::NAN;
    }
    let mn = m.rem_euclid(12.0) as u32 + 1;
    let days = days_from_civil(ym as i64, mn, 1) as f64 + day.trunc() - 1.0;
    let time = days * DAY_MS
        + hour.trunc() * 3_600_000.0
        + minute.trunc() * 60_000.0
        + second.trunc() * 1000.0;
    time_clip(time).unwrap_or(f64::NAN)
}

/// The zone's offset from UTC at `ms`, in milliseconds (New York in winter: -5 h); 0 for
/// UTC, an empty zone or an unknown one. The formatter is pinned to en-US because its
/// parts are PARSED back into numbers, never shown: a localized formatter could emit
/// other digits or calendars and break the arithmetic.
pub fn zone_offset_ms(intl: &dyn Intl, ms: f64, time_zone: &str) -> f64 {
    if time_zone.is_empty() || time_zone == "UTC" {
        return 0.0;
    }
    let Some(clipped) = time_clip(ms) else {
        return 0.0;
    };
    let options = DateTimeFormatOptions {
        time_zone: Some(time_zone.to_string()),
        hour_cycle: Some(HourCycle::H23),
        year: Some(NumericStyle::Numeric),
        month: Some(MonthStyle::TwoDigit),
        day: Some(NumericStyle::TwoDigit),
        hour: Some(NumericStyle::TwoDigit),
        minute: Some(NumericStyle::TwoDigit),
        second: Some(NumericStyle::TwoDigit),
        ..DateTimeFormatOptions::default()
    };
    let Some(parts) = intl.format_date_time_to_parts("en-US", &options, clipped) else {
        return 0.0;
    };
    let get = |kind: PartType| -> f64 {
        parts
            .iter()
            .find(|p| p.kind == kind)
            .and_then(|p| p.value.parse::<f64>().ok())
            .unwrap_or(f64::NAN)
    };
    let as_utc = date_utc(
        get(PartType::Year),
        get(PartType::Month) - 1.0,
        get(PartType::Day),
        get(PartType::Hour) % 24.0,
        get(PartType::Minute),
        get(PartType::Second),
    );
    as_utc - ms
}

/// A Gregorian date formatter that reads wall-clock fields in UTC (callers shift the
/// instant into the display zone first, the way the axes align their ticks), as text.
pub(crate) fn date_format_text(
    settings: &FormatSettings,
    options: &DateTimeFormatOptions,
    ms: f64,
) -> String {
    let Some(ms) = time_clip(ms) else {
        return INVALID_DATE.to_string();
    };
    let full = DateTimeFormatOptions {
        time_zone: Some("UTC".to_string()),
        calendar: Some("gregory".to_string()),
        numbering_system: Some(settings.numbering_system.clone()),
        ..options.clone()
    };
    settings
        .intl
        .format_date_time(&settings.locale, &full, ms)
        .or_else(|| {
            let english = DateTimeFormatOptions {
                time_zone: Some("UTC".to_string()),
                calendar: Some("gregory".to_string()),
                numbering_system: None,
                ..options.clone()
            };
            settings.intl.format_date_time("en", &english, ms)
        })
        .unwrap_or_else(|| INVALID_DATE.to_string())
}

/// The parts of the same formatter.
pub(crate) fn date_format_parts(
    settings: &FormatSettings,
    options: &DateTimeFormatOptions,
    ms: f64,
) -> Vec<crate::intl::FormatPart> {
    let Some(ms) = time_clip(ms) else {
        return Vec::new();
    };
    let full = DateTimeFormatOptions {
        time_zone: Some("UTC".to_string()),
        calendar: Some("gregory".to_string()),
        numbering_system: Some(settings.numbering_system.clone()),
        ..options.clone()
    };
    settings
        .intl
        .format_date_time_to_parts(&settings.locale, &full, ms)
        .or_else(|| {
            let english = DateTimeFormatOptions {
                time_zone: Some("UTC".to_string()),
                calendar: Some("gregory".to_string()),
                numbering_system: None,
                ..options.clone()
            };
            settings.intl.format_date_time_to_parts("en", &english, ms)
        })
        .unwrap_or_default()
}

/// The `Intl` options of a time of day under the settings' clock.
fn clock_options(settings: &FormatSettings) -> DateTimeFormatOptions {
    match settings.hour_cycle {
        HourCycleSetting::H23 => DateTimeFormatOptions {
            hour: Some(NumericStyle::TwoDigit),
            minute: Some(NumericStyle::TwoDigit),
            hour_cycle: Some(HourCycle::H23),
            ..DateTimeFormatOptions::default()
        },
        HourCycleSetting::H12 => DateTimeFormatOptions {
            hour: Some(NumericStyle::Numeric),
            minute: Some(NumericStyle::TwoDigit),
            hour_cycle: Some(HourCycle::H12),
            ..DateTimeFormatOptions::default()
        },
        HourCycleSetting::Locale => DateTimeFormatOptions {
            hour: Some(NumericStyle::Numeric),
            minute: Some(NumericStyle::TwoDigit),
            ..DateTimeFormatOptions::default()
        },
    }
}

fn pad2(n: f64) -> String {
    if n < 10.0 {
        format!("0{}", number_text(n))
    } else {
        number_text(n)
    }
}

/// The UTC fields of a time value, as `Date#getUTC*` reads them.
struct UtcFields {
    year: i64,
    month0: usize,
    day: u32,
    weekday: usize,
    hour: u32,
    minute: u32,
}

fn utc_fields(ms: f64) -> Option<UtcFields> {
    let ms = time_clip(ms)? as i64;
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    Some(UtcFields {
        year,
        month0: month as usize - 1,
        day,
        weekday: (days + 4).rem_euclid(7) as usize,
        hour: (rem / 3_600_000) as u32,
        minute: (rem / 60_000 % 60) as u32,
    })
}

/// English time of day: Vela's `14:30` on the default clock, the backend on any other.
fn english_clock(wall: &UtcFields, wall_ms: f64, settings: &FormatSettings) -> String {
    if settings.hour_cycle == HourCycleSetting::H23 {
        let text = format!(
            "{}:{}",
            pad2(f64::from(wall.hour)),
            pad2(f64::from(wall.minute))
        );
        return localize_ascii(&text, settings);
    }
    strip_bidi_marks(&date_format_text(
        settings,
        &clock_options(settings),
        wall_ms,
    ))
}

fn is_clock_unit(unit: TimeTickUnit) -> bool {
    matches!(
        unit,
        TimeTickUnit::Hour | TimeTickUnit::Minute | TimeTickUnit::Second
    )
}

fn tick_options(unit: TimeTickUnit) -> DateTimeFormatOptions {
    match unit {
        TimeTickUnit::Year => DateTimeFormatOptions {
            year: Some(NumericStyle::Numeric),
            ..DateTimeFormatOptions::default()
        },
        TimeTickUnit::Month => DateTimeFormatOptions {
            month: Some(MonthStyle::Short),
            ..DateTimeFormatOptions::default()
        },
        _ => DateTimeFormatOptions {
            month: Some(MonthStyle::Short),
            day: Some(NumericStyle::Numeric),
            ..DateTimeFormatOptions::default()
        },
    }
}

pub fn format_time_tick(
    ms: f64,
    options: &TimeTickFormatOptions,
    settings: &FormatSettings,
) -> String {
    let wall_ms = ms + options.offset_ms.unwrap_or(0.0);
    let unit = options.unit;
    if settings.english {
        let Some(wall) = utc_fields(wall_ms) else {
            return INVALID_DATE.to_string();
        };
        if is_clock_unit(unit) {
            return english_clock(&wall, wall_ms, settings);
        }
        let month = EN_MONTHS_SHORT[wall.month0 % 12];
        return match unit {
            TimeTickUnit::Year => localize_ascii(&wall.year.to_string(), settings),
            TimeTickUnit::Month => month.to_string(),
            _ => localize_ascii(&format!("{month} {}", wall.day), settings),
        };
    }
    let opts = if is_clock_unit(unit) {
        clock_options(settings)
    } else {
        tick_options(unit)
    };
    strip_bidi_marks(&date_format_text(settings, &opts, wall_ms))
}

pub fn format_time_stamp(
    ms: f64,
    options: &TimeStampFormatOptions,
    settings: &FormatSettings,
) -> String {
    let zone = options.time_zone.as_deref().unwrap_or("UTC");
    let wall_ms = ms + zone_offset_ms(settings.intl.as_ref(), ms, zone);
    let with_time = options.with_time.unwrap_or(true);
    if settings.english {
        let Some(wall) = utc_fields(wall_ms) else {
            return INVALID_DATE.to_string();
        };
        let date = localize_ascii(
            &format!(
                "{} {} {} '{}",
                EN_WEEKDAYS_SHORT[wall.weekday % 7],
                wall.day,
                EN_MONTHS_SHORT[wall.month0 % 12],
                pad2((wall.year % 100) as f64)
            ),
            settings,
        );
        return if with_time {
            format!("{date} {}", english_clock(&wall, wall_ms, settings))
        } else {
            date
        };
    }
    let mut opts = DateTimeFormatOptions {
        weekday: Some(WeekdayStyle::Short),
        day: Some(NumericStyle::Numeric),
        month: Some(MonthStyle::Short),
        year: Some(NumericStyle::TwoDigit),
        ..DateTimeFormatOptions::default()
    };
    if with_time {
        let clock = clock_options(settings);
        opts.hour = clock.hour;
        opts.minute = clock.minute;
        opts.hour_cycle = clock.hour_cycle;
    }
    strip_bidi_marks(&date_format_text(settings, &opts, wall_ms))
}

/// `MM:SS`, or `H:MM:SS` past an hour, rounded up to whole seconds.
fn countdown_text(ms: f64) -> String {
    let total = js_max(0.0, (ms / 1000.0).ceil());
    let s = total % 60.0;
    let m = (total / 60.0).floor() % 60.0;
    let h = (total / 3600.0).floor();
    let pad = |v: f64| format!("{:0>2}", number_text(v));
    if h > 0.0 {
        format!("{}:{}:{}", number_text(h), pad(m), pad(s))
    } else {
        format!("{}:{}", pad(m), pad(s))
    }
}

/// JavaScript truthiness of a number.
fn truthy(n: f64) -> bool {
    n != 0.0 && !n.is_nan()
}

/// The two largest units of a span: `2d 4h`, `3h 15m`, `45m`, `30s`.
fn span_parts(ms: f64) -> Vec<(f64, NumberUnit)> {
    let m = ms.abs();
    const HR: f64 = 3_600_000.0;
    const MIN: f64 = 60_000.0;
    if m >= DAY_MS {
        let d = (m / DAY_MS).floor();
        let h = js_round((m % DAY_MS) / HR);
        return if truthy(h) {
            vec![(d, NumberUnit::Day), (h, NumberUnit::Hour)]
        } else {
            vec![(d, NumberUnit::Day)]
        };
    }
    if m >= HR {
        let h = (m / HR).floor();
        let mn = js_round((m % HR) / MIN);
        return if truthy(mn) {
            vec![(h, NumberUnit::Hour), (mn, NumberUnit::Minute)]
        } else {
            vec![(h, NumberUnit::Hour)]
        };
    }
    if m >= MIN {
        return vec![(js_round(m / MIN), NumberUnit::Minute)];
    }
    vec![(js_round(m / 1000.0), NumberUnit::Second)]
}

fn english_unit(unit: NumberUnit) -> char {
    match unit {
        NumberUnit::Day => 'd',
        NumberUnit::Hour => 'h',
        NumberUnit::Minute => 'm',
        NumberUnit::Second => 's',
    }
}

pub fn format_duration(
    ms: f64,
    options: &DurationFormatOptions,
    settings: &FormatSettings,
) -> String {
    if options.style.unwrap_or_default() == DurationStyle::Countdown {
        return localize_digits(&countdown_text(ms), settings);
    }
    let parts = span_parts(ms);
    if settings.english {
        let text: Vec<String> = parts
            .iter()
            .map(|(n, u)| format!("{}{}", number_text(*n), english_unit(*u)))
            .collect();
        return localize_digits(&text.join(" "), settings);
    }
    let text: Vec<String> = parts
        .iter()
        .map(|(n, u)| format_unit(*n, *u, settings))
        .collect();
    text.join(" ")
}

/// A wall clock in a zone. English on the default clock keeps Vela's `14:05:09`; other
/// locales and clocks read the backend, with the settings' digits and hour cycle. Empty
/// when the zone or locale is unusable.
pub fn format_clock(ms: f64, options: &ClockFormatOptions, settings: &FormatSettings) -> String {
    let time_zone = options
        .time_zone
        .clone()
        .unwrap_or_else(|| "UTC".to_string());
    let seconds = options.seconds.unwrap_or(true);
    let english = settings.english && settings.hour_cycle == HourCycleSetting::H23;
    let base = DateTimeFormatOptions {
        hour: Some(NumericStyle::TwoDigit),
        minute: Some(NumericStyle::TwoDigit),
        second: seconds.then_some(NumericStyle::TwoDigit),
        time_zone: Some(time_zone),
        ..DateTimeFormatOptions::default()
    };
    let (locale, opts) = if english {
        (
            "en-GB",
            DateTimeFormatOptions {
                hour12: Some(false),
                ..base
            },
        )
    } else {
        (
            settings.locale.as_str(),
            DateTimeFormatOptions {
                calendar: Some("gregory".to_string()),
                numbering_system: Some(settings.numbering_system.clone()),
                hour_cycle: match settings.hour_cycle {
                    HourCycleSetting::H23 => Some(HourCycle::H23),
                    HourCycleSetting::H12 => Some(HourCycle::H12),
                    HourCycleSetting::Locale => None,
                },
                ..base
            },
        )
    };
    let Some(clipped) = time_clip(ms) else {
        return String::new();
    };
    settings
        .intl
        .format_date_time(locale, &opts, clipped)
        .map(|text| strip_bidi_marks(&text))
        .unwrap_or_default()
}

/// The time zone's name part of a formatted instant, for callers that need the raw part.
pub(crate) fn zone_name_part(intl: &dyn Intl, zone: &str, ms: f64) -> Option<String> {
    let options = DateTimeFormatOptions {
        time_zone: Some(zone.to_string()),
        time_zone_name: Some(TimeZoneNameStyle::ShortOffset),
        ..DateTimeFormatOptions::default()
    };
    let parts = intl.format_date_time_to_parts("en-US", &options, time_clip(ms)?)?;
    Some(
        parts
            .into_iter()
            .find(|p| p.kind == PartType::TimeZoneName)
            .map(|p| p.value)
            .unwrap_or_default(),
    )
}
