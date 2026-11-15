//! The original's public chart class, `new Vela(container, options, deps)`, for JavaScript.
//!
//! This is `Vela.ts` on a page: the same constructor shape, member names, argument and
//! payload shapes, event names and defaults, over [`vela_core::vela::Vela`] and the native
//! renderer on a [`BrowserHost`]. The control surfaces (`chart.data`, `chart.panes`,
//! `chart.replay`, `chart.renderer`, `chart.drawings`, `chart.marks`) are in [`controls`],
//! the JavaScript data sources in [`provider`], the option reading in [`options`], the
//! conversions and the event payloads in [`json`].
//!
//! ```js
//! import init, { Vela } from './pkg/vela_web.js';
//! await init();
//! const chart = new Vela('#chart', { data: bars, timeframe: '1h', theme: 'dark' });
//! await chart.ready();
//! const sma = chart.addNativeIndicator('moving-average');
//! chart.on('indicator:added', ({ id }) => console.log(id));
//! chart.drawings.add('hline', { anchors: [{ time: bars[10].time, price: 100 }] });
//! ```
//!
//! # Differences from the original
//!
//! - **No scripting engines.** `registerEngine` and `deps.engines` are refused with an error;
//!   `addIndicator(source)` therefore reports through the handle's `error` event, and
//!   `runIndicator` / `runScript` resolve `ok: false`. Native indicators work.
//! - **No custom renderer.** `options.renderer` and `deps.renderer` are refused with an error.
//! - **No JavaScript callbacks into the core's text and ticks:** `options.formatters`,
//!   `options.priceAxis.ticks`, a `freePan` predicate, a mark's lazy `content` function and
//!   `renderer.setSymbolPicker` / `setLegendActions` / ... are refused or not exposed.
//! - **Data:** with no feed the chart uses the provider registry as in the original (a
//!   symbol-backed chart parks until a provider that resolves it registers). `deps.dataFeed`
//!   and `chart.data.registerProvider` take plain JavaScript objects (see [`provider`]).
//!   `bars` as an object, `symbols` and `providers` are extensions (see [`options`]).
//! - **`this` returns** are real: the prototype methods return the receiver.
//! - **Handles are cached:** `chart.indicators()` returns the same objects `addIndicator` did.
//! - **After `destroy()`** every call is a quiet no-op (queries answer empty) and `on()`
//!   registers handlers that never fire; the original lets calls reach a torn-down engine.
//! - **`drawings.add` returns a record** (`{ id, type, anchors, ... }`), not a class instance.
//! - **`off(event, handler)`** is added next to `on`'s unsubscribe function.

#[macro_use]
pub(crate) mod json;
pub(crate) mod controls;
pub(crate) mod options;
pub(crate) mod provider;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use js_sys::{Array, Function, Object, Promise};
use vela_core::controls::data_control::DataControl;
use vela_core::controls::replay_control::{
    LowerTimeframeChart, ReplayTickRequest, bars_to_ticks, lower_timeframe_ticks,
};
use vela_core::engine::orchestrator::NativeIndicatorOptions;
use vela_core::events::Subscription;
use vela_core::events::types::ChartEventName;
use vela_core::i18n::{LocaleRegistry, MessageArg, MessageDescriptor, MessageLike};
use vela_core::options::{MarketSnapshot, RendererDisplayOptions};
use vela_core::ports::chart_renderer::{ChartRenderer, VisibleRange};
use vela_core::ports::market_data::MarketDataFeed;
use vela_core::render::native::{NativeHost, NativeParts, NativeRenderer};
use vela_core::runtime::{AbortController, Platform};
use vela_core::vela::{IndicatorHandleRef, Vela, VelaDeps, VelaEnv, VelaError, WeakVela};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;
use web_sys::Element;

use self::controls::{
    DATA_CHAIN, DRAWINGS_CHAIN, DataControlApi, DrawingsControlApi, HANDLE_CHAIN,
    IndicatorHandleApi, MARKS_CHAIN, MarksControlApi, PANES_CHAIN, PanesControlApi, RENDERER_CHAIN,
    REPLAY_CHAIN, RendererControlApi, ReplayControlApi, chain, unsub_fn,
};
use self::json::{
    console_error, direction_str, error, object, payload, present, prop, set, strings, type_error,
};
use crate::binding::{self, ChartInputs};
use crate::host::BrowserHost;
use crate::intl::BrowserIntl;
use crate::platform::BrowserPlatform;

/// One handler registered through `on`, kept for `off`.
struct Handler {
    event: String,
    callback: Function,
    sub: Subscription<ChartEventName>,
}

/// The core's chart as the slice `lowerTimeframeTicks` reads, held weakly.
struct ChartLower(WeakVela);

impl LowerTimeframeChart for ChartLower {
    fn market(&self) -> MarketSnapshot {
        self.0.upgrade().map(|c| c.market()).unwrap_or_default()
    }

    fn data(&self) -> DataControl {
        self.0
            .upgrade()
            .map(|c| c.data().clone())
            .unwrap_or_else(DataControl::detached)
    }
}

