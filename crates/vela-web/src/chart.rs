//! A small JavaScript-facing chart over [`NativeRenderer`]: demo-grade, NOT the
//! port of the original `Vela` class API (see [`crate::app`] for that).
//!
//! ```js
//! import init, { VelaChart } from './pkg/vela_web.js';
//! await init();
//! const chart = new VelaChart(document.getElementById('chart'), 'dark');
//! chart.setBars(bars);                    // [{ time, open, high, low, close, volume }]
//! chart.addVolume();
//! chart.addIndicator('ema');              // overlay on the price pane
//! chart.addIndicator('rsi');              // its own pane
//! chart.updateBar(t, o, h, l, c, v);      // replace the last bar, or append a newer one
//! ```
//!
//! The indicators are the classic catalog computed in this module (not by the
//! engine, which is the `app` entry's job): the output is wrapped into an
//! `IndicatorModel` the way the engine's private `build_native_model` does, and
//! recomputed on every bar change.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use js_sys::{Array, Object, Reflect};
use vela_core::chart_types::ChartTypeRegistry;
use vela_core::controls::drawings_control::DrawingsControl;
use vela_core::drawings::DrawingRegistry;
use vela_core::drawings::controller::{DrawingController, no_translator};
use vela_core::drawings::toolbar::DrawingsOption;
use vela_core::events::types::ChartEventBus;
use vela_core::i18n::{
    LocaleRegistry, LocalizationOptions, Translator, create_translator, text_of,
};
use vela_core::indicators::NativeIndicatorOutput;
use vela_core::indicators::classics::{
    ClassicIndicatorSpec, classic_descriptor, classic_native_output, classic_specs,
};
use vela_core::indicators::volume::volume_layer_data;
use vela_core::intl::Intl;
use vela_core::model::{
    AnchorUpdate, DirtyRange, IndicatorModel, InputValue, Inputs, NativeTag, Ohlcv, Pane, PaneHint,
    PaneKind, ScenePatch, SeriesData, SeriesValueDelta, SeriesValues, ValuePatch,
};
use vela_core::options::{NativeBackend, RendererDisplayOptions, resolve_animations};
use vela_core::ports::chart_renderer::{
    ChartRenderer, IndicatorRenderHandle, InputChangeEvent, InputChangeKind, MoveTarget,
    NewPanePlacement, SetBarsOptions, SettingsVisibility, VelaTheme,
};
use vela_core::price_styles::{BarTransform, bar_transform_for};
use vela_core::render::native::{NativeHost, NativeParts, NativeRenderer};
use vela_core::runtime::{Platform, Unsubscribe};
use wasm_bindgen::prelude::*;
use web_sys::Element;

use crate::drawings_api;
use crate::host::BrowserHost;
use crate::input::{InputWiring, SizeWatch};
use crate::intl::BrowserIntl;
use crate::platform::BrowserPlatform;

const PRICE_PANE: &str = "price";
const VOLUME_ID: &str = "volume";

fn js_err(message: impl Into<String>) -> JsValue {
    JsError::new(&message.into()).into()
}

/// `"dark"`, `"light"` or the id of a theme added with `registerTheme` (anything else is an
/// error, not a silent default).
pub(crate) fn theme_named(name: &str) -> Result<VelaTheme, JsValue> {
    crate::themes::resolve_named(name).map_err(js_err)
}

/// The renderer's display options the way the `Vela` facade derives them, for
/// the default option set.
pub(crate) fn display_options(theme: &VelaTheme) -> RendererDisplayOptions {
    let animations = resolve_animations(None);
    let color = |key: &str| {
        theme
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    // `auto` and no glow, unless a `withOptions` constructor chose otherwise.
    let (backend, glow): (NativeBackend, f64) = crate::gl::construction_options();
    RendererDisplayOptions {
        current_price_line: true,
        log_scale: false,
        native_backend: backend,
        anim_zoom: animations.anim_zoom,
        anim_pan: animations.anim_pan,
        anim_scroll: animations.anim_scroll,
        anim_autoscale: animations.anim_autoscale,
        anim_live_bar: animations.anim_live_bar,
        anim_intro: animations.anim_intro,
        glow,
        up_color: color("upColor"),
        down_color: color("downColor"),
        price_style: "candles".to_string(),
        price_axis_ticks: None,
        free_pan: None,
    }
}

/// One bar from a JS object (`{ time, open, high, low, close, volume }`, time in
/// epoch milliseconds, volume optional).
fn bar_of(value: &JsValue) -> Result<Ohlcv, JsValue> {
    let field = |name: &str| -> Option<f64> {
        Reflect::get(value, &JsValue::from_str(name))
            .ok()
            .and_then(|v| v.as_f64())
            .filter(|v| v.is_finite())
    };
    let need = |name: &str| {
        field(name).ok_or_else(|| js_err(format!("vela: bar.{name} must be a finite number")))
    };
    Ok(Ohlcv::new(
        need("time")? as i64,
        need("open")?,
        need("high")?,
        need("low")?,
        need("close")?,
        field("volume"),
    ))
}

/// The input overrides of a JS object (numbers, strings, booleans).
fn inputs_of(overrides: &JsValue, base: &mut Inputs) -> Result<(), JsValue> {
    if overrides.is_undefined() || overrides.is_null() {
        return Ok(());
    }
    let object: &Object = overrides
        .dyn_ref::<Object>()
        .ok_or_else(|| js_err("vela: indicator inputs must be an object"))?;
    for entry in Object::entries(object).iter() {
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
            return Err(js_err(format!(
                "vela: input \"{key}\" must be a number, string or boolean"
            )));
        };
        base.insert(key, parsed);
    }
    Ok(())
}

