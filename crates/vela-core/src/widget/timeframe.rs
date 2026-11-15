//! Timeframe grammar: the pure, DOM-free half of the timeframe picker ("type a number to
//! change timeframe"). An optional count followed by an optional unit letter; a bare number
//! is MINUTES; a lone unit letter implies a count of 1. Units are case-insensitive:
//!
//! ```text
//! (none) -> minutes    S -> seconds    H -> hours    D -> days
//! W -> weeks           M -> months     Y -> years
//! ```
//!
//! e.g. `"3"` is 3 minutes, `"30"` 30 minutes, `"1h"` 1 hour, `"3M"` 3 months, `"S"` 1 second.

use crate::i18n::{MessageArg, MessageLike, Translator, localize_number_text, msg, text_of};
use crate::util::js_number;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeframeUnit {
    S,
    Min,
    H,
    D,
    W,
    M,
    Y,
}

impl TimeframeUnit {
    /// The unit's key as the original spells it (`"MIN"` for minutes).
    pub fn as_str(self) -> &'static str {
        match self {
            TimeframeUnit::S => "S",
            TimeframeUnit::Min => "MIN",
            TimeframeUnit::H => "H",
            TimeframeUnit::D => "D",
            TimeframeUnit::W => "W",
            TimeframeUnit::M => "M",
            TimeframeUnit::Y => "Y",
        }
    }

    /// Per-unit duration in ms. Month = 30 days, year = 365 days: matches the core's bucketing.
    fn ms(self) -> f64 {
        match self {
            TimeframeUnit::S => 1_000.0,
            TimeframeUnit::Min => 60_000.0,
            TimeframeUnit::H => 3_600_000.0,
            TimeframeUnit::D => 86_400_000.0,
            TimeframeUnit::W => 604_800_000.0,
            TimeframeUnit::M => 2_592_000_000.0,
            TimeframeUnit::Y => 31_536_000_000.0,
        }
    }

    /// Minutes per unit: how each unit collapses to the canonical bare-minute timeframe.
    fn minutes(self) -> Option<f64> {
        Some(match self {
            TimeframeUnit::S => return None,
            TimeframeUnit::Min => 1.0,
            TimeframeUnit::H => 60.0,
            TimeframeUnit::D => 1440.0,
            TimeframeUnit::W => 10_080.0,
            TimeframeUnit::M => 43_200.0,
            TimeframeUnit::Y => 525_600.0,
        })
    }

    /// A count of the unit in words ("3 months"); `count` picks the plural form, `n` is
    /// the count as shown.
    fn name(self) -> MessageLike {
        match self {
            TimeframeUnit::S => msg(
                "widget.timeframe.seconds",
                "{count, plural, one {{n} second} other {{n} seconds}}",
            ),
            TimeframeUnit::Min => msg(
                "widget.timeframe.minutes",
                "{count, plural, one {{n} minute} other {{n} minutes}}",
            ),
            TimeframeUnit::H => msg(
                "widget.timeframe.hours",
                "{count, plural, one {{n} hour} other {{n} hours}}",
            ),
            TimeframeUnit::D => msg(
                "widget.timeframe.days",
                "{count, plural, one {{n} day} other {{n} days}}",
            ),
            TimeframeUnit::W => msg(
                "widget.timeframe.weeks",
                "{count, plural, one {{n} week} other {{n} weeks}}",
            ),
            TimeframeUnit::M => msg(
                "widget.timeframe.months",
                "{count, plural, one {{n} month} other {{n} months}}",
            ),
            TimeframeUnit::Y => msg(
                "widget.timeframe.years",
                "{count, plural, one {{n} year} other {{n} years}}",
            ),
        }
    }

    /// The compact topbar label ("30m", "1D", "3M"): a count and the unit's letter.
    fn short(self) -> MessageLike {
        match self {
            TimeframeUnit::S => msg("widget.timeframe.short.seconds", "{n}s"),
            TimeframeUnit::Min => msg("widget.timeframe.short.minutes", "{n}m"),
            TimeframeUnit::H => msg("widget.timeframe.short.hours", "{n}h"),
            TimeframeUnit::D => msg("widget.timeframe.short.days", "{n}D"),
            TimeframeUnit::W => msg("widget.timeframe.short.weeks", "{n}W"),
            TimeframeUnit::M => msg("widget.timeframe.short.months", "{n}M"),
            TimeframeUnit::Y => msg("widget.timeframe.short.years", "{n}Y"),
        }
    }
}

