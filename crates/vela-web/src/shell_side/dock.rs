//! The side panels and their dock: `side-panel.ts` (the column shell with its header, the drag
//! handle, the pin and the maximize button) and `panel-dock.ts` (the single owner of the
//! column: exclusivity, the toggle group, the persisted state, contributed panels).
//!
//! The rules are `vela_core::widget::{side_panel, panel_dock}`: the dock's
//! [`PanelDockModel`] decides what each toggle does and returns [`DockEffect`]s, applied here.
//! The dock's [`PanelDockModel`] holds the ONE [`SidePanelModel`] of each panel (open, width,
//! placement, maximized, the resizer drag); a view only paints what the model says and reports
//! the user's gestures to the dock.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vela_core::i18n::MessageLike;
use vela_core::vela::Vela;
use vela_core::widget::contributions::{
    Contributions, DockedPanelInfo, ElementHandle, PanelDockView, PanelHeaderMode, PanelPlacement,
    SidePanelButton, SidePanelDescriptor, SidePanelHandle, SidePanelHeader,
};
use vela_core::widget::panel_dock::{DockEffect, DockEntry, PanelDockModel};
use vela_core::widget::side_panel::{
    ResizeDrag, SidePanelModel, SidePanelOptions, ToggleOutcome, close_label,
};
use wasm_bindgen::JsValue;
use web_sys::Element;

use crate::dom::{prevent_default, set_style};

use super::kit::{Bag, Cx, ev_f64, icon_el, set_data, set_hidden, set_title};

// ── one panel ──

/// One panel's element: `div.vela-panel.<modifier>` with its header, body and resizer.
pub struct PanelView {
    cx: Cx,
    pub id: String,
    pub el: Element,
    heading: Element,
    slot: Element,
    close: Element,
    pin: Option<Element>,
    maxb: Option<Element>,
    pub body: Element,
    title: RefCell<MessageLike>,
    opts: SidePanelOptions,
    /// The dock whose model owns this panel's state (set when the view is docked).
    dock: RefCell<Weak<Dock>>,
    bag: Rc<Bag>,
    drag: Cell<Option<ResizeDrag>>,
    on_close: RefCell<Option<Rc<dyn Fn()>>>,
    open: Cell<bool>,
    /// The closures of the JavaScript face of the header (`velaHeader`), owned here so they
    /// are freed with the panel rather than leaked.
    js_header: RefCell<Vec<Box<dyn Any>>>,
    js_guards: RefCell<Vec<js_sys::Function>>,
}

impl PanelView {
    pub fn new(
        cx: &Cx,
        host: &Element,
        id: &str,
        title: MessageLike,
        modifier: &str,
        opts: SidePanelOptions,
        headerless: bool,
    ) -> Rc<PanelView> {
        let model = SidePanelModel::new(opts);
        let el = cx.el("div", &format!("vela-panel {modifier}"));
        set_hidden(&el, true);
        set_data(&el, "panel-id", Some(id));
        if model.overlay() {
            set_data(&el, "overlay", Some("1"));
        }
        set_style(&el, "--vela-panel-w", &format!("{}px", model.width()));
        let header = cx.el("div", "vela-panel-header");
        let heading = cx.text("span", "vela-panel-title", &cx.t(&title));
        let slot = cx.el("div", "vela-panel-header-slot");
        let close = cx.button("vela-panel-close", &cx.t(&close_label()));
        let _ = close.append_child(&icon_el(&cx.doc, "close"));
        let pin = opts.overlay.then(|| cx.button("vela-panel-pin", ""));
        let maxb = opts.maximizable.then(|| cx.button("vela-panel-max", ""));
        let _ = header.append_child(&heading);
        let _ = header.append_child(&slot);
        if let Some(p) = &pin {
            let _ = header.append_child(p);
        }
        if let Some(m) = &maxb {
            let _ = header.append_child(m);
        }
        let _ = header.append_child(&close);
        if headerless {
            set_hidden(&header, true);
        }
        let body = cx.el("div", "vela-panel-body");
        let _ = el.append_child(&header);
        let _ = el.append_child(&body);
        let resizer = opts.resizable.then(|| {
            let r = cx.el("div", "vela-panel-resizer");
            crate::dom::attr(&r, "role", "separator");
            crate::dom::attr(&r, "aria-orientation", "vertical");
            let _ = el.append_child(&r);
            r
        });
        let _ = host.append_child(&el);
        let view = Rc::new(PanelView {
            cx: cx.clone(),
            id: id.to_string(),
            el,
            heading,
            slot,
            close,
            pin,
            maxb,
            body,
            title: RefCell::new(title),
            opts,
            dock: RefCell::new(Weak::new()),
            bag: Bag::new(),
            drag: Cell::new(None),
            on_close: RefCell::default(),
            open: Cell::new(false),
            js_header: RefCell::default(),
            js_guards: RefCell::default(),
        });
        view.refresh_pin();
        view.refresh_max();
        view.wire(resizer);
        view
    }

