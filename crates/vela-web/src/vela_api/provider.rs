//! Data sources written in JavaScript, adapted to the core's traits.
//!
//! [`JsProvider`] is a plain object with the original's `DataProvider` method names
//! (`getBars` required; `getBarsProgressive`, `info`, `listSymbols`, `getSymbolInfo`,
//! `resolveSymbolIcon`, `subscribe`, `getCalendar`, `configure`, `capabilitiesFor`
//! optional). [`JsFeed`] is a plain object with the original's `MarketDataFeed` names
//! (`load` and `subscribe` required; `loadRange`, `symbolInfo`, `onUnresolved`, `destroy`
//! optional). Methods that return promises may return plain values too.
//!
//! A rejected promise (or a throw) is a failed fetch: logged and read as an empty answer,
//! as the original reads a thrown provider error. An error that says so, one with
//! `name === 'DataUnavailableError'` and a string `reason` (a [`DataUnavailableError`], or
//! any object with those fields, like the original's duck-typed check), is the series
//! refused for good: the chart stops asking and fires `data:unavailable`.
//!
//! The classes of the original's `browser.ts` bundle, `BinanceProvider`, `CoinbaseProvider`
//! and `HyperliquidProvider`, are exported here as thin tokens: `registerProvider` builds the
//! provider they name on the chart's platform. They reach their exchange endpoints once
//! registered.

use std::rc::Rc;

use js_sys::{Array, Function, Object, Promise, Reflect};
use serde_json::Value;
use vela_core::data_unavailable::{
    DATA_UNAVAILABLE_ERROR_NAME, DataUnavailableDetails, DataUnavailableError,
    DataUnavailableReason,
};
use vela_core::model::Ohlcv;
use vela_core::ports::data_provider::{
    CalendarRange, CalendarWindows, DataProvider, ProviderCapabilities, ProviderInfo,
    SymbolDescriptor,
};
use vela_core::ports::market_data::{
    BarRange, BarSink, BatchSink, InitialRange, MarketConfig, MarketDataFeed, SymbolInfo,
    UnresolvedSymbol,
};
use vela_core::runtime::{AbortSignal, DataError, LocalBoxFuture, Platform, Unsubscribe};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use super::json::{self, console_error, from_js, prop};
use crate::app::bars_of;

// ── helpers ──

fn function_of(obj: &JsValue, name: &str) -> Option<Function> {
    prop(obj, name).dyn_into::<Function>().ok()
}

/// Call `obj[name](...args)` with `obj` as `this`; `None` when it is not a function.
fn call(obj: &JsValue, name: &str, args: &[JsValue]) -> Option<Result<JsValue, JsValue>> {
    let f = function_of(obj, name)?;
    let list: Array = args.iter().cloned().collect();
    Some(f.apply(obj, &list))
}

/// The settled value of a promise, or the value itself.
pub(crate) async fn settle(value: JsValue) -> Result<JsValue, JsValue> {
    JsFuture::from(Promise::resolve(&value)).await
}

pub(crate) fn message_of(err: &JsValue) -> String {
    if let Some(m) = prop(err, "message").as_string() {
        return m;
    }
    err.as_string().unwrap_or_else(|| format!("{err:?}"))
}

/// The rejection as the core reads it: a refused series, or a failed fetch.
fn data_error(err: &JsValue) -> DataError {
    match unavailable_of(err) {
        Some(e) => DataError::Unavailable(Box::new(e)),
        None => DataError::Other(message_of(err)),
    }
}

/// `isDataUnavailableError`: the name, a string reason and message, and optional string
/// fields, read from the value (never its class).
fn unavailable_of(err: &JsValue) -> Option<DataUnavailableError> {
    if !err.is_object() {
        return None;
    }
    let mut map = serde_json::Map::new();
    for key in ["name", "reason", "message"] {
        if let Some(s) = prop(err, key).as_string() {
            map.insert(key.to_string(), Value::String(s));
        }
    }
    for key in ["symbol", "timeframe", "finest", "default"] {
        let v = prop(err, key);
        if v.is_undefined() {
            continue;
        }
        // A present non-string fails the check, as in the original.
        map.insert(
            key.to_string(),
            v.as_string().map_or(Value::Bool(true), Value::String),
        );
    }
    DataUnavailableError::from_value(&Value::Object(map))
}

fn parse_bars(value: &JsValue) -> Result<Vec<Ohlcv>, DataError> {
    let list = value
        .dyn_ref::<Array>()
        .ok_or_else(|| DataError::Other("the source must answer with an array of bars".into()))?;
    bars_of(list).map_err(|e| DataError::Other(message_of(&e)))
}

