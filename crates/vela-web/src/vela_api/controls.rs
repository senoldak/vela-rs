//! The control surfaces of the chart as JavaScript classes: `chart.data`, `chart.panes`,
//! `chart.replay`, `chart.renderer`, `chart.drawings`, `chart.marks`, and the
//! `IndicatorHandle` the indicator methods return. Each wraps the matching control of
//! `vela_core::controls` and takes and returns the original's shapes.
//!
//! A method the original documents as returning `this` returns `()` here; the prototype is
//! patched once (see [`chain`]) so the JavaScript method returns the receiver.
//!
//! After the chart is destroyed every call is a quiet no-op (queries answer empty).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use js_sys::{Array, Function, Promise};
use serde_json::{Value, json};
use vela_core::controls::replay_control::{
    ReplayStartOptions, ReplayTick, ReplayTickRequest, ReplayTickSource,
};
use vela_core::drawings::controller::{AddInit, DrawingUpdate};
use vela_core::drawings::toolbar::{DrawingsConfig, DrawingsOption, ToolbarGroupConfig};
use vela_core::drawings::{DrawingPoint, DrawingText, PartialDrawingStyle, SnapMode};
use vela_core::engine::indicator_handle::{IndicatorEvent, IndicatorEventName};
use vela_core::i18n::MessageLike;
use vela_core::marks::{
    MarkAction, MarkContent, MarkGlyph, MarkGroup, MarkPanelItem, TimelineMark,
};
use vela_core::model::{InputValue, Ohlcv};
use vela_core::options::PaneInfo;
use vela_core::ports::chart_renderer::{DrawingMode, MoveTarget, PaneMoveDir};
use vela_core::runtime::{DataError, LocalBoxFuture, Unsubscribe};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

use super::Inner;
use super::json::{
    self, bar_js, console_error, error, from_js, object, opt_num, opt_str, present, prop, strings,
    to_js, type_error,
};
use super::provider::{JsProvider, js_signal, message_of, native_kind, native_provider, settle};
use crate::app::{bars_of, inputs_of};

/// A JS function that runs `unsub` once, however often it is called.
pub(crate) fn unsub_fn(unsub: Unsubscribe) -> JsValue {
    let slot = RefCell::new(Some(unsub));
    Closure::<dyn FnMut()>::new(move || {
        let u = slot.borrow_mut().take();
        if let Some(u) = u {
            u();
        }
    })
    .into_js_value()
}

fn promise_of<F>(fut: F) -> Promise
where
    F: std::future::Future<Output = Result<JsValue, JsValue>> + 'static,
{
    future_to_promise(fut)
}

fn data_err(e: DataError) -> JsValue {
    error(e.to_string())
}

fn callback_of(handler: &JsValue, what: &str) -> Result<Function, JsValue> {
    handler
        .dyn_ref::<Function>()
        .cloned()
        .ok_or_else(|| type_error(format!("[vela] {what}: the handler must be a function")))
}

fn call_handler(f: &Function, payload: &JsValue, what: &str) {
    if let Err(e) = f.call1(&JsValue::NULL, payload) {
        console_error(&format!("[vela] {what} handler error"), &e);
    }
}

/// Make the listed methods of the class `instance` belongs to return their receiver (the
/// original's `this`), once per class. Safe to call again: a patched class is left alone.
pub(crate) fn chain(instance: &JsValue, names: &[&str]) {
    let list: Array = names.iter().map(|n| JsValue::from_str(n)).collect();
    vela_chain(instance, &list);
}

#[wasm_bindgen(inline_js = r#"
export function vela_chain(instance, names) {
    const proto = Object.getPrototypeOf(instance);
    if (!proto || Object.prototype.hasOwnProperty.call(proto, '__velaChained')) return;
    Object.defineProperty(proto, '__velaChained', { value: true, enumerable: false });
    for (const name of names) {
        const original = proto[name];
        if (typeof original !== 'function') continue;
        Object.defineProperty(proto, name, {
            value: function (...args) {
                original.apply(this, args);
                return this;
            },
            writable: true,
            configurable: true,
            enumerable: false,
        });
    }
}
"#)]
extern "C" {
    fn vela_chain(instance: &JsValue, names: &Array);
}

// ── parsing shared with the options ──

fn object_of<'a>(
    value: &'a Value,
    what: &str,
) -> Result<&'a serde_json::Map<String, Value>, JsValue> {
    value
        .as_object()
        .ok_or_else(|| type_error(format!("[vela] {what} must be an object")))
}

fn input_value(v: &JsValue) -> Result<InputValue, JsValue> {
    if let Some(n) = v.as_f64() {
        Ok(InputValue::Number(n))
    } else if let Some(s) = v.as_string() {
        Ok(InputValue::Text(s))
    } else if let Some(b) = v.as_bool() {
        Ok(InputValue::Bool(b))
    } else {
        Err(type_error(
            "[vela] an input value must be a number, a string or a boolean",
        ))
    }
}

/// `options.drawings` / `chart.drawings.setToolbar`: a boolean or `{ toolbar?, tools?, groups? }`.
pub(crate) fn drawings_option_of(v: &JsValue) -> Result<DrawingsOption, JsValue> {
    if let Some(b) = v.as_bool() {
        return Ok(DrawingsOption::Bool(b));
    }
    if !v.is_object() {
        return Err(type_error("[vela] drawings must be a boolean or an object"));
    }
    let toolbar = prop(v, "toolbar").as_bool();
    let tools = match prop(v, "tools") {
        t if present(&t) => Some(json::string_list(&t).ok_or_else(|| {
            type_error("[vela] drawings.tools must be an array of tool type keys")
        })?),
        _ => None,
    };
    let groups = match prop(v, "groups") {
        g if present(&g) => {
            let list = g
                .dyn_ref::<Array>()
                .ok_or_else(|| type_error("[vela] drawings.groups must be an array"))?;
            let mut out = Vec::new();
            for item in list.iter() {
                let id = prop(&item, "id")
                    .as_string()
                    .ok_or_else(|| type_error("[vela] drawings.groups[].id must be a string"))?;
                let label = prop(&item, "label")
                    .as_string()
                    .unwrap_or_else(|| id.clone());
                let tools = json::string_list(&prop(&item, "tools")).ok_or_else(|| {
                    type_error("[vela] drawings.groups[].tools must be an array of type keys")
                })?;
                out.push(ToolbarGroupConfig {
                    id,
                    label: MessageLike::Text(label.into()),
                    tools,
                });
            }
            Some(out)
        }
        _ => None,
    };
    Ok(DrawingsOption::Config(DrawingsConfig {
        toolbar,
        tools,
        groups,
    }))
}

fn add_init(v: &JsValue) -> Result<AddInit, JsValue> {
    let mut init = AddInit::default();
    if !present(v) {
        return Ok(init);
    }
    let j = from_js(v, "drawings.add init")?;
    let o = object_of(&j, "drawings.add init")?;
    init.pane_id = o.get("paneId").and_then(|x| x.as_str()).map(String::from);
    if let Some(a) = o.get("anchors") {
        init.anchors = Some(typed!(Vec<DrawingPoint>, a, "init.anchors")?);
    }
    if let Some(s) = o.get("style") {
        init.style = Some(typed!(PartialDrawingStyle, s, "init.style")?);
    }
    if let Some(t) = o.get("text") {
        init.text = Some(typed!(DrawingText, t, "init.text")?);
    }
    if let Some(p) = o.get("props").and_then(|p| p.as_object()) {
        init.props = Some(p.clone());
    }
    init.z_index = o.get("zIndex").and_then(|x| x.as_f64());
    Ok(init)
}

fn drawing_update(v: &JsValue) -> Result<DrawingUpdate, JsValue> {
    let j = from_js(v, "the drawing patch")?;
    let o = object_of(&j, "the drawing patch")?;
    let mut patch = DrawingUpdate::default();
    if let Some(a) = o.get("anchors") {
        patch.anchors = Some(typed!(Vec<DrawingPoint>, a, "patch.anchors")?);
    }
    if let Some(s) = o.get("style") {
        patch.style = Some(typed!(PartialDrawingStyle, s, "patch.style")?);
    }
    if let Some(t) = o.get("text") {
        patch.text = Some(if t.is_null() {
            None
        } else {
            Some(typed!(DrawingText, t, "patch.text")?)
        });
    }
    patch.pane_id = o.get("paneId").and_then(|x| x.as_str()).map(String::from);
    patch.locked = o.get("locked").and_then(|x| x.as_bool());
    patch.visible = o.get("visible").and_then(|x| x.as_bool());
    patch.z_index = o.get("zIndex").and_then(|x| x.as_f64());
    if let Some(p) = o.get("props").and_then(|p| p.as_object()) {
        patch.props = Some(p.clone());
    }
    if let Some(s) = o.get("showOn") {
        patch.show_on = Some(if s.is_null() {
            None
        } else {
            Some(typed!(Vec<String>, s, "patch.showOn")?)
        });
    }
    Ok(patch)
}