    fn wire(self: &Rc<Self>, resizer: Option<Element>) {
        {
            let w = Rc::downgrade(self);
            self.bag.on(&self.close, "click", move |_| {
                if let Some(v) = w.upgrade() {
                    let cb = v.on_close.borrow().clone();
                    if let Some(cb) = cb {
                        cb();
                    }
                }
            });
        }
        if let Some(pin) = &self.pin {
            let w = Rc::downgrade(self);
            self.bag.on(pin, "click", move |_| {
                let Some(v) = w.upgrade() else { return };
                let Some(dock) = v.dock.borrow().upgrade() else {
                    return;
                };
                let now = !v.overlay();
                dock.user_set_overlay(&v.id, now);
            });
        }
        if let Some(m) = &self.maxb {
            let w = Rc::downgrade(self);
            self.bag.on(m, "click", move |_| {
                if let Some(v) = w.upgrade() {
                    let now = !v.maximized();
                    v.set_maximized(now);
                }
            });
        }
        let Some(handle) = resizer else { return };
        {
            let w = Rc::downgrade(self);
            let h = handle.clone();
            self.bag.on(&handle, "pointerdown", move |ev: JsValue| {
                let Some(v) = w.upgrade() else { return };
                if ev_f64(&ev, "button") != 0.0 {
                    return;
                }
                let Some(dock) = v.dock.borrow().upgrade() else {
                    return;
                };
                let Some(drag) = dock.begin_resize(&v.id, ev_f64(&ev, "clientX")) else {
                    return;
                };
                v.drag.set(Some(drag));
                set_data(&h, "dragging", Some("1"));
                let _ = h.set_pointer_capture(ev_f64(&ev, "pointerId") as i32);
                prevent_default(&ev);
            });
        }
        {
            let w = Rc::downgrade(self);
            self.bag.on(&handle, "pointermove", move |ev: JsValue| {
                let Some(v) = w.upgrade() else { return };
                let Some(drag) = v.drag.get() else { return };
                let Some(dock) = v.dock.borrow().upgrade() else {
                    return;
                };
                if dock.resize_to(&v.id, &drag, ev_f64(&ev, "clientX")) {
                    v.apply_width();
                }
                prevent_default(&ev);
            });
        }
        for name in ["pointerup", "pointercancel"] {
            let w = Rc::downgrade(self);
            let h = handle.clone();
            self.bag.on(&handle, name, move |ev: JsValue| {
                let Some(v) = w.upgrade() else { return };
                let Some(drag) = v.drag.take() else { return };
                set_data(&h, "dragging", None);
                let _ = h.release_pointer_capture(ev_f64(&ev, "pointerId") as i32);
                let dock = v.dock.borrow().upgrade();
                if let Some(dock) = dock {
                    dock.end_resize(&v.id, &drag);
                }
            });
        }
        {
            let w = Rc::downgrade(self);
            self.bag.on(&handle, "dblclick", move |_| {
                let Some(v) = w.upgrade() else { return };
                let dock = v.dock.borrow().upgrade();
                if let Some(dock) = dock {
                    dock.reset_width(&v.id);
                    v.apply_width();
                }
            });
        }
    }

