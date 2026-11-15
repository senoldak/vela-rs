//! Per-drawing timeframe visibility. A drawing may be limited to some
//! timeframes (a weekly level that should stay off a one-minute chart).
//! Timeframes are grouped into bands, each covering every bar duration from
//! its own up to the next band's, so any timeframe lands in exactly one band.

use serde_json::Value;

use crate::i18n::{MessageLike, msg};

#[derive(Clone, Debug, PartialEq)]
pub struct TimeframeBand {
    /// Stable key persisted on the drawing.
    pub key: &'static str,
    /// Short label for a picker.
    pub label: MessageLike,
    /// The shortest bar duration (ms) in the band; it runs up to the next band's.
    pub from_ms: f64,
}

const MIN: f64 = 60_000.0;
const HOUR: f64 = 60.0 * MIN;
const DAY: f64 = 24.0 * HOUR;

/// Band keys and their shortest bar duration, shortest first.
const BANDS: [(&str, f64); 9] = [
    ("s", 0.0),
    ("1", MIN),
    ("5", 5.0 * MIN),
    ("15", 15.0 * MIN),
    ("60", HOUR),
    ("240", 4.0 * HOUR),
    ("D", DAY),
    ("W", 7.0 * DAY),
    ("M", 28.0 * DAY),
];

/// The timeframe bands, shortest first.
pub fn timeframe_bands() -> Vec<TimeframeBand> {
    let labels = [
        msg("drawing.showOn.seconds", "Sec"),
        msg("drawing.showOn.minute1", "1m"),
        msg("drawing.showOn.minute5", "5m"),
        msg("drawing.showOn.minute15", "15m"),
        msg("drawing.showOn.hour1", "1H"),
        msg("drawing.showOn.hour4", "4H"),
        msg("drawing.showOn.day", "D"),
        msg("drawing.showOn.week", "W"),
        msg("drawing.showOn.month", "M"),
    ];
    BANDS
        .iter()
        .zip(labels)
        .map(|(&(key, from_ms), label)| TimeframeBand {
            key,
            label,
            from_ms,
        })
        .collect()
}

/// The band a bar duration falls in, or `None` when the duration is unknown.
pub fn timeframe_band_of(bar_ms: f64) -> Option<&'static str> {
    // Written as a negated `>` so a NaN duration counts as unknown.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    if !(bar_ms > 0.0) {
        return None;
    }
    BANDS
        .iter()
        .rev()
        .find(|(_, from_ms)| bar_ms >= *from_ms)
        .map(|(key, _)| *key)
}

/// Whether a drawing limited to `show_on` shows on a chart whose bars last
/// `bar_ms`. No limit, or an unknown chart timeframe, always shows.
pub fn shown_on_timeframe(show_on: Option<&[String]>, bar_ms: f64) -> bool {
    let Some(show_on) = show_on else { return true };
    match timeframe_band_of(bar_ms) {
        None => true,
        Some(band) => show_on.iter().any(|k| k == band),
    }
}

/// Keep the known band keys of a list, in band order, without repeats.
pub fn sanitize_show_on_keys<S: AsRef<str>>(keys: &[S]) -> Vec<String> {
    BANDS
        .iter()
        .map(|(key, _)| *key)
        .filter(|key| keys.iter().any(|k| k.as_ref() == *key))
        .map(str::to_string)
        .collect()
}

/// Coerce an untrusted value into a band list (known keys, band order, no
/// repeats); `None` (every timeframe) for anything that is not an array.
pub fn sanitize_show_on(v: &Value) -> Option<Vec<String>> {
    let items = v.as_array()?;
    let wanted: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
    Some(sanitize_show_on_keys(&wanted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_duration_lands_in_one_band() {
        assert_eq!(timeframe_band_of(1_000.0), Some("s"));
        assert_eq!(timeframe_band_of(60_000.0), Some("1"));
        assert_eq!(timeframe_band_of(3.0 * 60_000.0), Some("1"));
        assert_eq!(timeframe_band_of(2.0 * HOUR), Some("60"));
        assert_eq!(timeframe_band_of(DAY), Some("D"));
        assert_eq!(timeframe_band_of(40.0 * DAY), Some("M"));
        assert_eq!(timeframe_band_of(0.0), None);
        assert_eq!(timeframe_band_of(f64::NAN), None);
    }

    #[test]
    fn a_limited_drawing_shows_only_on_its_bands() {
        let weekly = vec!["W".to_string()];
        assert!(shown_on_timeframe(Some(&weekly), 7.0 * DAY));
        assert!(!shown_on_timeframe(Some(&weekly), MIN));
        assert!(shown_on_timeframe(Some(&weekly), 0.0)); // unknown chart timeframe
        assert!(shown_on_timeframe(None, MIN));
    }

    #[test]
    fn sanitizing_keeps_known_keys_in_band_order() {
        let v = serde_json::json!(["W", "bogus", 5, "1", "W"]);
        assert_eq!(
            sanitize_show_on(&v),
            Some(vec!["1".to_string(), "W".to_string()])
        );
        assert_eq!(sanitize_show_on(&serde_json::json!("W")), None);
        assert_eq!(sanitize_show_on(&serde_json::json!([])), Some(vec![]));
        assert_eq!(timeframe_bands().len(), 9);
    }
}
