//! The JavaScript side of the widget's contribution registries: the original's module-global
//! `register*` functions (`plugin.ts` / `widget/contributions.ts`), shared by every widget of
//! the page. A widget reads them live (`refreshActions()` picks up a late registration), except
//! state persistence and shell defaults, which a widget reads when it is built.
//!
//! ```js
//! registerWidgetAction({ id, target: 'topbar' | 'sidebar' | 'context:body' | 'context:price-axis'
//!   | 'context:time-axis', label, icon, iconOnly, order, align: 'left' | 'right',
//!   mobile: 'bar' | 'menu', when(ctx), active(ctx), run(ctx) })                      -> off()
//! registerSidePanel({ id, title, icon, order, width, resizable, minWidth, maxWidth, overlay,
//!   maximizable, header: 'default' | 'none', placement: 'chart' | 'column', mount(body, header) })   -> off()
//! registerWidgetAttachment({ id, mount(ctx) { return disposer } })                   -> off()
//! registerLegendAction({ id, icon, tooltip, order, when(info), run(ctx, info) })     -> off()
//! registerLegendCallout({ id, order, callout(info) { return spec | null } })         -> off()
//! registerSymbolRanking(descriptors => descriptors | Promise<descriptors>)           -> off()
//! registerStatePersistence({ key, scope: 'cell' | 'global', serialize(ctx), restore(value, ctx) })
//! registerShellDefaults({ sidebar: true | false | { top, bottom } })                 -> off()
//! unregisterWidgetAction(id) unregisterSidePanel(id) unregisterWidgetAttachment(id)
//! unregisterLegendAction(id) unregisterLegendCallout(id) unregisterStatePersistence(key)
//! unregisterShellDefaults()
//! ```
//!
//! `ctx` (actions, attachments, legend callbacks, global persistence): `{ symbol, timeframe,
//! priceStyle, pointer, host, setSymbol(s), setTimeframe(tf), setPriceStyle(s),
//! openSymbolSearch(q), togglePanel(id, open), toast(message, kind), stateChanged(),
//! addNativeIndicator(type), dockStrip(el, { position }), dockEdge(el), topbar: { isVisible,
//! setVisible }, sidebar: { enabled, setBadge, refresh, isVisible, setVisible }, panels: { list,
//! openId, isMaximized } }`. A legend `info`: `{ id, title, source, language }`. A cell-scope
//! persistence `ctx`: `{ cellId }` (act on the cell through your own `ws.cell(cellId)`).
//!
//! Not exposed: `registerDefaultEngine` (an engine is a Rust trait object, not a script
//! callback).

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, Function, Object, Promise, Reflect};
use serde_json::Value;
use vela_core::i18n::MessageLike;
use vela_core::ports::data_provider::SymbolDescriptor;
use vela_core::widget::contributions::{
    ActionAlign, CellStateContext, Contributions, DockEdge, DockPosition, DockStripOptions,
    ElementHandle, LegendActionDescriptor, LegendCalloutContent, LegendCalloutDescriptor,
    LegendCalloutItem, LegendCalloutSpec, LegendGate, LegendIndicatorInfo, LegendRun,
    MobilePlacement, PanelHeaderMode, PanelMount, PanelPlacement, PointerPaneKind, RankedPool,
    SidePanelDescriptor, SidePanelHandle, SidePanelHeader, SidebarBadge, StatePersistenceHandler,
    SymbolRankingHook, ToastKind, TopbarItemId, WidgetActionDescriptor, WidgetActionTarget,
    WidgetAttachment, WidgetContext,
};
use vela_core::widget::shell_defaults::{ShellDefaultKey, ShellDefaults, ShellDefaultsRegistry};
use vela_core::workspace::ChartCell;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::Element;

use crate::platform::BrowserPlatform;

use super::context::RootContext;
use super::js;
use super::root::WidgetRoot;