fn bar_of(value: &JsValue) -> Result<Ohlcv, String> {
    let list = Array::of1(value);
    match bars_of(&list) {
        Ok(mut bars) if !bars.is_empty() => Ok(bars.remove(0)),
        Ok(_) => Err("a live update must be a bar".into()),
        Err(e) => Err(message_of(&e)),
    }
}

/// A JS `AbortSignal` that aborts when the core's does (a plain `{ aborted }` object where
/// the runtime has no `AbortController`).
pub(crate) fn js_signal(signal: Option<&AbortSignal>) -> JsValue {
    let Some(signal) = signal else {
        return JsValue::UNDEFINED;
    };
    let ctor = Reflect::get(&js_sys::global(), &JsValue::from_str("AbortController"))
        .unwrap_or(JsValue::UNDEFINED);
    let Some(ctor) = ctor.dyn_ref::<Function>() else {
        return json::object(&[("aborted", JsValue::from_bool(signal.aborted()))]);
    };
    let Ok(controller) = Reflect::construct(ctor, &Array::new()) else {
        return JsValue::UNDEFINED;
    };
    let out = prop(&controller, "signal");
    signal.on_abort(move || {
        let _ = call(&controller, "abort", &[]);
    });
    out
}

/// A JS function that forwards each call to `f`; keep it alive as long as JS may call it.
fn callback(f: impl FnMut(JsValue) + 'static) -> Closure<dyn FnMut(JsValue)> {
    Closure::<dyn FnMut(JsValue)>::new(f)
}

/// A subscription that ends by calling the JS unsubscribe the source returned (if any) and
/// then dropping what it held.
fn unsubscribe_of(result: JsValue, keep: Closure<dyn FnMut(JsValue)>) -> Unsubscribe {
    Box::new(move || {
        if let Some(f) = result.dyn_ref::<Function>() {
            let _ = f.call0(&JsValue::NULL);
        }
        drop(keep);
    })
}

fn market_config_js(cfg: &MarketConfig) -> JsValue {
    let o = Object::new();
    if let Some(s) = &cfg.symbol {
        json::set(&o, "symbol", JsValue::from_str(s));
    }
    if let Some(s) = &cfg.timeframe {
        json::set(&o, "timeframe", JsValue::from_str(s));
    }
    if let Some(n) = cfg.bars {
        json::set(&o, "bars", JsValue::from_f64(n as f64));
    }
    if let Some(s) = cfg.session {
        json::set(&o, "session", JsValue::from_str(s.as_str()));
    }
    match cfg.visible_range {
        Some(InitialRange::Preset(p)) => {
            json::set(&o, "visibleRange", JsValue::from_str(p.as_str()))
        }
        Some(InitialRange::Range(r)) => json::set(
            &o,
            "visibleRange",
            json::object(&[
                ("from", JsValue::from_f64(r.from as f64)),
                ("to", JsValue::from_f64(r.to as f64)),
            ]),
        ),
        None => {}
    }
    if let Some(data) = &cfg.data {
        json::set(&o, "data", json::bars_js(data));
    }
    o.into()
}

type BarsFuture = LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>>;

/// A promise-returning call as a future of bars.
fn bars_future(result: Result<JsValue, JsValue>) -> BarsFuture {
    Box::pin(async move {
        match result {
            Err(e) => Err(data_error(&e)),
            Ok(v) => match settle(v).await {
                Ok(list) => parse_bars(&list),
                Err(e) => Err(data_error(&e)),
            },
        }
    })
}

// ── a data provider written in JavaScript ──

/// A [`DataProvider`] over a JS object. See the module documentation.
pub(crate) struct JsProvider {
    obj: JsValue,
}

impl JsProvider {
    /// `Err` when `getBars` is missing.
    pub(crate) fn new(obj: JsValue) -> Result<Rc<Self>, JsValue> {
        if function_of(&obj, "getBars").is_none() {
            return Err(json::type_error(
                "[vela] a data provider must implement getBars(ticker, timeframe, range)",
            ));
        }
        Ok(Rc::new(Self { obj }))
    }

    fn has(&self, name: &str) -> bool {
        function_of(&self.obj, name).is_some()
    }
}

impl DataProvider for JsProvider {
    fn get_bars(&self, ticker: &str, timeframe: &str, range: &BarRange) -> BarsFuture {
        bars_future(
            call(
                &self.obj,
                "getBars",
                &[
                    JsValue::from_str(ticker),
                    JsValue::from_str(timeframe),
                    ser!(range),
                ],
            )
            .unwrap_or_else(|| Err(json::type_error("[vela] getBars is not a function"))),
        )
    }