    /// Read this panel's state from the dock's model (the declared options before the view
    /// is docked).
    fn read<R>(&self, f: impl Fn(&SidePanelModel) -> R) -> R {
        let dock = self.dock.borrow().upgrade();
        if let Some(d) = dock
            && let Some(r) = d.read_panel(&self.id, &f)
        {
            return r;
        }
        f(&SidePanelModel::new(self.opts))
    }

    pub fn width(&self) -> f64 {
        self.read(SidePanelModel::width)
    }

    pub fn overlay(&self) -> bool {
        self.read(SidePanelModel::overlay)
    }

    pub fn maximized(&self) -> bool {
        self.read(SidePanelModel::maximized)
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    fn apply_width(&self) {
        set_style(&self.el, "--vela-panel-w", &format!("{}px", self.width()));
    }

    /// Paint everything the dock's model decides: width, placement, maximize.
    pub fn refresh_from_model(&self) {
        self.apply_width();
        let overlay = self.overlay();
        set_data(&self.el, "overlay", if overlay { Some("1") } else { None });
        self.refresh_pin();
        let maximized = self.maximized();
        set_data(
            &self.el,
            "maximized",
            if maximized { Some("1") } else { None },
        );
        self.refresh_max();
    }

    /// Maximize or restore through the dock's model; a panel not declared `maximizable`
    /// ignores it.
    pub fn set_maximized(&self, maximized: bool) {
        let dock = self.dock.borrow().upgrade();
        if let Some(d) = dock {
            d.set_maximized(&self.id, maximized);
        }
    }

    pub fn set_title(&self, title: MessageLike) {
        self.heading.set_text_content(Some(&self.cx.t(&title)));
        *self.title.borrow_mut() = title;
    }

    pub fn content(&self) -> &Element {
        &self.body
    }

    /// Show / hide as the dock's toggle decided.
    pub fn set_open(&self, open: bool) {
        self.open.set(open);
        set_hidden(&self.el, !open);
    }

    fn refresh_pin(&self) {
        let Some(pin) = &self.pin else { return };
        let pinned = !self.overlay();
        super::kit::clear(pin);
        let _ = pin.append_child(&icon_el(
            &self.cx.doc,
            if pinned { "pin-filled" } else { "pin" },
        ));
        set_data(pin, "on", Some(if pinned { "1" } else { "0" }));
        let label = self.cx.t(&self.read(SidePanelModel::pin_label));
        set_title(pin, &label);
        crate::dom::attr(pin, "aria-label", &label);
        crate::dom::attr(pin, "aria-pressed", if pinned { "true" } else { "false" });
    }

    fn refresh_max(&self) {
        let Some(b) = &self.maxb else { return };
        let (icon, label, on) = self.read(|m| {
            (
                m.maximize_button_icon(),
                self.cx.t(&m.maximize_button_label()),
                m.maximized(),
            )
        });
        super::kit::clear(b);
        let _ = b.append_child(&icon_el(&self.cx.doc, icon));
        set_title(b, &label);
        crate::dom::attr(b, "aria-label", &label);
        crate::dom::attr(b, "aria-pressed", if on { "true" } else { "false" });
    }

    /// Re-read the header in the current language.
    pub fn relocalize(&self) {
        self.heading
            .set_text_content(Some(&self.cx.t(&self.title.borrow())));
        let label = self.cx.t(&close_label());
        set_title(&self.close, &label);
        crate::dom::attr(&self.close, "aria-label", &label);
        self.refresh_pin();
        self.refresh_max();
    }

    pub fn destroy(&self) {
        self.bag.clear();
        // A plugin may still hold copies of the header's callbacks: they turn into no-ops
        // before the closures behind them are freed (a call into a freed closure throws).
        for g in self.js_guards.borrow_mut().drain(..) {
            super::js::vela_ss_kill(&g);
        }
        self.js_header.borrow_mut().clear();
        self.el.remove();
    }
}

// ── the dock ──

/// What a panel needs to be docked.
pub struct PanelSpec {
    pub id: &'static str,
    pub title: MessageLike,
    pub icon: &'static str,
    pub order: f64,
    pub modifier: &'static str,
    pub opts: SidePanelOptions,
}

/// A callback a panel view reports a user's change through (width, placement).
/// A built-in panel's rebind hook.
pub type ChartHook = Rc<dyn Fn(&str, &Vela)>;
/// A built-in panel's open hook.
pub type OpenHook = Rc<dyn Fn()>;
/// Builds the context handed to contributions, afresh at every call.
pub type CtxFactory = Rc<dyn Fn() -> Rc<dyn vela_core::widget::contributions::WidgetContext>>;

type Listeners = RefCell<Vec<(u64, Rc<dyn Fn()>)>>;
type ActiveHook = Rc<dyn Fn(&str, bool)>;
type ButtonsHook = Rc<dyn Fn(&[SidePanelButton])>;

/// The dock. Cloning the `Rc` shares it.
pub struct Dock {
    cx: Cx,
    me: Weak<Dock>,
    model: RefCell<PanelDockModel>,
    views: RefCell<Vec<Rc<PanelView>>>,
    handles: RefCell<Vec<(String, Rc<dyn SidePanelHandle>)>>,
    built: RefCell<Vec<(String, Rc<SidePanelDescriptor>)>>,
    contributions: Option<Contributions>,
    listeners: Listeners,
    next_listener: Cell<u64>,
    on_active: RefCell<Vec<ActiveHook>>,
    on_buttons: RefCell<Vec<ButtonsHook>>,
    chart: RefCell<Option<Vela>>,
    chart_hooks: RefCell<Vec<(String, ChartHook)>>,
    open_hooks: RefCell<Vec<(String, OpenHook)>>,
    context: RefCell<Option<CtxFactory>>,
    persist: Cell<bool>,
    destroyed: Cell<bool>,
}

impl Dock {
    pub fn new(cx: &Cx, contributions: Option<Contributions>) -> Rc<Dock> {
        Rc::new_cyclic(|me| Dock {
            cx: cx.clone(),
            me: me.clone(),
            model: RefCell::new(PanelDockModel::new()),
            views: RefCell::default(),
            handles: RefCell::default(),
            built: RefCell::default(),
            contributions,
            listeners: RefCell::default(),
            next_listener: Cell::new(1),
            on_active: RefCell::default(),
            on_buttons: RefCell::default(),
            chart: RefCell::new(None),
            chart_hooks: RefCell::default(),
            open_hooks: RefCell::default(),
            context: RefCell::new(None),
            persist: Cell::new(true),
            destroyed: Cell::new(false),
        })
    }

