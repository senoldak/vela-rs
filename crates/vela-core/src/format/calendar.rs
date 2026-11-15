//! Calendar names for date pickers: month and weekday names, the year label, the order of
//! month and year in a header, and the first day of the week. English keeps the picker's
//! own tables (`September`, `Sep`, `Su`); other languages read the [`Intl`] backend,
//! always on the Gregorian calendar because the picker produces Gregorian dates.
//!
//! [`Intl`]: crate::intl::Intl

use crate::i18n::number_text;
use crate::intl::{DateTimeFormatOptions, Intl, MonthStyle, NumericStyle, PartType, WeekdayStyle};

use super::dates::{date_format_parts, date_format_text, date_utc};
use super::numbers::{localize_ascii, strip_bidi_marks};
use super::types::FormatSettings;

pub const EN_MONTHS_LONG: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
pub const EN_MONTHS_SHORT_PICKER: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
pub const EN_WEEKDAYS_MIN: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];

/// The width of a month name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameWidth {
    Long,
    Short,
}

/// Twelve month names, January first.
pub fn month_names(settings: &FormatSettings, width: NameWidth) -> Vec<String> {
    if settings.english {
        let table = match width {
            NameWidth::Long => &EN_MONTHS_LONG,
            NameWidth::Short => &EN_MONTHS_SHORT_PICKER,
        };
        return table.iter().map(|m| (*m).to_string()).collect();
    }
    let options = DateTimeFormatOptions {
        month: Some(match width {
            NameWidth::Long => MonthStyle::Long,
            NameWidth::Short => MonthStyle::Short,
        }),
        ..DateTimeFormatOptions::default()
    };
    (0..12)
        .map(|m| {
            strip_bidi_marks(&date_format_text(
                settings,
                &options,
                date_utc(2000.0, f64::from(m), 15.0, 0.0, 0.0, 0.0),
            ))
        })
        .collect()
}

/// Seven short weekday names, Sunday first (index = `Date#getDay()`).
pub fn weekday_names(settings: &FormatSettings) -> Vec<String> {
    if settings.english {
        return EN_WEEKDAYS_MIN.iter().map(|d| (*d).to_string()).collect();
    }
    let options = DateTimeFormatOptions {
        weekday: Some(WeekdayStyle::Short),
        ..DateTimeFormatOptions::default()
    };
    // 2 January 2000 was a Sunday.
    (0..7)
        .map(|d| {
            strip_bidi_marks(&date_format_text(
                settings,
                &options,
                date_utc(2000.0, 0.0, 2.0 + f64::from(d), 0.0, 0.0, 0.0),
            ))
        })
        .collect()
}

/// A year as a header label (`2026`, `2026年`).
pub fn year_label(settings: &FormatSettings, year: f64) -> String {
    if settings.english {
        return localize_ascii(&number_text(year), settings);
    }
    let options = DateTimeFormatOptions {
        year: Some(NumericStyle::Numeric),
        ..DateTimeFormatOptions::default()
    };
    strip_bidi_marks(&date_format_text(
        settings,
        &options,
        date_utc(year, 6.0, 1.0, 0.0, 0.0, 0.0),
    ))
}

/// An integer in the settings' digits, without grouping (`2026`, `٢٠٢٦`).
pub fn plain_integer(settings: &FormatSettings, n: f64) -> String {
    localize_ascii(&number_text(n.trunc()), settings)
}

/// Whether the locale writes the year before the month (`2026年9月`, not `September 2026`).
pub fn year_before_month(settings: &FormatSettings) -> bool {
    if settings.english {
        return false;
    }
    let options = DateTimeFormatOptions {
        year: Some(NumericStyle::Numeric),
        month: Some(MonthStyle::Long),
        ..DateTimeFormatOptions::default()
    };
    let parts = date_format_parts(
        settings,
        &options,
        date_utc(2026.0, 8.0, 15.0, 0.0, 0.0, 0.0),
    );
    let position = |kind: PartType| {
        parts
            .iter()
            .position(|p| p.kind == kind)
            .map_or(-1, |i| i as isize)
    };
    position(PartType::Year) < position(PartType::Month)
}

/// Regions whose week starts on Sunday (CLDR week data); every other region starts on
/// Monday unless it is in [`SATURDAY_REGIONS`]. Used where the engine has no week
/// information.
const SUNDAY_REGIONS: &[&str] = &[
    "AG", "AS", "BD", "BR", "BS", "BT", "BW", "BZ", "CA", "CN", "CO", "DM", "DO", "ET", "GT", "GU",
    "HK", "HN", "ID", "IL", "IN", "JM", "JP", "KE", "KH", "KR", "LA", "MH", "MM", "MO", "MT", "MX",
    "MZ", "NI", "NP", "PA", "PE", "PH", "PK", "PR", "PT", "PY", "SA", "SG", "SV", "TH", "TT", "TW",
    "UM", "US", "VE", "VI", "WS", "YE", "ZA", "ZW",
];
const SATURDAY_REGIONS: &[&str] = &[
    "AE", "AF", "BH", "DJ", "DZ", "EG", "IQ", "IR", "JO", "KW", "LY", "OM", "QA", "SD", "SY",
];

/// The locale's first day of the week as a `Date#getDay()` index: 0 Sunday, 1 Monday or
/// 6 Saturday. A bare language takes its likely region (`en` is US, so Sunday).
pub fn locale_week_start(intl: &dyn Intl, locale: &str) -> u8 {
    let Some(loc) = intl.parse_locale(locale) else {
        return 1;
    };
    if let Some(first_day) = intl.locale_week_first_day(locale) {
        return match first_day {
            7 => 0,
            6 => 6,
            _ => 1,
        };
    }
    let region = loc
        .region
        .or_else(|| intl.maximize_locale(locale).and_then(|m| m.region));
    match region.as_deref() {
        Some(r) if SUNDAY_REGIONS.contains(&r) => 0,
        Some(r) if SATURDAY_REGIONS.contains(&r) => 6,
        _ => 1,
    }
}