/// The compact label of `count` units in `i18n`'s language.
fn short_label(unit: TimeframeUnit, count: f64, i18n: Option<&dyn Translator>) -> String {
    text_of(
        i18n,
        &unit.short(),
        &[(
            "n",
            MessageArg::from(localize_number_text(&js_number(count), i18n)),
        )],
    )
}

fn unit_of(letter: &str) -> Option<TimeframeUnit> {
    Some(match letter {
        "" => TimeframeUnit::Min, // a bare number is minutes
        "S" => TimeframeUnit::S,
        "H" => TimeframeUnit::H,
        "D" => TimeframeUnit::D,
        "W" => TimeframeUnit::W,
        "M" => TimeframeUnit::M,
        "Y" => TimeframeUnit::Y,
        _ => return None,
    })
}

/// `^(\d*)\s*([a-zA-Z]?)$`: the digits and the unit letter (upper-cased).
fn split_timeframe(raw: &str) -> Option<(&str, String)> {
    let digits_end = raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len());
    let (digits, rest) = raw.split_at(digits_end);
    let rest = rest.trim_start_matches(char::is_whitespace);
    let mut chars = rest.chars();
    match (chars.next(), chars.next()) {
        (None, _) => Some((digits, String::new())),
        (Some(c), None) if c.is_ascii_alphabetic() => {
            Some((digits, c.to_ascii_uppercase().to_string()))
        }
        _ => None,
    }
}

/// Count and unit of a timeframe text, or `None` for garbage (count below 1, a bare
/// unit-less empty number, an unknown letter).
fn count_and_unit(text: &str) -> Option<(f64, TimeframeUnit)> {
    let (digits, letter) = split_timeframe(text)?;
    let unit = unit_of(&letter)?;
    // A bare unit with no number means "1 of it"; a bare number needs its digits.
    if digits.is_empty() && unit == TimeframeUnit::Min {
        return None;
    }
    let count = if digits.is_empty() {
        1.0
    } else {
        digits.parse::<f64>().ok()?
    };
    if !count.is_finite() || count < 1.0 {
        return None;
    }
    Some((count, unit))
}