/// The state every JavaScript object of one chart shares.
pub(crate) struct Inner {
    pub(crate) chart: Vela,
    host: Rc<BrowserHost>,
    pub(crate) renderer: Rc<NativeRenderer>,
    locales: Arc<LocaleRegistry>,
    destroyed: Cell<bool>,
    inputs: RefCell<Option<ChartInputs>>,
    /// The JavaScript provider objects registered through `chart.data`, by lower-cased name.
    pub(crate) js_providers: RefCell<Vec<(String, JsValue)>>,
    handlers: RefCell<Vec<Handler>>,
    handles: RefCell<HashMap<String, JsValue>>,
    lower: Rc<dyn LowerTimeframeChart>,
}

impl Inner {
    /// The chart was destroyed: calls do nothing.
    pub(crate) fn dead(&self) -> bool {
        self.destroyed.get()
    }

    /// The JavaScript object of an indicator handle, one per indicator.
    fn handle_js(self: &Rc<Self>, handle: IndicatorHandleRef) -> JsValue {
        let id = handle.id().to_string();
        if let Some(known) = self.handles.borrow().get(&id) {
            return known.clone();
        }
        let js = JsValue::from(IndicatorHandleApi {
            inner: self.clone(),
            handle,
        });
        chain(&js, HANDLE_CHAIN);
        self.handles.borrow_mut().insert(id, js.clone());
        js
    }

    fn destroy(&self) {
        if self.destroyed.replace(true) {
            return;
        }
        self.inputs.borrow_mut().take();
        self.chart.destroy();
        self.host.dispose();
        self.handlers.borrow_mut().clear();
        self.handles.borrow_mut().clear();
    }
}

fn resolve_element(container: &JsValue) -> Result<Element, JsValue> {
    if let Some(selector) = container.as_string() {
        let document = crate::dom::global_document()
            .ok_or_else(|| error("[vela] there is no document to look the container up in"))?;
        return match document.query_selector(&selector)? {
            Some(element) => Ok(element),
            None => Err(error(format!(
                "[vela] container not found for selector \"{selector}\""
            ))),
        };
    }
    if container.is_object() && prop(container, "appendChild").is_function() {
        return Ok(container.clone().unchecked_into());
    }
    Err(type_error(
        "[vela] container must be an HTMLElement or a CSS selector string",
    ))
}

fn vela_error(e: VelaError) -> JsValue {
    error(e.to_string())
}

fn noop_unsub() -> JsValue {
    unsub_fn(Box::new(|| {}))
}

fn ticks_js(ticks: &[vela_core::controls::replay_control::ReplayTick]) -> JsValue {
    ticks
        .iter()
        .map(|t| {
            let o = Object::new();
            set(&o, "price", JsValue::from_f64(t.price));
            for (k, v) in [
                ("high", t.high),
                ("low", t.low),
                ("open", t.open),
                ("volume", t.volume),
            ] {
                if let Some(v) = v {
                    set(&o, k, JsValue::from_f64(v));
                }
            }
            JsValue::from(o)
        })
        .collect::<Array>()
        .into()
}

fn native_indicator_options(o: &JsValue) -> NativeIndicatorOptions {
    let inputs = prop(o, "inputs");
    let values = crate::app::inputs_of(&inputs);
    NativeIndicatorOptions {
        inputs: (!values.is_empty()).then_some(values),
    }
}

// ── the translator ──

/// `chart.i18n`: the chart's resolved locale settings and formatters. One object for the
/// chart's lifetime that reads the new settings after `setLocale`.
#[wasm_bindgen(js_name = Translator)]
pub struct TranslatorApi {
    inner: Rc<Inner>,
    format: JsValue,
}

#[wasm_bindgen(js_class = Translator)]
impl TranslatorApi {
    #[wasm_bindgen(getter)]
    pub fn locale(&self) -> String {
        self.inner.chart.i18n().locale().to_string()
    }

    /// `'ltr'` or `'rtl'` (`'auto'` resolved).
    #[wasm_bindgen(getter)]
    pub fn direction(&self) -> String {
        direction_str(self.inner.chart.i18n().direction()).to_string()
    }

    #[wasm_bindgen(getter, js_name = numberingSystem)]
    pub fn numbering_system(&self) -> String {
        self.inner.chart.i18n().numbering_system().to_string()
    }

    #[wasm_bindgen(getter, js_name = hourCycle)]
    pub fn hour_cycle(&self) -> String {
        self.inner.chart.i18n().hour_cycle().as_str().to_string()
    }

    #[wasm_bindgen(getter, js_name = weekStart)]
    pub fn week_start(&self) -> u8 {
        self.inner.chart.i18n().week_start()
    }

