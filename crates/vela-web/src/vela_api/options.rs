//! `new Vela(container, options, deps)`: reading the original's option names and nesting into
//! [`VelaOptions`], and refusing, with a clear error, what cannot cross from JavaScript.
//!
//! What a plain-JavaScript host cannot give this build, and what the constructor says about it:
//!
//! | Input | Why | Error |
//! |-------|-----|-------|
//! | `options.renderer` (a renderer class) | the only renderer is the native one | `options.renderer is not supported` |
//! | `deps.renderer` (a renderer instance) | same | `deps.renderer is not supported` |
//! | `deps.engines` (scripting engines) | no engine runs JavaScript or Pine here | `deps.engines is not supported` |
//! | `options.formatters` (formatter functions) | a formatter is a callback into the core's text | `options.formatters is not supported` |
//! | `options.priceAxis.ticks` (a function) | same | `options.priceAxis.ticks is not supported` |
//! | `options.freePan` as a predicate | same | `options.freePan as a function is not supported` |
//!
//! `deps.dataFeed` IS supported when it is a plain object with `load` and `subscribe`.
//!
//! Extensions of this build (the original has no such option on `Vela`): `bars` given as an
//! object `{ SYMBOL: [bars] | { timeframe: [bars] } }` seeds an in-memory provider (the
//! workspace's `bars`; a number keeps the original's meaning, the history depth), `symbols`
//! describes those symbols for the symbol list, and `providers: ['binance' | 'coinbase' |
//! 'hyperliquid']` registers the bundled exchange providers (they reach their endpoints).

use std::collections::HashMap;
use std::rc::Rc;

use js_sys::Array;
use vela_core::format::types::HourCycleSetting;
use vela_core::i18n::{DirectionOption, LocalizationOptions, LocalizationPatch, Patch};
use vela_core::indicators::VisibleRange;
use vela_core::options::{
    AddIndicatorOptions, AddIndicatorPane, AnimationConfig, AnimationValue, AnimationsOption,
    FreePanTrigger, InitialRange, IntroConfig, IntroSetting, IntroStyle, MarketSession,
    MarketSwitch, SettingsVisibilityPolicy, ThemeSpec, VelaOptions,
};
use vela_core::ports::data_provider::SymbolDescriptor;
use vela_core::visible_range::VisibleRangePreset;
use wasm_bindgen::prelude::*;

use super::controls::{bar_list, drawings_option_of};
use super::json::{self, error, from_js, has_prop, present, prop, type_error};
use crate::app::inputs_of;
use crate::workspace_ui::{MemoryProvider, SymbolSeries};

fn not_supported(what: &str, why: &str) -> JsValue {
    error(format!(
        "[vela] {what} is not supported by this build: {why}"
    ))
}

/// What the constructor reads besides the chart options.
pub(crate) struct Parsed {
    pub options: VelaOptions,
    pub memory: Option<Rc<MemoryProvider>>,
    pub providers: Vec<String>,
}

/// The `deps` that cross: a feed object. The others are refused.
pub(crate) fn check_deps(deps: &JsValue) -> Result<JsValue, JsValue> {
    if !present(deps) {
        return Ok(JsValue::UNDEFINED);
    }
    if !deps.is_object() {
        return Err(type_error(
            "[vela] the third argument (deps) must be an object",
        ));
    }
    if present(&prop(deps, "renderer")) {
        return Err(not_supported(
            "deps.renderer",
            "a renderer instance cannot cross from JavaScript; only the built-in native renderer exists",
        ));
    }
    let engines = prop(deps, "engines");
    if present(&engines) && engines.dyn_ref::<Array>().is_none_or(|a| a.length() > 0) {
        return Err(not_supported(
            "deps.engines",
            "scripting engines are Rust types here and none runs scripts from JavaScript; the chart shows candles, drawings and native indicators",
        ));
    }
    Ok(prop(deps, "dataFeed"))
}

fn parse_preset(name: &str) -> Option<VisibleRangePreset> {
    VisibleRangePreset::ALL
        .into_iter()
        .find(|p| p.as_str() == name)
}

/// `visibleRange`: a preset name or `{ from, to }` (epoch-ms).
pub(crate) fn initial_range(v: &JsValue) -> Result<InitialRange, JsValue> {
    if let Some(name) = v.as_string() {
        return parse_preset(&name)
            .map(InitialRange::Preset)
            .ok_or_else(|| {
                type_error(format!(
                    "[vela] visibleRange: unknown preset \"{name}\" (use {})",
                    VisibleRangePreset::ALL.map(|p| p.as_str()).join(", ")
                ))
            });
    }
    let from = prop(v, "from").as_f64().filter(|n| n.is_finite());
    let to = prop(v, "to").as_f64().filter(|n| n.is_finite());
    match (from, to) {
        (Some(from), Some(to)) => Ok(InitialRange::Range(VisibleRange {
            from: from as i64,
            to: to as i64,
        })),
        _ => Err(type_error(
            "[vela] visibleRange must be a preset name or { from, to } in epoch-ms",
        )),
    }
}