    pub fn set_context_factory(&self, f: CtxFactory) {
        *self.context.borrow_mut() = Some(f);
    }

    /// A chrome that shows the toggles: told the button group and each pressed state.
    pub fn attach_chrome(&self, buttons: ButtonsHook, active: ActiveHook) {
        self.on_buttons.borrow_mut().push(buttons);
        self.on_active.borrow_mut().push(active);
    }

    /// The host element of a panel: the main row, or the column region.
    fn host_for(&self, placement: PanelPlacement) -> Element {
        match placement {
            PanelPlacement::Column => self
                .cx
                .root
                .region(crate::workspace_ui::Region::Column)
                .clone(),
            PanelPlacement::Chart => self
                .cx
                .root
                .region(crate::workspace_ui::Region::Main)
                .clone(),
        }
    }

    /// Dock a panel the shell owns: returns its view (the caller fills `view.body`).
    pub fn add_built_in(&self, spec: PanelSpec) -> Rc<PanelView> {
        let host = self.host_for(PanelPlacement::Chart);
        let view = PanelView::new(
            &self.cx,
            &host,
            spec.id,
            spec.title.clone(),
            spec.modifier,
            spec.opts,
            false,
        );
        self.dock_view(
            view.clone(),
            DockEntry::built_in(spec.id, spec.title, spec.icon, spec.order, spec.opts),
        );
        view
    }