    /// A plain string comes back unchanged; `{ id, defaultMessage }` is translated through
    /// the registered packs (English where none has it). `values` binds its arguments.
    pub fn t(&self, message: &JsValue, values: &JsValue) -> Result<String, JsValue> {
        let message_like = if let Some(text) = message.as_string() {
            MessageLike::Text(text.into())
        } else if message.is_object() {
            let id = prop(message, "id").as_string();
            let default_message = prop(message, "defaultMessage").as_string();
            match (id, default_message) {
                (Some(id), Some(default_message)) => MessageLike::Message(MessageDescriptor {
                    id: id.into(),
                    default_message: default_message.into(),
                    values: Vec::new(),
                }),
                _ => {
                    return Err(type_error(
                        "[vela] i18n.t takes a string or { id, defaultMessage }",
                    ));
                }
            }
        } else {
            return Err(type_error(
                "[vela] i18n.t takes a string or { id, defaultMessage }",
            ));
        };
        let mut args: Vec<(String, MessageArg)> = Vec::new();
        if values.is_object() {
            for e in Object::entries(values.unchecked_ref()).iter() {
                let pair: Array = e.unchecked_into();
                let key = pair.get(0).as_string().unwrap_or_default();
                let v = pair.get(1);
                if let Some(n) = v.as_f64() {
                    args.push((key, MessageArg::Number(n)));
                } else if let Some(s) = v.as_string() {
                    args.push((key, MessageArg::Text(s.into())));
                }
            }
        }
        let refs: Vec<(&str, MessageArg)> =
            args.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        Ok(self.inner.chart.i18n().t_with(&message_like, &refs))
    }

    /// The chart's formatters: `price`, `percent`, `compact`, `timeStamp`, `duration`.
    #[wasm_bindgen(getter)]
    pub fn format(&self) -> JsValue {
        self.format.clone()
    }
}

/// `chart.i18n.format`: the chart's formatters with their default options (plus `decimals`,
/// `grouping`, `signed`, `trimZeros` where the original names them).
#[wasm_bindgen(js_name = Formatters)]
pub struct FormatApi {
    inner: Rc<Inner>,
}

#[wasm_bindgen(js_class = Formatters)]
impl FormatApi {
    pub fn price(&self, value: f64, opts: &JsValue) -> String {
        use vela_core::format::types::PriceFormatOptions;
        let decimals = prop(opts, "decimals").as_f64().unwrap_or(2.0);
        let mut o = PriceFormatOptions::new(decimals);
        o.grouping = prop(opts, "grouping").as_bool();
        self.inner.chart.i18n().format().price(value, &o)
    }

    pub fn percent(&self, value: f64, opts: &JsValue) -> String {
        use vela_core::format::types::PercentFormatOptions;
        let o = PercentFormatOptions {
            decimals: prop(opts, "decimals").as_f64(),
            signed: prop(opts, "signed").as_bool(),
            ..PercentFormatOptions::default()
        };
        self.inner.chart.i18n().format().percent(value, &o)
    }

    pub fn compact(&self, value: f64, opts: &JsValue) -> String {
        use vela_core::format::types::CompactFormatOptions;
        let o = CompactFormatOptions {
            decimals: prop(opts, "decimals").as_f64(),
            trim_zeros: prop(opts, "trimZeros").as_bool(),
            ..CompactFormatOptions::default()
        };
        self.inner.chart.i18n().format().compact(value, &o)
    }

    #[wasm_bindgen(js_name = timeStamp)]
    pub fn time_stamp(&self, ms: f64) -> String {
        self.inner
            .chart
            .i18n()
            .format()
            .time_stamp(ms, &Default::default())
    }

    pub fn duration(&self, ms: f64) -> String {
        self.inner
            .chart
            .i18n()
            .format()
            .duration(ms, &Default::default())
    }
}

// ── the chart ──

const VELA_CHAIN: &[&str] = &[
    "setLocale",
    "setVisibleRange",
    "panBy",
    "setVisibleRangePreset",
    "setTheme",
];

thread_local! {
    static CHAINED: Cell<bool> = const { Cell::new(false) };
}

/// The public, imperative chart (`Vela` in JavaScript). See the module documentation.
#[wasm_bindgen(js_name = Vela)]
pub struct VelaApi {
    inner: Rc<Inner>,
    controls: [JsValue; 6],
    i18n: JsValue,
}

impl VelaApi {
    fn same(&self) -> VelaApi {
        VelaApi {
            inner: self.inner.clone(),
            controls: self.controls.clone(),
            i18n: self.i18n.clone(),
        }
    }
}