fn id_list(v: &JsValue, what: &str) -> Result<Vec<String>, JsValue> {
    if !present(v) {
        return Ok(Vec::new());
    }
    if let Some(s) = v.as_string() {
        return Ok(vec![s]);
    }
    json::string_list(v).ok_or_else(|| {
        type_error(format!(
            "[vela] {what} must be an id, an array of ids or null"
        ))
    })
}

fn panel_item(item: &JsValue) -> Result<MarkPanelItem, JsValue> {
    let text = |k: &str| prop(item, k).as_string().unwrap_or_default();
    match prop(item, "type").as_string().as_deref() {
        Some("text") => Ok(MarkPanelItem::Text { text: text("text") }),
        Some("field") => Ok(MarkPanelItem::Field {
            label: text("label"),
            value: text("value"),
        }),
        Some("button") => {
            let run = prop(item, "run")
                .dyn_into::<Function>()
                .map_err(|_| type_error("[vela] a panel button needs a run() function"))?;
            Ok(MarkPanelItem::Button {
                label: text("label"),
                primary: prop(item, "primary").as_bool(),
                close: prop(item, "close").as_bool(),
                run: MarkAction::new(move || {
                    if let Err(e) = run.call0(&JsValue::NULL) {
                        console_error("[vela] marks: a panel button's run() threw", &e);
                    }
                }),
            })
        }
        _ => Err(type_error(
            "[vela] a panel item is { type: 'text' | 'field' | 'button', ... }",
        )),
    }
}

fn mark_of(v: &JsValue) -> Result<TimelineMark, JsValue> {
    if !v.is_object() {
        return Err(type_error("[vela] a timeline mark must be an object"));
    }
    let id = prop(v, "id").as_string().unwrap_or_default();
    let time = prop(v, "time")
        .as_f64()
        .filter(|t| t.is_finite())
        .ok_or_else(|| type_error("[vela] a timeline mark's `time` must be epoch-ms"))?;
    let glyph_value = prop(v, "glyph");
    if !glyph_value.is_object() {
        return Err(type_error(
            "[vela] a timeline mark needs a `glyph` ({ color, letter | icon })",
        ));
    }
    let glyph = typed!(
        MarkGlyph,
        &from_js(&glyph_value, "mark.glyph")?,
        "mark.glyph"
    )?;
    let content = match prop(v, "content") {
        c if c.is_function() => {
            return Err(error(
                "[vela] a timeline mark's `content` function (lazy content) is not supported by this build: pass { text }, { html } or { panel }",
            ));
        }
        c if c.is_object() => {
            if let Some(t) = prop(&c, "text").as_string() {
                Some(MarkContent::Text(t))
            } else if let Some(h) = prop(&c, "html").as_string() {
                Some(MarkContent::Html(h))
            } else if prop(&c, "panel").is_object() {
                let panel = prop(&c, "panel");
                let items = prop(&panel, "items");
                let list = items
                    .dyn_ref::<Array>()
                    .ok_or_else(|| type_error("[vela] content.panel.items must be an array"))?;
                Some(MarkContent::Panel(
                    list.iter()
                        .map(|i| panel_item(&i))
                        .collect::<Result<_, _>>()?,
                ))
            } else {
                return Err(type_error(
                    "[vela] mark content is { text } | { html } | { panel: { items } }",
                ));
            }
        }
        _ => None,
    };
    Ok(TimelineMark {
        id,
        time: time as i64,
        title: prop(v, "title").as_string(),
        glyph,
        tooltip: prop(v, "tooltip").as_string(),
        group: prop(v, "group").as_string(),
        content,
    })
}

fn mark_js(mark: &TimelineMark) -> JsValue {
    let o = js_sys::Object::new();
    json::set(&o, "id", JsValue::from_str(&mark.id));
    json::set(&o, "time", JsValue::from_f64(mark.time as f64));
    if let Some(t) = &mark.title {
        json::set(&o, "title", JsValue::from_str(t));
    }
    json::set(&o, "glyph", ser!(&mark.glyph));
    if let Some(t) = &mark.tooltip {
        json::set(&o, "tooltip", JsValue::from_str(t));
    }
    if let Some(g) = &mark.group {
        json::set(&o, "group", JsValue::from_str(g));
    }
    match &mark.content {
        Some(MarkContent::Text(t)) => {
            json::set(&o, "content", object(&[("text", JsValue::from_str(t))]))
        }
        Some(MarkContent::Html(h)) => {
            json::set(&o, "content", object(&[("html", JsValue::from_str(h))]))
        }
        Some(MarkContent::Panel(items)) => {
            let list: Array = items
                .iter()
                .map(|item| match item {
                    MarkPanelItem::Text { text } => object(&[
                        ("type", JsValue::from_str("text")),
                        ("text", JsValue::from_str(text)),
                    ]),
                    MarkPanelItem::Field { label, value } => object(&[
                        ("type", JsValue::from_str("field")),
                        ("label", JsValue::from_str(label)),
                        ("value", JsValue::from_str(value)),
                    ]),
                    MarkPanelItem::Button {
                        label,
                        primary,
                        close,
                        run,
                    } => {
                        let run = run.clone();
                        let f = Closure::<dyn FnMut()>::new(move || run.run()).into_js_value();
                        let b = js_sys::Object::new();
                        json::set(&b, "type", JsValue::from_str("button"));
                        json::set(&b, "label", JsValue::from_str(label));
                        if let Some(p) = primary {
                            json::set(&b, "primary", JsValue::from_bool(*p));
                        }
                        if let Some(c) = close {
                            json::set(&b, "close", JsValue::from_bool(*c));
                        }
                        json::set(&b, "run", f);
                        b.into()
                    }
                })
                .collect();
            json::set(
                &o,
                "content",
                object(&[("panel", object(&[("items", list.into())]))]),
            );
        }
        None => {}
    }
    o.into()
}

fn pane_info_js(p: &PaneInfo) -> JsValue {
    let o = js_sys::Object::new();
    json::set(&o, "id", JsValue::from_str(&p.id));
    json::set(
        &o,
        "kind",
        JsValue::from_str(match p.kind {
            vela_core::model::PaneKind::Price => "price",
            vela_core::model::PaneKind::Study => "study",
        }),
    );
    json::set(&o, "order", JsValue::from_f64(p.order as f64));
    json::set(&o, "collapsed", JsValue::from_bool(p.collapsed));
    json::set(&o, "maximized", JsValue::from_bool(p.maximized));
    if let Some(w) = p.height_weight {
        json::set(&o, "heightWeight", JsValue::from_f64(w));
    }
    let indicators: Array = p
        .indicators
        .iter()
        .map(|i| {
            let e = js_sys::Object::new();
            json::set(&e, "id", JsValue::from_str(&i.id));
            json::set(&e, "title", JsValue::from_str(&i.title));
            if let Some(s) = &i.shorttitle {
                json::set(&e, "shorttitle", JsValue::from_str(s));
            }
            json::set(&e, "ownScale", JsValue::from_bool(i.own_scale));
            JsValue::from(e)
        })
        .collect();
    json::set(&o, "indicators", indicators.into());
    o.into()
}

fn move_target_of(v: &JsValue) -> Result<MoveTarget, JsValue> {
    typed!(
        MoveTarget,
        &from_js(v, "the move target")?,
        "the move target (\"price\", { pane }, { newPane })"
    )
}

fn pane_kind_str(k: Option<vela_core::model::PaneKind>) -> JsValue {
    match k {
        Some(vela_core::model::PaneKind::Price) => JsValue::from_str("price"),
        Some(vela_core::model::PaneKind::Study) => JsValue::from_str("study"),
        None => JsValue::NULL,
    }
}

// ── chart.data ──

/// `chart.data`: register market-data providers and query the registry.
#[wasm_bindgen(js_name = DataControl)]
pub struct DataControlApi {
    pub(crate) inner: Rc<Inner>,
}

#[wasm_bindgen(js_class = DataControl)]
impl DataControlApi {
    /// Register (or replace) a provider under `name`: a plain object with `getBars(ticker,
    /// timeframe, range)` (and any of the optional `DataProvider` methods), or one of the
    /// bundled `BinanceProvider` / `CoinbaseProvider` / `HyperliquidProvider`.
    #[wasm_bindgen(js_name = registerProvider)]
    pub fn register_provider(&self, name: &JsValue, provider: &JsValue) -> Result<(), JsValue> {
        let name = name
            .as_string()
            .ok_or_else(|| type_error("[vela] registerProvider: `name` must be a string"))?;
        if self.inner.dead() {
            return Ok(());
        }
        let rc: Rc<dyn vela_core::ports::data_provider::DataProvider> =
            if let Some(kind) = native_kind(provider) {
                native_provider(&kind, &self.inner.chart.env().platform).ok_or_else(|| {
                    type_error(format!(
                        "[vela] registerProvider(\"{name}\"): unknown provider kind"
                    ))
                })?
            } else {
                JsProvider::new(provider.clone())?
            };
        let key = name.to_lowercase();
        {
            let mut known = self.inner.js_providers.borrow_mut();
            known.retain(|(k, _)| *k != key);
            if native_kind(provider).is_none() {
                known.push((key, provider.clone()));
            }
        }
        self.inner.chart.data().register_provider(&name, rc);
        Ok(())
    }