/// A study mounted through [`VelaChart::add_indicator`].
struct Mounted {
    id: String,
    spec: ClassicIndicatorSpec,
    inputs: Inputs,
    handle: IndicatorRenderHandle,
    pane: String,
    /// Hidden by the legend eye: not computed, not painted.
    hidden: bool,
}

#[derive(Default)]
struct Book {
    /// The bars as the renderer draws them (the raw series, or its price-style view).
    bars: Vec<Ohlcv>,
    /// The raw series: the source of truth the view derives from.
    raw: Vec<Ohlcv>,
    /// The bar transform of the shown price style (Heikin Ashi), if any.
    transform: Option<Arc<dyn BarTransform>>,
    /// The study panes, top to bottom (the price pane is implicit).
    pane_order: Vec<String>,
    studies: Vec<Mounted>,
    volume: Option<IndicatorRenderHandle>,
    counter: u32,
    pane_counter: u32,
}

struct Inner {
    host: Rc<BrowserHost>,
    renderer: Rc<NativeRenderer>,
    wiring: RefCell<Option<InputWiring>>,
    watch: RefCell<Option<SizeWatch>>,
    book: RefCell<Book>,
    destroyed: std::cell::Cell<bool>,
    /// The core drawing controller over this renderer, and its control surface: the same
    /// pair the `Vela` facade builds, here without an engine (no series gateway, no
    /// chart events).
    drawing_ctrl: DrawingController,
    drawings: DrawingsControl,
    /// The chart-type registry the renderer and the settings dialog share.
    registry: Rc<RefCell<ChartTypeRegistry>>,
    /// The renderer event subscriptions (input edits, legend actions, theme, style).
    subs: RefCell<Vec<Unsubscribe>>,
    /// The JavaScript callback of `onConfigChanged`.
    config_cb: RefCell<Option<js_sys::Function>>,
    /// The locale packs the chrome's translator reads (shipped packs register lazily).
    locales: Arc<LocaleRegistry>,
}

/// Stamp the pane ids the way the engine's `place_model` does.
fn place(model: &mut IndicatorModel, id: &str, pane_id: &str) {
    model.id = id.to_string();
    model.pane_id = Some(pane_id.to_string());
    let pane_of = |overlay: bool| if overlay { PRICE_PANE } else { pane_id }.to_string();
    for s in &mut model.series {
        s.pane_id = pane_of(s.overlay);
    }
    for f in &mut model.fills {
        f.pane_id = pane_of(f.overlay);
    }
    for b in &mut model.backgrounds {
        b.pane_id = pane_of(b.overlay);
    }
    for l in &mut model.price_lines {
        l.pane_id = pane_id.to_string();
    }
    for l in &mut model.lines {
        l.pane_id = pane_of(l.overlay);
    }
    for b in &mut model.boxes {
        b.pane_id = pane_of(b.overlay);
    }
    for l in &mut model.labels {
        l.pane_id = pane_of(l.overlay);
    }
    for p in &mut model.polylines {
        p.pane_id = pane_of(p.overlay);
    }
    for l in &mut model.linefills {
        l.pane_id = pane_of(l.overlay);
    }
    for t in &mut model.tables {
        t.pane_id = pane_of(t.overlay);
    }
}

/// An `IndicatorModel` around a classic study's output (the engine's
/// `build_native_model`, which is private to `vela-core`).
fn study_model(
    spec: &ClassicIndicatorSpec,
    out: NativeIndicatorOutput,
    inputs: &Inputs,
    id: &str,
    pane_id: &str,
) -> IndicatorModel {
    let descriptor = classic_descriptor(spec);
    let mut model = IndicatorModel {
        id: id.to_string(),
        title: text_of(None, &spec.title, &[]),
        shorttitle: spec.short_title.as_ref().map(|m| text_of(None, m, &[])),
        display_title: None,
        display_shorttitle: None,
        overlay: spec.overlay,
        pane_hint: if spec.overlay {
            PaneHint::Price
        } else {
            PaneHint::New
        },
        native: Some(NativeTag {
            indicator_type: spec.indicator_type.to_string(),
            channel: None,
        }),
        legend: None,
        pane_axis: out.pane_axis,
        pane_id: None,
        own_scale: false,
        anchor_time: None,
        series: out.series,
        fills: out.fills,
        backgrounds: out.backgrounds,
        price_lines: out.price_lines,
        lines: out.lines,
        boxes: out.boxes,
        labels: out.labels,
        polylines: out.polylines,
        linefills: out.linefills,
        tables: out.tables,
        bar_colors: Vec::new(),
        trades: Vec::new(),
        inputs: descriptor.inputs_schema,
        input_values: inputs.clone(),
        props: Vec::new(),
        prop_values: Inputs::new(),
    };
    place(&mut model, id, pane_id);
    model
}

/// A value patch carrying the recomputed line series in full.
fn study_patch(id: &str, out: &NativeIndicatorOutput, bars: &[Ohlcv]) -> ScenePatch {
    let series = out
        .series
        .iter()
        .filter_map(|s| match &s.data {
            SeriesData::LineLike { points, .. } => Some(SeriesValueDelta {
                series_id: s.id.clone(),
                values: SeriesValues::Points(points.clone()),
            }),
            _ => None,
        })
        .collect();
    ScenePatch::Value(ValuePatch {
        indicator_id: id.to_string(),
        dirty: DirtyRange {
            from: bars.first().map_or(0, |b| b.time),
            to: bars.last().map_or(0, |b| b.time),
        },
        anchor_time: AnchorUpdate::Clear,
        series,
        lines: Some(out.lines.clone()),
        boxes: Some(out.boxes.clone()),
        labels: Some(out.labels.clone()),
        polylines: Some(out.polylines.clone()),
        linefills: Some(out.linefills.clone()),
        tables: Some(out.tables.clone()),
        trades: None,
    })
}