#[wasm_bindgen(js_class = Vela)]
impl VelaApi {
    /// `new Vela(container, options?, deps?)`: `container` is an element or a CSS selector
    /// (a missing selector throws).
    #[wasm_bindgen(constructor)]
    pub fn new(container: &JsValue, options: &JsValue, deps: &JsValue) -> Result<VelaApi, JsValue> {
        crate::install();
        let feed_obj = options::check_deps(deps)?;
        let parsed = options::parse(options)?;
        let element = resolve_element(container)?;
        let feed: Option<Rc<dyn MarketDataFeed>> = if present(&feed_obj) {
            Some(provider::JsFeed::new(feed_obj)? as Rc<dyn MarketDataFeed>)
        } else {
            None
        };

        let platform: Rc<dyn Platform> = Rc::new(BrowserPlatform::new());
        let mut env = VelaEnv::with_intl(platform.clone(), Arc::new(BrowserIntl));
        crate::themes::attach(&mut env);
        crate::locales::attach(&env.locales);
        if let Some(tag) = &parsed.options.localization.locale {
            crate::locales::ensure(&env.locales, tag);
        }
        let host = BrowserHost::new(&element, platform.clone())?;

        // The renderer is built by the facade, from the display options it resolves, as the
        // original instantiates `options.renderer ?? NativeRenderer`.
        let slot: Rc<RefCell<Option<Rc<NativeRenderer>>>> = Rc::default();
        let registry = env.registries.chart_types.clone();
        let factory = {
            let (host, slot) = (host.clone(), slot.clone());
            Rc::new(
                move |display: RendererDisplayOptions| -> Rc<dyn ChartRenderer> {
                    let native: Rc<dyn NativeHost> = host.clone();
                    let parts = NativeParts {
                        chart_types: registry.clone(),
                        ..NativeParts::default()
                    };
                    let renderer = NativeRenderer::with_parts(native, Some(display), parts);
                    binding::bind_renderer(&host, &renderer, registry.clone());
                    *slot.borrow_mut() = Some(renderer.clone());
                    renderer
                },
            )
        };
        let mut chart_options = parsed.options;
        chart_options.renderer = Some(factory);

        // Providers: registered before the first load, so a symbol they serve is not parked.
        let mut vela_deps = VelaDeps::default();
        match feed {
            Some(feed) => vela_deps.data_feed = Some(feed),
            None => {
                let registry_feed = vela_core::data::multi_provider_feed::MultiProviderFeed::new(
                    env.bar_store.clone(),
                    platform.clone(),
                );
                if let Some(memory) = &parsed.memory {
                    drop(registry_feed.register_provider(
                        "memory",
                        memory.clone() as Rc<dyn vela_core::ports::data_provider::DataProvider>,
                    ));
                }
                for name in &parsed.providers {
                    if let Some(p) = provider::native_provider(name, &platform) {
                        drop(registry_feed.register_provider(name, p));
                    }
                }
                vela_deps.registry_feed = Some(registry_feed);
            }
        }
        let chart = match Vela::with_deps(&env, chart_options, vela_deps) {
            Ok(chart) => chart,
            Err(e) => {
                host.dispose();
                return Err(vela_error(e));
            }
        };
        let renderer = slot
            .borrow_mut()
            .take()
            .ok_or_else(|| error("[vela] the renderer was not built"))?;
        // The dialogs, the legend and the drawings widgets read in the chart language and
        // the dialogs read the engine's indicator values and symbol facts.
        binding::bind_chart(&host, &chart);
        let inputs = ChartInputs::attach(renderer.clone(), host.clone(), platform.clone());
        let lower: Rc<dyn LowerTimeframeChart> = Rc::new(ChartLower(chart.downgrade()));
        let inner = Rc::new(Inner {
            locales: env.locales.clone(),
            chart,
            host,
            renderer,
            destroyed: Cell::new(false),
            inputs: RefCell::new(Some(inputs)),
            js_providers: RefCell::default(),
            handlers: RefCell::default(),
            handles: RefCell::default(),
            lower,
        });
        let make = |js: JsValue, names: &[&str]| {
            chain(&js, names);
            js
        };
        let controls = [
            make(
                JsValue::from(DataControlApi {
                    inner: inner.clone(),
                }),
                DATA_CHAIN,
            ),
            make(
                JsValue::from(PanesControlApi {
                    inner: inner.clone(),
                }),
                PANES_CHAIN,
            ),
            make(
                JsValue::from(ReplayControlApi {
                    inner: inner.clone(),
                }),
                REPLAY_CHAIN,
            ),
            make(
                JsValue::from(RendererControlApi {
                    inner: inner.clone(),
                }),
                RENDERER_CHAIN,
            ),
            make(
                JsValue::from(DrawingsControlApi {
                    inner: inner.clone(),
                }),
                DRAWINGS_CHAIN,
            ),
            make(
                JsValue::from(MarksControlApi {
                    inner: inner.clone(),
                }),
                MARKS_CHAIN,
            ),
        ];
        let format = JsValue::from(FormatApi {
            inner: inner.clone(),
        });
        let i18n = JsValue::from(TranslatorApi {
            inner: inner.clone(),
            format,
        });
        let api = VelaApi {
            inner,
            controls,
            i18n,
        };
        // Once per page: the methods of the class that return `this`. Any wrapper of the class
        // gives its prototype; this one is dropped at once.
        if !CHAINED.with(|c| c.replace(true)) {
            chain(&JsValue::from(api.same()), VELA_CHAIN);
        }
        Ok(api)
    }

    // ── localization ──

    /// The chart's translator: resolved locale and direction, `t()` and `format`.
    #[wasm_bindgen(getter)]
    pub fn i18n(&self) -> JsValue {
        self.i18n.clone()
    }

    /// Change the chart's language, direction or formatting: a locale tag, or any of
    /// `locale`, `direction`, `messages`, `numberingSystem`, `hourCycle`, `weekStart` (an
    /// explicit `undefined` restores the default). Emits `locale:changed`.
    #[wasm_bindgen(js_name = setLocale)]
    pub fn set_locale(&self, next: &JsValue) -> Result<(), JsValue> {
        let (patch, tag) = options::locale_patch(next)?;
        if self.inner.dead() {
            return Ok(());
        }
        if let Some(tag) = tag {
            crate::locales::ensure(&self.inner.locales, &tag);
        }
        self.inner.chart.update_localization(patch);
        binding::rebind_translators(&self.inner.host, &self.inner.chart);
        Ok(())
    }

    // ── indicators ──

