//! Named visible-range shortcuts (`1M`, `YTD`, ...), resolved against the
//! loaded bars.

use serde::{Deserialize, Serialize};

use crate::indicators::VisibleRange;
use crate::model::{Millis, Ohlcv};

/// A named visible-range shortcut, resolved against the loaded bars.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VisibleRangePreset {
    #[serde(rename = "1D")]
    Day1,
    #[serde(rename = "1W")]
    Week1,
    #[serde(rename = "1M")]
    Month1,
    #[serde(rename = "3M")]
    Month3,
    #[serde(rename = "6M")]
    Month6,
    #[serde(rename = "1Y")]
    Year1,
    #[serde(rename = "5Y")]
    Year5,
    #[serde(rename = "YTD")]
    YearToDate,
    #[serde(rename = "ALL")]
    All,
}

impl VisibleRangePreset {
    pub const ALL: [VisibleRangePreset; 9] = [
        Self::Day1,
        Self::Week1,
        Self::Month1,
        Self::Month3,
        Self::Month6,
        Self::Year1,
        Self::Year5,
        Self::YearToDate,
        Self::All,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Day1 => "1D",
            Self::Week1 => "1W",
            Self::Month1 => "1M",
            Self::Month3 => "3M",
            Self::Month6 => "6M",
            Self::Year1 => "1Y",
            Self::Year5 => "5Y",
            Self::YearToDate => "YTD",
            Self::All => "ALL",
        }
    }

    /// The preset's fixed span, for the presets that have one.
    fn span_ms(self) -> Option<Millis> {
        Some(match self {
            Self::Day1 => DAY_MS,
            Self::Week1 => 7 * DAY_MS,
            Self::Month1 => 30 * DAY_MS,
            Self::Month3 => 90 * DAY_MS,
            Self::Month6 => 180 * DAY_MS,
            Self::Year1 => 365 * DAY_MS,
            Self::Year5 => 5 * 365 * DAY_MS,
            Self::YearToDate | Self::All => return None,
        })
    }
}

const DAY_MS: Millis = 86_400_000;

/// The proleptic Gregorian year containing the day `days` since 1970-01-01
/// (Howard Hinnant's `civil_from_days`, year part only).
fn civil_year_from_days(days: i64) -> i64 {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let year = year_of_era + era * 400;
    // January and February belong to the next calendar year in the March-based count.
    if month_from_march >= 10 {
        year + 1
    } else {
        year
    }
}

/// Days since 1970-01-01 of the civil date `year-month-day` (Howard Hinnant's
/// `days_from_civil`); `month` is 1 to 12.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y.rem_euclid(400);
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// `Date.UTC(year, 0, 1)` for the UTC year of `ms`. Like `Date.UTC`, a year in
/// `0..=99` means 1900 to 1999.
fn start_of_utc_year(ms: Millis) -> Millis {
    let year = civil_year_from_days(ms.div_euclid(DAY_MS));
    let year = if (0..=99).contains(&year) {
        year + 1900
    } else {
        year
    };
    days_from_civil(year, 1, 1).saturating_mul(DAY_MS)
}

