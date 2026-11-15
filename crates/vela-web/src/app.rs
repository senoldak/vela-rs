//! The full facade on a page: [`vela_core::vela::Vela`] (engine and renderer)
//! over an in-memory data feed.
//!
//! Where [`crate::chart::VelaChart`] drives the renderer directly, this entry
//! builds the chart the way a host of the original does: a `Vela` with the
//! native renderer on a [`BrowserHost`], the engine owning bars, indicators,
//! panes, the volume layer and the live tick path. The data source is a
//! [`MemoryFeed`]: it serves the bars given at construction as the history and
//! forwards [`VelaApp::tick`] to the engine as the forming candle, so there is
//! no network involved.
//!
//! ```js
//! const app = new VelaApp(el, 'dark', 'DEMO', '1h', bars);
//! await app.ready();
//! app.addIndicator('sma');       // engine-routed: overlay on the price pane
//! app.addIndicator('rsi');       // its own pane
//! app.tick(time, o, h, l, c, v); // live update of the forming candle
//! ```
//!
//! The volume layer is on by default, as in the original.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use js_sys::{Array, Promise};
use vela_core::drawings::toolbar::DrawingsOption;
use vela_core::engine::orchestrator::NativeIndicatorOptions;
use vela_core::model::{InputValue, Inputs, Ohlcv};
use vela_core::options::VelaOptions;
use vela_core::ports::chart_renderer::{ChartRenderer, SettingsVisibility};
use vela_core::ports::market_data::{BarSink, MarketConfig, MarketDataFeed};
use vela_core::render::native::{NativeHost, NativeParts, NativeRenderer};
use vela_core::runtime::{DataError, LocalBoxFuture, Platform, Unsubscribe};
use vela_core::vela::{Vela, VelaDeps, VelaEnv};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;
use web_sys::Element;

use crate::binding::{self, ChartInputs};
use crate::chart::{display_options, inputs_json, theme_named};
use crate::drawings_api;
use crate::host::BrowserHost;
use crate::intl::BrowserIntl;
use crate::platform::BrowserPlatform;

/// A [`MarketDataFeed`] over bars held in memory.
pub struct MemoryFeed {
    bars: RefCell<Vec<Ohlcv>>,
    sink: Rc<RefCell<Option<BarSink>>>,
}

impl MemoryFeed {
    pub fn new(bars: Vec<Ohlcv>) -> Rc<Self> {
        Rc::new(Self {
            bars: RefCell::new(bars),
            sink: Rc::new(RefCell::new(None)),
        })
    }

    /// Hand one live tick to the subscribed chart; false while nobody listens.
    pub fn push(&self, bar: Ohlcv) -> bool {
        // Take the sink out while it runs: the engine may subscribe again from inside.
        let sink = self.sink.borrow_mut().take();
        let Some(mut sink) = sink else { return false };
        sink(bar);
        let mut slot = self.sink.borrow_mut();
        if slot.is_none() {
            *slot = Some(sink);
        }
        true
    }
}

impl MarketDataFeed for MemoryFeed {
    fn load(&self, _cfg: &MarketConfig) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
        let bars = self.bars.borrow().clone();
        Box::pin(std::future::ready(Ok(bars)))
    }

    fn subscribe(&self, _cfg: &MarketConfig, on_bar: BarSink) -> Unsubscribe {
        *self.sink.borrow_mut() = Some(on_bar);
        let sink = self.sink.clone();
        Box::new(move || {
            sink.borrow_mut().take();
        })
    }
}

pub(crate) fn bars_of(list: &Array) -> Result<Vec<Ohlcv>, JsValue> {
    let field = |value: &JsValue, name: &str| -> Option<f64> {
        js_sys::Reflect::get(value, &JsValue::from_str(name))
            .ok()
            .and_then(|v| v.as_f64())
            .filter(|v| v.is_finite())
    };
    list.iter()
        .map(|b| {
            let need = |name: &str| {
                field(&b, name).ok_or_else(|| {
                    JsValue::from(JsError::new(&format!(
                        "vela: bar.{name} must be a finite number"
                    )))
                })
            };
            Ok(Ohlcv::new(
                need("time")? as i64,
                need("open")?,
                need("high")?,
                need("low")?,
                need("close")?,
                field(&b, "volume"),
            ))
        })
        .collect()
}