pub(crate) fn preset_of(name: &str) -> Result<VisibleRangePreset, JsValue> {
    parse_preset(name).ok_or_else(|| {
        type_error(format!(
            "[vela] setVisibleRangePreset: unknown preset \"{name}\" (use {})",
            VisibleRangePreset::ALL.map(|p| p.as_str()).join(", ")
        ))
    })
}

fn theme_spec(v: &JsValue) -> Result<ThemeSpec, JsValue> {
    if let Some(id) = v.as_string() {
        return Ok(ThemeSpec::Id(id));
    }
    if v.is_object() {
        return Ok(ThemeSpec::Theme(from_js(v, "theme")?));
    }
    Err(type_error(
        "[vela] theme must be a theme id (\"dark\", \"light\", a registered id) or a theme object",
    ))
}

/// `setTheme(theme)` and `options.theme`.
pub(crate) fn theme_of(v: &JsValue) -> Result<ThemeSpec, JsValue> {
    theme_spec(v)
}

fn anim_value(v: &JsValue) -> Option<AnimationValue> {
    if let Some(b) = v.as_bool() {
        Some(AnimationValue::Enabled(b))
    } else {
        v.as_f64().map(AnimationValue::Ms)
    }
}

fn animations_of(v: &JsValue) -> Option<AnimationsOption> {
    if let Some(b) = v.as_bool() {
        return Some(AnimationsOption::Enabled(b));
    }
    if !v.is_object() {
        return None;
    }
    let intro = prop(v, "intro");
    let intro = if let Some(b) = intro.as_bool() {
        Some(IntroSetting::Enabled(b))
    } else if let Some(s) = intro.as_string() {
        match s.as_str() {
            "settle" => Some(IntroSetting::Style(IntroStyle::Settle)),
            "grow" => Some(IntroSetting::Style(IntroStyle::Grow)),
            _ => Some(IntroSetting::Enabled(false)),
        }
    } else if intro.is_object() {
        Some(IntroSetting::Config(IntroConfig {
            style: match prop(&intro, "style").as_string().as_deref() {
                Some("grow") => Some(IntroStyle::Grow),
                Some("settle") => Some(IntroStyle::Settle),
                _ => None,
            },
            duration: prop(&intro, "duration").as_f64(),
        }))
    } else {
        None
    };
    Some(AnimationsOption::Config(AnimationConfig {
        zoom: anim_value(&prop(v, "zoom")),
        pan: anim_value(&prop(v, "pan")),
        scroll: anim_value(&prop(v, "scroll")),
        autoscale: anim_value(&prop(v, "autoscale")),
        live_bar: anim_value(&prop(v, "liveBar")),
        intro,
    }))
}

fn free_pan_of(v: &JsValue) -> Result<Option<FreePanTrigger>, JsValue> {
    if v.is_function() {
        return Err(not_supported(
            "options.freePan as a function",
            "a predicate cannot be called from the renderer; use true, 'drag', 'alt', 'shift' or 'mod'",
        ));
    }
    if let Some(b) = v.as_bool() {
        return Ok(Some(FreePanTrigger::Enabled(b)));
    }
    Ok(match v.as_string().as_deref() {
        Some("drag") => Some(FreePanTrigger::Drag),
        Some("alt") => Some(FreePanTrigger::Alt),
        Some("shift") => Some(FreePanTrigger::Shift),
        Some("mod") => Some(FreePanTrigger::Mod),
        _ => None,
    })
}

fn direction_of(v: &JsValue) -> Result<DirectionOption, JsValue> {
    match v.as_string().as_deref() {
        Some("ltr") => Ok(DirectionOption::Ltr),
        Some("rtl") => Ok(DirectionOption::Rtl),
        Some("auto") => Ok(DirectionOption::Auto),
        _ => Err(type_error(
            "[vela] direction must be \"ltr\", \"rtl\" or \"auto\"",
        )),
    }
}