    fn dock_view(&self, view: Rc<PanelView>, entry: DockEntry) {
        let id = entry.id.clone();
        self.views.borrow_mut().push(view.clone());
        // The user's close.
        let w = self.me.clone();
        let wid = id.clone();
        *view.on_close.borrow_mut() = Some(Rc::new({
            let w = w.clone();
            let id = wid.clone();
            move || {
                if let Some(d) = w.upgrade() {
                    d.toggle(&id, Some(false));
                }
            }
        }));
        *view.dock.borrow_mut() = self.me.clone();
        let fx = self.model.borrow_mut().add(entry);
        self.sync_view(&id);
        self.apply(fx);
    }

    /// Paint a view from the dock's model (stored width and placement after `add` /
    /// `apply_state`).
    fn sync_view(&self, id: &str) {
        if let Some(view) = self.view(id) {
            view.refresh_from_model();
        }
    }

    /// Run `f` over panel `id`'s model.
    pub(super) fn read_panel<R>(&self, id: &str, f: &impl Fn(&SidePanelModel) -> R) -> Option<R> {
        self.model.borrow().panel(id).map(f)
    }

    pub(super) fn begin_resize(&self, id: &str, client_x: f64) -> Option<ResizeDrag> {
        self.model.borrow().begin_resize(id, client_x)
    }

    pub(super) fn resize_to(&self, id: &str, drag: &ResizeDrag, client_x: f64) -> bool {
        self.model.borrow_mut().resize_to(id, drag, client_x)
    }

    pub(super) fn end_resize(&self, id: &str, drag: &ResizeDrag) {
        let fx = self.model.borrow_mut().end_resize(id, drag);
        self.apply(fx);
    }

    pub(super) fn reset_width(&self, id: &str) {
        let fx = self.model.borrow_mut().reset_width(id);
        self.apply(fx);
    }

    /// The user pinned or floated a panel with its header pin.
    pub(super) fn user_set_overlay(&self, id: &str, overlay: bool) {
        let fx = self.model.borrow_mut().set_overlay(id, overlay);
        if !fx.is_empty() {
            self.sync_view(id);
        }
        self.apply(fx);
    }

    pub fn view(&self, id: &str) -> Option<Rc<PanelView>> {
        self.views.borrow().iter().find(|v| v.id == id).cloned()
    }

    /// (Re)build the CONTRIBUTED panels from the registry: those gone from it are dropped, a
    /// replaced descriptor is rebuilt, an unchanged one stays mounted.
    pub fn refresh(&self) {
        let Some(contrib) = self.contributions.clone() else {
            self.publish();
            return;
        };
        let open_before = self.open_id();
        let registered = contrib.side_panels();
        let stale: Vec<String> = self
            .built
            .borrow()
            .iter()
            .filter(|(_, d)| !registered.iter().any(|r| Rc::ptr_eq(r, d)))
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            self.drop_contributed(&id);
        }
        for desc in registered {
            if self
                .built
                .borrow()
                .iter()
                .any(|(_, d)| Rc::ptr_eq(d, &desc))
            {
                continue;
            }
            // A replaced descriptor under the same id: the old entry goes first.
            if self.view(&desc.id).is_some() {
                self.drop_contributed(&desc.id);
            }
            let host = self.host_for(desc.placement);
            let opts = SidePanelOptions {
                width: desc.width,
                resizable: desc.resizable,
                min_width: desc.min_width,
                max_width: desc.max_width,
                overlay: desc.overlay,
                maximizable: desc.maximizable,
                headerless: desc.header == PanelHeaderMode::None,
            };
            let view = PanelView::new(
                &self.cx,
                &host,
                &desc.id,
                desc.title.clone(),
                &format!("vela-panel-{}", desc.id),
                opts,
                desc.header == PanelHeaderMode::None,
            );
            install_js_header(&view, self.me.clone());
            let header = ViewHeader {
                view: view.clone(),
                dock: self.me.clone(),
            };
            let ctx = self.context.borrow().clone();
            if let Some(ctx) = ctx {
                let ctx = ctx();
                let body: ElementHandle = Rc::new(view.body.clone()) as Rc<dyn Any>;
                let handle = (desc.mount)(&*ctx, &body, &header);
                if let Some(h) = handle {
                    let h: Rc<dyn SidePanelHandle> = Rc::from(h);
                    if let Some(chart) = self.chart.borrow().clone() {
                        h.on_chart(&chart);
                    }
                    self.handles.borrow_mut().push((desc.id.clone(), h));
                }
            }
            self.built
                .borrow_mut()
                .push((desc.id.clone(), desc.clone()));
            self.dock_view(
                view,
                DockEntry::contributed(&desc.id, desc.title.clone(), &desc.icon, desc.order, opts),
            );
        }
        if open_before.is_some() && self.open_id().is_none() {
            self.toggle(open_before.as_deref().unwrap_or(""), Some(true));
        }
        self.publish();
    }