    fn get_bars_progressive(
        &self,
        ticker: &str,
        timeframe: &str,
        range: &BarRange,
        on_batch: BatchSink,
        signal: Option<AbortSignal>,
    ) -> Option<BarsFuture> {
        if !self.has("getBarsProgressive") {
            return None;
        }
        let sink = Rc::new(std::cell::RefCell::new(on_batch));
        let batches = {
            let sink = sink.clone();
            callback(move |list| match parse_bars(&list) {
                Ok(bars) => (sink.borrow_mut())(bars),
                Err(e) => console_error(
                    "[vela] getBarsProgressive batch",
                    &JsValue::from_str(&e.to_string()),
                ),
            })
        };
        let opts = json::object(&[("signal", js_signal(signal.as_ref()))]);
        let result = call(
            &self.obj,
            "getBarsProgressive",
            &[
                JsValue::from_str(ticker),
                JsValue::from_str(timeframe),
                ser!(range),
                batches.as_ref().clone(),
                opts,
            ],
        )?;
        Some(Box::pin(async move {
            let out = bars_future(result).await;
            drop(batches);
            out
        }))
    }

    fn info(&self) -> Option<ProviderInfo> {
        let v = call(&self.obj, "info", &[])?.ok()?;
        let value = from_js(&v, "provider.info()").ok()?;
        serde_json::from_value(value).ok()
    }

    fn list_symbols(
        &self,
    ) -> Option<LocalBoxFuture<'static, Result<Vec<SymbolDescriptor>, DataError>>> {
        let result = call(&self.obj, "listSymbols", &[])?;
        Some(Box::pin(async move {
            let v = settle(result.map_err(|e| data_error(&e))?)
                .await
                .map_err(|e| data_error(&e))?;
            let value =
                from_js(&v, "listSymbols()").map_err(|e| DataError::Other(message_of(&e)))?;
            serde_json::from_value(value).map_err(|e| DataError::Other(e.to_string()))
        }))
    }

    fn get_symbol_info(
        &self,
        ticker: &str,
    ) -> Option<LocalBoxFuture<'static, Result<Option<SymbolInfo>, DataError>>> {
        let result = call(&self.obj, "getSymbolInfo", &[JsValue::from_str(ticker)])?;
        Some(Box::pin(async move {
            let v = settle(result.map_err(|e| data_error(&e))?)
                .await
                .map_err(|e| data_error(&e))?;
            if !json::present(&v) {
                return Ok(None);
            }
            let value =
                from_js(&v, "getSymbolInfo()").map_err(|e| DataError::Other(message_of(&e)))?;
            serde_json::from_value(value)
                .map(Some)
                .map_err(|e| DataError::Other(e.to_string()))
        }))
    }

    fn resolve_symbol_icon(&self, symbol: &SymbolDescriptor) -> Option<String> {
        call(&self.obj, "resolveSymbolIcon", &[ser!(symbol)])?
            .ok()?
            .as_string()
    }

    fn subscribe(
        &self,
        ticker: &str,
        timeframe: &str,
        mut on_bar: BarSink,
        session: Option<&str>,
    ) -> Option<Unsubscribe> {
        function_of(&self.obj, "subscribe")?;
        let keep = callback(move |b| match bar_of(&b) {
            Ok(bar) => on_bar(bar),
            Err(e) => console_error("[vela] provider.subscribe bar", &JsValue::from_str(&e)),
        });
        let opts = json::object(&[(
            "session",
            session.map_or(JsValue::UNDEFINED, JsValue::from_str),
        )]);
        let result = call(
            &self.obj,
            "subscribe",
            &[
                JsValue::from_str(ticker),
                JsValue::from_str(timeframe),
                keep.as_ref().clone(),
                opts,
            ],
        )?;
        match result {
            Ok(unsub) => Some(unsubscribe_of(unsub, keep)),
            Err(e) => {
                console_error("[vela] provider.subscribe", &e);
                None
            }
        }
    }

    fn get_calendar(
        &self,
        ticker: &str,
        range: &CalendarRange,
    ) -> Option<LocalBoxFuture<'static, Result<CalendarWindows, DataError>>> {
        let window = json::object(&[
            ("from", JsValue::from_f64(range.from as f64)),
            ("to", JsValue::from_f64(range.to as f64)),
            (
                "session",
                range
                    .session
                    .as_deref()
                    .map_or(JsValue::UNDEFINED, JsValue::from_str),
            ),
        ]);
        let result = call(
            &self.obj,
            "getCalendar",
            &[JsValue::from_str(ticker), window],
        )?;
        Some(Box::pin(async move {
            let v = settle(result.map_err(|e| data_error(&e))?)
                .await
                .map_err(|e| data_error(&e))?;
            let value =
                from_js(&v, "getCalendar()").map_err(|e| DataError::Other(message_of(&e)))?;
            let pairs: Vec<(i64, i64)> =
                serde_json::from_value(value).map_err(|e| DataError::Other(e.to_string()))?;
            Ok(pairs)
        }))
    }

    fn supports_stream(&self) -> bool {
        self.has("subscribe")
    }

    fn supports_symbol_info(&self) -> bool {
        self.has("getSymbolInfo")
    }

    fn configure(&self, config: &Value) {
        let _ = call(&self.obj, "configure", &[json::to_js(config)]);
    }

    fn capabilities_for(&self, ticker: &str) -> Option<ProviderCapabilities> {
        let v = call(&self.obj, "capabilitiesFor", &[JsValue::from_str(ticker)])?.ok()?;
        serde_json::from_value(from_js(&v, "capabilitiesFor()").ok()?).ok()
    }
}

