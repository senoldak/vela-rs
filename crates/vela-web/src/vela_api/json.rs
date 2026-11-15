//! Conversions between JavaScript values and the core's types, and the chart event payloads.
//!
//! Values cross as JSON text (`JSON.stringify` / `JSON.parse`): the core types of the
//! documents the original exchanges (`SerializedDrawing`, `DrawingsDocument`, `SymbolInfo`,
//! `ProviderInfo`, ...) already carry the original's field names through serde.

/// A serde value as a JS value (`JSON.parse` of its JSON text). A macro, not a function: this
/// crate has no direct `serde` dependency to name the `Serialize` bound with.
macro_rules! ser {
    ($value:expr) => {
        match serde_json::to_string($value) {
            Ok(text) => js_sys::JSON::parse(&text).unwrap_or(wasm_bindgen::JsValue::UNDEFINED),
            Err(_) => wasm_bindgen::JsValue::UNDEFINED,
        }
    };
}

/// A JSON value as a core type, or a `TypeError` naming `what`.
macro_rules! typed {
    ($ty:ty, $value:expr, $what:expr) => {
        serde_json::from_value::<$ty>(($value).clone())
            .map_err(|e| $crate::vela_api::json::type_error(format!("[vela] {}: {e}", $what)))
    };
}

use js_sys::{Array, Function, Object, Reflect};
use serde_json::Value;
use vela_core::events::types::{ChartEvent, HistoryReason};
use vela_core::intl::TextDirection;
use vela_core::model::Ohlcv;
use vela_core::ports::chart_renderer::PaneMoveDir;
use wasm_bindgen::prelude::*;

/// An `Error` with a message (the original throws `Error` for a bad call).
pub(crate) fn error(message: impl AsRef<str>) -> JsValue {
    js_sys::Error::new(message.as_ref()).into()
}

/// A `TypeError`, for an argument of the wrong kind.
pub(crate) fn type_error(message: impl AsRef<str>) -> JsValue {
    js_sys::TypeError::new(message.as_ref()).into()
}

pub(crate) fn prop(o: &JsValue, key: &str) -> JsValue {
    if !o.is_object() {
        return JsValue::UNDEFINED;
    }
    Reflect::get(o, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

/// Whether `o` has `key` as a property, even one set to `undefined`.
pub(crate) fn has_prop(o: &JsValue, key: &str) -> bool {
    o.is_object() && Reflect::has(o, &JsValue::from_str(key)).unwrap_or(false)
}

pub(crate) fn present(v: &JsValue) -> bool {
    !v.is_undefined() && !v.is_null()
}

pub(crate) fn set(o: &Object, key: &str, value: JsValue) {
    let _ = Reflect::set(o, &JsValue::from_str(key), &value);
}

pub(crate) fn object(fields: &[(&str, JsValue)]) -> JsValue {
    let o = Object::new();
    for (k, v) in fields {
        set(&o, k, v.clone());
    }
    o.into()
}

pub(crate) fn opt_str(s: Option<&str>) -> JsValue {
    s.map_or(JsValue::NULL, JsValue::from_str)
}

pub(crate) fn opt_num(n: Option<f64>) -> JsValue {
    n.map_or(JsValue::NULL, JsValue::from_f64)
}

pub(crate) fn strings(list: &[String]) -> JsValue {
    list.iter()
        .map(|s| JsValue::from_str(s))
        .collect::<Array>()
        .into()
}

/// A JS value as JSON. `what` names it in the error.
pub(crate) fn from_js(v: &JsValue, what: &str) -> Result<Value, JsValue> {
    let text = js_sys::JSON::stringify(v)
        .ok()
        .and_then(|s| s.as_string())
        .ok_or_else(|| type_error(format!("[vela] {what} must be plain JSON data")))?;
    serde_json::from_str(&text).map_err(|e| type_error(format!("[vela] {what}: {e}")))
}

pub(crate) fn to_js(v: &Value) -> JsValue {
    js_sys::JSON::parse(&v.to_string()).unwrap_or(JsValue::UNDEFINED)
}

/// A plain `string[]`; `None` when `v` is not an array of strings.
pub(crate) fn string_list(v: &JsValue) -> Option<Vec<String>> {
    let list = v.dyn_ref::<Array>()?;
    list.iter().map(|x| x.as_string()).collect()
}

/// A bar as the original's `OHLCV`: `volume` only when there is one.
pub(crate) fn bar_js(bar: &Ohlcv) -> JsValue {
    let o = Object::new();
    set(&o, "time", JsValue::from_f64(bar.time as f64));
    set(&o, "open", JsValue::from_f64(bar.open));
    set(&o, "high", JsValue::from_f64(bar.high));
    set(&o, "low", JsValue::from_f64(bar.low));
    set(&o, "close", JsValue::from_f64(bar.close));
    if let Some(v) = bar.volume {
        set(&o, "volume", JsValue::from_f64(v));
    }
    o.into()
}

pub(crate) fn bars_js(bars: &[Ohlcv]) -> JsValue {
    bars.iter().map(bar_js).collect::<Array>().into()
}

pub(crate) fn direction_str(d: TextDirection) -> &'static str {
    match d {
        TextDirection::Rtl => "rtl",
        TextDirection::Ltr => "ltr",
    }
}

/// `console.error(message, detail)` (a handler that throws is logged, never rethrown).
pub(crate) fn console_error(message: &str, detail: &JsValue) {
    let global = js_sys::global();
    let console =
        Reflect::get(&global, &JsValue::from_str("console")).unwrap_or(JsValue::UNDEFINED);
    if let Ok(f) = Reflect::get(&console, &JsValue::from_str("error"))
        && let Some(f) = f.dyn_ref::<Function>()
    {
        let _ = f.call2(&console, &JsValue::from_str(message), detail);
    }
}

fn history_reason(r: HistoryReason) -> &'static str {
    match r {
        HistoryReason::Depth => "depth",
        HistoryReason::Genesis => "genesis",
        HistoryReason::Aborted => "aborted",
    }
}