impl Inner {
    fn live(&self) -> Result<(), JsValue> {
        if self.destroyed.get() {
            Err(js_err("vela: the chart was destroyed"))
        } else {
            Ok(())
        }
    }

    /// Recompute every study over the current bars and push the new values.
    fn refresh_studies(&self) {
        let (bars, jobs): (Vec<Ohlcv>, Vec<_>) = {
            let b = self.book.borrow();
            (
                b.bars.clone(),
                b.studies
                    .iter()
                    .filter(|s| !s.hidden)
                    .map(|s| {
                        (
                            s.id.clone(),
                            s.spec.clone(),
                            s.inputs.clone(),
                            s.handle.clone(),
                        )
                    })
                    .collect(),
            )
        };
        if bars.is_empty() {
            return;
        }
        for (id, spec, inputs, handle) in jobs {
            let out = classic_native_output(&spec, &bars, &inputs);
            self.renderer
                .update_indicator(&handle, &study_patch(&id, &out, &bars));
        }
    }
}

impl Inner {
    /// An input edit from the settings dialog: store it (the chart keeps the new values),
    /// tell the legend feed, and recompute that study over the current bars.
    fn on_input_change(&self, e: &InputChangeEvent) {
        if e.kind() == InputChangeKind::Prop {
            return; // the classic studies declare no props
        }
        let (handle, inputs) = {
            let mut book = self.book.borrow_mut();
            let Some(s) = book.studies.iter_mut().find(|s| s.id == e.indicator_id) else {
                return;
            };
            s.inputs.insert(e.key.clone(), e.value.clone());
            (s.handle.clone(), s.inputs.clone())
        };
        self.renderer.set_indicator_inputs(&handle, &inputs, None);
        self.refresh_studies();
    }

    /// The legend close button.
    fn on_remove(&self, id: &str) {
        if id == VOLUME_ID {
            let handle = self.book.borrow_mut().volume.take();
            if let Some(handle) = handle {
                self.renderer.remove_indicator(&handle);
            }
            return;
        }
        let removed = {
            let mut book = self.book.borrow_mut();
            book.studies
                .iter()
                .position(|s| s.id == id)
                .map(|i| book.studies.remove(i))
        };
        if let Some(study) = removed {
            self.renderer.remove_indicator(&study.handle);
            if study.pane != PRICE_PANE {
                self.renderer.remove_pane(&study.pane);
                self.book
                    .borrow_mut()
                    .pane_order
                    .retain(|p| *p != study.pane);
            }
        }
    }

    /// The legend eye: a hidden study is neither drawn nor computed; showing it
    /// recomputes it over the current bars.
    fn on_toggle(&self, id: &str, visible: bool) {
        if id == VOLUME_ID {
            let handle = self.book.borrow().volume.clone();
            if let Some(handle) = handle {
                self.renderer.set_indicator_visible(&handle, visible);
            }
            return;
        }
        let handle = {
            let mut book = self.book.borrow_mut();
            let Some(s) = book.studies.iter_mut().find(|s| s.id == id) else {
                return;
            };
            s.hidden = !visible;
            s.handle.clone()
        };
        self.renderer.set_indicator_visible(&handle, visible);
        if visible {
            self.refresh_studies();
        }
    }

    /// The legend Move-to menu or row drag: the engine move_indicator for the direct
    /// chart panes.
    fn on_move(&self, id: &str, target: &MoveTarget) {
        let (handle, old_pane, overlay) = {
            let book = self.book.borrow();
            let Some(s) = book.studies.iter().find(|s| s.id == id) else {
                return;
            };
            (s.handle.clone(), s.pane.clone(), s.spec.overlay)
        };
        let (pane, own_scale, insert): (String, bool, Option<NewPanePlacement>) = match target {
            MoveTarget::Price => (PRICE_PANE.to_string(), !overlay, None),
            MoveTarget::Pane(p) if p == PRICE_PANE => (PRICE_PANE.to_string(), !overlay, None),
            MoveTarget::Pane(p) => {
                if !self.book.borrow().pane_order.contains(p) {
                    return;
                }
                (p.clone(), true, None)
            }
            MoveTarget::NewPane(spec) => {
                let mut book = self.book.borrow_mut();
                book.pane_counter += 1;
                let mut n = book.pane_counter;
                let mut pane = format!("pane-{id}-{n}");
                while book.pane_order.contains(&pane) || pane == old_pane {
                    n += 1;
                    pane = format!("pane-{id}-{n}");
                }
                (pane, false, Some(spec.clone()))
            }
        };
        if pane == old_pane {
            return;
        }
        if pane != PRICE_PANE && !self.book.borrow().pane_order.contains(&pane) {
            let mut book = self.book.borrow_mut();
            let mut at = book.pane_order.len();
            if let Some(spec) = &insert {
                let find = |name: &Option<String>| {
                    name.as_deref()
                        .filter(|n| !n.is_empty())
                        .and_then(|n| book.pane_order.iter().position(|p| p == n))
                };
                if let Some(i) = find(&spec.before) {
                    at = i;
                } else if let Some(i) = find(&spec.after) {
                    at = i + 1;
                }
            }
            book.pane_order.insert(at, pane.clone());
            let order = book.pane_order.len() as i32;
            drop(book);
            self.renderer.ensure_pane(&Pane {
                id: pane.clone(),
                kind: PaneKind::Study,
                order,
                height_weight: None,
                title: None,
            });
        }
        self.renderer.set_indicator_pane(&handle, &pane, own_scale);
        if let Some(s) = self
            .book
            .borrow_mut()
            .studies
            .iter_mut()
            .find(|s| s.id == id)
        {
            s.pane = pane;
        }
        let unused = old_pane != PRICE_PANE
            && !self
                .book
                .borrow()
                .studies
                .iter()
                .any(|s| s.pane == old_pane);
        if unused {
            self.renderer.remove_pane(&old_pane);
            self.book.borrow_mut().pane_order.retain(|p| *p != old_pane);
        }
        let mut order = vec![PRICE_PANE.to_string()];
        order.extend(self.book.borrow().pane_order.iter().cloned());
        self.renderer.order_panes(&order);
    }