// ── a market-data feed written in JavaScript ──

/// A [`MarketDataFeed`] over a JS object, injected as `deps.dataFeed` (used bare: no provider
/// registry, no cache; `chart.data.registerProvider` then warns and does nothing).
pub(crate) struct JsFeed {
    obj: JsValue,
}

impl JsFeed {
    pub(crate) fn new(obj: JsValue) -> Result<Rc<Self>, JsValue> {
        if function_of(&obj, "load").is_none() || function_of(&obj, "subscribe").is_none() {
            return Err(json::type_error(
                "[vela] deps.dataFeed must implement load(config) and subscribe(config, onBar)",
            ));
        }
        Ok(Rc::new(Self { obj }))
    }

    fn has(&self, name: &str) -> bool {
        function_of(&self.obj, name).is_some()
    }
}

impl MarketDataFeed for JsFeed {
    fn load(&self, cfg: &MarketConfig) -> BarsFuture {
        bars_future(
            call(&self.obj, "load", &[market_config_js(cfg)])
                .unwrap_or_else(|| Err(json::type_error("[vela] feed.load is not a function"))),
        )
    }

    fn subscribe(&self, cfg: &MarketConfig, mut on_bar: BarSink) -> Unsubscribe {
        let keep = callback(move |b| match bar_of(&b) {
            Ok(bar) => on_bar(bar),
            Err(e) => console_error("[vela] feed.subscribe bar", &JsValue::from_str(&e)),
        });
        let result = call(
            &self.obj,
            "subscribe",
            &[market_config_js(cfg), keep.as_ref().clone()],
        );
        match result {
            Some(Ok(unsub)) => unsubscribe_of(unsub, keep),
            Some(Err(e)) => {
                console_error("[vela] feed.subscribe", &e);
                Box::new(move || drop(keep))
            }
            None => Box::new(move || drop(keep)),
        }
    }

    fn symbol_info(&self, cfg: &MarketConfig) -> Option<SymbolInfo> {
        let v = call(&self.obj, "symbolInfo", &[market_config_js(cfg)])?.ok()?;
        if !json::present(&v) {
            return None;
        }
        serde_json::from_value(from_js(&v, "feed.symbolInfo()").ok()?).ok()
    }

    fn load_range(&self, cfg: &MarketConfig, range: &BarRange) -> Option<BarsFuture> {
        let result = call(
            &self.obj,
            "loadRange",
            &[market_config_js(cfg), ser!(range)],
        )?;
        Some(bars_future(result))
    }

    fn supports_load_range(&self) -> bool {
        self.has("loadRange")
    }

    fn on_unresolved(
        &self,
        mut callback_fn: Box<dyn FnMut(&UnresolvedSymbol)>,
    ) -> Option<Unsubscribe> {
        function_of(&self.obj, "onUnresolved")?;
        let keep = callback(move |info| {
            let symbol = prop(&info, "symbol").as_string().unwrap_or_default();
            let providers = json::string_list(&prop(&info, "providers")).unwrap_or_default();
            callback_fn(&UnresolvedSymbol { symbol, providers });
        });
        let result = call(&self.obj, "onUnresolved", &[keep.as_ref().clone()])?;
        match result {
            Ok(unsub) => Some(unsubscribe_of(unsub, keep)),
            Err(e) => {
                console_error("[vela] feed.onUnresolved", &e);
                None
            }
        }
    }

    fn destroy(&self) {
        let _ = call(&self.obj, "destroy", &[]);
    }
}