fn dir_str(d: PaneMoveDir) -> &'static str {
    match d {
        PaneMoveDir::Up => "up",
        PaneMoveDir::Down => "down",
    }
}

/// A script run as the original's `ScriptRun` (the two history loaders, `trades()` and
/// `series(key)`, are not carried: no scripting engine runs in this build, so no run is
/// ever produced).
pub(crate) fn script_run_js(run: &vela_core::script_run::ScriptRun) -> JsValue {
    let plots = Object::new();
    for (k, v) in &run.plots {
        set(&plots, k, v.map_or(JsValue::NULL, JsValue::from_f64));
    }
    let vars = Object::new();
    for (k, v) in &run.vars {
        set(&vars, k, to_js(v));
    }
    let mut fields = vec![
        ("id", JsValue::from_str(&run.id)),
        ("title", JsValue::from_str(&run.title)),
        ("kind", ser!(&run.kind)),
        ("cause", ser!(&run.cause)),
        ("first", JsValue::from_bool(run.first)),
        ("bar", JsValue::from_f64(run.bar as f64)),
        ("time", JsValue::from_f64(run.time as f64)),
        ("forming", JsValue::from_bool(run.forming)),
        ("complete", JsValue::from_bool(run.complete)),
        ("plots", plots.into()),
        ("vars", vars.into()),
        ("warnings", ser!(&run.warnings)),
    ];
    if let Some(strategy) = &run.strategy {
        fields.push(("strategy", ser!(strategy)));
    }
    object(&fields)
}

