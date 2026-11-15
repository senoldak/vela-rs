//! Chart numbers and dates in the chart's locale: English keeps today's shapes, every
//! language keeps Western digits, the Gregorian calendar and a 24-hour clock unless the
//! host asks otherwise, and a host can replace any formatter. Ported from
//! `i18n-formatting.test.ts`; the axis, countdown and date-picker cases of that file
//! belong to the renderer and the UI kit.
//!
//! German and Arabic separators come from the `FakeIntl` test double. Requests that real
//! locale data would answer (Thai, Korean, Hindi, Japanese) are asserted on the
//! `RecordingIntl` instead.

mod i18n_common;

use std::sync::{Arc, Mutex};

use i18n_common::{Call, RecordingIntl, english, fake, locale, translator};
use vela_core::format::{
    ClockFormatOptions, CompactFormatOptions, DurationFormatOptions, DurationStyle, FormatSurface,
    HourCycleSetting, PercentFormatOptions, PriceFormatOptions, PriceMode, TimeStampFormatOptions,
    TimeTickFormatOptions, TimeTickUnit, VelaFormatters,
};
use vela_core::i18n::{DirectionOption, LocalizationOptions, Translator};
use vela_core::intl::{HourCycle, Intl, MonthStyle, NumericStyle};

const SEP_5_2026_1430: f64 = 1_788_618_600_000.0;
const DAY_MS: f64 = 86_400_000.0;

fn price(decimals: f64) -> PriceFormatOptions {
    PriceFormatOptions::new(decimals)
}

fn tick(unit: TimeTickUnit) -> TimeTickFormatOptions {
    TimeTickFormatOptions::new(unit, false)
}

fn utc_ms(y: i32, mo0: u32, d: u32, h: u32, mi: u32) -> f64 {
    let days = vela_core_days(y, mo0 + 1, d);
    (days * 86_400 + i64::from(h) * 3600 + i64::from(mi) * 60) as f64 * 1000.0
}

/// Days since 1970-01-01 (a local copy: the crate's helper is private).
fn vela_core_days(year: i32, month: u32, day: u32) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

mod a_chart_with_no_locale {
    use super::*;

    #[test]
    fn keeps_todays_english_shapes_on_the_axes_chips_and_readouts() {
        let t = translator(&english(), LocalizationOptions::default());
        let f = t.format();
        assert_eq!(f.price(58_732.5, &price(2.0)), "58732.50");
        let grouped = PriceFormatOptions {
            grouping: Some(true),
            ..price(2.0)
        };
        assert_eq!(f.price(58_732.5, &grouped), "58,732.50");
        let pct = PriceFormatOptions {
            mode: Some(PriceMode::Percent),
            ..price(2.0)
        };
        assert_eq!(f.price(5.0, &pct), "+5.00%");
        assert_eq!(f.price(-1.2, &pct), "-1.20%");
        let indexed = PriceFormatOptions {
            mode: Some(PriceMode::Indexed),
            ..price(2.0)
        };
        assert_eq!(f.price(103.456, &indexed), "103.46");
        let compact = CompactFormatOptions::default();
        assert_eq!(f.compact(1_234_567.0, &compact), "1.23M");
        assert_eq!(f.compact(1_200_000.0, &compact), "1.2M");
        assert_eq!(
            f.compact(
                1_200_000.0,
                &CompactFormatOptions {
                    trim_zeros: Some(false),
                    ..compact
                }
            ),
            "1.20M"
        );
        assert_eq!(
            f.time_tick(SEP_5_2026_1430, &tick(TimeTickUnit::Minute)),
            "14:30"
        );
        assert_eq!(
            f.time_tick(
                SEP_5_2026_1430,
                &TimeTickFormatOptions::new(TimeTickUnit::Day, true)
            ),
            "Sep 5"
        );
        assert_eq!(
            f.time_tick(
                SEP_5_2026_1430,
                &TimeTickFormatOptions::new(TimeTickUnit::Month, true)
            ),
            "Sep"
        );
        assert_eq!(
            f.time_tick(
                SEP_5_2026_1430,
                &TimeTickFormatOptions::new(TimeTickUnit::Year, true)
            ),
            "2026"
        );
        assert_eq!(
            f.time_stamp(utc_ms(2031, 0, 3, 9, 5), &TimeStampFormatOptions::default()),
            "Fri 3 Jan '31 09:05"
        );
        let countdown = DurationFormatOptions::default();
        assert_eq!(f.duration(299_000.0, &countdown), "04:59");
        assert_eq!(f.duration(3_665_000.0, &countdown), "1:01:05");
        let span = DurationFormatOptions {
            style: Some(DurationStyle::Span),
            ..Default::default()
        };
        assert_eq!(f.duration(2.0 * DAY_MS + 4.0 * 3_600_000.0, &span), "2d 4h");
        assert_eq!(
            f.duration(3.0 * 3_600_000.0 + 15.0 * 60_000.0, &span),
            "3h 15m"
        );
        assert_eq!(f.duration(45.0 * 60_000.0, &span), "45m");
        assert_eq!(f.duration(30_000.0, &span), "30s");
    }