fn hour_cycle_of(v: &JsValue) -> Result<HourCycleSetting, JsValue> {
    match v.as_string().as_deref() {
        Some("h23") => Ok(HourCycleSetting::H23),
        Some("h12") => Ok(HourCycleSetting::H12),
        Some("locale") => Ok(HourCycleSetting::Locale),
        _ => Err(type_error(
            "[vela] hourCycle must be \"h23\", \"h12\" or \"locale\"",
        )),
    }
}

fn week_start_of(v: &JsValue) -> Option<u8> {
    v.as_f64()
        .filter(|n| matches!(*n as i64, 0 | 1 | 6) && n.fract() == 0.0)
        .map(|n| n as u8)
}

fn messages_of(v: &JsValue) -> Result<HashMap<String, String>, JsValue> {
    if !v.is_object() {
        return Err(type_error("[vela] messages must be an object of id: text"));
    }
    Ok(js_sys::Object::entries(v.unchecked_ref())
        .iter()
        .filter_map(|e| {
            let pair: Array = e.unchecked_into();
            Some((pair.get(0).as_string()?, pair.get(1).as_string()?))
        })
        .collect())
}

fn reject_formatters(o: &JsValue) -> Result<(), JsValue> {
    if present(&prop(o, "formatters")) {
        return Err(not_supported(
            "options.formatters",
            "a formatter is a JavaScript function called from the core's text; the built-in formatters follow locale, numberingSystem and hourCycle",
        ));
    }
    Ok(())
}

/// `setLocale(next)`: a tag, or any localization fields (an explicit `undefined` restores the
/// default, as in the original). Returns the patch and the locale tag whose pack to ensure.
pub(crate) fn locale_patch(next: &JsValue) -> Result<(LocalizationPatch, Option<String>), JsValue> {
    if let Some(tag) = next.as_string() {
        return Ok((LocalizationPatch::locale(tag.clone()), Some(tag)));
    }
    if !next.is_object() {
        return Err(type_error(
            "[vela] setLocale takes a locale tag or an object of localization options",
        ));
    }
    reject_formatters(next)?;
    let mut patch = LocalizationPatch::default();
    let mut tag = None;
    // A key that is present decides; one set to `undefined` clears.
    macro_rules! field {
        ($key:literal, $slot:ident, $parse:expr) => {
            if has_prop(next, $key) {
                let v = prop(next, $key);
                patch.$slot = if v.is_undefined() {
                    Patch::Clear
                } else {
                    Patch::Set($parse(&v)?)
                };
            }
        };
    }
    if has_prop(next, "locale") {
        let v = prop(next, "locale");
        patch.locale = if v.is_undefined() {
            Patch::Clear
        } else {
            let s = v
                .as_string()
                .ok_or_else(|| type_error("[vela] locale must be a string"))?;
            tag = Some(s.clone());
            Patch::Set(s)
        };
    }
    field!("direction", direction, direction_of);
    field!("messages", messages, messages_of);
    field!("numberingSystem", numbering_system, |v: &JsValue| v
        .as_string()
        .ok_or_else(|| type_error(
            "[vela] numberingSystem must be a string"
        )));
    field!("hourCycle", hour_cycle, hour_cycle_of);
    field!("weekStart", week_start, |v: &JsValue| week_start_of(v)
        .ok_or_else(|| type_error(
            "[vela] weekStart must be 0, 1 or 6"
        )));
    Ok((patch, tag))
}

fn localization_of(o: &JsValue) -> Result<LocalizationOptions, JsValue> {
    reject_formatters(o)?;
    let mut out = LocalizationOptions::default();
    if present(&prop(o, "locale")) {
        out.locale = Some(
            prop(o, "locale")
                .as_string()
                .ok_or_else(|| type_error("[vela] options.locale must be a string"))?,
        );
    }
    if present(&prop(o, "direction")) {
        out.direction = Some(direction_of(&prop(o, "direction"))?);
    }
    if present(&prop(o, "messages")) {
        out.messages = Some(messages_of(&prop(o, "messages"))?);
    }
    out.numbering_system = prop(o, "numberingSystem").as_string();
    if present(&prop(o, "hourCycle")) {
        out.hour_cycle = Some(hour_cycle_of(&prop(o, "hourCycle"))?);
    }
    out.week_start = week_start_of(&prop(o, "weekStart"));
    Ok(out)
}

// The two readers below follow `workspace_ui/api.rs` (`series_of`, the `symbols` loop): that
// file keeps them private.
fn series_of(value: &JsValue) -> Result<SymbolSeries, JsValue> {
    if value.is_array() {
        return Ok(SymbolSeries::new().with_any(bar_list(value, "bars")?));
    }
    let mut series = SymbolSeries::new();
    for e in js_sys::Object::entries(value.unchecked_ref()).iter() {
        let pair: Array = e.unchecked_into();
        let timeframe = pair.get(0).as_string().unwrap_or_default();
        let bars = pair.get(1);
        if !bars.is_array() {
            return Err(type_error(format!(
                "[vela] bars for \"{timeframe}\" must be an array of bars"
            )));
        }
        series = series.with_timeframe(&timeframe, bar_list(&bars, "bars")?);
    }
    Ok(series)
}