/// An indicator inputs object (`{ length: 14, source: "close", fill: true }`) as engine
/// inputs; entries that are not numbers, strings or booleans are skipped.
pub(crate) fn inputs_of(inputs: &JsValue) -> Inputs {
    let mut values = Inputs::new();
    if inputs.is_object() {
        for entry in js_sys::Object::entries(inputs.unchecked_ref()).iter() {
            let pair: Array = entry.unchecked_into();
            let key = pair.get(0).as_string().unwrap_or_default();
            let value = pair.get(1);
            let parsed = if let Some(n) = value.as_f64() {
                InputValue::Number(n)
            } else if let Some(s) = value.as_string() {
                InputValue::Text(s)
            } else if let Some(b) = value.as_bool() {
                InputValue::Bool(b)
            } else {
                continue;
            };
            values.insert(key, parsed);
        }
    }
    values
}

/// A `Vela` chart on a container element. See the module documentation.
#[wasm_bindgen]
pub struct VelaApp {
    chart: Vela,
    feed: Rc<MemoryFeed>,
    renderer: Rc<NativeRenderer>,
    host: Rc<BrowserHost>,
    inputs: RefCell<Option<ChartInputs>>,
    destroyed: std::cell::Cell<bool>,
    /// The renderer config-change subscription behind `onConfigChanged`.
    config_sub: RefCell<Option<Unsubscribe>>,
    config_cb: Rc<RefCell<Option<js_sys::Function>>>,
    /// The locale packs of the chart's translator (shipped packs register lazily).
    locales: Arc<vela_core::i18n::LocaleRegistry>,
}

#[wasm_bindgen]
impl VelaApp {
    /// Build the chart. `theme` is `"dark"` or `"light"`; `timeframe` names the
    /// bar interval of `bars` (`"1m"`, `"5m"`, `"1h"`, `"1d"`, ...); `bars` is an
    /// array of `{ time, open, high, low, close, volume }` (epoch ms, ascending).
    #[wasm_bindgen(constructor)]
    pub fn new(
        container: &Element,
        theme: &str,
        symbol: &str,
        timeframe: &str,
        bars: &JsValue,
    ) -> Result<VelaApp, JsValue> {
        crate::install();
        let list: &Array = bars
            .dyn_ref::<Array>()
            .ok_or_else(|| JsValue::from(JsError::new("vela: bars must be an array")))?;
        let history = bars_of(list)?;
        let palette = theme_named(theme)?;
        let platform: Rc<dyn Platform> = Rc::new(BrowserPlatform::new());
        let mut env = VelaEnv::with_intl(platform.clone(), Arc::new(BrowserIntl));
        crate::themes::attach(&mut env);
        crate::locales::attach(&env.locales);
        let host = BrowserHost::new(container, platform.clone())?;
        let native: Rc<dyn NativeHost> = host.clone();
        // The renderer shares the engine's chart-type registry, as `NativeRenderer::constructor` would.
        let parts = NativeParts {
            chart_types: env.registries.chart_types.clone(),
            ..NativeParts::default()
        };
        let registry = parts.chart_types.clone();
        let renderer = NativeRenderer::with_parts(native, Some(display_options(&palette)), parts);
        binding::bind_renderer(&host, &renderer, registry);
        let feed = MemoryFeed::new(history);
        let options = VelaOptions {
            market: MarketConfig {
                symbol: Some(symbol.to_string()),
                timeframe: Some(timeframe.to_string()),
                ..MarketConfig::default()
            },
            live: Some(true),
            theme: Some(vela_core::options::ThemeSpec::from(theme)),
            // The toolbar starts hidden (`showDrawingToolbar(true)` docks it); the drawing
            // subsystem is on either way.
            drawings: Some(DrawingsOption::Bool(false)),
            ..VelaOptions::default()
        };
        let deps = VelaDeps {
            renderer: Some(renderer.clone() as Rc<dyn ChartRenderer>),
            data_feed: Some(feed.clone() as Rc<dyn MarketDataFeed>),
            ..VelaDeps::default()
        };
        let chart = Vela::with_deps(&env, options, deps)
            .map_err(|e| JsValue::from(JsError::new(&e.to_string())))?;
        // The dialogs, the legend and the drawings widgets read in the chart language and
        // the dialogs read the engine's indicator values and symbol facts.
        binding::bind_chart(&host, &chart);
        let config_cb: Rc<RefCell<Option<js_sys::Function>>> = Rc::new(RefCell::new(None));
        let config_sub = {
            let cb = config_cb.clone();
            renderer.on_config_changed(Box::new(move || {
                let f = cb.borrow().clone();
                if let Some(f) = f {
                    let _ = f.call0(&JsValue::NULL);
                }
            }))
        };
        let inputs = ChartInputs::attach(renderer.clone(), host.clone(), platform);
        Ok(VelaApp {
            chart,
            feed,
            renderer,
            host,
            inputs: RefCell::new(Some(inputs)),
            destroyed: std::cell::Cell::new(false),
            config_sub: RefCell::new(config_sub),
            config_cb,
            locales: env.locales.clone(),
        })
    }