    #[test]
    fn a_percent_value_reads_signed_by_default() {
        let f = translator(&english(), LocalizationOptions::default());
        let f = f.format();
        assert_eq!(f.percent(2.34, &PercentFormatOptions::default()), "+2.34%");
        assert_eq!(f.percent(-2.34, &PercentFormatOptions::default()), "-2.34%");
        let plain = PercentFormatOptions {
            signed: Some(false),
            decimals: Some(1.0),
            ..Default::default()
        };
        assert_eq!(f.percent(2.34, &plain), "2.3%");
    }

    #[test]
    fn a_clock_reads_the_wall_time_in_the_zone() {
        let t = translator(&english(), LocalizationOptions::default());
        let at = utc_ms(2026, 8, 5, 14, 5) + 9_000.0;
        assert_eq!(
            t.format().clock(at, &ClockFormatOptions::default()),
            "14:05:09"
        );
        let no_seconds = ClockFormatOptions {
            seconds: Some(false),
            ..Default::default()
        };
        assert_eq!(t.format().clock(at, &no_seconds), "14:05");
        // A zone the built-in backend does not know reads as an unusable `Intl` zone: empty.
        let paris = ClockFormatOptions {
            time_zone: Some("Europe/Paris".into()),
            ..Default::default()
        };
        assert_eq!(t.format().clock(at, &paris), "");
        let paris_in_fake = translator(&fake(), LocalizationOptions::default());
        assert_eq!(paris_in_fake.format().clock(at, &paris), "16:05:09");
    }

    #[test]
    fn non_finite_values_read_as_javascript_writes_them() {
        let f = translator(&english(), LocalizationOptions::default());
        let f = f.format();
        assert_eq!(f.price(f64::NAN, &price(2.0)), "NaN");
        assert_eq!(f.price(f64::INFINITY, &price(2.0)), "Infinity");
        assert_eq!(
            f.percent(f64::NAN, &PercentFormatOptions::default()),
            "-NaN%"
        );
        assert_eq!(f.compact(f64::NAN, &CompactFormatOptions::default()), "NaN");
        assert_eq!(
            f.duration(f64::NAN, &DurationFormatOptions::default()),
            "NaN:NaN"
        );
        assert_eq!(
            f.time_tick(f64::NAN, &tick(TimeTickUnit::Minute)),
            "Invalid Date"
        );
        // Out-of-range fraction digits clamp like `toFixed`'s 0 to 20.
        assert_eq!(f.price(1.5, &price(-3.0)), "2");
        assert_eq!(f.price(1.5, &price(f64::NAN)), "1.50");
    }

    #[test]
    fn a_twelve_hour_clock_reads_with_a_day_period() {
        let h12 = LocalizationOptions {
            hour_cycle: Some(HourCycleSetting::H12),
            ..Default::default()
        };
        let t = translator(&english(), h12);
        assert_eq!(
            t.format()
                .time_tick(utc_ms(2026, 0, 1, 21, 5), &tick(TimeTickUnit::Minute)),
            "9:05 PM"
        );
        assert_eq!(t.hour_cycle(), HourCycleSetting::H12);
    }
}

mod digits {
    use super::*;