fn entries(o: &JsValue) -> Vec<(String, JsValue)> {
    if !o.is_object() {
        return Vec::new();
    }
    js_sys::Object::entries(o.unchecked_ref())
        .iter()
        .filter_map(|e| {
            let pair: Array = e.unchecked_into();
            Some((pair.get(0).as_string()?, pair.get(1)))
        })
        .collect()
}

/// Read the constructor's options.
pub(crate) fn parse(o: &JsValue) -> Result<Parsed, JsValue> {
    let mut options = VelaOptions::default();
    let mut memory = None;
    let mut providers = Vec::new();
    if !present(o) {
        return Ok(Parsed {
            options,
            memory,
            providers,
        });
    }
    if !o.is_object() {
        return Err(type_error(
            "[vela] the second argument (options) must be an object",
        ));
    }
    if present(&prop(o, "renderer")) {
        return Err(not_supported(
            "options.renderer",
            "a renderer class cannot cross from JavaScript; only the built-in native renderer exists (omit the option)",
        ));
    }
    let price_axis = prop(o, "priceAxis");
    if present(&price_axis) && present(&prop(&price_axis, "ticks")) {
        return Err(not_supported(
            "options.priceAxis.ticks",
            "the tick function would be called from the renderer; the built-in ticks are used",
        ));
    }

    // market
    let string = |key: &str| -> Result<Option<String>, JsValue> {
        let v = prop(o, key);
        if !present(&v) {
            return Ok(None);
        }
        v.as_string()
            .map(Some)
            .ok_or_else(|| type_error(format!("[vela] options.{key} must be a string")))
    };
    options.market.symbol = string("symbol")?;
    options.market.timeframe = string("timeframe")?;
    let bars = prop(o, "bars");
    if bars.is_object() {
        let mem = MemoryProvider::new("memory");
        let mut first: Option<(String, Option<String>)> = None;
        for (symbol, value) in entries(&bars) {
            let series = series_of(&value)?;
            if first.is_none() {
                let tf = if value.is_object() && !value.is_array() {
                    entries(&value).first().map(|(k, _)| k.clone())
                } else {
                    None
                };
                first = Some((symbol.clone(), tf));
            }
            mem.set_symbol(&symbol, series);
        }
        for (symbol, info) in entries(&prop(o, "symbols")) {
            mem.set_info(
                &symbol,
                SymbolDescriptor {
                    description: prop(&info, "description").as_string(),
                    kind: prop(&info, "type").as_string(),
                    prefix: prop(&info, "prefix").as_string(),
                    ..SymbolDescriptor::default()
                },
            );
        }
        if let Some((symbol, tf)) = first {
            options.market.symbol.get_or_insert(symbol);
            if options.market.timeframe.is_none() {
                options.market.timeframe = tf;
            }
        }
        memory = Some(mem);
    } else if let Some(n) = bars.as_f64() {
        if !(n.is_finite() && n > 0.0) {
            return Err(type_error("[vela] options.bars must be a positive number"));
        }
        options.market.bars = Some(n as usize);
    }
    if let Some(s) = prop(o, "session").as_string() {
        options.market.session = MarketSession::normalize(&s);
    }
    let range = prop(o, "visibleRange");
    if present(&range) {
        options.market.visible_range = Some(initial_range(&range)?);
    }
    let data = prop(o, "data");
    if present(&data) {
        options.market.data = Some(bar_list(&data, "options.data")?);
    }
    if let Some(list) = prop(o, "providers").dyn_ref::<Array>() {
        for name in list.iter() {
            let name = name
                .as_string()
                .ok_or_else(|| type_error("[vela] options.providers lists provider names"))?;
            if !matches!(name.as_str(), "binance" | "coinbase" | "hyperliquid") {
                return Err(error(format!(
                    "[vela] unknown provider \"{name}\" (use \"binance\", \"coinbase\" or \"hyperliquid\")"
                )));
            }
            if !providers.contains(&name) {
                providers.push(name);
            }
        }
    }

    // display and behavior
    options.localization = localization_of(o)?;
    options.live = prop(o, "live").as_bool();
    if present(&prop(o, "theme")) {
        options.theme = Some(theme_spec(&prop(o, "theme"))?);
    }
    options.default_language = string("defaultLanguage")?;
    options.current_price_line = prop(o, "currentPriceLine").as_bool();
    options.log_scale = prop(o, "logScale").as_bool();
    if let Some(name) = prop(o, "nativeBackend").as_string() {
        options.native_backend = Some(match name.as_str() {
            "auto" => vela_core::options::NativeBackend::Auto,
            "canvas2d" => vela_core::options::NativeBackend::Canvas2d,
            "webgl2" => vela_core::options::NativeBackend::Webgl2,
            other => {
                return Err(type_error(format!(
                    "[vela] options.nativeBackend must be \"auto\", \"canvas2d\" or \"webgl2\" (got \"{other}\")"
                )));
            }
        });
    }
    options.animations = animations_of(&prop(o, "animations"));
    options.glow = prop(o, "glow")
        .as_f64()
        .filter(|g| g.is_finite() && *g >= 0.0);
    options.up_color = string("upColor")?;
    options.down_color = string("downColor")?;
    options.price_style = string("priceStyle")?;
    if present(&prop(o, "drawings")) {
        options.drawings = Some(drawings_option_of(&prop(o, "drawings"))?);
    }
    options.volume = prop(o, "volume").as_bool();
    let settings = prop(o, "settings");
    if present(&settings) {
        let hidden = match prop(&settings, "hidden") {
            h if present(&h) => Some(json::string_list(&h).ok_or_else(|| {
                type_error("[vela] options.settings.hidden must be an array of setting ids")
            })?),
            _ => None,
        };
        options.settings = Some(SettingsVisibilityPolicy { hidden });
    }
    if present(&prop(o, "freePan")) {
        options.free_pan = free_pan_of(&prop(o, "freePan"))?;
    }
    // `height` is declared by the original's options and read nowhere: the container's own
    // size drives the layout. Accepted and ignored here too.
    Ok(Parsed {
        options,
        memory,
        providers,
    })
}