    #[wasm_bindgen(js_name = unregisterProvider)]
    pub fn unregister_provider(&self, name: &str) {
        if self.inner.dead() {
            return;
        }
        self.inner
            .js_providers
            .borrow_mut()
            .retain(|(k, _)| *k != name.to_lowercase());
        self.inner.chart.data().unregister_provider(name);
    }

    /// Metadata for every registered provider.
    pub fn providers(&self) -> JsValue {
        if self.inner.dead() {
            return Array::new().into();
        }
        ser!(&self.inner.chart.data().providers())
    }

    /// How a symbol routes now: `{ provider, ticker }`, or `null`.
    pub fn resolve(&self, symbol: &str) -> JsValue {
        if self.inner.dead() {
            return JsValue::NULL;
        }
        match self.inner.chart.data().resolve(symbol) {
            Some(r) => object(&[
                ("provider", JsValue::from_str(&r.provider)),
                ("ticker", JsValue::from_str(&r.ticker)),
            ]),
            None => JsValue::NULL,
        }
    }

    #[wasm_bindgen(js_name = displayPrefix)]
    pub fn display_prefix(&self, symbol: &str) -> JsValue {
        if self.inner.dead() {
            return JsValue::NULL;
        }
        opt_str(self.inner.chart.data().display_prefix(symbol).as_deref())
    }

    #[wasm_bindgen(js_name = canonicalSymbol)]
    pub fn canonical_symbol(&self, symbol: &str) -> JsValue {
        if self.inner.dead() {
            return JsValue::NULL;
        }
        opt_str(self.inner.chart.data().canonical_symbol(symbol).as_deref())
    }

    /// The provider object registered under `name` when it was a JavaScript object;
    /// `undefined` for a bundled provider, an unknown name, or a custom feed.
    #[wasm_bindgen(js_name = providerInstance)]
    pub fn provider_instance(&self, name: &str) -> JsValue {
        let key = name.to_lowercase();
        self.inner
            .js_providers
            .borrow()
            .iter()
            .find(|(k, _)| *k == key)
            .map_or(JsValue::UNDEFINED, |(_, v)| v.clone())
    }

    /// Indexed symbols for one provider (or all).
    pub fn symbols(&self, provider: Option<String>) -> JsValue {
        if self.inner.dead() {
            return Array::new().into();
        }
        ser!(&self.inner.chart.data().symbols(provider.as_deref()))
    }

    #[wasm_bindgen(js_name = symbolIcon)]
    pub fn symbol_icon(&self, symbol: &str) -> Option<String> {
        if self.inner.dead() {
            return None;
        }
        self.inner.chart.data().symbol_icon(symbol)
    }

    /// `Promise` of the symbol's facts, or `undefined`.
    #[wasm_bindgen(js_name = symbolInfo)]
    pub fn symbol_info(&self, symbol: &str) -> Promise {
        if self.inner.dead() {
            return Promise::resolve(&JsValue::UNDEFINED);
        }
        let fut = self.inner.chart.data().symbol_info(symbol);
        promise_of(async move {
            Ok(match fut.await {
                Ok(Some(info)) => ser!(&info),
                _ => JsValue::UNDEFINED,
            })
        })
    }

    /// The resolved capabilities for `symbol`, or `null`.
    pub fn capabilities(&self, symbol: &str) -> JsValue {
        if self.inner.dead() {
            return JsValue::NULL;
        }
        self.inner
            .chart
            .data()
            .capabilities(symbol)
            .map_or(JsValue::NULL, |c| ser!(&c))
    }

    /// Resolves when every registered provider's symbol index has settled.
    pub fn ready(&self) -> Promise {
        if self.inner.dead() {
            return Promise::resolve(&JsValue::UNDEFINED);
        }
        let fut = self.inner.chart.data().ready();
        promise_of(async move {
            fut.await;
            Ok(JsValue::UNDEFINED)
        })
    }
}

// ── chart.panes ──

/// `chart.panes`: list the panes and move, merge, collapse or maximize them.
#[wasm_bindgen(js_name = PanesControl)]
pub struct PanesControlApi {
    pub(crate) inner: Rc<Inner>,
}

#[wasm_bindgen(js_class = PanesControl)]
impl PanesControlApi {
    /// Whether the renderer supports pane management.
    #[wasm_bindgen(getter)]
    pub fn supported(&self) -> bool {
        !self.inner.dead() && self.inner.chart.panes().supported()
    }

    pub fn list(&self) -> JsValue {
        if self.inner.dead() {
            return Array::new().into();
        }
        self.inner
            .chart
            .panes()
            .list()
            .iter()
            .map(pane_info_js)
            .collect::<Array>()
            .into()
    }

    /// Move a study pane one slot `'up'` or `'down'`.
    #[wasm_bindgen(js_name = move)]
    pub fn move_pane(&self, pane_id: &str, dir: &str) -> Result<(), JsValue> {
        let dir = match dir {
            "up" => PaneMoveDir::Up,
            "down" => PaneMoveDir::Down,
            other => {
                return Err(type_error(format!(
                    "[vela] panes.move: dir must be \"up\" or \"down\" (got \"{other}\")"
                )));
            }
        };
        if !self.inner.dead() {
            self.inner.chart.panes().move_pane(pane_id, dir);
        }
        Ok(())
    }

    pub fn remove(&self, pane_id: &str) {
        if !self.inner.dead() {
            self.inner.chart.panes().remove(pane_id);
        }
    }

    /// Collapse a pane to a strip, or restore it (`collapsed = false`).
    pub fn collapse(&self, pane_id: &str, collapsed: Option<bool>) {
        if !self.inner.dead() {
            self.inner
                .chart
                .panes()
                .collapse(pane_id, collapsed.unwrap_or(true));
        }
    }

    /// Let one pane fill the plot, or restore the split with `null`.
    pub fn maximize(&self, pane_id: Option<String>) {
        if !self.inner.dead() {
            self.inner.chart.panes().maximize(pane_id.as_deref());
        }
    }

    #[wasm_bindgen(js_name = setHeightWeight)]
    pub fn set_height_weight(&self, pane_id: &str, weight: f64) {
        if !self.inner.dead() {
            self.inner.chart.panes().set_height_weight(pane_id, weight);
        }
    }

    /// Move or merge an indicator into a pane: `'price'`, `{ pane }` or `{ newPane }`.
    #[wasm_bindgen(js_name = moveIndicator)]
    pub fn move_indicator(&self, id: &str, target: &JsValue) -> Result<(), JsValue> {
        let target = move_target_of(target)?;
        if !self.inner.dead() {
            self.inner.chart.panes().move_indicator(id, target);
        }
        Ok(())
    }
}

// ── chart.replay ──

fn replay_ticks(value: &JsValue) -> Result<Vec<ReplayTick>, DataError> {
    let list = value
        .dyn_ref::<Array>()
        .ok_or_else(|| DataError::other("a tick source must answer with an array of ticks"))?;
    list.iter()
        .map(|t| {
            let num = |k: &str| prop(&t, k).as_f64().filter(|n| n.is_finite());
            let price =
                num("price").ok_or_else(|| DataError::other("a tick needs a finite `price`"))?;
            Ok(ReplayTick {
                price,
                high: num("high"),
                low: num("low"),
                open: num("open"),
                volume: num("volume"),
            })
        })
        .collect()
}

/// `chart.replay`: rewind to a past bar and reveal the following ones.
#[wasm_bindgen(js_name = ReplayControl)]
pub struct ReplayControlApi {
    pub(crate) inner: Rc<Inner>,
}

#[wasm_bindgen(js_class = ReplayControl)]
impl ReplayControlApi {
    /// Enter replay (or seek) at `{ from }` (epoch ms). Resolves once the rewound history is
    /// on screen. Starts paused.
    pub fn start(&self, opts: &JsValue) -> Result<Promise, JsValue> {
        let from = prop(opts, "from")
            .as_f64()
            .filter(|f| f.is_finite())
            .ok_or_else(|| type_error("[vela] replay.start({ from }): `from` must be epoch-ms"))?;
        if self.inner.dead() {
            return Ok(Promise::resolve(&JsValue::UNDEFINED));
        }
        let fut = self
            .inner
            .chart
            .replay()
            .start(ReplayStartOptions { from: from as i64 });
        Ok(promise_of(async move {
            fut.await.map(|()| JsValue::UNDEFINED).map_err(data_err)
        }))
    }