    #[test]
    fn arabic_in_saudi_arabia_shows_western_digits_by_default_and_arabic_indic_digits_on_request() {
        let intl = fake();
        let latn = translator(&intl, locale("ar-SA"));
        assert_eq!(latn.numbering_system(), "latn");
        let grouped = PriceFormatOptions {
            grouping: Some(true),
            ..price(2.0)
        };
        assert_eq!(latn.format().price(58_732.5, &grouped), "58,732.50");
        assert_eq!(
            latn.format()
                .time_tick(SEP_5_2026_1430, &tick(TimeTickUnit::Minute)),
            "14:30"
        );

        let native_options = LocalizationOptions {
            numbering_system: Some("locale".into()),
            ..locale("ar-SA")
        };
        let native = translator(&intl, native_options);
        assert_eq!(native.numbering_system(), "arab");
        assert_eq!(native.format().price(58_732.5, &grouped), "٥٨٬٧٣٢٫٥٠");
        assert_eq!(native.format().price(-1.5, &price(1.0)), "-١٫٥");
        assert_eq!(
            native
                .format()
                .duration(299_000.0, &DurationFormatOptions::default()),
            "٠٤:٥٩"
        );
        let arab = LocalizationOptions {
            numbering_system: Some("arab".into()),
            ..locale("ar")
        };
        assert_eq!(
            translator(&intl, arab)
                .format()
                .time_tick(SEP_5_2026_1430, &tick(TimeTickUnit::Minute)),
            "١٤:٣٠"
        );
    }

    #[test]
    fn chart_numbers_carry_no_bidi_control_marks_in_right_to_left_languages() {
        // The Arabic stand-in writes its minus with a leading ALM, as ICU does.
        let intl = fake();
        let has_marks = |s: &str| s.contains(['\u{200e}', '\u{200f}', '\u{61c}']);
        let ar = translator(&intl, locale("ar"));
        let f = ar.format();
        let grouped = PriceFormatOptions {
            grouping: Some(true),
            ..price(2.0)
        };
        let pct = PriceFormatOptions {
            mode: Some(PriceMode::Percent),
            ..price(2.0)
        };
        for text in [
            f.price(-58_732.5, &grouped),
            f.percent(-1.2, &PercentFormatOptions::default()),
            f.price(-3.0, &pct),
            f.compact(-1_234_567.0, &CompactFormatOptions::default()),
        ] {
            assert!(!has_marks(&text), "{text:?}");
        }
        // Without the strip the stand-in would have shown the mark.
        let raw = intl
            .format_number("ar", &Default::default(), -1.0)
            .unwrap_or_default();
        assert!(has_marks(&raw));
    }

    #[test]
    fn an_unknown_numbering_system_falls_back_to_western_digits() {
        let t = translator(
            &english(),
            LocalizationOptions {
                numbering_system: Some("zzzz".into()),
                ..Default::default()
            },
        );
        assert_eq!(t.numbering_system(), "latn");
        let tagged = translator(&english(), locale("en-u-nu-deva"));
        assert_eq!(tagged.numbering_system(), "deva");
        assert_eq!(tagged.format().price(1234.5, &price(1.0)), "१२३४.५");
    }
}

mod calendar_and_clock {
    use super::*;