// ── DataUnavailableError ──

/// The original's `DataUnavailableError`: throw it from a provider (or reject with it) to say
/// a series is never served. Unlike the original it is not an `Error` subclass (no `stack`,
/// `instanceof Error` is false); detect it with [`is_data_unavailable_error`], as the
/// original advises.
#[wasm_bindgen(js_name = DataUnavailableError)]
pub struct JsDataUnavailableError {
    inner: DataUnavailableError,
}

#[wasm_bindgen(js_class = DataUnavailableError)]
impl JsDataUnavailableError {
    /// `new DataUnavailableError({ reason, symbol?, timeframe?, finest?, default? }, message?)`.
    #[wasm_bindgen(constructor)]
    pub fn new(
        details: &JsValue,
        message: Option<String>,
    ) -> Result<JsDataUnavailableError, JsValue> {
        let reason = prop(details, "reason").as_string().ok_or_else(|| {
            json::type_error("[vela] DataUnavailableError: details.reason must be a string")
        })?;
        let text = |key: &str| prop(details, key).as_string();
        let details = DataUnavailableDetails {
            reason: DataUnavailableReason::from(reason),
            symbol: text("symbol"),
            timeframe: text("timeframe"),
            finest: text("finest"),
            default: text("default"),
        };
        Ok(Self {
            inner: DataUnavailableError::new(details, message),
        })
    }

    #[wasm_bindgen(getter)]
    pub fn name(&self) -> String {
        DATA_UNAVAILABLE_ERROR_NAME.to_string()
    }

    #[wasm_bindgen(getter)]
    pub fn message(&self) -> String {
        self.inner.message().to_string()
    }

    #[wasm_bindgen(getter)]
    pub fn reason(&self) -> String {
        self.inner.reason.as_str().to_string()
    }

    #[wasm_bindgen(getter)]
    pub fn symbol(&self) -> Option<String> {
        self.inner.symbol.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn timeframe(&self) -> Option<String> {
        self.inner.timeframe.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn finest(&self) -> Option<String> {
        self.inner.finest.clone()
    }

    #[wasm_bindgen(getter, js_name = default)]
    pub fn default_timeframe(&self) -> Option<String> {
        self.inner.default.clone()
    }

    #[wasm_bindgen(js_name = toString)]
    pub fn to_js_string(&self) -> String {
        format!("{DATA_UNAVAILABLE_ERROR_NAME}: {}", self.inner.message())
    }
}

/// Whether `e` is a `DataUnavailableError`, from this build or any other copy: it reads the
/// `name` and the fields, never the class.
#[wasm_bindgen(js_name = isDataUnavailableError)]
pub fn is_data_unavailable_error(e: &JsValue) -> bool {
    unavailable_of(e).is_some()
}

// ── the bundled exchange providers ──

/// What `registerProvider` does with a provider that is not a JS object of the shape above:
/// the bundled exchange provider a class token names.
pub(crate) fn native_provider(
    kind: &str,
    platform: &Rc<dyn Platform>,
) -> Option<Rc<dyn DataProvider>> {
    use vela_core::data::providers::{
        binance::BinanceProvider, coinbase::CoinbaseProvider, hyperliquid::HyperliquidProvider,
    };
    Some(match kind {
        "binance" => Rc::new(BinanceProvider::new(platform.clone())),
        "coinbase" => Rc::new(CoinbaseProvider::new(platform.clone())),
        "hyperliquid" => Rc::new(HyperliquidProvider::new(platform.clone())),
        _ => return None,
    })
}

/// The kind a provider class token carries (`"binance"`, ...), else `None`.
pub(crate) fn native_kind(obj: &JsValue) -> Option<String> {
    prop(obj, "__velaNativeProvider").as_string()
}

macro_rules! native_token {
    ($rust:ident, $js:literal, $kind:literal) => {
        #[doc = concat!("The original's `", $js, "` (see the module documentation).")]
        #[wasm_bindgen(js_name = $js)]
        pub struct $rust;

        #[wasm_bindgen(js_class = $js)]
        impl $rust {
            #[wasm_bindgen(constructor)]
            pub fn new() -> $rust {
                $rust
            }

            #[wasm_bindgen(getter, js_name = __velaNativeProvider)]
            pub fn native_provider_kind(&self) -> String {
                $kind.to_string()
            }
        }

        impl Default for $rust {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

native_token!(JsBinanceProvider, "BinanceProvider", "binance");
native_token!(JsCoinbaseProvider, "CoinbaseProvider", "coinbase");
native_token!(JsHyperliquidProvider, "HyperliquidProvider", "hyperliquid");