    /// Not supported by this build: no scripting engine can be registered.
    #[wasm_bindgen(js_name = registerEngine)]
    pub fn register_engine(&self, language: &JsValue, _engine: &JsValue) -> Result<(), JsValue> {
        Err(error(format!(
            "[vela] registerEngine({}) is not supported by this build: scripting engines are Rust types and none runs scripts from JavaScript. Native indicators (addNativeIndicator) work.",
            language.as_string().unwrap_or_default()
        )))
    }

    /// Run an indicator script on the chart's data. Without an engine for its language the
    /// handle reports the failure on its `error` event.
    #[wasm_bindgen(js_name = addIndicator)]
    pub fn add_indicator(&self, source: &JsValue, options: &JsValue) -> Result<JsValue, JsValue> {
        let source = source
            .as_string()
            .ok_or_else(|| type_error("[vela] addIndicator: `source` must be a string"))?;
        let options = options::add_indicator_options(options)?;
        if self.inner.dead() {
            return Err(error("[vela] addIndicator: the chart was destroyed"));
        }
        let handle = self
            .inner
            .chart
            .add_indicator(&source, options)
            .map_err(vela_error)?;
        Ok(self.inner.handle_js(handle))
    }

    /// Add a built-in native indicator by registered type (`'moving-average'`, `'rsi'`,
    /// `'vpvr'`, ...); `options.inputs` seeds inputs. An unregistered type warns and returns a
    /// handle that never mounts.
    #[wasm_bindgen(js_name = addNativeIndicator)]
    pub fn add_native_indicator(
        &self,
        kind: &JsValue,
        options: &JsValue,
    ) -> Result<JsValue, JsValue> {
        let kind = kind
            .as_string()
            .ok_or_else(|| type_error("[vela] addNativeIndicator: `type` must be a string"))?;
        if self.inner.dead() {
            return Err(error("[vela] addNativeIndicator: the chart was destroyed"));
        }
        let handle = self
            .inner
            .chart
            .add_native_indicator(&kind, native_indicator_options(options));
        Ok(self.inner.handle_js(handle))
    }

    /// `Promise` of the catalog of native indicators with their live state on this chart:
    /// `{ type, title, supported, present, multiInstance, beta }`.
    #[wasm_bindgen(js_name = availableNativeIndicators)]
    pub fn available_native_indicators(&self) -> Promise {
        if self.inner.dead() {
            return Promise::resolve(&JsValue::from(Array::new()));
        }
        let fut = self.inner.chart.available_native_indicators();
        future_to_promise(async move {
            let list: Array = fut
                .await
                .iter()
                .map(|i| {
                    object(&[
                        ("type", JsValue::from_str(&i.indicator_type)),
                        ("title", JsValue::from_str(&i.title)),
                        ("supported", JsValue::from_bool(i.supported)),
                        ("present", JsValue::from_bool(i.present)),
                        ("multiInstance", JsValue::from_bool(i.multi_instance)),
                        ("beta", JsValue::from_bool(i.beta)),
                    ])
                })
                .collect();
            Ok(list.into())
        })
    }

    /// The native-indicator types present now, one entry per instance.
    #[wasm_bindgen(js_name = presentNativeIndicators)]
    pub fn present_native_indicators(&self) -> JsValue {
        if self.inner.dead() {
            return Array::new().into();
        }
        strings(&self.inner.chart.present_native_indicators())
    }

    /// The native-indicator types the chart is about to add on its own.
    #[wasm_bindgen(js_name = pendingNativeIndicators)]
    pub fn pending_native_indicators(&self) -> JsValue {
        if self.inner.dead() {
            return Array::new().into();
        }
        strings(&self.inner.chart.pending_native_indicators())
    }

    /// The facts the chart applies about its symbol (`{ ohlc?, volume? }`), or `null`.
    #[wasm_bindgen(js_name = symbolFacts)]
    pub fn symbol_facts(&self) -> JsValue {
        if self.inner.dead() {
            return JsValue::NULL;
        }
        self.inner
            .chart
            .symbol_facts()
            .map_or(JsValue::NULL, |f| ser!(&f))
    }

    /// Live handles of every indicator on the chart.
    pub fn indicators(&self) -> JsValue {
        if self.inner.dead() {
            return Array::new().into();
        }
        self.inner
            .chart
            .indicators()
            .into_iter()
            .map(|h| self.inner.handle_js(h))
            .collect::<Array>()
            .into()
    }

    /// Execute a script and inject it only if it runs: resolves
    /// `{ ok, handle, error, context }`, never rejects.
    #[wasm_bindgen(js_name = runIndicator)]
    pub fn run_indicator(&self, source: &JsValue, options: &JsValue) -> Result<Promise, JsValue> {
        let source = source
            .as_string()
            .ok_or_else(|| type_error("[vela] runIndicator: `source` must be a string"))?;
        let options = options::add_indicator_options(options)?;
        if self.inner.dead() {
            return Ok(Promise::resolve(&object(&[
                ("ok", JsValue::FALSE),
                ("handle", JsValue::NULL),
                (
                    "error",
                    error("[vela] runIndicator: the chart was destroyed"),
                ),
                ("context", JsValue::NULL),
            ])));
        }
        let fut = self.inner.chart.run_indicator(&source, options);
        let inner = self.inner.clone();
        Ok(future_to_promise(async move {
            let r = fut.await;
            Ok(object(&[
                ("ok", JsValue::from_bool(r.ok)),
                (
                    "handle",
                    r.handle.map_or(JsValue::NULL, |h| inner.handle_js(h)),
                ),
                ("error", r.error.as_deref().map_or(JsValue::NULL, error)),
                (
                    "context",
                    r.context.as_ref().map_or(JsValue::NULL, |c| ser!(c)),
                ),
            ]))
        }))
    }