/// Map a date-range preset to a `[from, to]` in epoch-ms from the loaded bars.
/// `to` is the last loaded bar; `from` is the preset's span before it, clamped
/// to the first loaded bar: the visible window cannot extend past loaded
/// history, so a deeper preset than the data simply frames everything
/// (fetching more is out of scope here). `YTD` starts at Jan 1 (UTC) of the
/// last bar's year. Returns `None` when there are no bars.
pub fn preset_to_range(preset: VisibleRangePreset, bars: &[Ohlcv]) -> Option<VisibleRange> {
    let first = bars.first()?.time;
    let last = bars.last()?.time;
    let from = match preset {
        VisibleRangePreset::All => {
            return Some(VisibleRange {
                from: first,
                to: last,
            });
        }
        VisibleRangePreset::YearToDate => start_of_utc_year(last),
        other => last.saturating_sub(other.span_ms().unwrap_or(0)),
    };
    Some(VisibleRange {
        from: from.max(first),
        to: last,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = DAY_MS;

    /// `Date.UTC(year, month0, day)` for the years the tests use.
    fn utc(year: i64, month0: i64, day: i64) -> i64 {
        days_from_civil(year, month0 + 1, day) * DAY
    }

    /// Bars one day apart, ending at `last`, `n` of them.
    fn bars(n: i64, last: i64) -> Vec<Ohlcv> {
        (0..n)
            .rev()
            .map(|i| Ohlcv::new(last - i * DAY, 1.0, 2.0, 0.5, 1.5, Some(1.0)))
            .collect()
    }

    fn fixture() -> (i64, Vec<Ohlcv>) {
        let last = utc(2024, 5, 15); // 2024-06-15
        (last, bars(400, last))
    }

    #[test]
    fn no_bars_gives_none() {
        assert_eq!(preset_to_range(VisibleRangePreset::Month1, &[]), None);
    }

    #[test]
    fn a_span_preset_frames_the_last_bar_as_to() {
        let (last, data) = fixture();
        let r = preset_to_range(VisibleRangePreset::Week1, &data).unwrap();
        assert_eq!(r.to, last);
        assert_eq!(r.from, last - 7 * DAY);
    }

    #[test]
    fn each_span_preset_maps_to_its_window_before_the_last_bar() {
        let (last, data) = fixture();
        let from = |p| preset_to_range(p, &data).unwrap().from;
        assert_eq!(from(VisibleRangePreset::Day1), last - DAY);
        assert_eq!(from(VisibleRangePreset::Month1), last - 30 * DAY);
        assert_eq!(from(VisibleRangePreset::Month3), last - 90 * DAY);
        assert_eq!(from(VisibleRangePreset::Month6), last - 180 * DAY);
        assert_eq!(from(VisibleRangePreset::Year1), last - 365 * DAY);
    }

    #[test]
    fn five_years_clamps_to_the_first_bar_when_history_is_shorter() {
        let (_, data) = fixture();
        let r = preset_to_range(VisibleRangePreset::Year5, &data).unwrap();
        assert_eq!(r.from, data[0].time);
    }

    #[test]
    fn from_is_clamped_to_the_first_loaded_bar_when_the_preset_is_deeper_than_history() {
        let (last, _) = fixture();
        let shallow = bars(10, last);
        let r = preset_to_range(VisibleRangePreset::Year1, &shallow).unwrap();
        assert_eq!(r.from, shallow[0].time);
        assert_eq!(r.to, last);
    }

    #[test]
    fn ytd_starts_at_jan_1_utc_of_the_last_bar_year() {
        let (last, data) = fixture();
        let r = preset_to_range(VisibleRangePreset::YearToDate, &data).unwrap();
        assert_eq!(r.from, utc(2024, 0, 1));
        assert_eq!(r.from, 1_704_067_200_000);
        assert_eq!(r.to, last);
    }

    #[test]
    fn ytd_is_clamped_to_the_first_bar_when_history_starts_mid_year() {
        let last = utc(2024, 5, 15);
        let data = bars(5, last);
        let r = preset_to_range(VisibleRangePreset::YearToDate, &data).unwrap();
        assert_eq!(r.from, data[0].time);
    }

    #[test]
    fn all_spans_the_first_to_the_last_bar() {
        let (last, data) = fixture();
        let r = preset_to_range(VisibleRangePreset::All, &data).unwrap();
        assert_eq!(r.from, data[0].time);
        assert_eq!(r.to, last);
    }

    #[test]
    fn a_single_bar_frames_itself() {
        let data = bars(1, utc(2024, 5, 15));
        for preset in VisibleRangePreset::ALL {
            let r = preset_to_range(preset, &data).unwrap();
            assert_eq!(
                (r.from, r.to),
                (data[0].time, data[0].time),
                "{}",
                preset.as_str()
            );
        }
    }

    #[test]
    fn preset_strings_round_trip_through_serde() {
        for preset in VisibleRangePreset::ALL {
            let json = serde_json::to_string(&preset).unwrap();
            assert_eq!(json, format!("\"{}\"", preset.as_str()));
            assert_eq!(
                serde_json::from_str::<VisibleRangePreset>(&json).unwrap(),
                preset
            );
        }
        assert!(serde_json::from_str::<VisibleRangePreset>("\"2Y\"").is_err());
    }

    // ---- the civil-calendar helpers ----

    #[test]
    fn days_from_civil_matches_known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2000, 1, 1), 10_957);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(2024, 1, 1), 19_723);
        assert_eq!(days_from_civil(2024, 3, 1), 19_783); // 2024 is a leap year
        assert_eq!(days_from_civil(1900, 1, 1), -25_567);
        assert_eq!(days_from_civil(1900, 3, 1), -25_567 + 31 + 28); // 1900 is not
    }

    #[test]
    fn civil_year_from_days_handles_year_boundaries_and_negative_days() {
        assert_eq!(civil_year_from_days(0), 1970);
        assert_eq!(civil_year_from_days(-1), 1969);
        assert_eq!(civil_year_from_days(19_722), 2023); // 2023-12-31
        assert_eq!(civil_year_from_days(19_723), 2024); // 2024-01-01
        assert_eq!(civil_year_from_days(19_723 + 365), 2024); // 2024-12-31
        assert_eq!(civil_year_from_days(19_723 + 366), 2025);
        assert_eq!(civil_year_from_days(-25_567), 1900);
        assert_eq!(civil_year_from_days(-25_568), 1899);
        assert_eq!(civil_year_from_days(11_016), 2000); // 2000-02-29
        assert_eq!(civil_year_from_days(days_from_civil(2100, 2, 28)), 2100);
    }

    #[test]
    fn the_two_helpers_agree_on_every_year_start() {
        for year in 1601..=2400 {
            let start = days_from_civil(year, 1, 1);
            assert_eq!(civil_year_from_days(start), year);
            assert_eq!(civil_year_from_days(start - 1), year - 1);
        }
    }

    #[test]
    fn start_of_utc_year_works_for_negative_timestamps() {
        assert_eq!(start_of_utc_year(-1), -DAY * 365); // 1969-12-31 -> 1969-01-01
        assert_eq!(start_of_utc_year(0), 0);
        assert_eq!(start_of_utc_year(utc(1960, 6, 1)), utc(1960, 0, 1));
        assert!(start_of_utc_year(i64::MAX) > 0);
        assert!(start_of_utc_year(i64::MIN) < 0);
    }

    #[test]
    fn a_year_in_0_to_99_means_1900_like_date_utc() {
        // Year 50 AD: `Date.UTC(50, 0, 1)` is 1950-01-01.
        let ms = days_from_civil(50, 6, 1) * DAY;
        assert_eq!(start_of_utc_year(ms), utc(1950, 0, 1));
    }
}
