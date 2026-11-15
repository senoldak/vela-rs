//! The free functions at the bottom of the original `NativeRenderer.ts`, and the
//! small JavaScript coercions its `applyFeature` relies on.

use serde_json::Value;

use crate::model::{
    IndicatorModel, Millis, Ohlcv, ScenePatch, SeriesData, SeriesSpec, SeriesValues,
};
use crate::render::core::color::parse_rgb;
use crate::render::core::scene_graph::{HighlightArea, ScaleMode, SessionZones};
use crate::util::js_number;

/// A theme color by key (empty when absent).
pub(crate) fn theme_string(theme: &Value, key: &str) -> String {
    theme
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Perceived-dark check (relative luminance below mid-gray) of the UI kit's
/// `isDarkColor`. Unparseable input counts as dark: the dark palette is the
/// default, so it is the safer assumption.
pub(crate) fn is_dark_color(color: &str) -> bool {
    match parse_rgb(color) {
        Some(c) => 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b < 128.0,
        None => true,
    }
}

/// JavaScript `Boolean(value)`.
pub(crate) fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// JavaScript `Number(value)` (NaN where it does not parse).
pub(crate) fn js_to_number(value: &Value) -> f64 {
    match value {
        Value::Null => 0.0,
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        Value::Array(_) | Value::Object(_) => f64::NAN,
    }
}

/// `Number(value) || 0`.
pub(crate) fn js_number_or_zero(value: &Value) -> f64 {
    let n = js_to_number(value);
    if n.is_nan() { 0.0 } else { n }
}

/// JavaScript `String(value)` for the primitives a feature value can be.
pub(crate) fn js_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.as_f64().map_or_else(|| n.to_string(), js_number),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

pub(crate) fn clamp01(v: f64) -> f64 {
    crate::indicators::classics::math::js_max(
        0.0,
        crate::indicators::classics::math::js_min(1.0, v),
    )
}

/// Coerce an untrusted value to a valid `ScaleMode`, defaulting to absolute `price`.
pub(crate) fn as_scale_mode(v: &Value) -> ScaleMode {
    match v.as_str() {
        Some("percent") => ScaleMode::Percent,
        Some("indexed") => ScaleMode::Indexed,
        _ => ScaleMode::Price,
    }
}

/// Sort ascending and dedupe by time (last write wins).
pub(crate) fn normalize_bars(bars: &[Ohlcv]) -> Vec<Ohlcv> {
    let mut sorted: Vec<Ohlcv> = bars.to_vec();
    sorted.sort_by_key(|b| b.time);
    let mut out: Vec<Ohlcv> = Vec::with_capacity(sorted.len());
    for bar in sorted {
        match out.last_mut() {
            Some(last) if last.time == bar.time => *last = bar,
            _ => out.push(bar),
        }
    }
    out
}

fn millis(n: f64) -> Millis {
    n as Millis
}

/// Coerce arbitrary input into a clean, time-ordered list of highlight bands.
pub(crate) fn sanitize_highlights(value: &Value) -> Vec<HighlightArea> {
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for raw in items {
        let Some(r) = raw.as_object() else { continue };
        let from = r.get("from").map_or(f64::NAN, js_to_number);
        let to = r.get("to").map_or(f64::NAN, js_to_number);
        if !from.is_finite() || !to.is_finite() || to <= from {
            continue;
        }
        let color = r
            .get("color")
            .and_then(Value::as_str)
            .unwrap_or("rgba(120,130,160,0.10)")
            .to_string();
        out.push(HighlightArea {
            from: millis(from),
            to: millis(to),
            color,
        });
    }
    out.sort_by_key(|h| h.from);
    out
}

/// Coerce arbitrary input into clean session bands, or `None` (no session structure).
pub(crate) fn sanitize_session_zones(value: &Value) -> Option<SessionZones> {
    let v = value.as_object()?;
    let windows = |raw: Option<&Value>| -> Vec<(Millis, Millis)> {
        let Some(items) = raw.and_then(Value::as_array) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for w in items {
            let Some(pair) = w.as_array() else { continue };
            let from = pair.first().map_or(f64::NAN, js_to_number);
            let to = pair.get(1).map_or(f64::NAN, js_to_number);
            if from.is_finite() && to.is_finite() && to > from {
                out.push((millis(from), millis(to)));
            }
        }
        out.sort_by_key(|w| w.0);
        out
    };
    Some(SessionZones {
        pre: windows(v.get("pre")),
        post: windows(v.get("post")),
        extended: windows(v.get("extended")),
    })
}

/// `(year, month, day, hour, minute)` of the UTC fields of `ms`.
fn utc_fields(ms: f64) -> (i64, i64, i64, i64, i64) {
    let total_minutes = (ms / 60_000.0).floor() as i64;
    let minute = total_minutes.rem_euclid(60);
    let hours = total_minutes.div_euclid(60);
    let hour = hours.rem_euclid(24);
    let days = hours.div_euclid(24);
    // days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day, hour, minute)
}

/// Split a zoned timestamp (already shifted so its UTC fields read the wall
/// clock) into a `YYYY-MM-DD` date and an `HH:MM` time.
pub(crate) fn format_stamp_parts(zoned_ms: f64) -> (String, String) {
    let (y, mo, d, h, mi) = utc_fields(zoned_ms);
    let p = |n: i64| {
        if n < 10 {
            format!("0{n}")
        } else {
            n.to_string()
        }
    };
    (
        format!("{y}-{}-{}", p(mo), p(d)),
        format!("{}:{}", p(h), p(mi)),
    )
}

/// How many of a model's own index-aligned values sit strictly BEFORE `head_time`:
/// the count to skip when the chart's first bar is later than the model's anchor.
/// `None` when the model carries no index-aligned series to measure.
pub(crate) fn leading_points_before(model: &IndicatorModel, head_time: Millis) -> Option<usize> {
    for s in &model.series {
        let times: Vec<Option<Millis>> = match &s.data {
            SeriesData::Candle { bars, .. } => bars.iter().map(|b| Some(b.time)).collect(),
            SeriesData::LineLike { points, .. } => points.iter().map(|p| Some(p.time)).collect(),
            SeriesData::Markers { .. } => continue,
        };
        if times.is_empty() {
            continue;
        }
        if let Some(count) = slots_before(&times, head_time) {
            return Some(count);
        }
    }
    None
}

/// How many of `times`' slots sit before `head_time`: a lower-bound search, or a
/// scan once it meets an empty slot. `None` when every slot is empty.
fn slots_before(times: &[Option<Millis>], head_time: Millis) -> Option<usize> {
    let (mut lo, mut hi) = (0usize, times.len());
    while lo < hi {
        let mid = (lo + hi) >> 1;
        let Some(t) = times[mid] else {
            return slots_before_scan(times, head_time);
        };
        if t < head_time {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    Some(lo)
}

/// [`slots_before`] over empty slots: the index of the first entry at or after
/// `head_time` when it IS the head bar, else one past the last entry before it.
fn slots_before_scan(times: &[Option<Millis>], head_time: Millis) -> Option<usize> {
    let mut last_before: i64 = -1;
    let mut any = false;
    for (i, t) in times.iter().enumerate() {
        let Some(t) = t else { continue };
        any = true;
        if *t >= head_time {
            return Some(if *t == head_time {
                i
            } else {
                (last_before + 1) as usize
            });
        }
        last_before = i as i64;
    }
    any.then_some(times.len())
}

/// Apply a scene patch to a stored indicator model so the next frame reflects it.
pub(crate) fn apply_patch(model: &mut IndicatorModel, patch: &ScenePatch) {
    match patch {
        ScenePatch::Value(vp) => {
            for delta in &vp.series {
                let Some(s) = model.series.iter_mut().find(|x| x.id == delta.series_id) else {
                    continue;
                };
                match (&delta.values, &mut s.data) {
                    (SeriesValues::Points(p), SeriesData::LineLike { points, .. }) => {
                        *points = p.clone();
                    }
                    (
                        SeriesValues::Bars { bars, bar_colors },
                        SeriesData::Candle {
                            bars: sb,
                            bar_colors: sc,
                            ..
                        },
                    ) => {
                        *sb = bars.clone();
                        if let Some(bc) = bar_colors {
                            *sc = Some(bc.clone());
                        }
                    }
                    _ => {}
                }
            }
            if let Some(v) = &vp.lines {
                model.lines = v.clone();
            }
            if let Some(v) = &vp.boxes {
                model.boxes = v.clone();
            }
            if let Some(v) = &vp.labels {
                model.labels = v.clone();
            }
            if let Some(v) = &vp.polylines {
                model.polylines = v.clone();
            }
            if let Some(v) = &vp.linefills {
                model.linefills = v.clone();
            }
            if let Some(v) = &vp.tables {
                model.tables = v.clone();
            }
            if let Some(v) = &vp.trades {
                model.trades = v.clone();
            }
        }
        ScenePatch::Schema(sp) => {
            for id in &sp.removed {
                if let Some(i) = model.series.iter().position(|x| &x.id == id) {
                    model.series.remove(i);
                }
            }
            for spec in &sp.added {
                let spec: SeriesSpec = spec.clone();
                model.series.push(spec);
            }
        }
    }
}