    /// Execute a script and resolve its first computed run:
    /// `{ ok, run, error, onUpdate(cb), remove() }`, never rejects.
    #[wasm_bindgen(js_name = runScript)]
    pub fn run_script(&self, source: &JsValue, options: &JsValue) -> Result<Promise, JsValue> {
        let source = source
            .as_string()
            .ok_or_else(|| type_error("[vela] runScript: `source` must be a string"))?;
        let options = options::add_indicator_options(options)?;
        let failed = |message: &str| {
            object(&[
                ("ok", JsValue::FALSE),
                ("run", JsValue::NULL),
                ("error", error(message)),
                (
                    "onUpdate",
                    Closure::<dyn FnMut(JsValue) -> JsValue>::new(|_| noop_unsub()).into_js_value(),
                ),
                ("remove", Closure::<dyn FnMut()>::new(|| {}).into_js_value()),
            ])
        };
        if self.inner.dead() {
            return Ok(Promise::resolve(&failed(
                "[vela] runScript: the chart was destroyed",
            )));
        }
        let fut = self.inner.chart.run_script(&source, options);
        Ok(future_to_promise(async move {
            let r = fut.await;
            let Some(run) = r.run.clone().filter(|_| r.ok) else {
                return Ok(failed(
                    r.error.as_deref().unwrap_or("[vela] the script failed"),
                ));
            };
            let result = Rc::new(r);
            let follow = {
                let result = result.clone();
                Closure::<dyn FnMut(JsValue) -> JsValue>::new(move |handler: JsValue| {
                    let Some(f) = handler.dyn_ref::<Function>().cloned() else {
                        return noop_unsub();
                    };
                    unsub_fn(result.on_update(move |run| {
                        if let Err(e) = f.call1(&JsValue::NULL, &json::script_run_js(run)) {
                            console_error("[vela] runScript onUpdate handler error", &e);
                        }
                    }))
                })
                .into_js_value()
            };
            let remove = {
                let result = result.clone();
                Closure::<dyn FnMut()>::new(move || result.remove()).into_js_value()
            };
            Ok(object(&[
                ("ok", JsValue::TRUE),
                ("run", json::script_run_js(&run)),
                ("error", JsValue::NULL),
                ("onUpdate", follow),
                ("remove", remove),
            ]))
        }))
    }

    // ── market ──

    /// Switch the market in place (`{ symbol?, timeframe?, bars?, session?, data?,
    /// visibleRange? }`): resolves once the new history is painted. Emits `market:changed`.
    #[wasm_bindgen(js_name = setMarket)]
    pub fn set_market(&self, next: &JsValue) -> Result<Promise, JsValue> {
        let next = options::market_switch(next)?;
        if self.inner.dead() {
            return Ok(Promise::resolve(&JsValue::UNDEFINED));
        }
        let fut = self.inner.chart.set_market(next);
        Ok(future_to_promise(async move {
            fut.await.map(|()| JsValue::UNDEFINED).map_err(vela_error)
        }))
    }

    /// The current market identity: `{ symbol?, provider?, timeframe?, bars?, session?, offline }`.
    #[wasm_bindgen(getter)]
    pub fn market(&self) -> JsValue {
        let m = if self.inner.dead() {
            MarketSnapshot::default()
        } else {
            self.inner.chart.market()
        };
        let o = Object::new();
        for (k, v) in [
            ("symbol", &m.symbol),
            ("provider", &m.provider),
            ("timeframe", &m.timeframe),
        ] {
            if let Some(v) = v {
                set(&o, k, JsValue::from_str(v));
            }
        }
        if let Some(b) = m.bars {
            set(&o, "bars", JsValue::from_f64(b as f64));
        }
        if let Some(s) = m.session {
            set(&o, "session", JsValue::from_str(s.as_str()));
        }
        set(&o, "offline", JsValue::from_bool(m.offline));
        o.into()
    }

    /// Resolves once the chart is painted and interactive.
    pub fn ready(&self) -> Promise {
        if self.inner.dead() {
            return Promise::resolve(&JsValue::UNDEFINED);
        }
        let fut = self.inner.chart.ready();
        future_to_promise(async move { fut.await.map(|()| JsValue::UNDEFINED).map_err(vela_error) })
    }