    pub fn step(&self) -> bool {
        !self.inner.dead() && self.inner.chart.replay().step()
    }

    #[wasm_bindgen(js_name = stepUpdate)]
    pub fn step_update(&self) -> bool {
        !self.inner.dead() && self.inner.chart.replay().step_update()
    }

    /// Reveal one bar (one tick, with `setTicks`) every `intervalMs`.
    pub fn play(&self, interval_ms: Option<f64>) {
        if !self.inner.dead() {
            self.inner.chart.replay().play(interval_ms);
        }
    }

    pub fn pause(&self) {
        if !self.inner.dead() {
            self.inner.chart.replay().pause();
        }
    }

    pub fn stop(&self) {
        if !self.inner.dead() {
            self.inner.chart.replay().stop();
        }
    }

    /// `{ active, playing, cursorTime, remaining, nextTime, intervalMs }`.
    #[wasm_bindgen(getter)]
    pub fn state(&self) -> JsValue {
        let s = if self.inner.dead() {
            vela_core::controls::replay_control::ReplayState::off()
        } else {
            self.inner.chart.replay().state()
        };
        object(&[
            ("active", JsValue::from_bool(s.active)),
            ("playing", JsValue::from_bool(s.playing)),
            ("cursorTime", opt_num(s.cursor_time.map(|t| t as f64))),
            ("remaining", JsValue::from_f64(s.remaining as f64)),
            ("nextTime", opt_num(s.next_time.map(|t| t as f64))),
            ("intervalMs", JsValue::from_f64(s.interval_ms)),
        ])
    }

    /// `{ first, last }`, the open times of the oldest and newest bar a replay can start from,
    /// or `null` before any bar loaded.
    #[wasm_bindgen(getter)]
    pub fn bounds(&self) -> JsValue {
        if self.inner.dead() {
            return JsValue::NULL;
        }
        match self.inner.chart.replay().bounds() {
            Some(b) => object(&[
                ("first", JsValue::from_f64(b.first as f64)),
                ("last", JsValue::from_f64(b.last as f64)),
            ]),
            None => JsValue::NULL,
        }
    }

    /// Play each revealed bar as intrabar updates: `source(bar, { end, signal })` returns
    /// `{ price, high?, low?, open?, volume? }[]` (or a promise of it); `null` goes back to
    /// whole bars.
    #[wasm_bindgen(js_name = setTicks)]
    pub fn set_ticks(&self, source: &JsValue) -> Result<(), JsValue> {
        let source = if !present(source) {
            None
        } else {
            let f = source.dyn_ref::<Function>().cloned().ok_or_else(|| {
                type_error("[vela] replay.setTicks: the source must be a function or null")
            })?;
            Some(Rc::new(move |bar: &Ohlcv, req: ReplayTickRequest| -> LocalBoxFuture<'static, Result<Vec<ReplayTick>, DataError>> {
                let opts = object(&[
                    ("end", JsValue::from_f64(req.end as f64)),
                    ("signal", js_signal(Some(&req.signal))),
                ]);
                let result = f.call2(&JsValue::NULL, &bar_js(bar), &opts);
                Box::pin(async move {
                    let v = result.map_err(|e| DataError::other(message_of(&e)))?;
                    let v = settle(v).await.map_err(|e| DataError::other(message_of(&e)))?;
                    replay_ticks(&v)
                })
            }) as ReplayTickSource)
        };
        if !self.inner.dead() {
            self.inner.chart.replay().set_ticks(source);
        }
        Ok(())
    }
}

// ── chart.renderer ──

/// The chart as one PNG data URL: the renderer's pile (backdrop, data, volume, chrome, drawings)
/// composited on an offscreen canvas of its backing size. The core's native renderer has no
/// export of its own (`supports_screenshot` is false), so this is the web layer's, as the
/// workspace's single-cell shot is.
fn screenshot_png(inner: &Inner) -> Option<String> {
    use vela_core::workspace::screenshot::{
        LayoutShotFrame, LayoutShotTile, ShotRect, composite_layout_screenshot_with,
    };
    let pile = inner.renderer.screenshot_pile()?;
    let dpr = prop(&js_sys::global().into(), "devicePixelRatio")
        .as_f64()
        .filter(|d| d.is_finite() && *d > 0.0)
        .unwrap_or(1.0);
    let (width, height) = (pile.width / dpr, pile.height / dpr);
    let frame = LayoutShotFrame {
        width,
        height,
        dpr,
        gap_color: "#000000".into(),
    };
    let tile = LayoutShotTile {
        background: Some(pile.background),
        sources: pile.images,
        rect: ShotRect {
            x: 0.0,
            y: 0.0,
            width,
            height,
        },
    };
    let (w, h) = frame.backing_size();
    let document = crate::dom::global_document()?;
    let canvas: web_sys::HtmlCanvasElement =
        document.create_element("canvas").ok()?.unchecked_into();
    canvas.set_width(w.max(1.0) as u32);
    canvas.set_height(h.max(1.0) as u32);
    let ctx: web_sys::CanvasRenderingContext2d =
        canvas.get_context("2d").ok().flatten()?.unchecked_into();
    let surface = crate::canvas::BrowserCanvas::new(ctx.clone());
    // The DOM chrome over the plot (legend, attribution mark) is rasterized into the shot,
    // as the original's `screenshotCanvas` does.
    let host = inner.host.clone();
    composite_layout_screenshot_with(&surface, &frame, &[tile], &|_, phase, dest| {
        crate::raster::paint_overlays(
            &ctx,
            host.container(),
            host.plot(),
            phase,
            (dest.dx, dest.dy, dest.dw),
        );
    })
    .then(|| crate::workspace_ui::js::vela_ws_to_png(canvas.as_ref()))
    .flatten()
}

/// `chart.renderer`: read and set render features at runtime, with no indicator re-run.
#[wasm_bindgen(js_name = RendererControl)]
pub struct RendererControlApi {
    pub(crate) inner: Rc<Inner>,
}

fn feature_value(v: &JsValue, key: &str) -> Result<Value, JsValue> {
    if v.is_undefined() {
        return Ok(Value::Null);
    }
    if v.is_function() {
        return Err(error(format!(
            "[vela] renderer.set(\"{key}\"): a function value is not supported by this build"
        )));
    }
    from_js(v, &format!("renderer.set(\"{key}\") value"))
}

#[wasm_bindgen(js_class = RendererControl)]
impl RendererControlApi {
    /// The active renderer's identity (`'native'`).
    #[wasm_bindgen(getter)]
    pub fn name(&self) -> String {
        self.inner.chart.renderer().name().to_string()
    }

    /// What the renderer can draw.
    #[wasm_bindgen(getter)]
    pub fn capabilities(&self) -> JsValue {
        ser!(&self.inner.chart.renderer().capabilities())
    }

    pub fn supports(&self, feature: &str) -> bool {
        self.inner.chart.renderer().supports(feature)
    }

    /// A feature's current value; `undefined` if unsupported.
    pub fn get(&self, feature: &str) -> JsValue {
        self.inner
            .chart
            .renderer()
            .get(feature)
            .map_or(JsValue::UNDEFINED, |v| to_js(&v))
    }

    /// `set('glow', 0.6)` or `set({ logScale: true, upColor: '#fff' })`. A feature the
    /// renderer does not support warns and is ignored.
    pub fn set(&self, feature: &JsValue, value: &JsValue) -> Result<(), JsValue> {
        let mut entries: Vec<(String, Value)> = Vec::new();
        if let Some(key) = feature.as_string() {
            entries.push((key.clone(), feature_value(value, &key)?));
        } else if feature.is_object() {
            for e in js_sys::Object::entries(feature.unchecked_ref()).iter() {
                let pair: Array = e.unchecked_into();
                let key = pair.get(0).as_string().unwrap_or_default();
                let v = feature_value(&pair.get(1), &key)?;
                entries.push((key, v));
            }
        } else {
            return Err(type_error(
                "[vela] renderer.set: pass a feature name and a value, or an object of features",
            ));
        }
        if !self.inner.dead() {
            self.inner.chart.renderer().set_many(entries);
        }
        Ok(())
    }

    /// The chart as a PNG data URL, or `null`.
    pub fn screenshot(&self) -> JsValue {
        if self.inner.dead() {
            return JsValue::NULL;
        }
        opt_str(screenshot_png(&self.inner).as_deref())
    }

    /// The renderer's cosmetics as a versioned JSON document, or `null`.
    #[wasm_bindgen(js_name = getConfig)]
    pub fn get_config(&self) -> JsValue {
        if self.inner.dead() {
            return JsValue::NULL;
        }
        self.inner
            .chart
            .renderer()
            .get_config()
            .map_or(JsValue::NULL, |v| to_js(&v))
    }