    fn drop_contributed(&self, id: &str) {
        let handle = {
            let mut hs = self.handles.borrow_mut();
            hs.iter().position(|(i, _)| i == id).map(|p| hs.remove(p).1)
        };
        if let Some(h) = handle {
            h.destroy();
        }
        self.built.borrow_mut().retain(|(i, _)| i != id);
        let view = {
            let mut vs = self.views.borrow_mut();
            vs.iter().position(|v| v.id == id).map(|p| vs.remove(p))
        };
        let fx = self.model.borrow_mut().drop_entry(id);
        if let Some(v) = view {
            v.destroy();
        }
        self.apply(fx);
    }

    /// Bind (or rebind) every docked panel to a chart instance.
    pub fn on_chart(&self, cell: &str, chart: &Vela) {
        *self.chart.borrow_mut() = Some(chart.clone());
        let hooks: Vec<_> = self
            .chart_hooks
            .borrow()
            .iter()
            .map(|(_, f)| f.clone())
            .collect();
        for h in hooks {
            h(cell, chart);
        }
        let handles: Vec<_> = self
            .handles
            .borrow()
            .iter()
            .map(|(_, h)| h.clone())
            .collect();
        for h in handles {
            h.on_chart(chart);
        }
    }

    /// A built-in panel's hook, run each time it opens.
    pub fn on_open_hook(&self, id: &str, f: OpenHook) {
        self.open_hooks.borrow_mut().push((id.to_string(), f));
    }

    /// A built-in panel's rebind hook.
    pub fn on_chart_hook(&self, id: &str, f: ChartHook) {
        self.chart_hooks.borrow_mut().push((id.to_string(), f));
    }

    /// Open / close one panel by id (`None` flips it). Unknown ids are ignored.
    pub fn toggle(&self, id: &str, open: Option<bool>) {
        let fx = self.model.borrow_mut().toggle(id, open);
        self.apply(fx);
    }

    pub fn open_id(&self) -> Option<String> {
        self.model.borrow().open_id().map(str::to_string)
    }

    pub fn is_open(&self, id: &str) -> bool {
        self.view(id).is_some_and(|v| v.is_open())
    }

    pub fn list(&self) -> Vec<SidePanelButton> {
        let tr = self.cx.tr();
        self.model.borrow().list(tr.as_deref())
    }

    pub fn infos(&self) -> Vec<DockedPanelInfo> {
        let tr = self.cx.tr();
        let model = self.model.borrow();
        model
            .entries()
            .iter()
            .map(|e| DockedPanelInfo {
                id: e.id.clone(),
                title: vela_core::i18n::text_of(tr.as_deref(), &e.title, &[]),
                icon: e.icon.clone(),
                built_in: !e.contributed,
            })
            .collect()
    }

    pub fn is_maximized(&self, id: &str) -> bool {
        self.model.borrow().is_maximized(id)
    }

    pub fn set_maximized(&self, id: &str, on: bool) {
        let fx = self.model.borrow_mut().set_maximized(id, on);
        if !fx.is_empty() {
            self.sync_view(id);
        }
        self.apply(fx);
    }