    /// The language of the chart (a BCP 47 tag): the shipped pack for it is registered on
    /// first use (English where there is none), the chart re-renders in it and the
    /// dialogs, legend and drawing widgets read it.
    #[wasm_bindgen(js_name = setLocale)]
    pub fn set_locale(&self, locale: &str) -> Result<(), JsValue> {
        if self.destroyed.get() {
            return Err(JsValue::from(JsError::new("vela: the chart was destroyed")));
        }
        crate::locales::ensure(&self.locales, locale);
        self.chart
            .update_localization(vela_core::i18n::LocalizationPatch::locale(locale));
        binding::rebind_translators(&self.host, &self.chart);
        Ok(())
    }

    /// A promise that settles when the first load has finished.
    pub fn ready(&self) -> Promise {
        let ready = self.chart.ready();
        future_to_promise(async move {
            ready
                .await
                .map(|()| JsValue::UNDEFINED)
                .map_err(|e| JsValue::from(JsError::new(&e.to_string())))
        })
    }

    /// Add a built-in native indicator by registered type (`"sma"`, `"rsi"`,
    /// `"vpvr"`, ...) with its default inputs, optionally overridden by `inputs`
    /// (an object of numbers, strings and booleans). Returns the indicator id.
    #[wasm_bindgen(js_name = addIndicator)]
    pub fn add_indicator(&self, kind: &str, inputs: &JsValue) -> Result<String, JsValue> {
        let values = inputs_of(inputs);
        let handle = self.chart.add_native_indicator(
            kind,
            NativeIndicatorOptions {
                inputs: (!values.is_empty()).then_some(values),
            },
        );
        Ok(handle.id().to_string())
    }

    /// One live tick for the forming candle (a bar with the last bar's time
    /// replaces it, a newer one opens the next). A NaN volume is a missing one.
    /// Returns false while the chart is not subscribed yet.
    pub fn tick(&self, time: f64, open: f64, high: f64, low: f64, close: f64, volume: f64) -> bool {
        self.feed.push(Ohlcv::new(
            time as i64,
            open,
            high,
            low,
            close,
            Some(volume).filter(|v| v.is_finite()),
        ))
    }

    /// Re-read the container's size now (the chart also watches it itself).
    pub fn resize(&self) {
        self.chart.resize();
    }

    /// Switch between `"dark"` and `"light"`.
    #[wasm_bindgen(js_name = setTheme)]
    pub fn set_theme(&self, theme: &str) -> Result<(), JsValue> {
        theme_named(theme)?;
        self.chart.set_theme(theme);
        self.host.dialogs.refresh_open();
        Ok(())
    }

    /// The viewport as `[barSpacing, rightOffset]`.
    pub fn viewport(&self) -> Vec<f64> {
        let vp = self.renderer.coordinates().viewport();
        vec![vp.bar_spacing, vp.right_offset]
    }

    // ── user drawings ──

    /// The drawing tool type keys `armDrawingTool` accepts.
    #[wasm_bindgen(js_name = drawingToolTypes)]
    pub fn drawing_tool_types() -> Vec<String> {
        drawings_api::tool_types()
    }