    /// Apply a full or partial document from `getConfig()`.
    #[wasm_bindgen(js_name = applyConfig)]
    pub fn apply_config(&self, config: &JsValue) -> Result<(), JsValue> {
        let doc = from_js(config, "renderer.applyConfig(config)")?;
        if !self.inner.dead() {
            self.inner.chart.renderer().apply_config(&doc);
        }
        Ok(())
    }

    /// Subscribe to cosmetic-config changes; returns the unsubscribe function.
    #[wasm_bindgen(js_name = onConfigChanged)]
    pub fn on_config_changed(&self, cb: &JsValue) -> Result<JsValue, JsValue> {
        let f = callback_of(cb, "renderer.onConfigChanged")?;
        if self.inner.dead() {
            return Ok(unsub_fn(Box::new(|| {})));
        }
        let unsub = self
            .inner
            .chart
            .renderer()
            .on_config_changed(Box::new(move || {
                call_handler(&f, &JsValue::UNDEFINED, "renderer.onConfigChanged");
            }));
        Ok(unsub_fn(unsub))
    }

    /// Subscribe to crosshair movement; returns the unsubscribe function.
    #[wasm_bindgen(js_name = onCrosshairMove)]
    pub fn on_crosshair_move(&self, cb: &JsValue) -> Result<JsValue, JsValue> {
        let f = callback_of(cb, "renderer.onCrosshairMove")?;
        if self.inner.dead() {
            return Ok(unsub_fn(Box::new(|| {})));
        }
        let unsub = self
            .inner
            .chart
            .renderer()
            .on_crosshair_move(Box::new(move |e| {
                let values = js_sys::Map::new();
                for (id, v) in &e.values {
                    values.set(&JsValue::from_str(id), &JsValue::from_f64(*v));
                }
                let ohlc = match &e.ohlc {
                    Some(o) => ser!(o),
                    None => JsValue::NULL,
                };
                let payload = object(&[
                    ("time", opt_num(e.time.map(|t| t as f64))),
                    ("price", opt_num(e.price)),
                    ("paneKind", pane_kind_str(e.pane_kind)),
                    ("paneId", opt_str(e.pane_id.as_deref())),
                    ("values", values.into()),
                    ("ohlc", ohlc),
                ]);
                call_handler(&f, &payload, "renderer.onCrosshairMove");
            }));
        Ok(unsub_fn(unsub))
    }

    /// Subscribe to clicks on the plot: `{ time, price, paneKind, paneId }`.
    #[wasm_bindgen(js_name = onClick)]
    pub fn on_click(&self, cb: &JsValue) -> Result<JsValue, JsValue> {
        let f = callback_of(cb, "renderer.onClick")?;
        if self.inner.dead() {
            return Ok(unsub_fn(Box::new(|| {})));
        }
        let unsub = self.inner.chart.renderer().on_click(Box::new(move |e| {
            let payload = object(&[
                ("time", opt_num(e.time.map(|t| t as f64))),
                ("price", opt_num(e.price)),
                ("paneKind", pane_kind_str(e.pane_kind)),
                ("paneId", opt_str(e.pane_id.as_deref())),
            ]);
            call_handler(&f, &payload, "renderer.onClick");
        }));
        Ok(unsub_fn(unsub))
    }

    /// Subscribe to touch long-presses on an axis strip.
    #[wasm_bindgen(js_name = onAxisLongPress)]
    pub fn on_axis_long_press(&self, cb: &JsValue) -> Result<JsValue, JsValue> {
        let f = callback_of(cb, "renderer.onAxisLongPress")?;
        if self.inner.dead() {
            return Ok(unsub_fn(Box::new(|| {})));
        }
        let unsub = self
            .inner
            .chart
            .renderer()
            .on_axis_long_press(Box::new(move |e| {
                call_handler(&f, &ser!(e), "renderer.onAxisLongPress");
            }));
        Ok(unsub_fn(unsub))
    }

    /// The bar under the crosshair as a display-ready snapshot, or `null`.
    #[wasm_bindgen(js_name = dataWindowReadout)]
    pub fn data_window_readout(&self) -> JsValue {
        if self.inner.dead() {
            return JsValue::NULL;
        }
        self.inner
            .chart
            .renderer()
            .data_window_readout()
            .map_or(JsValue::NULL, |r| ser!(&r))
    }

    #[wasm_bindgen(getter, js_name = supportsExternalCrosshair)]
    pub fn supports_external_crosshair(&self) -> bool {
        self.inner.chart.renderer().supports_external_crosshair()
    }

    /// Show (or clear, with `null`) a ghost crosshair driven from outside this chart.
    #[wasm_bindgen(js_name = setExternalCrosshair)]
    pub fn set_external_crosshair(&self, time: Option<f64>, price: Option<f64>) {
        if !self.inner.dead() {
            self.inner
                .chart
                .renderer()
                .set_external_crosshair(time.map(|t| t as i64), price);
        }
    }

    /// Move keyboard focus onto the chart's interactive surface.
    pub fn focus(&self) {
        if !self.inner.dead() {
            self.inner.chart.renderer().focus();
        }
    }

    #[wasm_bindgen(js_name = closeDialogs)]
    pub fn close_dialogs(&self) {
        if !self.inner.dead() {
            self.inner.chart.renderer().close_dialogs();
        }
    }

    /// Open (or toggle) the chart settings dialog, on the tab titled `section`.
    #[wasm_bindgen(js_name = openSettings)]
    pub fn open_settings(&self, section: Option<String>) {
        if !self.inner.dead() {
            self.inner
                .chart
                .renderer()
                .open_settings(section.as_deref());
        }
    }

    #[wasm_bindgen(getter, js_name = supportsIndicatorSettings)]
    pub fn supports_indicator_settings(&self) -> bool {
        self.inner.chart.renderer().supports_indicator_settings()
    }

    #[wasm_bindgen(js_name = openIndicatorSettings)]
    pub fn open_indicator_settings(&self, indicator_id: &str) {
        if !self.inner.dead() {
            self.inner
                .chart
                .renderer()
                .open_indicator_settings(indicator_id);
        }
    }

    /// Hide settings by id (`{ hidden: [...] }`): presentation only.
    #[wasm_bindgen(js_name = setSettingsVisibility)]
    pub fn set_settings_visibility(&self, policy: &JsValue) -> Result<(), JsValue> {
        let hidden = match prop(policy, "hidden") {
            h if present(&h) => Some(json::string_list(&h).ok_or_else(|| {
                type_error("[vela] setSettingsVisibility: `hidden` must be an array of setting ids")
            })?),
            _ => None,
        };
        if !self.inner.dead() {
            self.inner
                .chart
                .renderer()
                .set_settings_visibility(&vela_core::options::SettingsVisibilityPolicy { hidden });
        }
        Ok(())
    }

    /// Every addressable setting id of this chart.
    #[wasm_bindgen(js_name = listSettingsIds)]
    pub fn list_settings_ids(&self) -> JsValue {
        if self.inner.dead() {
            return Array::new().into();
        }
        strings(&self.inner.chart.renderer().list_settings_ids())
    }

    /// `'mobile'` or `'desktop'`: the size class of the chrome around the chart.
    #[wasm_bindgen(js_name = setLayoutMode)]
    pub fn set_layout_mode(&self, mode: &str) -> Result<(), JsValue> {
        use vela_core::ports::chart_renderer::LayoutMode;
        let mode = match mode {
            "mobile" => LayoutMode::Mobile,
            "desktop" => LayoutMode::Desktop,
            other => {
                return Err(type_error(format!(
                    "[vela] setLayoutMode: \"mobile\" or \"desktop\" (got \"{other}\")"
                )));
            }
        };
        if !self.inner.dead() {
            self.inner.chart.renderer().set_layout_mode(mode);
        }
        Ok(())
    }
}

// ── chart.drawings ──

/// `chart.drawings`: tools, programmatic drawings and persistence.
#[wasm_bindgen(js_name = DrawingsControl)]
pub struct DrawingsControlApi {
    pub(crate) inner: Rc<Inner>,
}

fn snap_of(mode: &str) -> Result<SnapMode, JsValue> {
    match mode {
        "off" => Ok(SnapMode::Off),
        "weak" => Ok(SnapMode::Weak),
        "strong" => Ok(SnapMode::Strong),
        other => Err(type_error(format!(
            "[vela] drawings.setSnapMode: \"off\", \"weak\" or \"strong\" (got \"{other}\")"
        ))),
    }
}

fn template_js(t: &vela_core::drawings::DrawingToolTemplate) -> JsValue {
    let o = js_sys::Object::new();
    json::set(&o, "name", JsValue::from_str(&t.name));
    if let Some(MessageLike::Text(label)) = &t.label {
        json::set(&o, "label", JsValue::from_str(label));
    }
    json::set(&o, "settings", ser!(&t.settings));
    o.into()
}