/// The payload a handler of `event` receives, as the original's `VelaEventMap` shapes it:
/// `undefined` for `ready`, `pane:changed` and `replay:pause`.
pub(crate) fn payload(event: &ChartEvent) -> JsValue {
    use ChartEvent as E;
    let s = JsValue::from_str;
    let n = |v: f64| JsValue::from_f64(v);
    match event {
        E::Ready | E::PaneChanged | E::ReplayPause => JsValue::UNDEFINED,
        E::MarketChanged {
            symbol,
            timeframe,
            prev,
        } => object(&[
            ("symbol", s(symbol)),
            ("timeframe", s(timeframe)),
            (
                "prev",
                object(&[
                    ("symbol", s(&prev.symbol)),
                    ("timeframe", s(&prev.timeframe)),
                ]),
            ),
        ]),
        E::LoadStart {
            symbol,
            timeframe,
            first_load,
        } => object(&[
            ("symbol", s(symbol)),
            ("timeframe", s(timeframe)),
            ("firstLoad", JsValue::from_bool(*first_load)),
        ]),
        E::LoadEnd {
            symbol,
            timeframe,
            bars,
        } => object(&[
            ("symbol", s(symbol)),
            ("timeframe", s(timeframe)),
            ("bars", n(*bars as f64)),
        ]),
        E::IndicatorAdded { id }
        | E::IndicatorRemoved { id }
        | E::IndicatorInputs { id }
        | E::DrawingCreated { id }
        | E::DrawingEdited { id }
        | E::DrawingRemoved { id }
        | E::DrawingSettings { id }
        | E::ContextChanged { id } => object(&[("id", s(id))]),
        E::IndicatorError { id, error: message } => {
            object(&[("id", s(id)), ("error", error(message))])
        }
        E::DataUnresolved { symbol, providers } => {
            object(&[("symbol", s(symbol)), ("providers", strings(providers))])
        }
        E::DataUnavailable {
            symbol,
            timeframe,
            reason,
            finest,
            default,
        } => {
            let o = Object::new();
            set(&o, "symbol", s(symbol));
            set(&o, "timeframe", s(timeframe));
            set(&o, "reason", s(reason.as_str()));
            if let Some(f) = finest {
                set(&o, "finest", s(f));
            }
            if let Some(d) = default {
                set(&o, "default", s(d));
            }
            o.into()
        }
        E::SymbolFacts { symbol, facts } => object(&[
            ("symbol", s(symbol)),
            ("facts", facts.as_ref().map_or(JsValue::NULL, |f| ser!(f))),
        ]),
        E::IndicatorMoved { id, pane_id } => object(&[("id", s(id)), ("paneId", s(pane_id))]),
        E::IndicatorVisibility { id, visible } => {
            object(&[("id", s(id)), ("visible", JsValue::from_bool(*visible))])
        }
        E::ThemeChanged(theme) => to_js(theme),
        E::LocaleChanged(change) => object(&[
            ("locale", s(&change.locale)),
            ("direction", s(direction_str(change.direction))),
        ]),
        E::PriceStyleChange { from, to } => object(&[("from", s(from)), ("to", s(to))]),
        E::PaneMoved { pane_id, dir } => {
            object(&[("paneId", s(pane_id)), ("dir", s(dir_str(*dir)))])
        }
        E::DrawingDraft { doc } => {
            object(&[("doc", doc.as_ref().map_or(JsValue::NULL, |d| ser!(d)))])
        }
        E::DrawingSelected { id, ids } => {
            object(&[("id", opt_str(id.as_deref())), ("ids", strings(ids))])
        }
        E::DrawingFavorites { favorites } => object(&[("favorites", strings(favorites))]),
        E::DrawingDefaults { type_key } | E::DrawingTemplates { type_key } => {
            object(&[("type", s(type_key))])
        }
        E::DrawingTool { type_key } => object(&[("type", opt_str(type_key.as_deref()))]),
        E::DrawingSnap { mode } => object(&[("mode", s(snap_str(*mode)))]),
        E::DrawingStay { on } => object(&[("on", JsValue::from_bool(*on))]),
        E::DrawingMode { mode } => object(&[("mode", mode.map_or(JsValue::NULL, |m| ser!(&m)))]),
        E::MarkClick(click) => ser!(click),
        E::ScriptRun(run) => script_run_js(run),
        E::Bar(bar) => bar_js(bar),
        E::ViewportChanged { from, to } => {
            object(&[("from", n(*from as f64)), ("to", n(*to as f64))])
        }
        E::HistoryProgress { loaded, target } => {
            object(&[("loaded", n(*loaded as f64)), ("target", n(*target as f64))])
        }
        E::HistoryComplete {
            reason,
            oldest_time,
            bars_loaded,
        } => object(&[
            ("reason", s(history_reason(*reason))),
            ("oldestTime", n(*oldest_time as f64)),
            ("barsLoaded", n(*bars_loaded as f64)),
        ]),
        E::ReplayStart {
            cursor_time,
            remaining,
        }
        | E::ReplayStep {
            cursor_time,
            remaining,
        } => object(&[
            ("cursorTime", n(*cursor_time as f64)),
            ("remaining", n(*remaining as f64)),
        ]),
        E::ReplayTick {
            cursor_time,
            index,
            count,
        } => object(&[
            ("cursorTime", n(*cursor_time as f64)),
            ("index", n(*index as f64)),
            ("count", n(*count as f64)),
        ]),
        E::ReplayPlay { interval_ms } => object(&[("intervalMs", n(*interval_ms))]),
        E::ReplayEnd { reason } => object(&[("reason", s(replay_end_str(*reason)))]),
        E::Alert(alert) => ser!(alert),
        E::Warning(warning) => ser!(warning),
    }
}

pub(crate) fn snap_str(mode: vela_core::drawings::SnapMode) -> &'static str {
    use vela_core::drawings::SnapMode as M;
    match mode {
        M::Off => "off",
        M::Weak => "weak",
        M::Strong => "strong",
    }
}

pub(crate) fn replay_end_str(reason: vela_core::events::types::ReplayEndReason) -> &'static str {
    use vela_core::events::types::ReplayEndReason as R;
    match reason {
        R::Stopped => "stopped",
        R::Finished => "finished",
        R::Market => "market",
    }
}