    #[test]
    fn the_calendar_is_always_gregorian_in_utc_wall_time() {
        let recording = RecordingIntl::new();
        let intl: Arc<dyn Intl> = recording.clone();
        let th = translator(&intl, locale("th"));
        recording.clear();
        th.format()
            .time_stamp(SEP_5_2026_1430, &TimeStampFormatOptions::default());
        th.format().time_tick(
            SEP_5_2026_1430,
            &TimeTickFormatOptions::new(TimeTickUnit::Year, true),
        );
        let dates: Vec<_> = recording
            .calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Date {
                    locale,
                    options,
                    ms,
                } => Some((locale, options, ms)),
                Call::Number { .. } => None,
            })
            .collect();
        assert_eq!(dates.len(), 2);
        for (locale, options, ms) in &dates {
            assert_eq!(locale, "th");
            assert_eq!(options.calendar.as_deref(), Some("gregory"));
            assert_eq!(options.time_zone.as_deref(), Some("UTC"));
            assert_eq!(options.numbering_system.as_deref(), Some("latn"));
            assert_eq!(*ms, SEP_5_2026_1430);
        }
        let stamp = &dates[0].1;
        assert_eq!(stamp.year, Some(NumericStyle::TwoDigit));
        assert_eq!(stamp.month, Some(MonthStyle::Short));
        assert_eq!(dates[1].1.year, Some(NumericStyle::Numeric));
    }

    #[test]
    fn the_clock_is_24_hour_in_every_language_by_default() {
        let at = utc_ms(2026, 0, 1, 21, 5);
        for tag in ["en", "en-US"] {
            let t = translator(&english(), locale(tag));
            assert_eq!(
                t.format().time_tick(at, &tick(TimeTickUnit::Minute)),
                "21:05",
                "{tag}"
            );
        }
        let recording = RecordingIntl::new();
        let intl: Arc<dyn Intl> = recording.clone();
        for tag in ["ko", "hi", "ar"] {
            let t = translator(&intl, locale(tag));
            recording.clear();
            t.format().time_tick(at, &tick(TimeTickUnit::Minute));
            let calls = recording.calls();
            let Some(Call::Date { options, .. }) = calls.first() else {
                panic!("{tag}: no date request");
            };
            assert_eq!(options.hour_cycle, Some(HourCycle::H23), "{tag}");
            assert_eq!(options.hour, Some(NumericStyle::TwoDigit), "{tag}");
        }
        // 'locale' leaves the hour cycle to the engine.
        let follow = LocalizationOptions {
            hour_cycle: Some(HourCycleSetting::Locale),
            ..locale("hi")
        };
        let t = translator(&intl, follow);
        recording.clear();
        t.format().time_tick(at, &tick(TimeTickUnit::Minute));
        let calls = recording.calls();
        let Some(Call::Date { options, .. }) = calls.first() else {
            panic!("no date request");
        };
        assert_eq!(options.hour_cycle, None);
    }

    #[test]
    fn a_tick_labels_the_wall_clock_time_the_axis_aligned_it_with() {
        let t = translator(&fake(), locale("de"));
        let shifted = TimeTickFormatOptions {
            offset_ms: Some(3_600_000.0),
            ..tick(TimeTickUnit::Minute)
        };
        assert_eq!(
            t.format().time_tick(utc_ms(2026, 0, 1, 21, 5), &shifted),
            "22:05"
        );
    }

    #[test]
    fn a_locale_the_backend_rejects_reads_english() {
        let recording = RecordingIntl::new();
        recording.reject_locale("xx");
        let intl: Arc<dyn Intl> = recording.clone();
        let t = translator(&intl, locale("xx"));
        recording.clear();
        let text = t
            .format()
            .time_tick(SEP_5_2026_1430, &tick(TimeTickUnit::Minute));
        assert_eq!(text, format!("D[en]{SEP_5_2026_1430}"));
        let calls = recording.calls();
        assert_eq!(calls.len(), 2);
        let Some(Call::Date { options, .. }) = calls.get(1) else {
            panic!("no retry");
        };
        assert_eq!(
            options.numbering_system, None,
            "the English retry drops the numbering system"
        );
    }
}

mod separators_and_abbreviations {
    use super::*;

    #[test]
    fn german_prices_take_german_separators_and_axis_prices_stay_ungrouped() {
        let t = translator(&fake(), locale("de"));
        let f = t.format();
        assert_eq!(f.price(58_732.5, &price(2.0)), "58732,50");
        assert_eq!(
            f.price(
                58_732.5,
                &PriceFormatOptions {
                    grouping: Some(true),
                    ..price(2.0)
                }
            ),
            "58.732,50"
        );
        assert_eq!(
            f.price(
                2.5,
                &PriceFormatOptions {
                    mode: Some(PriceMode::Percent),
                    ..price(2.0)
                }
            ),
            "+2,50\u{a0}%"
        );
        assert_eq!(
            f.percent(1.0, &PercentFormatOptions::default()),
            "+1,00\u{a0}%"
        );
    }

    #[test]
    fn a_price_rounds_the_same_in_every_language() {
        for tag in ["de", "fr", "ja", "ar", "hi"] {
            let t = translator(&fake(), locale(tag));
            let f = t.format();
            assert_eq!(
                f.price(1.005, &price(2.0)).replace(',', "."),
                "1.00",
                "{tag}"
            );
            assert_eq!(
                f.price(2.675, &price(2.0)).replace(',', "."),
                "2.67",
                "{tag}"
            );
            assert_eq!(
                f.price(0.125, &price(2.0)).replace(',', "."),
                "0.13",
                "{tag}"
            );
        }
    }

    #[test]
    fn india_groups_in_lakhs_and_crores() {
        let t = translator(&english(), locale("en-IN"));
        let grouped = PriceFormatOptions {
            grouping: Some(true),
            ..price(2.0)
        };
        assert_eq!(t.format().price(12_345_678.9, &grouped), "1,23,45,678.90");
    }