#[wasm_bindgen(js_class = DrawingsControl)]
impl DrawingsControlApi {
    /// Whether the renderer takes interactive drawings.
    #[wasm_bindgen(getter)]
    pub fn supported(&self) -> bool {
        !self.inner.dead() && self.inner.chart.drawings().supported()
    }

    /// Arm a tool for the next clicks; `null` returns to select/idle.
    #[wasm_bindgen(js_name = setTool)]
    pub fn set_tool(&self, type_key: Option<String>) {
        if !self.inner.dead() {
            self.inner.chart.drawings().set_tool(type_key.as_deref());
        }
    }

    #[wasm_bindgen(js_name = getTool)]
    pub fn get_tool(&self) -> JsValue {
        if self.inner.dead() {
            return JsValue::NULL;
        }
        opt_str(self.inner.chart.drawings().get_tool().as_deref())
    }

    #[wasm_bindgen(js_name = setSnapMode)]
    pub fn set_snap_mode(&self, mode: &str) -> Result<(), JsValue> {
        let mode = snap_of(mode)?;
        if !self.inner.dead() {
            self.inner.chart.drawings().set_snap_mode(mode);
        }
        Ok(())
    }

    #[wasm_bindgen(js_name = getSnapMode)]
    pub fn get_snap_mode(&self) -> String {
        if self.inner.dead() {
            return "off".into();
        }
        json::snap_str(self.inner.chart.drawings().get_snap_mode()).into()
    }

    #[wasm_bindgen(js_name = setStayMode)]
    pub fn set_stay_mode(&self, on: bool) {
        if !self.inner.dead() {
            self.inner.chart.drawings().set_stay_mode(on);
        }
    }

    #[wasm_bindgen(js_name = getStayMode)]
    pub fn get_stay_mode(&self) -> bool {
        !self.inner.dead() && self.inner.chart.drawings().get_stay_mode()
    }

    /// `'measure'`, `'eraser'` or `null`.
    #[wasm_bindgen(js_name = setMode)]
    pub fn set_mode(&self, mode: Option<String>) -> Result<(), JsValue> {
        let mode = match mode.as_deref() {
            None => None,
            Some("measure") => Some(DrawingMode::Measure),
            Some("eraser") => Some(DrawingMode::Eraser),
            Some(other) => {
                return Err(type_error(format!(
                    "[vela] drawings.setMode: \"measure\", \"eraser\" or null (got \"{other}\")"
                )));
            }
        };
        if !self.inner.dead() {
            self.inner.chart.drawings().set_mode(mode);
        }
        Ok(())
    }

    #[wasm_bindgen(js_name = getMode)]
    pub fn get_mode(&self) -> JsValue {
        if self.inner.dead() {
            return JsValue::NULL;
        }
        self.inner
            .chart
            .drawings()
            .get_mode()
            .map_or(JsValue::NULL, |m| ser!(&m))
    }

    #[wasm_bindgen(js_name = showToolbar)]
    pub fn show_toolbar(&self, visible: Option<bool>) {
        if !self.inner.dead() {
            self.inner
                .chart
                .drawings()
                .show_toolbar(visible.unwrap_or(true));
        }
    }

    /// Reconfigure the toolbar: a boolean, or `{ toolbar?, tools?, groups? }`.
    #[wasm_bindgen(js_name = setToolbar)]
    pub fn set_toolbar(&self, option: &JsValue) -> Result<(), JsValue> {
        let option = drawings_option_of(option)?;
        if !self.inner.dead() {
            self.inner.chart.drawings().set_toolbar(option);
        }
        Ok(())
    }

    /// Shortcut hints for the toolbar flyouts: `{ trendline: 'Alt+T', ... }`.
    #[wasm_bindgen(js_name = setToolShortcuts)]
    pub fn set_tool_shortcuts(&self, map: &JsValue) -> Result<(), JsValue> {
        let parsed = typed!(
            BTreeMap<String, String>,
            &from_js(map, "setToolShortcuts map")?,
            "setToolShortcuts map"
        )?;
        if !self.inner.dead() {
            self.inner.chart.drawings().set_tool_shortcuts(&parsed);
        }
        Ok(())
    }

    /// Show another chart's in-progress placement as a ghost (`null` clears it).
    #[wasm_bindgen(js_name = setExternalGhost)]
    pub fn set_external_ghost(&self, doc: &JsValue) -> Result<(), JsValue> {
        let parsed = if present(doc) {
            Some(typed!(
                vela_core::drawings::SerializedDrawing,
                &from_js(doc, "setExternalGhost doc")?,
                "setExternalGhost doc"
            )?)
        } else {
            None
        };
        if !self.inner.dead() {
            self.inner
                .chart
                .drawings()
                .set_external_ghost(parsed.as_ref());
        }
        Ok(())
    }

    /// Create a drawing from code; returns its record (`null` where unsupported).
    pub fn add(&self, type_key: &str, init: &JsValue) -> Result<JsValue, JsValue> {
        let init = add_init(init)?;
        if self.inner.dead() {
            return Ok(JsValue::NULL);
        }
        Ok(self
            .inner
            .chart
            .drawings()
            .add(type_key, init)
            .map_or(JsValue::NULL, |d| ser!(&d)))
    }

    pub fn remove(&self, id: &str) {
        if !self.inner.dead() {
            self.inner.chart.drawings().remove(id);
        }
    }

    /// Apply a partial record to a drawing.
    pub fn update(&self, id: &str, patch: &JsValue) -> Result<(), JsValue> {
        let patch = drawing_update(patch)?;
        if !self.inner.dead() {
            self.inner.chart.drawings().update(id, patch);
        }
        Ok(())
    }

    /// Apply several partial records as one undo step: `[{ id, patch }]`.
    #[wasm_bindgen(js_name = updateMany)]
    pub fn update_many(&self, patches: &JsValue) -> Result<(), JsValue> {
        let list = patches.dyn_ref::<Array>().ok_or_else(|| {
            type_error("[vela] drawings.updateMany takes an array of { id, patch }")
        })?;
        let mut out = Vec::new();
        for item in list.iter() {
            let id = prop(&item, "id").as_string().ok_or_else(|| {
                type_error("[vela] drawings.updateMany: each entry needs an `id`")
            })?;
            out.push((id, drawing_update(&prop(&item, "patch"))?));
        }
        if !self.inner.dead() {
            self.inner.chart.drawings().update_many(out);
        }
        Ok(())
    }

    #[wasm_bindgen(js_name = removeMany)]
    pub fn remove_many(&self, ids: Vec<String>) {
        if !self.inner.dead() {
            self.inner.chart.drawings().remove_many(&ids);
        }
    }

    pub fn lock(&self, id: &str, locked: Option<bool>) {
        if !self.inner.dead() {
            self.inner.chart.drawings().lock(id, locked.unwrap_or(true));
        }
    }

    pub fn show(&self, id: &str, visible: Option<bool>) {
        if !self.inner.dead() {
            self.inner
                .chart
                .drawings()
                .show(id, visible.unwrap_or(true));
        }
    }

    /// Select drawings on the chart: an id, an array of ids, or `null` to clear.
    pub fn select(&self, ids: &JsValue) -> Result<(), JsValue> {
        let ids = id_list(ids, "drawings.select(ids)")?;
        if !self.inner.dead() {
            self.inner.chart.drawings().select(&ids);
        }
        Ok(())
    }

    #[wasm_bindgen(js_name = openSettings)]
    pub fn open_settings(&self, id: &str) {
        if !self.inner.dead() {
            self.inner.chart.drawings().open_settings(id);
        }
    }

    #[wasm_bindgen(js_name = bringToFront)]
    pub fn bring_to_front(&self, id: &str) {
        if !self.inner.dead() {
            self.inner.chart.drawings().bring_to_front(id);
        }
    }

    #[wasm_bindgen(js_name = sendToBack)]
    pub fn send_to_back(&self, id: &str) {
        if !self.inner.dead() {
            self.inner.chart.drawings().send_to_back(id);
        }
    }

    pub fn undo(&self) {
        if !self.inner.dead() {
            self.inner.chart.drawings().undo();
        }
    }

    pub fn redo(&self) {
        if !self.inner.dead() {
            self.inner.chart.drawings().redo();
        }
    }

    #[wasm_bindgen(js_name = canUndo)]
    pub fn can_undo(&self) -> bool {
        !self.inner.dead() && self.inner.chart.drawings().can_undo()
    }

    #[wasm_bindgen(js_name = canRedo)]
    pub fn can_redo(&self) -> bool {
        !self.inner.dead() && self.inner.chart.drawings().can_redo()
    }

    /// Duplicate a drawing in place; the clone is selected.
    pub fn clone(&self, id: &str) {
        if !self.inner.dead() {
            self.inner.chart.drawings().clone_drawing(id);
        }
    }

    pub fn duplicate(&self, ids: Vec<String>) {
        if !self.inner.dead() {
            self.inner.chart.drawings().duplicate(&ids);
        }
    }