/// The canonical timeframe string the chart consumes. Every unit from minutes up collapses
/// to BARE MINUTES: the one form providers and the core read identically (a provider's
/// interval table knows `M`, not every `NM` count). Seconds pass through as `NS`.
fn canonical_for(unit: TimeframeUnit, count: f64) -> String {
    match unit.minutes() {
        None => format!("{}S", js_number(count)),
        Some(m) => js_number(count * m),
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedTimeframe {
    pub valid: bool,
    pub count: Option<f64>,
    pub unit: Option<TimeframeUnit>,
    pub ms: Option<f64>,
    /// What the chart consumes (`"180"` for 3h).
    pub canonical: Option<String>,
    /// Human line (`"3 months"`).
    pub label: Option<String>,
    /// Compact chip (`"3M"`).
    pub short: Option<String>,
}

/// Parse a typed timeframe string. `valid` is false for empty/garbage input (count >= 1).
/// The labels read in `i18n`'s language (English without one).
pub fn parse_timeframe(text: &str, i18n: Option<&dyn Translator>) -> ParsedTimeframe {
    let raw = text.trim();
    let Some((count, unit)) = count_and_unit(raw) else {
        return ParsedTimeframe::default();
    };
    let n = MessageArg::from(localize_number_text(&js_number(count), i18n));
    ParsedTimeframe {
        valid: true,
        count: Some(count),
        unit: Some(unit),
        ms: Some(count * unit.ms()),
        canonical: Some(canonical_for(unit, count)),
        label: Some(text_of(
            i18n,
            &unit.name(),
            &[("count", MessageArg::Number(count)), ("n", n)],
        )),
        short: Some(short_label(unit, count, i18n)),
    }
}

/// A data source's timeframe (`"1D"`, `"W"`, `"1M"`, `"3M"`, `"12M"`, `"240"`, `"30S"`) in the
/// form the chart consumes: `D`, `W` or `M` for one day, week or month (the default presets'
/// own values) and the canonical bare minutes for any other count (`"3M"` becomes `"129600"`).
/// Unit letters read as sources write them, upper-case: `M` is a month. `None` when unparsed.
pub fn chart_timeframe_of(value: Option<&str>) -> Option<String> {
    let parsed = parse_timeframe(value?, None);
    if !parsed.valid {
        return None;
    }
    if parsed.count == Some(1.0)
        && let Some(unit @ (TimeframeUnit::D | TimeframeUnit::W | TimeframeUnit::M)) = parsed.unit
    {
        return Some(unit.as_str().to_string());
    }
    parsed.canonical
}

/// Duration in ms of an existing timeframe value (`1`, `60`, `4h`, `D`, `W`, ...), NaN if unknown.
pub fn timeframe_ms(value: &str) -> f64 {
    match count_and_unit(value.trim()) {
        Some((count, unit)) => count * unit.ms(),
        None => f64::NAN,
    }
}

/// Favorite chips in duration order (shortest first). First-seen wins on duplicates;
/// values that do not parse sort last, keeping their relative order. The current
/// timeframe is included when it is a favorite so the highlight can sit in place.
pub fn favorite_timeframe_chips<S: AsRef<str>>(favorites: &[S]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for tf in favorites {
        if seen.insert(tf.as_ref().to_string()) {
            out.push(tf.as_ref().to_string());
        }
    }
    out.sort_by(|a, b| {
        let (ma, mb) = (timeframe_ms(a), timeframe_ms(b));
        let (a_ok, b_ok) = (ma.is_finite(), mb.is_finite());
        if a_ok && b_ok && ma != mb {
            return ma.total_cmp(&mb);
        }
        if a_ok != b_ok {
            return if a_ok {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        std::cmp::Ordering::Equal
    });
    out
}

/// Compact display label for any timeframe value (`"60"` becomes `"1h"`, `"D"` `"1D"`), in
/// `i18n`'s language (English without one).
pub fn timeframe_label(value: &str, i18n: Option<&dyn Translator>) -> String {
    let parsed = parse_timeframe(value, i18n);
    let (Some(ms), Some(short)) = (parsed.ms, parsed.short) else {
        return value.to_string();
    };
    // Prefer the largest unit that divides evenly (60 -> 1h, 240 -> 4h, 1440 -> 1D).
    const ORDER: [TimeframeUnit; 7] = [
        TimeframeUnit::Y,
        TimeframeUnit::M,
        TimeframeUnit::W,
        TimeframeUnit::D,
        TimeframeUnit::H,
        TimeframeUnit::Min,
        TimeframeUnit::S,
    ];
    for unit in ORDER {
        if ms % unit.ms() == 0.0 && ms / unit.ms() >= 1.0 {
            return short_label(unit, ms / unit.ms(), i18n);
        }
    }
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canon(text: &str) -> Option<String> {
        parse_timeframe(text, None).canonical
    }

    #[test]
    fn a_bare_number_is_minutes() {
        let p = parse_timeframe("3", None);
        assert!(p.valid);
        assert_eq!(p.count, Some(3.0));
        assert_eq!(p.unit, Some(TimeframeUnit::Min));
        assert_eq!(p.ms, Some(180_000.0));
        assert_eq!(p.canonical.as_deref(), Some("3"));
        assert_eq!(p.label.as_deref(), Some("3 minutes"));
        assert_eq!(p.short.as_deref(), Some("3m"));
    }

    #[test]
    fn units_are_case_insensitive_and_collapse_to_bare_minutes() {
        assert_eq!(canon("1h").as_deref(), Some("60"));
        assert_eq!(canon("3H").as_deref(), Some("180"));
        assert_eq!(canon("1d").as_deref(), Some("1440"));
        assert_eq!(canon("1W").as_deref(), Some("10080"));
        assert_eq!(canon("3M").as_deref(), Some("129600"));
        assert_eq!(canon("1y").as_deref(), Some("525600"));
        assert_eq!(canon("30S").as_deref(), Some("30S"));
    }

    #[test]
    fn a_lone_unit_letter_implies_one() {
        let p = parse_timeframe("S", None);
        assert_eq!(p.count, Some(1.0));
        assert_eq!(p.label.as_deref(), Some("1 second"));
        assert_eq!(parse_timeframe("w", None).label.as_deref(), Some("1 week"));
        assert_eq!(
            parse_timeframe("3M", None).label.as_deref(),
            Some("3 months")
        );
    }

    #[test]
    fn garbage_is_invalid() {
        for text in [
            "", "  ", "0", "0h", "h3", "3x", "3 hh", "-3", "1.5", "abc", "3 4", "\u{663}",
        ] {
            assert!(!parse_timeframe(text, None).valid, "{text:?}");
        }
        // Whitespace between the count and the unit is allowed.
        assert!(parse_timeframe(" 3  h ", None).valid);
    }

    #[test]
    fn chart_timeframe_of_keeps_the_single_unit_forms() {
        assert_eq!(chart_timeframe_of(Some("1D")).as_deref(), Some("D"));
        assert_eq!(chart_timeframe_of(Some("W")).as_deref(), Some("W"));
        assert_eq!(chart_timeframe_of(Some("1M")).as_deref(), Some("M"));
        assert_eq!(chart_timeframe_of(Some("3M")).as_deref(), Some("129600"));
        assert_eq!(chart_timeframe_of(Some("240")).as_deref(), Some("240"));
        assert_eq!(chart_timeframe_of(Some("30S")).as_deref(), Some("30S"));
        assert_eq!(chart_timeframe_of(Some("2D")).as_deref(), Some("2880"));
        assert_eq!(chart_timeframe_of(Some("nope")), None);
        assert_eq!(chart_timeframe_of(None), None);
    }

    #[test]
    fn timeframe_ms_reads_existing_values() {
        assert_eq!(timeframe_ms("1"), 60_000.0);
        assert_eq!(timeframe_ms("60"), 3_600_000.0);
        assert_eq!(timeframe_ms("4h"), 14_400_000.0);
        assert_eq!(timeframe_ms("D"), 86_400_000.0);
        assert_eq!(timeframe_ms("W"), 604_800_000.0);
        assert!(timeframe_ms("").is_nan());
        assert!(timeframe_ms("x").is_nan());
        assert!(timeframe_ms("0").is_nan());
    }

    #[test]
    fn favorites_sort_by_duration_dedupe_and_park_garbage_last() {
        let chips = favorite_timeframe_chips(&["D", "5", "1h", "5", "junk", "1", "also-junk"]);
        assert_eq!(chips, ["1", "5", "1h", "D", "junk", "also-junk"]);
        let none: [&str; 0] = [];
        assert!(favorite_timeframe_chips(&none).is_empty());
    }

    #[test]
    fn timeframe_label_prefers_the_largest_even_unit() {
        assert_eq!(timeframe_label("60", None), "1h");
        assert_eq!(timeframe_label("240", None), "4h");
        assert_eq!(timeframe_label("1440", None), "1D");
        assert_eq!(timeframe_label("D", None), "1D");
        assert_eq!(timeframe_label("W", None), "1W");
        assert_eq!(timeframe_label("5", None), "5m");
        assert_eq!(timeframe_label("30S", None), "30s");
        assert_eq!(timeframe_label("43200", None), "1M");
        assert_eq!(timeframe_label("525600", None), "1Y");
        assert_eq!(timeframe_label("nonsense", None), "nonsense");
    }

    #[test]
    fn a_huge_count_does_not_panic() {
        let digits = "9".repeat(400);
        assert!(!parse_timeframe(&digits, None).valid);
        let big = format!("{}h", "9".repeat(30));
        assert!(parse_timeframe(&big, None).valid);
    }
}