    #[test]
    fn non_english_compact_asks_the_backend_for_compact_short() {
        let recording = RecordingIntl::new();
        let intl: Arc<dyn Intl> = recording.clone();
        let t = translator(&intl, locale("ja"));
        recording.clear();
        let text = t
            .format()
            .compact(1_234_567.0, &CompactFormatOptions::default());
        assert_eq!(text, "N[ja]1234567");
        let calls = recording.calls();
        let Some(Call::Number { options, .. }) = calls.last() else {
            panic!("no number request");
        };
        assert_eq!(options.notation, vela_core::intl::Notation::Compact);
        assert_eq!(
            options.compact_display,
            Some(vela_core::intl::CompactDisplay::Short)
        );
        assert_eq!(options.maximum_fraction_digits, Some(2));
        assert_eq!(options.minimum_fraction_digits, Some(0));
        let untrimmed = CompactFormatOptions {
            trim_zeros: Some(false),
            ..Default::default()
        };
        recording.clear();
        t.format().compact(1_200_000.0, &untrimmed);
        let calls = recording.calls();
        let Some(Call::Number { options, .. }) = calls.last() else {
            panic!("no number request");
        };
        assert_eq!(options.minimum_fraction_digits, Some(2));
    }

    #[test]
    fn non_english_spans_ask_for_narrow_units() {
        let recording = RecordingIntl::new();
        let intl: Arc<dyn Intl> = recording.clone();
        let t = translator(&intl, locale("de"));
        recording.clear();
        let span = DurationFormatOptions {
            style: Some(DurationStyle::Span),
            ..Default::default()
        };
        let text = t.format().duration(2.0 * DAY_MS + 4.0 * 3_600_000.0, &span);
        assert_eq!(text, "N[de]2 N[de]4");
        let units: Vec<_> = recording
            .calls()
            .into_iter()
            .filter_map(|c| match c {
                Call::Number { options, .. } => options.unit,
                Call::Date { .. } => None,
            })
            .collect();
        use vela_core::intl::NumberUnit::{Day, Hour};
        assert_eq!(units, [Day, Hour]);
    }
}

mod week_start_and_direction {
    use super::*;

    #[test]
    fn the_week_starts_on_the_locales_first_day_or_the_hosts_choice() {
        let intl = english();
        let week = |options: LocalizationOptions| translator(&intl, options).week_start();
        assert_eq!(week(locale("en")), 0);
        assert_eq!(week(locale("en-GB")), 1);
        assert_eq!(week(locale("de")), 1);
        assert_eq!(week(locale("ar")), 6);
        assert_eq!(week(locale("ja")), 0);
        assert_eq!(
            week(LocalizationOptions {
                week_start: Some(0),
                ..locale("de")
            }),
            0
        );
        // Only Sunday, Monday and Saturday are valid choices.
        assert_eq!(
            week(LocalizationOptions {
                week_start: Some(3),
                ..locale("de")
            }),
            1
        );
    }

    #[test]
    fn direction_auto_follows_the_script_and_the_default_stays_left_to_right_in_every_language() {
        let intl = english();
        let auto = |tag: &str| {
            translator(
                &intl,
                LocalizationOptions {
                    direction: Some(DirectionOption::Auto),
                    ..locale(tag)
                },
            )
            .direction()
        };
        use vela_core::i18n::TextDirection::{Ltr, Rtl};
        assert_eq!(auto("ar"), Rtl);
        assert_eq!(auto("he"), Rtl);
        assert_eq!(auto("fa"), Rtl);
        assert_eq!(auto("iw"), Rtl);
        assert_eq!(auto("ja"), Ltr);
        assert_eq!(translator(&intl, locale("ar")).direction(), Ltr);
        assert_eq!(
            translator(
                &intl,
                LocalizationOptions {
                    direction: Some(DirectionOption::Rtl),
                    ..locale("de")
                }
            )
            .direction(),
            Rtl
        );
        // A language the likely-subtags data lacks reads left to right.
        assert_eq!(auto("xx"), Ltr);
    }
}

mod host_formatters {
    use super::*;