    pub fn on_did_change(&self, cb: Rc<dyn Fn()>) -> Box<dyn FnOnce()> {
        let id = self.next_listener.get();
        self.next_listener.set(id + 1);
        self.listeners.borrow_mut().push((id, cb));
        let me = self.me.clone();
        Box::new(move || {
            if let Some(d) = me.upgrade() {
                d.listeners.borrow_mut().retain(|(i, _)| *i != id);
            }
        })
    }

    /// Re-read every panel's header and the toggles in the current language.
    pub fn relocalize(&self) {
        for v in self.views.borrow().iter() {
            v.relocalize();
        }
        self.publish();
    }

    /// The dock's persistable state.
    pub fn get_state(&self) -> Option<vela_core::state::PanelsState> {
        self.model.borrow().get_state()
    }

    /// Restore a persisted state.
    pub fn apply_state(&self, state: Option<&vela_core::state::PanelsState>) {
        let was = self.persist.replace(false);
        let fx = self.model.borrow_mut().apply_state(state);
        let ids: Vec<String> = self.views.borrow().iter().map(|v| v.id.clone()).collect();
        for id in ids {
            self.sync_view(&id);
        }
        self.apply(fx);
        self.persist.set(was);
    }

    /// Push the current toggle group (pressed states included) to every chrome.
    pub fn publish(&self) {
        let tr = self.cx.tr();
        let (buttons, states) = self.model.borrow().publish(tr.as_deref());
        for f in self.on_buttons.borrow().clone() {
            f(&buttons);
        }
        let hooks = self.on_active.borrow().clone();
        for (id, open) in states {
            for f in &hooks {
                f(&id, open);
            }
        }
    }

    fn notify(&self) {
        let list: Vec<_> = self
            .listeners
            .borrow()
            .iter()
            .map(|(_, f)| f.clone())
            .collect();
        for f in list {
            f();
        }
    }

    fn apply(&self, effects: Vec<DockEffect>) {
        if self.destroyed.get() {
            return;
        }
        for fx in effects {
            match fx {
                DockEffect::Panel { id, outcome } => self.apply_outcome(&id, outcome),
                DockEffect::ChromeActive { id, open } => {
                    for f in self.on_active.borrow().clone() {
                        f(&id, open);
                    }
                }
                DockEffect::HandleOpen { id } => {
                    let h = self
                        .handles
                        .borrow()
                        .iter()
                        .find(|(i, _)| *i == id)
                        .map(|(_, h)| h.clone());
                    if let Some(h) = h {
                        h.on_open();
                    }
                }
                DockEffect::Changed => {
                    if self.persist.get()
                        && let Some(ws) = self.cx.root.workspace()
                    {
                        // The state is read first: the borrow of the model must not be held
                        // while the workspace raises its own events.
                        let state = self.model.borrow().get_state();
                        ws.set_panels_state(state);
                    }
                }
                DockEffect::Notify => self.notify(),
            }
        }
    }

    fn apply_outcome(&self, id: &str, o: ToggleOutcome) {
        let Some(view) = self.view(id) else { return };
        if o.unchanged {
            return;
        }
        if o.show {
            view.set_open(true);
            let hooks: Vec<_> = self
                .open_hooks
                .borrow()
                .iter()
                .filter(|(i, _)| i == id)
                .map(|(_, f)| f.clone())
                .collect();
            for f in hooks {
                f();
            }
        }
        // The exit animation (kit's `holdForExit`) completes at once when no exit CSS runs,
        // and the panel styles define none: a held close is a close.
        if o.hide_now || o.hold_for_exit {
            view.set_open(false);
        }
        if o.hold_for_exit {
            // A closed panel reopens at its own size: the exit is over, the maximize with it.
            let fx = self.model.borrow_mut().exit_finished(id);
            if !fx.is_empty() {
                view.refresh_from_model();
                self.apply(fx);
            }
        }
        if o.maximized_changed {
            view.refresh_from_model();
        }
    }