thread_local! {
    static GLOBAL: Contributions = Contributions::new(Rc::new(BrowserPlatform::new()));
    static DEFAULTS: ShellDefaultsRegistry = ShellDefaultsRegistry::new();
}

/// The registries every widget of the page reads.
pub fn global_contributions() -> Contributions {
    GLOBAL.with(Clone::clone)
}

/// The shell option defaults every widget built afterwards uses.
pub fn global_shell_defaults() -> ShellDefaultsRegistry {
    DEFAULTS.with(Clone::clone)
}

fn prop(o: &JsValue, key: &str) -> JsValue {
    Reflect::get(o, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

fn string(o: &JsValue, key: &str) -> Option<String> {
    prop(o, key).as_string()
}

fn number(o: &JsValue, key: &str) -> Option<f64> {
    prop(o, key).as_f64()
}

fn flag(o: &JsValue, key: &str) -> bool {
    prop(o, key).is_truthy()
}

fn set(o: &Object, key: &str, v: &JsValue) {
    let _ = Reflect::set(o, &JsValue::from_str(key), v);
}

fn err(m: &str) -> JsValue {
    JsError::new(m).into()
}

fn function(o: &JsValue, key: &str) -> Option<Function> {
    prop(o, key).dyn_into::<Function>().ok()
}

fn unsubscribe(off: Box<dyn FnOnce()>) -> Function {
    let cell = Rc::new(RefCell::new(Some(off)));
    Closure::<dyn FnMut()>::new(move || {
        // Taken before it runs: a second call from inside it finds the slot empty.
        let off = cell.borrow_mut().take();
        if let Some(f) = off {
            f();
        }
    })
    .into_js_value()
    .unchecked_into()
}

fn message(o: &JsValue, key: &str, fallback: &str) -> MessageLike {
    MessageLike::from(string(o, key).unwrap_or_else(|| fallback.to_string()))
}

fn to_js_json(value: &Value) -> JsValue {
    js_sys::JSON::parse(&value.to_string()).unwrap_or(JsValue::NULL)
}

fn from_js_json(value: &JsValue) -> Option<Value> {
    if value.is_undefined() || value.is_null() {
        return None;
    }
    let text = js_sys::JSON::stringify(value).ok()?.as_string()?;
    serde_json::from_str(&text).ok()
}

// ── the context object ──

type Dispatcher = (
    Function,
    Closure<dyn Fn(String, JsValue, JsValue) -> JsValue>,
);

fn dispatcher(root: &Rc<WidgetRoot>) -> Function {
    if let Some(d) = root.script_slot().borrow().as_ref()
        && let Some(d) = d.downcast_ref::<Dispatcher>()
    {
        return d.0.clone();
    }
    let weak = Rc::downgrade(root);
    let closure = Closure::<dyn Fn(String, JsValue, JsValue) -> JsValue>::new(
        move |op: String, a: JsValue, b: JsValue| -> JsValue {
            let Some(root) = weak.upgrade() else {
                return JsValue::UNDEFINED;
            };
            dispatch(&root, &op, &a, &b)
        },
    );
    let func: Function = closure.as_ref().unchecked_ref::<Function>().clone();
    *root.script_slot().borrow_mut() =
        Some(Rc::new((func.clone(), closure)) as Rc<dyn std::any::Any>);
    func
}

fn dispatch(root: &Rc<WidgetRoot>, op: &str, a: &JsValue, b: &JsValue) -> JsValue {
    let ctx = root.context();
    let text = |v: &JsValue| v.as_string().unwrap_or_default();
    match op {
        "setSymbol" => ctx.set_symbol(&text(a)),
        "setTimeframe" => ctx.set_timeframe(&text(a)),
        "setPriceStyle" => ctx.set_price_style(&text(a)),
        "openSymbolSearch" => ctx.open_symbol_search(a.as_string().as_deref()),
        "togglePanel" => ctx.toggle_panel(&text(a), b.as_bool()),
        "toast" => ctx.toast(
            &text(a),
            match b.as_string().as_deref() {
                Some("success") => ToastKind::Success,
                Some("error") => ToastKind::Error,
                _ => ToastKind::Info,
            },
        ),
        "stateChanged" => ctx.state_changed(),
        "addNativeIndicator" => ctx.add_native_indicator(&text(a)),
        "dockStrip" => {
            let el: ElementHandle = Rc::new(a.clone().unchecked_into::<Element>());
            let below = string(b, "position").as_deref() == Some("below-bottombar");
            let strip = ctx.dock_strip(
                el,
                DockStripOptions {
                    position: if below {
                        DockPosition::BelowBottombar
                    } else {
                        DockPosition::AboveBottombar
                    },
                },
            );
            let strip = Rc::new(strip);
            // As the original's `DockedStrip`: the strip IS the undock function, with a
            // `cover(on)` member (`undock` is kept as an alias).
            let s_undock = strip.clone();
            let undock: Function = Closure::<dyn Fn()>::new(move || s_undock.undock())
                .into_js_value()
                .unchecked_into();
            set(
                &undock,
                "cover",
                &Closure::<dyn Fn(JsValue)>::new(move |on: JsValue| strip.cover(on.is_truthy()))
                    .into_js_value(),
            );
            set(&undock, "undock", &undock.clone());
            return undock.into();
        }
        "dockEdge" => {
            let el: ElementHandle = Rc::new(a.clone().unchecked_into::<Element>());
            let off = ctx.dock_edge(el, DockEdge::Right);
            return unsubscribe(off).into();
        }
        "topbar.isVisible" => {
            return JsValue::from_bool(
                TopbarItemId::from_id(&text(a)).is_some_and(|id| ctx.topbar().is_visible(id)),
            );
        }
        "topbar.setVisible" => {
            if let Some(id) = TopbarItemId::from_id(&text(a)) {
                ctx.topbar().set_visible(id, b.is_truthy());
            }
        }
        "sidebar.enabled" => return JsValue::from_bool(ctx.sidebar().enabled()),
        "sidebar.setBadge" => {
            let badge = if let Some(n) = b.as_f64() {
                Some(SidebarBadge::Number(n))
            } else {
                b.as_string().map(SidebarBadge::Text)
            };
            ctx.sidebar().set_badge(&text(a), badge);
        }
        "sidebar.refresh" => ctx.sidebar().refresh(),
        "sidebar.isVisible" => return JsValue::from_bool(ctx.sidebar().is_visible(&text(a))),
        "sidebar.setVisible" => ctx.sidebar().set_visible(&text(a), b.is_truthy()),
        "panels.list" => {
            return ctx
                .panels()
                .list()
                .into_iter()
                .map(|p| {
                    let o = Object::new();
                    set(&o, "id", &JsValue::from_str(&p.id));
                    set(&o, "title", &JsValue::from_str(&p.title));
                    set(&o, "icon", &JsValue::from_str(&p.icon));
                    set(&o, "builtIn", &JsValue::from_bool(p.built_in));
                    JsValue::from(o)
                })
                .collect::<Array>()
                .into();
        }
        "panels.openId" => {
            return ctx
                .panels()
                .open_id()
                .map_or(JsValue::NULL, |id| JsValue::from_str(&id));
        }
        "panels.onDidChange" => {
            let Some(cb) = a.dyn_ref::<Function>().cloned() else {
                return JsValue::UNDEFINED;
            };
            let off = ctx.panels().on_did_change(Box::new(move || {
                let _ = cb.call0(&JsValue::NULL);
            }));
            return unsubscribe(off).into();
        }
        "panels.isMaximized" => return JsValue::from_bool(ctx.panels().is_maximized(&text(a))),
        _ => {}
    }
    JsValue::UNDEFINED
}

fn pointer_object(ctx: &dyn WidgetContext) -> JsValue {
    let Some(p) = ctx.pointer() else {
        return JsValue::NULL;
    };
    let po = Object::new();
    set(
        &po,
        "price",
        &p.price.map_or(JsValue::NULL, JsValue::from_f64),
    );
    set(
        &po,
        "time",
        &p.time.map_or(JsValue::NULL, JsValue::from_f64),
    );
    set(
        &po,
        "paneId",
        &p.pane_id
            .as_deref()
            .map_or(JsValue::NULL, JsValue::from_str),
    );
    set(
        &po,
        "paneKind",
        &match p.pane_kind {
            Some(PointerPaneKind::Price) => JsValue::from_str("price"),
            Some(PointerPaneKind::Study) => JsValue::from_str("study"),
            None => JsValue::NULL,
        },
    );
    po.into()
}

/// The context object a script callback receives (see the module documentation).
pub fn context_object(ctx: &dyn WidgetContext) -> JsValue {
    let data = Object::new();
    set(&data, "symbol", &JsValue::from_str(&ctx.symbol()));
    set(&data, "timeframe", &JsValue::from_str(&ctx.timeframe()));
    set(&data, "priceStyle", &JsValue::from_str(&ctx.price_style()));
    set(&data, "pointer", &pointer_object(ctx));
    let root = ctx
        .as_any()
        .and_then(|a| a.downcast_ref::<RootContext>())
        .and_then(RootContext::widget_root);
    match root {
        Some(root) => {
            let host: JsValue = root.element().clone().into();
            js::vela_ctx_build(&dispatcher(&root), &data, &host)
        }
        None => data.into(),
    }
}

fn info_object(info: &LegendIndicatorInfo) -> JsValue {
    let o = Object::new();
    set(&o, "id", &JsValue::from_str(&info.id));
    set(&o, "title", &JsValue::from_str(&info.title));
    set(
        &o,
        "source",
        &info
            .source
            .as_deref()
            .map_or(JsValue::NULL, JsValue::from_str),
    );
    set(
        &o,
        "language",
        &info
            .language
            .as_deref()
            .map_or(JsValue::NULL, JsValue::from_str),
    );
    o.into()
}

// ── side panels ──

struct JsHandle(JsValue);

impl SidePanelHandle for JsHandle {
    fn on_chart(&self, _chart: &vela_core::vela::Vela) {
        if let Some(f) = function(&self.0, "onChart") {
            let _ = f.call0(&self.0);
        }
    }
    fn on_open(&self) {
        if let Some(f) = function(&self.0, "onOpen") {
            let _ = f.call0(&self.0);
        }
    }
    fn destroy(&self) {
        if let Some(f) = function(&self.0, "destroy") {
            let _ = f.call0(&self.0);
        }
    }
}

fn header_object(header: &dyn SidePanelHeader) -> JsValue {
    let o = Object::new();
    let slot = header.slot();
    if let Some(el) = slot.downcast_ref::<Element>() {
        set(&o, "slot", el);
        // The dock parked the functions on the slot.
        let api = prop(el, "velaHeader");
        for name in ["setTitle", "setMaximized", "close"] {
            set(&o, name, &prop(&api, name));
        }
    }
    set(&o, "maximized", &JsValue::from_bool(header.maximized()));
    o.into()
}

/// Register a side panel in the page-wide registry. Returns the unregister function.
#[wasm_bindgen(js_name = registerSidePanel)]
pub fn register_side_panel(desc: &JsValue) -> Result<Function, JsValue> {
    let id = string(desc, "id").ok_or_else(|| err("registerSidePanel: `id` is required"))?;
    let title = string(desc, "title").unwrap_or_else(|| id.clone());
    let icon = string(desc, "icon").unwrap_or_else(|| "objects".to_string());
    let mount_fn: Function = prop(desc, "mount")
        .dyn_into()
        .map_err(|_| err("registerSidePanel: `mount` is required"))?;
    let mount: PanelMount = Rc::new(
        move |_ctx: &dyn WidgetContext, body: &ElementHandle, header: &dyn SidePanelHeader| {
            let body_el = body.downcast_ref::<Element>()?.clone();
            let h = header_object(header);
            let result = mount_fn.call2(&JsValue::NULL, &body_el, &h).ok()?;
            if result.is_object() {
                Some(Box::new(JsHandle(result)) as Box<dyn SidePanelHandle>)
            } else {
                None
            }
        },
    );
    let mut d = SidePanelDescriptor::new(id, title, icon, mount);
    d.order = number(desc, "order");
    d.width = number(desc, "width");
    d.resizable = flag(desc, "resizable");
    d.min_width = number(desc, "minWidth");
    d.max_width = number(desc, "maxWidth");
    d.overlay = flag(desc, "overlay");
    d.maximizable = flag(desc, "maximizable");
    // The original's option is `header: 'default' | 'none'` (`headerless` is kept as an alias).
    if string(desc, "header").as_deref() == Some("none") || flag(desc, "headerless") {
        d.header = PanelHeaderMode::None;
    }
    if string(desc, "placement").as_deref() == Some("column") {
        d.placement = PanelPlacement::Column;
    }
    Ok(unsubscribe(global_contributions().register_side_panel(d)))
}

#[wasm_bindgen(js_name = unregisterSidePanel)]
pub fn unregister_side_panel(id: &str) {
    global_contributions().unregister_side_panel(id);
}

// ── widget actions ──

/// Register a widget action (sidebar, topbar or context-menu row) in the page-wide registry.
#[wasm_bindgen(js_name = registerWidgetAction)]
pub fn register_widget_action(desc: &JsValue) -> Result<Function, JsValue> {
    let id = string(desc, "id").ok_or_else(|| err("registerWidgetAction: `id` is required"))?;
    let target = match string(desc, "target").as_deref() {
        Some("sidebar") => WidgetActionTarget::Sidebar,
        Some("topbar") => WidgetActionTarget::Topbar,
        Some("context:body") => WidgetActionTarget::ContextBody,
        Some("context:price-axis") => WidgetActionTarget::ContextPriceAxis,
        Some("context:time-axis") => WidgetActionTarget::ContextTimeAxis,
        _ => return Err(err("registerWidgetAction: unknown `target`")),
    };
    let label = message(desc, "label", &id);
    let run: Function =
        function(desc, "run").ok_or_else(|| err("registerWidgetAction: `run` is required"))?;
    let mut a = WidgetActionDescriptor::new(id, target, label, move |ctx: &dyn WidgetContext| {
        let _ = run.call1(&JsValue::NULL, &context_object(ctx));
    });
    a.icon = string(desc, "icon");
    a.icon_only = flag(desc, "iconOnly");
    a.order = number(desc, "order");
    a.align = match string(desc, "align").as_deref() {
        Some("left") => Some(ActionAlign::Left),
        Some("right") => Some(ActionAlign::Right),
        _ => None,
    };
    a.mobile = match string(desc, "mobile").as_deref() {
        Some("bar") => Some(MobilePlacement::Bar),
        Some("menu") => Some(MobilePlacement::Menu),
        _ => None,
    };
    if let Some(when) = function(desc, "when") {
        a.when = Some(Rc::new(move |ctx: &dyn WidgetContext| {
            when.call1(&JsValue::NULL, &context_object(ctx))
                .map(|v| v.is_truthy())
                .unwrap_or(false)
        }));
    }
    if let Some(active) = function(desc, "active") {
        a.active = Some(Rc::new(move |ctx: &dyn WidgetContext| {
            active
                .call1(&JsValue::NULL, &context_object(ctx))
                .map(|v| v.is_truthy())
                .unwrap_or(false)
        }));
    }
    Ok(unsubscribe(
        global_contributions().register_widget_action(a),
    ))
}

#[wasm_bindgen(js_name = unregisterWidgetAction)]
pub fn unregister_widget_action(id: &str) {
    global_contributions().unregister_widget_action(id);
}

// ── attachments ──

/// Register a widget attachment: `mount(ctx)` runs once per widget and may return a
/// disposer that runs at the widget's destroy.
#[wasm_bindgen(js_name = registerWidgetAttachment)]
pub fn register_widget_attachment(desc: &JsValue) -> Result<Function, JsValue> {
    let id = string(desc, "id").ok_or_else(|| err("registerWidgetAttachment: `id` is required"))?;
    let mount: Function = function(desc, "mount")
        .ok_or_else(|| err("registerWidgetAttachment: `mount` is required"))?;
    let att = WidgetAttachment::new(id, move |ctx: &dyn WidgetContext| {
        let dispose = mount
            .call1(&JsValue::NULL, &context_object(ctx))
            .ok()
            .and_then(|v| v.dyn_into::<Function>().ok());
        Box::new(move || {
            if let Some(f) = dispose {
                let _ = f.call0(&JsValue::NULL);
            }
        })
    });
    Ok(unsubscribe(
        global_contributions().register_widget_attachment(att),
    ))
}

#[wasm_bindgen(js_name = unregisterWidgetAttachment)]
pub fn unregister_widget_attachment(id: &str) {
    global_contributions().unregister_widget_attachment(id);
}

// ── legend actions and callouts ──

fn legend_run(f: Function) -> LegendRun {
    Rc::new(move |ctx: &dyn WidgetContext, info: &LegendIndicatorInfo| {
        let _ = f.call2(&JsValue::NULL, &context_object(ctx), &info_object(info));
    })
}

/// Register a legend-row action: an icon button on the indicators' legend rows.
#[wasm_bindgen(js_name = registerLegendAction)]
pub fn register_legend_action(desc: &JsValue) -> Result<Function, JsValue> {
    let id = string(desc, "id").ok_or_else(|| err("registerLegendAction: `id` is required"))?;
    let icon =
        string(desc, "icon").ok_or_else(|| err("registerLegendAction: `icon` is required"))?;
    let run =
        function(desc, "run").ok_or_else(|| err("registerLegendAction: `run` is required"))?;
    let when: Option<LegendGate> = function(desc, "when").map(|f| {
        Rc::new(move |info: &LegendIndicatorInfo| {
            f.call1(&JsValue::NULL, &info_object(info))
                .map(|v| v.is_truthy())
                .unwrap_or(false)
        }) as LegendGate
    });
    let d = LegendActionDescriptor {
        tooltip: message(desc, "tooltip", &id),
        id,
        icon,
        order: number(desc, "order"),
        when,
        run: legend_run(run),
    };
    Ok(unsubscribe(
        global_contributions().register_legend_action(d),
    ))
}

#[wasm_bindgen(js_name = unregisterLegendAction)]
pub fn unregister_legend_action(id: &str) {
    global_contributions().unregister_legend_action(id);
}

fn callout_spec(spec: &JsValue) -> Option<LegendCalloutSpec> {
    if !spec.is_object() {
        return None;
    }
    let content = {
        let c = prop(spec, "content");
        c.is_object().then(|| {
            let items = prop(&c, "items");
            let items: Vec<LegendCalloutItem> = items
                .dyn_ref::<Array>()
                .map(|list| {
                    list.iter()
                        .filter_map(|item| {
                            if let Some(run) = function(&item, "run") {
                                Some(LegendCalloutItem::Button {
                                    label: message(&item, "label", ""),
                                    primary: prop(&item, "primary").as_bool(),
                                    close: prop(&item, "close").as_bool(),
                                    run: legend_run(run),
                                })
                            } else if string(&item, "text").is_some() {
                                Some(LegendCalloutItem::Text {
                                    text: message(&item, "text", ""),
                                })
                            } else {
                                None
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            LegendCalloutContent {
                title: string(&c, "title").map(MessageLike::from),
                items,
            }
        })
    };
    Some(LegendCalloutSpec {
        icon: string(spec, "icon")?,
        background: string(spec, "background").unwrap_or_default(),
        color: string(spec, "color"),
        tooltip: message(spec, "tooltip", ""),
        content,
    })
}

/// Register a legend callout: a tinted bubble on an indicator's legend row, resolved per row.
#[wasm_bindgen(js_name = registerLegendCallout)]
pub fn register_legend_callout(desc: &JsValue) -> Result<Function, JsValue> {
    let id = string(desc, "id").ok_or_else(|| err("registerLegendCallout: `id` is required"))?;
    let resolve = function(desc, "callout")
        .ok_or_else(|| err("registerLegendCallout: `callout` is required"))?;
    let d = LegendCalloutDescriptor {
        id,
        order: number(desc, "order"),
        callout: Rc::new(move |info: &LegendIndicatorInfo| {
            let spec = resolve.call1(&JsValue::NULL, &info_object(info)).ok()?;
            callout_spec(&spec)
        }),
    };
    Ok(unsubscribe(
        global_contributions().register_legend_callout(d),
    ))
}

#[wasm_bindgen(js_name = unregisterLegendCallout)]
pub fn unregister_legend_callout(id: &str) {
    global_contributions().unregister_legend_callout(id);
}

// ── symbol ranking ──

fn descriptors_from(value: &JsValue) -> Option<Vec<SymbolDescriptor>> {
    serde_json::from_value(from_js_json(value)?).ok()
}

fn descriptors_to(list: &[SymbolDescriptor]) -> JsValue {
    serde_json::to_value(list)
        .map(|v| to_js_json(&v))
        .unwrap_or(JsValue::NULL)
}

/// Register the symbol ranking (one at a time, the last wins): reorders the picker's pool.
#[wasm_bindgen(js_name = registerSymbolRanking)]
pub fn register_symbol_ranking(rank: &Function) -> Function {
    let rank = rank.clone();
    let hook: SymbolRankingHook = Rc::new(move |pool: Vec<SymbolDescriptor>| {
        let Ok(result) = rank.call1(&JsValue::NULL, &descriptors_to(&pool)) else {
            return RankedPool::Ready(pool);
        };
        if let Some(promise) = result.dyn_ref::<Promise>() {
            let promise = promise.clone();
            return RankedPool::Pending(Box::pin(async move {
                match JsFuture::from(promise).await {
                    Ok(v) => descriptors_from(&v).unwrap_or(pool),
                    Err(_) => pool,
                }
            }));
        }
        RankedPool::Ready(descriptors_from(&result).unwrap_or(pool))
    });
    unsubscribe(global_contributions().register_symbol_ranking(hook))
}

// ── state persistence ──

struct JsCellState<'a>(&'a ChartCell);

impl CellStateContext for JsCellState<'_> {
    fn cell_id(&self) -> String {
        self.0.id().to_string()
    }
    fn chart(&self) -> vela_core::vela::Vela {
        self.0.try_chart().expect("vela: the cell has no chart")
    }
    fn add_indicator(&self, entry: vela_core::widget::contributions::ExternalIndicatorEntry) {
        self.0
            .add_external_indicator(vela_core::workspace::ExternalIndicatorEntry {
                name: entry.name,
                script: entry.script,
                language: entry.language,
                category: None,
                id: entry.id,
                inputs: entry.inputs,
                props: entry.props,
                hidden: entry.hidden.unwrap_or(false),
            });
    }
    fn add_native_indicator(&self, indicator_type: &str) {
        self.0.add_native(indicator_type);
    }
}

/// Register a state-persistence handler: how a script puts its own state into the persisted
/// document. A widget reads the registrations when it is built.
#[wasm_bindgen(js_name = registerStatePersistence)]
pub fn register_state_persistence(desc: &JsValue) -> Result<Function, JsValue> {
    let key =
        string(desc, "key").ok_or_else(|| err("registerStatePersistence: `key` is required"))?;
    let serialize = function(desc, "serialize")
        .ok_or_else(|| err("registerStatePersistence: `serialize` is required"))?;
    let restore = function(desc, "restore")
        .ok_or_else(|| err("registerStatePersistence: `restore` is required"))?;
    let handler = match string(desc, "scope").as_deref() {
        None | Some("cell") => StatePersistenceHandler::Cell {
            key: key.clone(),
            serialize: Rc::new(move |ctx: &dyn CellStateContext| {
                let o = Object::new();
                set(&o, "cellId", &JsValue::from_str(&ctx.cell_id()));
                from_js_json(&serialize.call1(&JsValue::NULL, &o).ok()?)
            }),
            restore: Rc::new(move |value: &Value, ctx: &dyn CellStateContext| {
                let o = Object::new();
                set(&o, "cellId", &JsValue::from_str(&ctx.cell_id()));
                let _ = restore.call2(&JsValue::NULL, &to_js_json(value), &o);
            }),
        },
        Some("global") => StatePersistenceHandler::Global {
            key: key.clone(),
            serialize: Rc::new(move |ctx: &dyn WidgetContext| {
                from_js_json(&serialize.call1(&JsValue::NULL, &context_object(ctx)).ok()?)
            }),
            restore: Rc::new(move |value: &Value, ctx: &dyn WidgetContext| {
                let _ = restore.call2(&JsValue::NULL, &to_js_json(value), &context_object(ctx));
            }),
        },
        Some(_) => {
            return Err(err(
                "registerStatePersistence: `scope` is 'cell' or 'global'",
            ));
        }
    };
    Ok(unsubscribe(
        global_contributions().register_state_persistence(handler),
    ))
}

#[wasm_bindgen(js_name = unregisterStatePersistence)]
pub fn unregister_state_persistence(key: &str) {
    global_contributions().unregister_state_persistence(key);
}

/// A [`CellStateContext`] over a cell.
pub(super) fn cell_state(cell: &ChartCell) -> impl CellStateContext + '_ {
    JsCellState(cell)
}

// ── shell defaults ──

/// Register defaults for shell options (`sidebar`): every widget built afterwards uses one
/// where its host gave no value for that option.
#[wasm_bindgen(js_name = registerShellDefaults)]
pub fn register_shell_defaults(values: &JsValue) -> Function {
    let sidebar = crate::shell_side::sidebar_option_of(values);
    unsubscribe(global_shell_defaults().register(ShellDefaults { sidebar }))
}

/// The defaults in force: `{}` or `{ sidebar: true | false | { top?, bottom? } }`.
#[wasm_bindgen(js_name = shellDefaults)]
pub fn shell_defaults() -> JsValue {
    use vela_core::widget::shell_defaults::SidebarOption;
    let o = Object::new();
    if let Some(side) = global_shell_defaults().defaults().sidebar {
        let value = match side {
            SidebarOption::Enabled(b) => JsValue::from_bool(b),
            SidebarOption::Composition(c) => {
                let comp = Object::new();
                for (key, list) in [("top", c.top), ("bottom", c.bottom)] {
                    if let Some(list) = list {
                        let arr: Array = list.iter().map(|s| JsValue::from_str(s)).collect();
                        set(&comp, key, &arr);
                    }
                }
                comp.into()
            }
        };
        set(&o, "sidebar", &value);
    }
    o.into()
}

#[wasm_bindgen(js_name = unregisterShellDefaults)]
pub fn unregister_shell_defaults() {
    global_shell_defaults().unregister(&[ShellDefaultKey::Sidebar]);
}