    #[test]
    fn a_host_formatter_replaces_a_built_in_one_and_can_fall_back_to_it() {
        let formatters = VelaFormatters {
            price: Some(Arc::new(|_, ctx| {
                Some(if ctx.surface == FormatSurface::StatusLine {
                    format!("€ {}", ctx.fallback())
                } else {
                    ctx.fallback()
                })
            })),
            compact: Some(Arc::new(|v, _| Some(format!("{}k", (v / 1000.0).round())))),
            ..Default::default()
        };
        let t = translator(
            &fake(),
            LocalizationOptions {
                formatters: Some(formatters),
                ..locale("de")
            },
        );
        let f = t.format();
        assert_eq!(
            f.price(
                5.0,
                &PriceFormatOptions {
                    surface: Some(FormatSurface::StatusLine),
                    ..price(2.0)
                }
            ),
            "€ 5,00"
        );
        assert_eq!(f.price(5.0, &price(2.0)), "5,00");
        assert_eq!(f.compact(12_345.0, &CompactFormatOptions::default()), "12k");
        assert_eq!(
            f.percent(1.0, &PercentFormatOptions::default()),
            "+1,00\u{a0}%"
        );
    }

    #[test]
    fn a_host_formatter_that_fails_or_returns_no_text_falls_back_to_the_built_in_one() {
        let formatters = VelaFormatters {
            price: Some(Arc::new(|_, _| None)),
            time_tick: Some(Arc::new(|_, _| None)),
            ..Default::default()
        };
        let t = translator(
            &english(),
            LocalizationOptions {
                formatters: Some(formatters),
                ..Default::default()
            },
        );
        assert_eq!(t.format().price(1.5, &price(2.0)), "1.50");
        assert_eq!(
            t.format()
                .time_tick(SEP_5_2026_1430, &tick(TimeTickUnit::Minute)),
            "14:30"
        );
    }

    #[test]
    fn a_host_formatter_learns_where_the_value_appears() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = Arc::clone(&seen);
        let formatters = VelaFormatters {
            time_stamp: Some(Arc::new(move |_, ctx| {
                if let Ok(mut s) = log.lock() {
                    s.push(format!(
                        "{} {} {} {}",
                        ctx.surface.as_str(),
                        ctx.time_zone,
                        ctx.locale,
                        ctx.with_time
                    ));
                }
                Some(ctx.fallback())
            })),
            ..Default::default()
        };
        let t = translator(
            &english(),
            LocalizationOptions {
                formatters: Some(formatters),
                ..Default::default()
            },
        );
        let tokyo = TimeStampFormatOptions {
            time_zone: Some("Asia/Tokyo".into()),
            ..Default::default()
        };
        t.format().time_stamp(SEP_5_2026_1430, &tokyo);
        // A daily bar's date reads in UTC without a time.
        t.format().time_stamp(
            SEP_5_2026_1430,
            &TimeStampFormatOptions {
                with_time: Some(false),
                ..Default::default()
            },
        );
        let seen = seen.lock().map(|s| s.clone()).unwrap_or_default();
        assert_eq!(
            seen,
            ["crosshair Asia/Tokyo en true", "crosshair UTC en false"]
        );
    }

    #[test]
    fn each_method_reports_its_default_surface() {
        let seen: Arc<Mutex<Vec<&'static str>>> = Arc::default();
        let (a, b, c) = (Arc::clone(&seen), Arc::clone(&seen), Arc::clone(&seen));
        let record = |log: Arc<Mutex<Vec<&'static str>>>, surface: FormatSurface| {
            if let Ok(mut s) = log.lock() {
                s.push(surface.as_str());
            }
        };
        let formatters = VelaFormatters {
            duration: Some(Arc::new(move |_, ctx| {
                record(a.clone(), ctx.surface);
                None
            })),
            clock: Some(Arc::new(move |_, ctx| {
                record(b.clone(), ctx.surface);
                None
            })),
            time_tick: Some(Arc::new(move |_, ctx| {
                record(c.clone(), ctx.surface);
                None
            })),
            ..Default::default()
        };
        let t = translator(
            &english(),
            LocalizationOptions {
                formatters: Some(formatters),
                ..Default::default()
            },
        );
        t.format().duration(1000.0, &Default::default());
        t.format().clock(0.0, &Default::default());
        t.format().time_tick(0.0, &tick(TimeTickUnit::Day));
        assert_eq!(
            seen.lock().map(|s| s.clone()).unwrap_or_default(),
            ["lastPrice", "clock", "timeAxis"]
        );
    }
}