    pub fn destroy(&self) {
        if self.destroyed.replace(true) {
            return;
        }
        let handles: Vec<_> = self.handles.borrow_mut().drain(..).collect();
        for (_, h) in handles {
            h.destroy();
        }
        for v in self.views.borrow_mut().drain(..) {
            v.destroy();
        }
        self.model.borrow_mut().destroy();
        self.listeners.borrow_mut().clear();
        self.on_active.borrow_mut().clear();
        self.on_buttons.borrow_mut().clear();
        self.chart_hooks.borrow_mut().clear();
        self.open_hooks.borrow_mut().clear();
        *self.chart.borrow_mut() = None;
        *self.context.borrow_mut() = None;
    }
}

/// `ctx.panels` over the dock.
pub struct DockView(pub Weak<Dock>);

impl PanelDockView for DockView {
    fn list(&self) -> Vec<DockedPanelInfo> {
        self.0.upgrade().map(|d| d.infos()).unwrap_or_default()
    }
    fn open_id(&self) -> Option<String> {
        self.0.upgrade().and_then(|d| d.open_id())
    }
    fn is_maximized(&self, id: &str) -> bool {
        self.0.upgrade().is_some_and(|d| d.is_maximized(id))
    }
    fn on_did_change(&self, cb: Box<dyn Fn()>) -> vela_core::runtime::Unsubscribe {
        match self.0.upgrade() {
            Some(d) => d.on_did_change(Rc::from(cb)),
            None => Box::new(|| {}),
        }
    }
}

struct ViewHeader {
    view: Rc<PanelView>,
    dock: Weak<Dock>,
}

impl SidePanelHeader for ViewHeader {
    fn slot(&self) -> ElementHandle {
        Rc::new(self.view.slot.clone()) as Rc<dyn Any>
    }
    fn set_title(&self, title: MessageLike) {
        self.view.set_title(title);
    }
    fn maximized(&self) -> bool {
        self.view.maximized()
    }
    fn set_maximized(&self, maximized: bool) {
        self.view.set_maximized(maximized);
    }
    fn close(&self) {
        if let Some(d) = self.dock.upgrade() {
            d.toggle(&self.view.id, Some(false));
        }
    }
}

/// The element an [`ElementHandle`] carries, when it is one.
pub fn element_of(handle: &ElementHandle) -> Option<Element> {
    handle.downcast_ref::<Element>().cloned()
}

/// The JavaScript face of a panel's header, parked on its slot element as `velaHeader`
/// (`{ setTitle, setMaximized, close }`), for the page-level `registerSidePanel`.
fn install_js_header(view: &Rc<PanelView>, dock: Weak<Dock>) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::Closure;
    fn guard<T: ?Sized>(view: &Rc<PanelView>, c: &Closure<T>) -> JsValue {
        let g = super::js::vela_ss_guard(c.as_ref().unchecked_ref());
        view.js_guards.borrow_mut().push(g.clone());
        g.into()
    }
    let o = js_sys::Object::new();
    let set = |name: &str, f: &JsValue| {
        let _ = js_sys::Reflect::set(&o, &JsValue::from_str(name), f);
    };
    let w = Rc::downgrade(view);
    let c = Closure::<dyn Fn(JsValue)>::new(move |t: JsValue| {
        if let Some(v) = w.upgrade() {
            v.set_title(MessageLike::from(t.as_string().unwrap_or_default()));
        }
    });
    set("setTitle", &guard(view, &c));
    view.js_header.borrow_mut().push(Box::new(c));
    let w = Rc::downgrade(view);
    let c = Closure::<dyn Fn(JsValue)>::new(move |on: JsValue| {
        if let Some(v) = w.upgrade() {
            v.set_maximized(on.is_truthy());
        }
    });
    set("setMaximized", &guard(view, &c));
    view.js_header.borrow_mut().push(Box::new(c));
    let id = view.id.clone();
    let c = Closure::<dyn Fn()>::new(move || {
        if let Some(d) = dock.upgrade() {
            d.toggle(&id, Some(false));
        }
    });
    set("close", &guard(view, &c));
    view.js_header.borrow_mut().push(Box::new(c));
    let _ = js_sys::Reflect::set(&view.slot, &JsValue::from_str("velaHeader"), &o);
}