    #[wasm_bindgen(js_name = copyToClipboard)]
    pub fn copy_to_clipboard(&self, ids: Vec<String>) {
        if !self.inner.dead() {
            self.inner.chart.drawings().copy_to_clipboard(&ids);
        }
    }

    pub fn paste(&self) {
        if !self.inner.dead() {
            self.inner.chart.drawings().paste();
        }
    }

    /// Every drawing as plain JSON, in paint order.
    pub fn all(&self) -> JsValue {
        if self.inner.dead() {
            return Array::new().into();
        }
        ser!(&self.inner.chart.drawings().all())
    }

    /// Snapshot all drawings as a versioned document.
    #[wasm_bindgen(js_name = toJSON)]
    pub fn to_json(&self) -> JsValue {
        if self.inner.dead() {
            return to_js(&json!({ "version": 1, "drawings": [] }));
        }
        ser!(&self.inner.chart.drawings().to_json())
    }

    /// Restore drawings from a document produced by `toJSON` (untrusted-safe: a missing or
    /// invalid document restores an empty chart).
    #[wasm_bindgen(js_name = fromJSON)]
    pub fn restore_json(&self, doc: &JsValue) {
        let value = from_js(doc, "drawings.fromJSON(doc)").unwrap_or(Value::Null);
        if !self.inner.dead() {
            self.inner.chart.drawings().from_json(&value);
        }
    }

    pub fn favorites(&self) -> JsValue {
        if self.inner.dead() {
            return Array::new().into();
        }
        strings(&self.inner.chart.drawings().favorites())
    }

    #[wasm_bindgen(js_name = isFavorite)]
    pub fn is_favorite(&self, type_key: &str) -> bool {
        !self.inner.dead() && self.inner.chart.drawings().is_favorite(type_key)
    }

    #[wasm_bindgen(js_name = setFavorite)]
    pub fn set_favorite(&self, type_key: &str, on: bool) {
        if !self.inner.dead() {
            self.inner.chart.drawings().set_favorite(type_key, on);
        }
    }

    #[wasm_bindgen(js_name = setFavorites)]
    pub fn set_favorites(&self, types: Vec<String>) {
        if !self.inner.dead() {
            self.inner.chart.drawings().set_favorites(&types);
        }
    }

    /// Every tool's remembered settings, by type.
    #[wasm_bindgen(js_name = toolDefaults)]
    pub fn tool_defaults(&self) -> JsValue {
        if self.inner.dead() {
            return object(&[]);
        }
        ser!(&self.inner.chart.drawings().tool_defaults())
    }

    #[wasm_bindgen(js_name = setToolDefaults)]
    pub fn set_tool_defaults(&self, map: &JsValue) -> Result<(), JsValue> {
        let value = from_js(map, "drawings.setToolDefaults(map)")?;
        if !self.inner.dead() {
            self.inner.chart.drawings().set_tool_defaults(&value);
        }
        Ok(())
    }

    /// Forget one tool's remembered settings (every tool's without an argument).
    #[wasm_bindgen(js_name = resetToolDefaults)]
    pub fn reset_tool_defaults(&self, type_key: Option<String>) {
        if !self.inner.dead() {
            self.inner
                .chart
                .drawings()
                .reset_tool_defaults(type_key.as_deref());
        }
    }

    /// Every tool's saved looks, by type: `{ trendline: [{ name, settings }] }`.
    #[wasm_bindgen(js_name = toolTemplates)]
    pub fn tool_templates(&self) -> JsValue {
        let out = js_sys::Object::new();
        if self.inner.dead() {
            return out.into();
        }
        for (type_key, list) in self.inner.chart.drawings().tool_templates() {
            let arr: Array = list.iter().map(template_js).collect();
            json::set(&out, &type_key, arr.into());
        }
        out.into()
    }

    #[wasm_bindgen(js_name = setToolTemplates)]
    pub fn set_tool_templates(&self, map: &JsValue) -> Result<(), JsValue> {
        let value = from_js(map, "drawings.setToolTemplates(map)")?;
        if !self.inner.dead() {
            self.inner.chart.drawings().set_tool_templates(&value);
        }
        Ok(())
    }

    /// Alias of `toJSON`.
    #[wasm_bindgen(js_name = getConfig)]
    pub fn get_config(&self) -> JsValue {
        self.to_json()
    }

    /// Alias of `fromJSON`.
    #[wasm_bindgen(js_name = applyConfig)]
    pub fn apply_config(&self, doc: &JsValue) {
        self.restore_json(doc);
    }
}

// ── chart.marks ──

/// `chart.marks`: host events pinned to a bar, on a lane above the time axis.
#[wasm_bindgen(js_name = MarksControl)]
pub struct MarksControlApi {
    pub(crate) inner: Rc<Inner>,
}

fn marks_err(e: vela_core::marks::MarksError) -> JsValue {
    error(e.to_string())
}

#[wasm_bindgen(js_class = MarksControl)]
impl MarksControlApi {
    /// Whether the renderer paints timeline marks.
    #[wasm_bindgen(getter)]
    pub fn supported(&self) -> bool {
        !self.inner.dead() && self.inner.chart.marks().supported()
    }

    /// Add one mark; an existing id is replaced in place.
    pub fn add(&self, mark: &JsValue) -> Result<(), JsValue> {
        let mark = mark_of(mark)?;
        if !self.inner.dead() {
            self.inner.chart.marks().add(mark).map_err(marks_err)?;
        }
        Ok(())
    }

    /// Replace the whole set.
    pub fn set(&self, marks: &JsValue) -> Result<(), JsValue> {
        let list = marks
            .dyn_ref::<Array>()
            .ok_or_else(|| type_error("[vela] marks.set takes an array of marks"))?;
        let marks: Vec<TimelineMark> =
            list.iter().map(|m| mark_of(&m)).collect::<Result<_, _>>()?;
        if !self.inner.dead() {
            self.inner.chart.marks().set(marks).map_err(marks_err)?;
        }
        Ok(())
    }

    pub fn remove(&self, id: &str) {
        if !self.inner.dead() {
            self.inner.chart.marks().remove(id);
        }
    }

    pub fn clear(&self) {
        if !self.inner.dead() {
            self.inner.chart.marks().clear();
        }
    }

    /// Every mark, in insertion order.
    pub fn all(&self) -> JsValue {
        if self.inner.dead() {
            return Array::new().into();
        }
        self.inner
            .chart
            .marks()
            .all()
            .iter()
            .map(mark_js)
            .collect::<Array>()
            .into()
    }

    /// Define a visibility group: `{ id, label, visible?, parent? }`.
    #[wasm_bindgen(js_name = defineGroup)]
    pub fn define_group(&self, group: &JsValue) -> Result<(), JsValue> {
        let group = typed!(
            MarkGroup,
            &from_js(group, "marks.defineGroup(group)")?,
            "marks.defineGroup(group)"
        )?;
        if !self.inner.dead() {
            self.inner
                .chart
                .marks()
                .define_group(group)
                .map_err(marks_err)?;
        }
        Ok(())
    }

    pub fn groups(&self) -> JsValue {
        if self.inner.dead() {
            return Array::new().into();
        }
        ser!(&self.inner.chart.marks().groups())
    }

    #[wasm_bindgen(js_name = setGroupVisible)]
    pub fn set_group_visible(&self, id: &str, visible: Option<bool>) {
        if !self.inner.dead() {
            self.inner
                .chart
                .marks()
                .set_group_visible(id, visible.unwrap_or(true));
        }
    }

    #[wasm_bindgen(js_name = isGroupVisible)]
    pub fn is_group_visible(&self, id: &str) -> bool {
        !self.inner.dead() && self.inner.chart.marks().is_group_visible(id)
    }
}

// ── IndicatorHandle ──

fn input_type_str(t: vela_core::model::InputType) -> &'static str {
    use vela_core::model::InputType as T;
    match t {
        T::Int => "int",
        T::Float => "float",
        T::Bool => "bool",
        T::String => "string",
        T::Source => "source",
        T::Color => "color",
        T::Price => "price",
        T::Time => "time",
        T::Session => "session",
        T::Timeframe => "timeframe",
        T::Symbol => "symbol",
        T::TextArea => "text_area",
    }
}

fn input_value_js(v: &InputValue) -> JsValue {
    match v {
        InputValue::Number(n) => JsValue::from_f64(*n),
        InputValue::Text(s) => JsValue::from_str(s),
        InputValue::Bool(b) => JsValue::from_bool(*b),
    }
}