    /// The settings dialog Theme row: dark or light.
    fn on_theme(&self, name: &str) {
        if let Ok(theme) = theme_named(name) {
            self.renderer.set_theme(&theme);
            self.host.dialogs.refresh_open();
        }
    }

    /// The shown price style changed: a style with a bar transform (Heikin Ashi) draws the
    /// transformed view of the raw bars, which the studies then compute over.
    fn on_price_style(&self, style: &str) {
        let transform = bar_transform_for(&self.registry.borrow(), style);
        let view = {
            let mut book = self.book.borrow_mut();
            let same = match (&transform, &book.transform) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            };
            if same {
                return;
            }
            book.transform = transform.clone();
            if book.raw.is_empty() {
                return;
            }
            book.bars = match &transform {
                Some(t) => t.full(&book.raw),
                None => book.raw.clone(),
            };
            book.bars.clone()
        };
        self.renderer.set_bars(
            &view,
            SetBarsOptions {
                preserve_view: true,
            },
        );
        self.refresh_studies();
    }

    /// Subscribe to the renderer events. Weak: the subscriptions never keep the chart alive.
    fn wire(self: &Rc<Self>) {
        let mut subs = self.subs.borrow_mut();
        let wk = Rc::downgrade(self);
        let w = wk.clone();
        subs.push(self.renderer.on_input_change(Box::new(move |e| {
            if let Some(i) = w.upgrade() {
                i.on_input_change(e);
            }
        })));
        let w = wk.clone();
        subs.push(
            self.renderer
                .on_remove_indicator(Box::new(move |id: &String| {
                    if let Some(i) = w.upgrade() {
                        i.on_remove(id);
                    }
                })),
        );
        let w = wk.clone();
        subs.extend(self.renderer.on_toggle_indicator_visible(Box::new(
            move |id: &str, visible| {
                if let Some(i) = w.upgrade() {
                    i.on_toggle(id, visible);
                }
            },
        )));
        let w = wk.clone();
        subs.extend(self.renderer.on_move_indicator(Box::new(
            move |id: &str, target: &MoveTarget| {
                if let Some(i) = w.upgrade() {
                    i.on_move(id, target);
                }
            },
        )));
        let w = wk.clone();
        subs.extend(
            self.renderer
                .on_theme_select(Box::new(move |name: &String| {
                    if let Some(i) = w.upgrade() {
                        i.on_theme(name);
                    }
                })),
        );
        let w = wk.clone();
        subs.extend(
            self.renderer
                .on_price_style_change(Box::new(move |style: &String| {
                    if let Some(i) = w.upgrade() {
                        i.on_price_style(style);
                    }
                })),
        );
        let w = wk;
        subs.extend(self.renderer.on_config_changed(Box::new(move || {
            let Some(i) = w.upgrade() else { return };
            let cb = i.config_cb.borrow().clone();
            if let Some(cb) = cb {
                let _ = cb.call0(&JsValue::NULL);
            }
        })));
    }
}

/// The input values as a JSON object (text).
pub(crate) fn inputs_json(inputs: &Inputs) -> String {
    let map: serde_json::Map<String, serde_json::Value> = inputs
        .iter()
        .map(|(k, v)| {
            let value = match v {
                InputValue::Number(n) => serde_json::json!(n),
                InputValue::Text(s) => serde_json::json!(s),
                InputValue::Bool(b) => serde_json::json!(b),
            };
            (k.clone(), value)
        })
        .collect();
    serde_json::Value::Object(map).to_string()
}

/// A translator over the browser Intl: the dialogs' zone labels carry live UTC offsets.
/// Over `registry` for `locale` (English when none).
fn translator_over(registry: &Arc<LocaleRegistry>, locale: Option<&str>) -> Rc<dyn Translator> {
    let intl: Arc<dyn Intl> = Arc::new(BrowserIntl);
    Rc::new(create_translator(
        LocalizationOptions {
            locale: locale.map(str::to_string),
            ..LocalizationOptions::default()
        },
        intl,
        registry.clone(),
    ))
}

impl VelaChart {
    fn apply_bars(&self, bars: Vec<Ohlcv>) {
        let view = {
            let mut book = self.inner.book.borrow_mut();
            book.raw = bars.clone();
            book.bars = match &book.transform {
                Some(t) => t.full(&bars),
                None => bars,
            };
            book.bars.clone()
        };
        self.inner
            .renderer
            .set_bars(&view, SetBarsOptions::default());
        self.inner.refresh_studies();
    }
}

/// A chart on one container element. See the module documentation.
#[wasm_bindgen]
pub struct VelaChart {
    inner: Rc<Inner>,
}