/// `chart.setMarket(next)`.
pub(crate) fn market_switch(next: &JsValue) -> Result<MarketSwitch, JsValue> {
    if !next.is_object() {
        return Err(type_error(
            "[vela] setMarket takes an object: { symbol?, timeframe?, bars?, session?, data?, visibleRange? }",
        ));
    }
    let text = |key: &str| -> Result<Option<String>, JsValue> {
        let v = prop(next, key);
        if !present(&v) {
            return Ok(None);
        }
        v.as_string()
            .map(Some)
            .ok_or_else(|| type_error(format!("[vela] setMarket: `{key}` must be a string")))
    };
    let mut out = MarketSwitch {
        symbol: text("symbol")?,
        timeframe: text("timeframe")?,
        ..MarketSwitch::default()
    };
    if let Some(n) = prop(next, "bars").as_f64() {
        if !(n.is_finite() && n > 0.0) {
            return Err(type_error(
                "[vela] setMarket: `bars` must be a positive number",
            ));
        }
        out.bars = Some(n as usize);
    }
    if let Some(s) = prop(next, "session").as_string() {
        out.session = MarketSession::normalize(&s);
    }
    let data = prop(next, "data");
    if present(&data) {
        out.data = Some(bar_list(&data, "setMarket data")?);
    }
    let range = prop(next, "visibleRange");
    if present(&range) {
        out.visible_range = Some(initial_range(&range)?);
    }
    Ok(out)
}

/// `chart.addIndicator(source, options)`.
pub(crate) fn add_indicator_options(o: &JsValue) -> Result<AddIndicatorOptions, JsValue> {
    let mut out = AddIndicatorOptions::default();
    if !present(o) {
        return Ok(out);
    }
    if !o.is_object() {
        return Err(type_error("[vela] addIndicator options must be an object"));
    }
    if present(&prop(o, "id")) {
        out.id =
            Some(prop(o, "id").as_string().ok_or_else(|| {
                type_error("[vela] addIndicator: `id` must be a non-empty string")
            })?);
    }
    out.language = prop(o, "language").as_string();
    if present(&prop(o, "inputs")) {
        out.inputs = Some(inputs_of(&prop(o, "inputs")));
    }
    if present(&prop(o, "props")) {
        out.props = Some(inputs_of(&prop(o, "props")));
    }
    out.overlay = prop(o, "overlay").as_bool();
    out.pane = match prop(o, "pane").as_string().as_deref() {
        Some("price") => Some(AddIndicatorPane::Price),
        Some("new") => Some(AddIndicatorPane::New),
        _ => None,
    };
    out.title = prop(o, "title").as_string();
    Ok(out)
}