    /// Resolves once the full requested history has loaded; never rejects.
    #[wasm_bindgen(js_name = historyComplete)]
    pub fn history_complete(&self) -> Promise {
        if self.inner.dead() {
            return Promise::resolve(&JsValue::UNDEFINED);
        }
        let fut = self.inner.chart.history_complete();
        future_to_promise(async move {
            fut.await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// A renderer-agnostic snapshot of the graphic elements the core generated.
    pub fn inspect(&self) -> JsValue {
        let scene = if self.inner.dead() {
            Default::default()
        } else {
            self.inner.chart.inspect()
        };
        let indicators: Array = scene
            .indicators
            .iter()
            .map(|i| {
                let series = Object::new();
                for (k, n) in &i.series {
                    set(&series, k, JsValue::from_f64(*n as f64));
                }
                let o = Object::new();
                set(&o, "id", JsValue::from_str(&i.id));
                set(&o, "title", JsValue::from_str(&i.title));
                set(&o, "overlay", JsValue::from_bool(i.overlay));
                if let Some(p) = &i.pane_id {
                    set(&o, "paneId", JsValue::from_str(p));
                }
                set(&o, "native", JsValue::from_bool(i.native));
                if let Some(t) = &i.native_type {
                    set(&o, "nativeType", JsValue::from_str(t));
                }
                set(&o, "series", series.into());
                for (k, n) in [
                    ("fills", i.fills),
                    ("backgrounds", i.backgrounds),
                    ("priceLines", i.price_lines),
                    ("lines", i.lines),
                    ("boxes", i.boxes),
                    ("labels", i.labels),
                    ("polylines", i.polylines),
                    ("linefills", i.linefills),
                    ("tables", i.tables),
                    ("barColors", i.bar_colors),
                    ("trades", i.trades),
                    ("inputs", i.inputs),
                    ("forcedOverlay", i.forced_overlay),
                ] {
                    set(&o, k, JsValue::from_f64(n as f64));
                }
                JsValue::from(o)
            })
            .collect();
        let t = &scene.totals;
        let totals = Object::new();
        for (k, n) in [
            ("panes", t.panes),
            ("series", t.series),
            ("fills", t.fills),
            ("backgrounds", t.backgrounds),
            ("priceLines", t.price_lines),
            ("lines", t.lines),
            ("boxes", t.boxes),
            ("labels", t.labels),
            ("polylines", t.polylines),
            ("linefills", t.linefills),
            ("tables", t.tables),
            ("barColors", t.bar_colors),
            ("trades", t.trades),
            ("forcedOverlay", t.forced_overlay),
        ] {
            set(&totals, k, JsValue::from_f64(n as f64));
        }
        object(&[("indicators", indicators.into()), ("totals", totals.into())])
    }

    // ── control surfaces ──

    /// The renderer control surface (`chart.renderer`).
    #[wasm_bindgen(getter)]
    pub fn renderer(&self) -> JsValue {
        self.controls[3].clone()
    }

    /// The pane control surface (`chart.panes`).
    #[wasm_bindgen(getter)]
    pub fn panes(&self) -> JsValue {
        self.controls[1].clone()
    }

    /// The data control surface (`chart.data`).
    #[wasm_bindgen(getter)]
    pub fn data(&self) -> JsValue {
        self.controls[0].clone()
    }

    /// The user-drawings control surface (`chart.drawings`).
    #[wasm_bindgen(getter)]
    pub fn drawings(&self) -> JsValue {
        self.controls[4].clone()
    }

    /// The timeline-marks control surface (`chart.marks`).
    #[wasm_bindgen(getter)]
    pub fn marks(&self) -> JsValue {
        self.controls[5].clone()
    }

    /// The bar-replay control surface (`chart.replay`).
    #[wasm_bindgen(getter)]
    pub fn replay(&self) -> JsValue {
        self.controls[2].clone()
    }

    // ── events ──

    /// Subscribe to a chart event; returns the unsubscribe function. Handler errors are
    /// logged, never thrown. An unknown event name registers nothing, as in the original,
    /// where it simply never fires.
    pub fn on(&self, event: &str, handler: &JsValue) -> Result<JsValue, JsValue> {
        let f = handler
            .dyn_ref::<Function>()
            .cloned()
            .ok_or_else(|| type_error("[vela] on: the handler must be a function"))?;
        let Some(name) = ChartEventName::from_name(event) else {
            return Ok(noop_unsub());
        };
        if self.inner.dead() {
            return Ok(noop_unsub());
        }
        let label = event.to_string();
        let callback = f.clone();
        let sub = self.inner.chart.on(name, move |e| {
            if let Err(err) = callback.call1(&JsValue::NULL, &payload(e)) {
                console_error(&format!("[vela] event handler error for \"{label}\""), &err);
            }
        });
        self.inner.handlers.borrow_mut().push(Handler {
            event: event.to_string(),
            callback: f,
            sub: sub.clone(),
        });
        let weak = Rc::downgrade(&self.inner);
        Ok(unsub_fn(Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.chart.off(&sub);
                inner.handlers.borrow_mut().retain(|h| h.sub != sub);
            }
        })))
    }

    /// Remove a handler given to `on`; whether it was registered. (An addition: the original
    /// has only the unsubscribe function `on` returns.)
    pub fn off(&self, event: &str, handler: &JsValue) -> bool {
        let Some(f) = handler.dyn_ref::<Function>() else {
            return false;
        };
        let found = {
            let mut handlers = self.inner.handlers.borrow_mut();
            handlers
                .iter()
                .position(|h| h.event == event && Object::is(&h.callback, f))
                .map(|at| handlers.remove(at))
        };
        match found {
            Some(h) => !self.inner.dead() && self.inner.chart.off(&h.sub),
            None => false,
        }
    }