#[wasm_bindgen]
impl VelaChart {
    /// Build a chart inside `container` (an element with a size) and mount it.
    /// `theme` is `"dark"` (default) or `"light"`.
    #[wasm_bindgen(constructor)]
    pub fn new(container: &Element, theme: Option<String>) -> Result<VelaChart, JsValue> {
        crate::install();
        let theme = theme_named(theme.as_deref().unwrap_or("dark"))?;
        let platform: Rc<dyn Platform> = Rc::new(BrowserPlatform::new());
        let host = BrowserHost::new(container, platform.clone())?;
        let native: Rc<dyn NativeHost> = host.clone();
        let registry = Rc::new(RefCell::new(ChartTypeRegistry::builtin()));
        let parts = NativeParts {
            chart_types: registry.clone(),
            ..NativeParts::default()
        };
        let renderer = NativeRenderer::with_parts(native, Some(display_options(&theme)), parts);
        renderer.mount(&theme);
        host.bind_renderer(&renderer);
        host.dialogs.bind(&renderer, registry.clone());
        let locales = LocaleRegistry::new(Arc::new(BrowserIntl) as Arc<dyn Intl>);
        crate::locales::attach(&locales);
        let translator = translator_over(&locales, None);
        host.dialogs.set_translator(Some(translator.clone()));
        host.chrome().set_translator(Some(translator.clone()));
        host.drawings_ui().set_translator(Some(translator.clone()));
        host.set_translator(Some(translator));
        // Drawings need the core controller, which owns the model, history and tool state.
        // Without an engine it is built straight over the renderer: the toolbar starts
        // hidden (`showDrawingToolbar(true)` docks it), and finer-timeframe series for the
        // magnifier are not available (no series gateway).
        let drawing_ctrl = DrawingController::new(
            renderer.clone() as Rc<dyn ChartRenderer>,
            ChartEventBus::new(),
            Rc::new(DrawingRegistry::builtin()),
            Some(DrawingsOption::Bool(false)),
            None,
            no_translator(),
        );
        let drawings = DrawingsControl::new(drawing_ctrl.clone(), platform.clone());
        let wiring = InputWiring::attach(renderer.clone(), host.clone(), platform);
        let watch = SizeWatch::start(renderer.clone(), host.clone());
        let inner = Rc::new(Inner {
            host,
            renderer,
            wiring: RefCell::new(wiring),
            watch: RefCell::new(Some(watch)),
            book: RefCell::new(Book::default()),
            destroyed: std::cell::Cell::new(false),
            drawing_ctrl,
            drawings,
            registry,
            subs: RefCell::new(Vec::new()),
            config_cb: RefCell::new(None),
            locales,
        });
        inner.wire();
        // The indicator dialog reads the values the chart keeps (the mounted model keeps
        // its open-time ones).
        let weak = Rc::downgrade(&inner);
        inner
            .host
            .dialogs
            .set_values_source(Some(Rc::new(move |id: &str| {
                let i = weak.upgrade()?;
                let book = i.book.borrow();
                book.studies
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| (s.inputs.clone(), Inputs::new()))
            })));
        Ok(VelaChart { inner })
    }

    /// Replace every bar: an array of `{ time, open, high, low, close, volume }`
    /// (`time` in epoch milliseconds, ascending; `volume` optional). Re-frames
    /// the view.
    #[wasm_bindgen(js_name = setBars)]
    pub fn set_bars(&self, bars: &JsValue) -> Result<(), JsValue> {
        self.inner.live()?;
        let list: &Array = bars
            .dyn_ref::<Array>()
            .ok_or_else(|| js_err("vela: setBars takes an array of bars"))?;
        let parsed = list
            .iter()
            .map(|v| bar_of(&v))
            .collect::<Result<Vec<_>, _>>()?;
        self.apply_bars(parsed);
        Ok(())
    }

    /// Replace every bar from parallel typed arrays (`Float64Array`s; time in
    /// epoch milliseconds; a NaN volume is a missing one).
    #[wasm_bindgen(js_name = setBarsTyped)]
    pub fn set_bars_typed(
        &self,
        time: &[f64],
        open: &[f64],
        high: &[f64],
        low: &[f64],
        close: &[f64],
        volume: &[f64],
    ) -> Result<(), JsValue> {
        self.inner.live()?;
        let n = time.len();
        if [open.len(), high.len(), low.len(), close.len()]
            .iter()
            .any(|l| *l != n)
        {
            return Err(js_err("vela: setBarsTyped needs arrays of one length"));
        }
        let parsed = (0..n)
            .map(|i| {
                Ohlcv::new(
                    time[i] as i64,
                    open[i],
                    high[i],
                    low[i],
                    close[i],
                    volume.get(i).copied().filter(|v| v.is_finite()),
                )
            })
            .collect();
        self.apply_bars(parsed);
        Ok(())
    }

    /// Replace the forming (last) bar when `time` equals its time, append a
    /// newer bar otherwise (an older one is ignored). A NaN volume is a missing one.
    #[wasm_bindgen(js_name = updateBar)]
    pub fn update_bar(
        &self,
        time: f64,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        volume: f64,
    ) -> Result<(), JsValue> {
        self.inner.live()?;
        let bar = Ohlcv::new(
            time as i64,
            open,
            high,
            low,
            close,
            Some(volume).filter(|v| v.is_finite()),
        );
        let view_bar = {
            let mut book = self.inner.book.borrow_mut();
            let replace = match book.raw.last().map(|b| b.time) {
                Some(last) if bar.time == last => {
                    *book.raw.last_mut().expect("last exists") = bar;
                    true
                }
                Some(last) if bar.time < last => return Ok(()),
                _ => {
                    book.raw.push(bar);
                    false
                }
            };
            let view = match &book.transform {
                Some(t) => {
                    let prev = if replace {
                        book.bars.len().checked_sub(2).map(|i| book.bars[i])
                    } else {
                        book.bars.last().copied()
                    };
                    t.next(&bar, prev.as_ref())
                }
                None => bar,
            };
            if replace {
                *book.bars.last_mut().expect("last exists") = view;
            } else {
                book.bars.push(view);
            }
            view
        };
        self.inner.renderer.update_bar(&view_bar);
        self.inner.refresh_studies();
        Ok(())
    }

    /// The catalog of classic indicator types `addIndicator` takes.
    #[wasm_bindgen(js_name = indicatorTypes)]
    pub fn indicator_types() -> Vec<String> {
        classic_specs()
            .iter()
            .map(|s| s.indicator_type.to_string())
            .collect()
    }

    /// Add a classic indicator by type (`"sma"`, `"ema"`, `"rsi"`, `"macd"`,
    /// ...) with its default inputs, optionally overridden by `inputs` (an
    /// object). An overlay study draws on the price pane, any other in a pane
    /// of its own. Returns the indicator id.
    #[wasm_bindgen(js_name = addIndicator)]
    pub fn add_indicator(&self, kind: &str, inputs: &JsValue) -> Result<String, JsValue> {
        self.inner.live()?;
        let spec = classic_specs()
            .into_iter()
            .find(|s| s.indicator_type == kind)
            .ok_or_else(|| js_err(format!("vela: unknown indicator type \"{kind}\"")))?;
        let mut values = spec.default_inputs();
        inputs_of(inputs, &mut values)?;
        let (id, pane, order) = {
            let mut book = self.inner.book.borrow_mut();
            book.counter += 1;
            let id = format!("{kind}-{}", book.counter);
            if spec.overlay {
                (id, PRICE_PANE.to_string(), 0)
            } else {
                book.pane_counter += 1;
                let pane = format!("pane-{}", id);
                (id, pane, book.pane_counter as i32)
            }
        };
        if !spec.overlay {
            self.inner.renderer.ensure_pane(&Pane {
                id: pane.clone(),
                kind: PaneKind::Study,
                order,
                height_weight: None,
                title: None,
            });
        }
        let bars = self.inner.book.borrow().bars.clone();
        let out = classic_native_output(&spec, &bars, &values);
        let model = study_model(&spec, out, &values, &id, &pane);
        let handle = self.inner.renderer.mount_indicator(&model);
        self.inner.book.borrow_mut().studies.push(Mounted {
            id: id.clone(),
            spec,
            inputs: values,
            handle,
            pane: pane.clone(),
            hidden: false,
        });
        if pane != PRICE_PANE {
            self.inner.book.borrow_mut().pane_order.push(pane);
        }
        Ok(id)
    }

    /// Remove a study by the id `addIndicator` returned; its pane goes with it.
    #[wasm_bindgen(js_name = removeIndicator)]
    pub fn remove_indicator(&self, id: &str) -> Result<bool, JsValue> {
        self.inner.live()?;
        let removed = {
            let mut book = self.inner.book.borrow_mut();
            book.studies
                .iter()
                .position(|s| s.id == id)
                .map(|i| book.studies.remove(i))
        };
        let Some(study) = removed else {
            return Ok(false);
        };
        self.inner.renderer.remove_indicator(&study.handle);
        if study.pane != PRICE_PANE {
            self.inner.renderer.remove_pane(&study.pane);
            self.inner
                .book
                .borrow_mut()
                .pane_order
                .retain(|p| *p != study.pane);
        }
        Ok(true)
    }

    /// Show the volume columns (bottom of the price pane). Idempotent.
    #[wasm_bindgen(js_name = addVolume)]
    pub fn add_volume(&self) -> Result<(), JsValue> {
        self.inner.live()?;
        if self.inner.book.borrow().volume.is_some() {
            return Ok(());
        }
        let mut model = IndicatorModel {
            id: VOLUME_ID.to_string(),
            title: "Volume".to_string(),
            shorttitle: None,
            display_title: None,
            display_shorttitle: None,
            overlay: true,
            pane_hint: PaneHint::Price,
            native: Some(NativeTag {
                indicator_type: VOLUME_ID.to_string(),
                channel: None,
            }),
            legend: None,
            pane_axis: None,
            pane_id: None,
            own_scale: false,
            anchor_time: None,
            series: Vec::new(),
            fills: Vec::new(),
            backgrounds: Vec::new(),
            price_lines: Vec::new(),
            lines: Vec::new(),
            boxes: Vec::new(),
            labels: Vec::new(),
            polylines: Vec::new(),
            linefills: Vec::new(),
            tables: Vec::new(),
            bar_colors: Vec::new(),
            trades: Vec::new(),
            inputs: Vec::new(),
            input_values: Inputs::new(),
            props: Vec::new(),
            prop_values: Inputs::new(),
        };
        place(&mut model, VOLUME_ID, PRICE_PANE);
        let handle = self.inner.renderer.mount_indicator(&model);
        // The columns are a renderer layer fed through the native-data channel.
        if let Ok(payload) = serde_json::to_value(volume_layer_data(&Inputs::new())) {
            self.inner.renderer.set_native_data(VOLUME_ID, &payload);
        }
        self.inner.book.borrow_mut().volume = Some(handle);
        Ok(())
    }

    /// Hide the volume columns.
    #[wasm_bindgen(js_name = removeVolume)]
    pub fn remove_volume(&self) -> Result<(), JsValue> {
        self.inner.live()?;
        let handle = self.inner.book.borrow_mut().volume.take();
        if let Some(handle) = handle {
            self.inner.renderer.remove_indicator(&handle);
        }
        Ok(())
    }

    /// Re-read the container's size now (the chart also watches it itself).
    pub fn resize(&self) -> Result<(), JsValue> {
        self.inner.live()?;
        self.inner.renderer.resize();
        Ok(())
    }

    /// Switch between `"dark"` and `"light"`.
    #[wasm_bindgen(js_name = setTheme)]
    pub fn set_theme(&self, theme: &str) -> Result<(), JsValue> {
        self.inner.live()?;
        self.inner.renderer.set_theme(&theme_named(theme)?);
        self.inner.host.dialogs.refresh_open();
        Ok(())
    }

    /// The language of the dialogs, the legend and the drawing widgets (a BCP 47 tag; the
    /// shipped pack for it is registered on first use, English where there is none).
    #[wasm_bindgen(js_name = setLocale)]
    pub fn set_locale(&self, locale: &str) -> Result<(), JsValue> {
        self.inner.live()?;
        crate::locales::ensure(&self.inner.locales, locale);
        let translator = translator_over(&self.inner.locales, Some(locale));
        self.inner
            .host
            .dialogs
            .set_translator(Some(translator.clone()));
        self.inner
            .host
            .chrome()
            .set_translator(Some(translator.clone()));
        self.inner
            .host
            .drawings_ui()
            .set_translator(Some(translator.clone()));
        self.inner.host.set_translator(Some(translator));
        Ok(())
    }

    /// The number of bars now on the chart.
    #[wasm_bindgen(js_name = barCount)]
    pub fn bar_count(&self) -> usize {
        self.inner.book.borrow().bars.len()
    }

    /// The viewport as `[barSpacing, rightOffset]` (device-independent units:
    /// CSS px per bar and bars of whitespace right of the last one).
    pub fn viewport(&self) -> Vec<f64> {
        let vp = self.inner.renderer.coordinates().viewport();
        vec![vp.bar_spacing, vp.right_offset]
    }

    /// The visible time range as `[from, to]` in epoch milliseconds, or an
    /// empty array before there are bars.
    #[wasm_bindgen(js_name = visibleRange)]
    pub fn visible_range(&self) -> Vec<f64> {
        self.inner
            .renderer
            .get_visible_range()
            .map(|r| vec![r.from as f64, r.to as f64])
            .unwrap_or_default()
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
        self.inner.live()?;
        drawings_api::arm(&self.inner.drawings, type_key)
    }

    /// Back to the pointer: disarm the tool, leave the measure ruler and the eraser.
    #[wasm_bindgen(js_name = cancelDrawingTool)]
    pub fn cancel_drawing_tool(&self) -> Result<(), JsValue> {
        self.inner.live()?;
        drawings_api::cancel(&self.inner.drawings)
    }

    /// The armed tool's type key, or `undefined` for the pointer.
    #[wasm_bindgen(js_name = drawingTool)]
    pub fn drawing_tool(&self) -> Option<String> {
        drawings_api::tool(&self.inner.drawings)
    }

    /// Undo the last drawing edit; whether there was one.
    #[wasm_bindgen(js_name = undoDrawing)]
    pub fn undo_drawing(&self) -> bool {
        drawings_api::undo(&self.inner.drawings)
    }

    /// Redo the last undone drawing edit; whether there was one.
    #[wasm_bindgen(js_name = redoDrawing)]
    pub fn redo_drawing(&self) -> bool {
        drawings_api::redo(&self.inner.drawings)
    }

    /// Remove every drawing (one undo step). Returns how many there were.
    #[wasm_bindgen(js_name = clearDrawings)]
    pub fn clear_drawings(&self) -> usize {
        drawings_api::clear(&self.inner.drawings)
    }

    /// How many drawings the chart has.
    #[wasm_bindgen(js_name = drawingCount)]
    pub fn drawing_count(&self) -> usize {
        drawings_api::count(&self.inner.drawings)
    }

    /// The drawings as a JSON document (text): `{ "version": 1, "drawings": [...] }`.
    #[wasm_bindgen(js_name = exportDrawings)]
    pub fn export_drawings(&self) -> Result<String, JsValue> {
        drawings_api::export(&self.inner.drawings)
    }

    /// Replace the drawings with a document from `exportDrawings` (unreadable entries are
    /// dropped; text that is not JSON is an error). Returns how many drawings the chart has.
    #[wasm_bindgen(js_name = importDrawings)]
    pub fn import_drawings(&self, json: &str) -> Result<usize, JsValue> {
        self.inner.live()?;
        drawings_api::import(&self.inner.drawings, json)
    }

    /// Show or hide the docked drawing toolbar (hidden by default; showing it reserves
    /// its gutter on the left of the plot).
    #[wasm_bindgen(js_name = showDrawingToolbar)]
    pub fn show_drawing_toolbar(&self, visible: bool) -> Result<(), JsValue> {
        self.inner.live()?;
        drawings_api::show_toolbar(&self.inner.drawings, visible)
    }

    // ── dialogs and config ──

    /// Show or hide the gear that opens the chart settings dialog (hidden by default).
    #[wasm_bindgen(js_name = showSettings)]
    pub fn show_settings(&self, visible: bool) -> Result<(), JsValue> {
        self.inner.live()?;
        self.inner
            .renderer
            .apply_feature("settings", &serde_json::json!(visible));
        Ok(())
    }

    /// Open (or toggle) the chart settings dialog, landing on the tab titled `section`
    /// (`"Symbol"`, `"Scales and lines"`, `"Canvas"`, `"Events"`, or a chart type tab).
    #[wasm_bindgen(js_name = openSettings)]
    pub fn open_settings(&self, section: Option<String>) -> Result<(), JsValue> {
        self.inner.live()?;
        self.inner.renderer.open_settings_dialog(section.as_deref());
        Ok(())
    }

    /// Open the settings dialog of one indicator by the id `addIndicator` returned.
    #[wasm_bindgen(js_name = openIndicatorSettings)]
    pub fn open_indicator_settings(&self, id: &str) -> Result<(), JsValue> {
        self.inner.live()?;
        self.inner.renderer.open_indicator_settings(id);
        Ok(())
    }

    /// Close every in-chart dialog that is open.
    #[wasm_bindgen(js_name = closeDialogs)]
    pub fn close_dialogs(&self) {
        if !self.inner.destroyed.get() {
            self.inner.renderer.close_dialogs();
        }
    }

    /// The chart configuration as JSON text (what the settings dialog edits).
    #[wasm_bindgen(js_name = getConfig)]
    pub fn get_config(&self) -> Option<String> {
        self.inner.renderer.get_config().map(|v| v.to_string())
    }

    /// Apply a configuration document (JSON text, whole or partial: only what it names
    /// changes). Text that is not a JSON object is an error.
    #[wasm_bindgen(js_name = applyConfig)]
    pub fn apply_config(&self, json: &str) -> Result<(), JsValue> {
        self.inner.live()?;
        let doc: serde_json::Value =
            serde_json::from_str(json).map_err(|e| js_err(format!("vela: applyConfig: {e}")))?;
        if !doc.is_object() {
            return Err(js_err("vela: applyConfig takes a JSON object"));
        }
        self.inner.renderer.apply_config(&doc);
        Ok(())
    }

    /// Call `callback` whenever the configuration changes (a settings edit, `applyConfig`);
    /// `null` removes it. Read the new state with `getConfig`.
    #[wasm_bindgen(js_name = onConfigChanged)]
    pub fn on_config_changed(&self, callback: Option<js_sys::Function>) {
        *self.inner.config_cb.borrow_mut() = callback;
    }

    /// The current input values of a study as JSON text, or `undefined` for an unknown id.
    #[wasm_bindgen(js_name = indicatorInputs)]
    pub fn indicator_inputs(&self, id: &str) -> Option<String> {
        let book = self.inner.book.borrow();
        book.studies
            .iter()
            .find(|s| s.id == id)
            .map(|s| inputs_json(&s.inputs))
    }

    /// Hide settings by id (a tab, a group or a row, subtree semantics): presentation only,
    /// hidden values keep being stored and applied. `listSettingsIds` names every id.
    #[wasm_bindgen(js_name = setHiddenSettings)]
    pub fn set_hidden_settings(&self, ids: Vec<String>) {
        self.inner
            .renderer
            .set_settings_visibility(&SettingsVisibility { hidden: Some(ids) });
    }

    /// Every setting id the dialog stamps (for `setHiddenSettings`).
    #[wasm_bindgen(js_name = listSettingsIds)]
    pub fn list_settings_ids(&self) -> Vec<String> {
        self.inner.renderer.list_settings_ids()
    }

    /// The plot values the legend shows beside an indicator title, as of the last frame.
    #[wasm_bindgen(js_name = indicatorReadout)]
    pub fn indicator_readout(&self, id: &str) -> Vec<String> {
        self.inner
            .renderer
            .legend_values()
            .into_iter()
            .find(|(i, _)| i == id)
            .map(|(_, values)| values.into_iter().map(|v| v.value).collect())
            .unwrap_or_default()
    }

    /// Tear the chart down: listeners, observers, canvases and DOM. Idempotent.
    pub fn destroy(&self) {
        if self.inner.destroyed.replace(true) {
            return;
        }
        // Listeners and observers first: nothing may call into a dying renderer.
        let subs: Vec<_> = self.inner.subs.borrow_mut().drain(..).collect();
        for unsub in subs {
            unsub();
        }
        // Taken out first and dropped after the borrow ends: dropping a listener or observer
        // set must not run while the cell is borrowed.
        let wiring = self.inner.wiring.borrow_mut().take();
        drop(wiring);
        let watch = self.inner.watch.borrow_mut().take();
        drop(watch);
        self.inner.drawing_ctrl.destroy();
        self.inner.renderer.destroy();
        self.inner.host.dispose();
    }
}