fn schema_js(list: Vec<vela_core::model::InputSchema>) -> JsValue {
    list.iter()
        .map(|i| {
            let o = js_sys::Object::new();
            json::set(&o, "key", JsValue::from_str(&i.key));
            json::set(&o, "title", JsValue::from_str(&i.title));
            json::set(&o, "type", JsValue::from_str(input_type_str(i.input_type)));
            json::set(&o, "defval", input_value_js(&i.defval));
            for (k, v) in [("min", i.min), ("max", i.max), ("step", i.step)] {
                if let Some(v) = v {
                    json::set(&o, k, JsValue::from_f64(v));
                }
            }
            if let Some(options) = &i.options {
                json::set(&o, "options", strings(options));
            }
            for (k, v) in [
                ("group", &i.group),
                ("inline", &i.inline),
                ("tab", &i.tab),
                ("tooltip", &i.tooltip),
            ] {
                if let Some(v) = v {
                    json::set(&o, k, JsValue::from_str(v));
                }
            }
            JsValue::from(o)
        })
        .collect::<Array>()
        .into()
}

/// What `addIndicator` / `addNativeIndicator` return: usable at once, data fills in later.
#[wasm_bindgen(js_name = IndicatorHandle)]
pub struct IndicatorHandleApi {
    pub(crate) inner: Rc<Inner>,
    pub(crate) handle: vela_core::vela::IndicatorHandleRef,
}

#[wasm_bindgen(js_class = IndicatorHandle)]
impl IndicatorHandleApi {
    #[wasm_bindgen(getter)]
    pub fn id(&self) -> String {
        self.handle.id().to_string()
    }

    #[wasm_bindgen(getter)]
    pub fn title(&self) -> String {
        self.handle.title()
    }

    /// The script source; `undefined` for a native indicator.
    #[wasm_bindgen(getter)]
    pub fn source(&self) -> Option<String> {
        self.handle.source()
    }

    #[wasm_bindgen(getter, js_name = nativeType)]
    pub fn native_type(&self) -> Option<String> {
        self.handle.native_type().map(String::from)
    }

    #[wasm_bindgen(getter)]
    pub fn language(&self) -> Option<String> {
        self.handle.language().map(String::from)
    }

    /// The inputs: `{ key, title, type, defval, min?, max?, step?, options?, group?, ... }`.
    #[wasm_bindgen(getter)]
    pub fn inputs(&self) -> JsValue {
        schema_js(self.handle.inputs())
    }

    #[wasm_bindgen(getter)]
    pub fn props(&self) -> JsValue {
        schema_js(self.handle.props())
    }

    #[wasm_bindgen(getter)]
    pub fn visible(&self) -> bool {
        self.handle.visible()
    }

    #[wasm_bindgen(js_name = setInput)]
    pub fn set_input(&self, key: &str, value: &JsValue) -> Result<(), JsValue> {
        let value = input_value(value)?;
        if !self.inner.dead() {
            self.handle.set_input(key, value);
        }
        Ok(())
    }

    #[wasm_bindgen(js_name = setInputs)]
    pub fn set_inputs(&self, values: &JsValue) {
        if !self.inner.dead() {
            self.handle.set_inputs(inputs_of(values));
        }
    }

    #[wasm_bindgen(js_name = setProp)]
    pub fn set_prop(&self, key: &str, value: &JsValue) -> Result<(), JsValue> {
        let value = input_value(value)?;
        if !self.inner.dead() {
            self.handle.set_prop(key, value);
        }
        Ok(())
    }

    #[wasm_bindgen(js_name = setProps)]
    pub fn set_props(&self, values: &JsValue) {
        if !self.inner.dead() {
            self.handle.set_props(inputs_of(values));
        }
    }

    /// Replace the script and re-run it in place: resolves `{ ok, error? }`, never rejects.
    #[wasm_bindgen(js_name = updateCode)]
    pub fn update_code(&self, source: &str) -> Promise {
        if self.inner.dead() {
            return Promise::resolve(&object(&[
                ("ok", JsValue::FALSE),
                (
                    "error",
                    error("[vela] updateCode \u{2014} the chart was destroyed."),
                ),
            ]));
        }
        let fut = self.handle.update_code(source);
        promise_of(async move {
            let r = fut.await;
            let o = js_sys::Object::new();
            json::set(&o, "ok", JsValue::from_bool(r.ok));
            if let Some(e) = &r.error {
                json::set(&o, "error", error(e));
            }
            Ok(o.into())
        })
    }

    #[wasm_bindgen(js_name = setVisible)]
    pub fn set_visible(&self, visible: bool) {
        if !self.inner.dead() {
            self.handle.set_visible(visible);
        }
    }

    /// Move or merge into a pane: `'price'`, `{ pane }`, `{ newPane }`.
    #[wasm_bindgen(js_name = moveTo)]
    pub fn move_to(&self, target: &JsValue) -> Result<(), JsValue> {
        let target = move_target_of(target)?;
        if !self.inner.dead() {
            self.handle.move_to(target);
        }
        Ok(())
    }

    /// `ready`, `error` (`{ error }`) or `alert` (`{ id, message, title?, time }`); returns
    /// the unsubscribe function.
    pub fn on(&self, event: &str, handler: &JsValue) -> Result<JsValue, JsValue> {
        let f = callback_of(handler, "indicator.on")?;
        let name = match event {
            "ready" => IndicatorEventName::Ready,
            "error" => IndicatorEventName::Error,
            "alert" => IndicatorEventName::Alert,
            _ => return Ok(unsub_fn(Box::new(|| {}))),
        };
        if self.inner.dead() {
            return Ok(unsub_fn(Box::new(|| {})));
        }
        let event_name = event.to_string();
        let unsub = self.handle.on(
            name,
            Box::new(move |e: &IndicatorEvent| {
                let payload = match e {
                    IndicatorEvent::Ready => JsValue::UNDEFINED,
                    IndicatorEvent::Error { error: message } => {
                        object(&[("error", error(message))])
                    }
                    IndicatorEvent::Alert(a) => {
                        let o = js_sys::Object::new();
                        json::set(&o, "id", JsValue::from_str(&a.id));
                        json::set(&o, "message", JsValue::from_str(&a.message));
                        if let Some(t) = &a.title {
                            json::set(&o, "title", JsValue::from_str(t));
                        }
                        json::set(&o, "time", JsValue::from_f64(a.time));
                        o.into()
                    }
                };
                call_handler(&f, &payload, &format!("indicator \"{event_name}\""));
            }),
        );
        Ok(unsub_fn(unsub))
    }

    /// `Promise` of a read-only snapshot of the engine's execution context; `null` when the
    /// engine lacks the capability or nothing ran (always so for a native indicator).
    pub fn context(&self) -> Promise {
        if self.inner.dead() {
            return Promise::resolve(&JsValue::NULL);
        }
        let fut = self.handle.context(None);
        promise_of(async move {
            Ok(match fut.await {
                Ok(Some(snapshot)) => ser!(&snapshot),
                _ => JsValue::NULL,
            })
        })
    }

    pub fn remove(&self) {
        if !self.inner.dead() {
            self.handle.remove();
        }
    }
}

/// Every chain method of the classes above, by class (see [`chain`]).
pub(crate) const DATA_CHAIN: &[&str] = &["registerProvider", "unregisterProvider"];
pub(crate) const PANES_CHAIN: &[&str] = &[
    "move",
    "remove",
    "collapse",
    "maximize",
    "setHeightWeight",
    "moveIndicator",
];
pub(crate) const REPLAY_CHAIN: &[&str] = &["play", "pause", "stop", "setTicks"];
pub(crate) const RENDERER_CHAIN: &[&str] = &[
    "set",
    "applyConfig",
    "setExternalCrosshair",
    "focus",
    "closeDialogs",
    "openSettings",
    "openIndicatorSettings",
    "setSettingsVisibility",
    "setLayoutMode",
];
pub(crate) const DRAWINGS_CHAIN: &[&str] = &[
    "setTool",
    "setSnapMode",
    "setStayMode",
    "setMode",
    "showToolbar",
    "setToolbar",
    "setToolShortcuts",
    "setExternalGhost",
    "remove",
    "update",
    "updateMany",
    "removeMany",
    "lock",
    "show",
    "select",
    "openSettings",
    "bringToFront",
    "sendToBack",
    "undo",
    "redo",
    "clone",
    "duplicate",
    "copyToClipboard",
    "paste",
    "fromJSON",
    "setFavorite",
    "setFavorites",
    "setToolDefaults",
    "resetToolDefaults",
    "setToolTemplates",
    "applyConfig",
];
pub(crate) const MARKS_CHAIN: &[&str] = &[
    "add",
    "set",
    "remove",
    "clear",
    "defineGroup",
    "setGroupVisible",
];
pub(crate) const HANDLE_CHAIN: &[&str] = &[
    "setInput",
    "setInputs",
    "setProp",
    "setProps",
    "setVisible",
    "moveTo",
];

/// Parse a bar list (used by the options and `setMarket`).
pub(crate) fn bar_list(v: &JsValue, what: &str) -> Result<Vec<Ohlcv>, JsValue> {
    let list = v
        .dyn_ref::<Array>()
        .ok_or_else(|| type_error(format!("[vela] {what} must be an array of bars")))?;
    bars_of(list)
}