    // ── view ──

    /// The visible time range `{ from, to }` (epoch-ms), or `null` before data loads.
    #[wasm_bindgen(js_name = getVisibleRange)]
    pub fn get_visible_range(&self) -> JsValue {
        if self.inner.dead() {
            return JsValue::NULL;
        }
        match self.inner.chart.get_visible_range() {
            Some(r) => object(&[
                ("from", JsValue::from_f64(r.from as f64)),
                ("to", JsValue::from_f64(r.to as f64)),
            ]),
            None => JsValue::NULL,
        }
    }

    /// Set the visible time range explicitly (epoch-ms).
    #[wasm_bindgen(js_name = setVisibleRange)]
    pub fn set_visible_range(&self, range: &JsValue) -> Result<(), JsValue> {
        let from = prop(range, "from").as_f64().filter(|n| n.is_finite());
        let to = prop(range, "to").as_f64().filter(|n| n.is_finite());
        let (Some(from), Some(to)) = (from, to) else {
            return Err(type_error(
                "[vela] setVisibleRange takes { from, to } in epoch-ms",
            ));
        };
        if !self.inner.dead() {
            self.inner.chart.set_visible_range(VisibleRange {
                from: from as i64,
                to: to as i64,
            });
        }
        Ok(())
    }

    /// Pan by a fraction of the visible width: positive toward the latest bars.
    #[wasm_bindgen(js_name = panBy)]
    pub fn pan_by(&self, fraction: f64) {
        if !self.inner.dead() {
            self.inner.chart.pan_by(fraction);
        }
    }

    /// Frame a named range over the loaded bars: `'1D'`, `'1W'`, `'1M'`, `'3M'`, `'6M'`,
    /// `'1Y'`, `'5Y'`, `'YTD'` or `'ALL'`.
    #[wasm_bindgen(js_name = setVisibleRangePreset)]
    pub fn set_visible_range_preset(&self, preset: &str) -> Result<(), JsValue> {
        let preset = options::preset_of(preset)?;
        if !self.inner.dead() {
            self.inner.chart.set_visible_range_preset(preset);
        }
        Ok(())
    }

    /// Re-measure the container and relayout.
    pub fn resize(&self) {
        if !self.inner.dead() {
            self.inner.chart.resize();
        }
    }

    /// Swap the app theme: `'dark'`, `'light'`, a registered id or a theme object. Emits
    /// `theme:changed`.
    #[wasm_bindgen(js_name = setTheme)]
    pub fn set_theme(&self, theme: &JsValue) -> Result<(), JsValue> {
        let spec = options::theme_of(theme)?;
        if !self.inner.dead() {
            self.inner.chart.set_theme(spec);
            self.inner.host.dialogs.refresh_open();
        }
        Ok(())
    }

    /// Tear the chart down: loads, timers, live updates, listeners, observers, the renderer
    /// and its DOM. Idempotent.
    pub fn destroy(&self) {
        self.inner.destroy();
    }
}

// ── replay tick sources ──

/// `lowerTimeframeTicks(chart, timeframe)`: a tick source for `chart.replay.setTicks` that
/// plays each replayed bar through the chart's own provider's bars of `timeframe`.
#[wasm_bindgen(js_name = lowerTimeframeTicks)]
pub fn lower_timeframe_ticks_js(chart: &VelaApi, timeframe: &str) -> JsValue {
    let weak = Rc::downgrade(&chart.inner.lower);
    let source = lower_timeframe_ticks(weak, timeframe);
    Closure::<dyn FnMut(JsValue, JsValue) -> Promise>::new(move |bar: JsValue, opts: JsValue| {
        let Ok(list) = crate::app::bars_of(&Array::of1(&bar)) else {
            return Promise::reject(&type_error("[vela] a tick source is called with a bar"));
        };
        let Some(ohlc) = list.first().copied() else {
            return Promise::reject(&type_error("[vela] a tick source is called with a bar"));
        };
        let end = prop(&opts, "end").as_f64().unwrap_or(ohlc.time as f64) as i64;
        let controller = AbortController::new();
        let signal = prop(&opts, "signal");
        if let Some(add) = prop(&signal, "addEventListener").dyn_ref::<Function>() {
            let c = controller.clone();
            let on_abort = Closure::<dyn FnMut()>::new(move || c.abort()).into_js_value();
            let _ = add.call2(&signal, &JsValue::from_str("abort"), &on_abort);
        }
        let fut = source(
            &ohlc,
            ReplayTickRequest {
                end,
                signal: controller.signal(),
            },
        );
        future_to_promise(async move {
            fut.await
                .map(|ticks| ticks_js(&ticks))
                .map_err(|e| error(e.to_string()))
        })
    })
    .into_js_value()
}

/// `barsToTicks(bars)`: one tick per bar (its close, high, low and volume), oldest first.
#[wasm_bindgen(js_name = barsToTicks)]
pub fn bars_to_ticks_js(bars: &JsValue) -> Result<JsValue, JsValue> {
    let bars = controls::bar_list(bars, "barsToTicks(bars)")?;
    Ok(ticks_js(&bars_to_ticks(&bars)))
}