    /// Arm a drawing tool by type key (`"trendline"`, `"box"`, `"text"`, ...): the next
    /// clicks on the plot place it. `null` / `undefined` returns to the pointer. An unknown
    /// key is an error.
    #[wasm_bindgen(js_name = armDrawingTool)]
    pub fn arm_drawing_tool(&self, type_key: Option<String>) -> Result<(), JsValue> {
        drawings_api::arm(self.chart.drawings(), type_key)
    }

    /// Back to the pointer: disarm the tool, leave the measure ruler and the eraser.
    #[wasm_bindgen(js_name = cancelDrawingTool)]
    pub fn cancel_drawing_tool(&self) -> Result<(), JsValue> {
        drawings_api::cancel(self.chart.drawings())
    }

    /// The armed tool's type key, or `undefined` for the pointer.
    #[wasm_bindgen(js_name = drawingTool)]
    pub fn drawing_tool(&self) -> Option<String> {
        drawings_api::tool(self.chart.drawings())
    }

    /// Undo the last drawing edit; whether there was one.
    #[wasm_bindgen(js_name = undoDrawing)]
    pub fn undo_drawing(&self) -> bool {
        drawings_api::undo(self.chart.drawings())
    }

    /// Redo the last undone drawing edit; whether there was one.
    #[wasm_bindgen(js_name = redoDrawing)]
    pub fn redo_drawing(&self) -> bool {
        drawings_api::redo(self.chart.drawings())
    }

    /// Remove every drawing (one undo step). Returns how many there were.
    #[wasm_bindgen(js_name = clearDrawings)]
    pub fn clear_drawings(&self) -> usize {
        drawings_api::clear(self.chart.drawings())
    }

    /// How many drawings the chart has.
    #[wasm_bindgen(js_name = drawingCount)]
    pub fn drawing_count(&self) -> usize {
        drawings_api::count(self.chart.drawings())
    }

    /// The drawings as a JSON document (text): `{ "version": 1, "drawings": [...] }`.
    #[wasm_bindgen(js_name = exportDrawings)]
    pub fn export_drawings(&self) -> Result<String, JsValue> {
        drawings_api::export(self.chart.drawings())
    }

    /// Replace the drawings with a document from `exportDrawings` (unreadable entries are
    /// dropped; text that is not JSON is an error). Returns how many drawings the chart has.
    #[wasm_bindgen(js_name = importDrawings)]
    pub fn import_drawings(&self, json: &str) -> Result<usize, JsValue> {
        drawings_api::import(self.chart.drawings(), json)
    }

    /// Show or hide the docked drawing toolbar (hidden by default; showing it reserves
    /// its gutter on the left of the plot).
    #[wasm_bindgen(js_name = showDrawingToolbar)]
    pub fn show_drawing_toolbar(&self, visible: bool) -> Result<(), JsValue> {
        drawings_api::show_toolbar(self.chart.drawings(), visible)
    }

    // ── dialogs and config ──

    /// Show or hide the gear that opens the chart settings dialog (hidden by default).
    #[wasm_bindgen(js_name = showSettings)]
    pub fn show_settings(&self, visible: bool) {
        self.renderer
            .apply_feature("settings", &serde_json::json!(visible));
    }

    /// Open (or toggle) the chart settings dialog, landing on the tab titled `section`
    /// (`"Symbol"`, `"Scales and lines"`, `"Canvas"`, `"Events"`, or a chart type tab).
    #[wasm_bindgen(js_name = openSettings)]
    pub fn open_settings(&self, section: Option<String>) {
        self.chart.renderer().open_settings(section.as_deref());
    }

    /// Open the settings dialog of one indicator by the id `addIndicator` returned.
    #[wasm_bindgen(js_name = openIndicatorSettings)]
    pub fn open_indicator_settings(&self, id: &str) {
        self.chart.renderer().open_indicator_settings(id);
    }

    /// Close every in-chart dialog that is open.
    #[wasm_bindgen(js_name = closeDialogs)]
    pub fn close_dialogs(&self) {
        if !self.destroyed.get() {
            self.renderer.close_dialogs();
        }
    }

    /// The chart configuration as JSON text (what the settings dialog edits).
    #[wasm_bindgen(js_name = getConfig)]
    pub fn get_config(&self) -> Option<String> {
        self.renderer.get_config().map(|v| v.to_string())
    }

