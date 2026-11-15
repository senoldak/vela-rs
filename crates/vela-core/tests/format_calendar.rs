//! Calendar names for date pickers. `date-calendar.test.ts` drives the UI kit's popover,
//! so only the names it reads are ported: the month and weekday tables, the year label,
//! the header order and the week start. The non-English cases record what is requested of
//! the backend, since the names themselves come from the engine's locale data.

mod i18n_common;

use std::sync::Arc;

use i18n_common::{Call, RecordingIntl, english, locale, translator};
use vela_core::format::{
    NameWidth, month_names, plain_integer, weekday_names, year_before_month, year_label,
};
use vela_core::i18n::{LocalizationOptions, Translator};
use vela_core::intl::{Intl, MonthStyle, NumericStyle, WeekdayStyle};

fn settings(options: LocalizationOptions) -> vela_core::format::FormatSettings {
    translator(&english(), options).format().settings().clone()
}

#[test]
fn english_keeps_the_pickers_own_tables() {
    let s = settings(LocalizationOptions::default());
    assert_eq!(
        month_names(&s, NameWidth::Long),
        [
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
            "December"
        ]
    );
    assert_eq!(
        month_names(&s, NameWidth::Short),
        [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"
        ]
    );
    assert_eq!(
        weekday_names(&s),
        ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"]
    );
    assert_eq!(year_label(&s, 2024.0), "2024");
    assert!(!year_before_month(&s));
}

#[test]
fn a_year_decade_reads_in_the_chosen_digits() {
    let s = settings(LocalizationOptions::default());
    assert_eq!(plain_integer(&s, 2026.9), "2026");
    assert_eq!(plain_integer(&s, -0.5), "0");
    let deva = settings(locale("en-u-nu-deva"));
    assert_eq!(plain_integer(&deva, 2026.0), "२०२६");
    assert_eq!(year_label(&deva, 2026.0), "२०२६");
}

#[test]
fn other_languages_ask_the_backend_for_gregorian_names_in_utc() {
    let recording = RecordingIntl::new();
    let intl: Arc<dyn Intl> = recording.clone();
    let ja = translator(&intl, locale("ja"));
    let s = ja.format().settings().clone();
    recording.clear();

    let months = month_names(&s, NameWidth::Long);
    assert_eq!(months.len(), 12);
    let weekdays = weekday_names(&s);
    assert_eq!(weekdays.len(), 7);
    let year = year_label(&s, 2026.0);
    let calls = recording.calls();
    assert_eq!(calls.len(), 12 + 7 + 1);

    let date_options = |i: usize| match &calls[i] {
        Call::Date {
            locale,
            options,
            ms,
        } => (locale.clone(), options.clone(), *ms),
        Call::Number { .. } => panic!("expected a date request"),
    };
    // 15 January 2000 for January: the middle of the month never crosses a zone boundary.
    let (loc, options, ms) = date_options(0);
    assert_eq!(loc, "ja");
    assert_eq!(options.month, Some(MonthStyle::Long));
    assert_eq!(options.calendar.as_deref(), Some("gregory"));
    assert_eq!(options.time_zone.as_deref(), Some("UTC"));
    assert_eq!(ms, 947_894_400_000.0);
    assert_eq!(months[0], "D[ja]947894400000");
    // 2 January 2000 was a Sunday.
    let (_, options, ms) = date_options(12);
    assert_eq!(options.weekday, Some(WeekdayStyle::Short));
    assert_eq!(ms, 946_771_200_000.0);
    assert_eq!(weekdays[1], "D[ja]946857600000");
    let (_, options, ms) = date_options(19);
    assert_eq!(options.year, Some(NumericStyle::Numeric));
    assert_eq!(ms, 1_782_864_000_000.0); // 1 July 2026
    assert_eq!(year, "D[ja]1782864000000");
}

#[test]
fn the_year_leads_where_the_backends_parts_say_so() {
    // The stand-in answers year then month, as Japanese does.
    let recording = RecordingIntl::new();
    let intl: Arc<dyn Intl> = recording.clone();
    let s = translator(&intl, locale("ja")).format().settings().clone();
    assert!(year_before_month(&s));
    // English never asks.
    recording.clear();
    let british = translator(&intl, locale("en-GB"))
        .format()
        .settings()
        .clone();
    recording.clear();
    assert!(!year_before_month(&british));
    assert!(recording.calls().is_empty());
}

#[test]
fn a_two_digit_year_reads_as_the_nineteen_hundreds_like_date_utc() {
    let recording = RecordingIntl::new();
    let intl: Arc<dyn Intl> = recording.clone();
    let s = translator(&intl, locale("de")).format().settings().clone();
    recording.clear();
    year_label(&s, 50.0);
    let calls = recording.calls();
    let Some(Call::Date { ms, .. }) = calls.first() else {
        panic!("no date request");
    };
    // `Date.UTC(50, 6, 1)` is 1 July 1950.
    assert_eq!(*ms, -615_513_600_000.0);
    // Not a number: the date is invalid and the label says so, with no request made.
    recording.clear();
    assert_eq!(year_label(&s, f64::NAN), "Invalid Date");
    assert!(recording.calls().is_empty());
}