/// The geometry backend choice (see [`crate::gl`]).
#[wasm_bindgen]
impl VelaChart {
    /// As the constructor, with `options` `{ backend?, glow? }`: `backend` is `"auto"`
    /// (default: WebGL2 when the browser has it, else canvas2d), `"webgl2"` or `"canvas2d"`;
    /// `glow` is the WebGL2 neon-glow amount (default 0). The backend is fixed at mount, as in
    /// the original; `glow` can change later with `setGlow`.
    #[wasm_bindgen(js_name = withOptions)]
    pub fn with_options(
        container: &Element,
        theme: Option<String>,
        options: &JsValue,
    ) -> Result<VelaChart, JsValue> {
        let chosen = crate::gl::render_options_of(options)?;
        crate::gl::with_construction_options(chosen, || VelaChart::new(container, theme))
    }

    /// The geometry backend in force: `"webgl2"` or `"canvas2d"` (also `"canvas2d"` after a
    /// WebGL2 program failed to build or a lost context could not be restored).
    pub fn backend(&self) -> String {
        crate::gl::backend_name(self.inner.renderer.geometry_backend()).to_string()
    }

    /// The WebGL2 neon-glow amount (canvas2d ignores it); 0 turns it off.
    #[wasm_bindgen(js_name = setGlow)]
    pub fn set_glow(&self, amount: f64) {
        let amount = if amount.is_finite() {
            amount.max(0.0)
        } else {
            0.0
        };
        self.inner
            .renderer
            .apply_feature("glow", &serde_json::json!(amount));
    }
}