    /// Apply a configuration document (JSON text, whole or partial: only what it names
    /// changes). Text that is not a JSON object is an error.
    #[wasm_bindgen(js_name = applyConfig)]
    pub fn apply_config(&self, json: &str) -> Result<(), JsValue> {
        let doc: serde_json::Value = serde_json::from_str(json)
            .map_err(|e| JsValue::from(JsError::new(&format!("vela: applyConfig: {e}"))))?;
        if !doc.is_object() {
            return Err(JsError::new("vela: applyConfig takes a JSON object").into());
        }
        self.renderer.apply_config(&doc);
        Ok(())
    }

    /// Call `callback` whenever the configuration changes (a settings edit, `applyConfig`);
    /// `null` removes it. Read the new state with `getConfig`.
    #[wasm_bindgen(js_name = onConfigChanged)]
    pub fn on_config_changed(&self, callback: Option<js_sys::Function>) {
        *self.config_cb.borrow_mut() = callback;
    }

    /// The current input values of an indicator as JSON text, or `undefined` for an
    /// unknown id.
    #[wasm_bindgen(js_name = indicatorInputs)]
    pub fn indicator_inputs(&self, id: &str) -> Option<String> {
        self.chart
            .indicators()
            .into_iter()
            .find(|h| h.id() == id)
            .map(|h| inputs_json(&h.input_values()))
    }

    /// Hide settings by id (a tab, a group or a row, subtree semantics): presentation only,
    /// hidden values keep being stored and applied. `listSettingsIds` names every id.
    #[wasm_bindgen(js_name = setHiddenSettings)]
    pub fn set_hidden_settings(&self, ids: Vec<String>) {
        self.renderer
            .set_settings_visibility(&SettingsVisibility { hidden: Some(ids) });
    }

    /// Every setting id the dialog stamps (for `setHiddenSettings`).
    #[wasm_bindgen(js_name = listSettingsIds)]
    pub fn list_settings_ids(&self) -> Vec<String> {
        self.renderer.list_settings_ids()
    }

    /// The plot values the legend shows beside an indicator title, as of the last frame.
    #[wasm_bindgen(js_name = indicatorReadout)]
    pub fn indicator_readout(&self, id: &str) -> Vec<String> {
        self.renderer
            .legend_values()
            .into_iter()
            .find(|(i, _)| i == id)
            .map(|(_, values)| values.into_iter().map(|v| v.value).collect())
            .unwrap_or_default()
    }

    /// Tear the chart down: listeners, observers, engine, renderer and DOM.
    pub fn destroy(&self) {
        if self.destroyed.replace(true) {
            return;
        }
        let sub = self.config_sub.borrow_mut().take();
        if let Some(unsub) = sub {
            unsub();
        }
        let inputs = self.inputs.borrow_mut().take();
        drop(inputs);
        self.chart.destroy();
        self.host.dispose();
    }
}

/// The geometry backend choice (see [`crate::gl`]).
#[wasm_bindgen]
impl VelaApp {
    /// As the constructor, with `options` `{ backend?, glow? }` (see `VelaChart.withOptions`).
    #[wasm_bindgen(js_name = withOptions)]
    pub fn with_options(
        container: &Element,
        theme: &str,
        symbol: &str,
        timeframe: &str,
        bars: &JsValue,
        options: &JsValue,
    ) -> Result<VelaApp, JsValue> {
        let chosen = crate::gl::render_options_of(options)?;
        crate::gl::with_construction_options(chosen, || {
            VelaApp::new(container, theme, symbol, timeframe, bars)
        })
    }

    /// The geometry backend in force: `"webgl2"` or `"canvas2d"`.
    pub fn backend(&self) -> String {
        crate::gl::backend_name(self.renderer.geometry_backend()).to_string()
    }

    /// The WebGL2 neon-glow amount (canvas2d ignores it); 0 turns it off.
    #[wasm_bindgen(js_name = setGlow)]
    pub fn set_glow(&self, amount: f64) {
        let amount = if amount.is_finite() {
            amount.max(0.0)
        } else {
            0.0
        };
        self.renderer
            .apply_feature("glow", &serde_json::json!(amount));
    }
}
