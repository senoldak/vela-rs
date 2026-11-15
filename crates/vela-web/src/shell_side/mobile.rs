//! The mobile surfaces: the foot bar (`mobile-bar.ts`), the three-dots drawer
//! (`more-drawer.ts`), the drawing-tool drawer (`drawings-drawer.ts`), the price-scale
//! sheet a long press on the price axis opens (`price-scale-drawer.ts`) and the floating drawing
//! pill (`drawing-pill.ts`).
//!
//! They exist only while the root is in the mobile layout mode (`data-layout="mobile"`): a
//! change of the mode mounts or removes them (the bar in [`Region::MobileBar`], the pill in the
//! grid), which is the "replace the desktop parts" half of the original's CSS flips (the CSS
//! flips themselves, the topbar and the desktop bottom bar hidden on mobile, are in this
//! module's stylesheet too).
//!
//! The rules are `vela_core::widget::{mobile_bar, more_drawer, drawings_drawer,
//! price_scale_drawer, drawing_pill}`. The kit's bottom `Drawer` is rebuilt natively here
//! ([`Sheet`]): a dimmed backdrop over the root, a sheet that sits on its bottom edge with a
//! grab handle, a press on the backdrop or Escape dismisses it, `role=dialog` `aria-modal`.
//!
//! Surfaces that belong to other parts of the shell go through [`Intent`]s: the symbol search
//! (the symbol stop), the timeframe drawer (the timeframe stop), the indicator picker (the
//! indicators stop), the alerts.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vela_core::drawings::toolbar::toolbar_source;
use vela_core::events::types::{ChartEvent, ChartEventName};
use vela_core::widget::contributions::{
    Contributions, TopbarItemId, WidgetActionDescriptor, WidgetActionTarget, WidgetContext,
    mobile_placement,
};
use vela_core::widget::drawing_pill::{
    PillGlyph, drawing_pill_state, eraser_label, exit_label, magnet_label, next_eraser_mode,
    next_snap_mode, stay_label,
};
use vela_core::widget::drawings_drawer::{
    DrawerList, DrawerModel, favorite_label, search_placeholder, title_message,
};
use vela_core::widget::layout_picker::{
    LayoutPickerShape, PickerRow, grid_cells, layout_grid_label, paint_layout_grid,
};
use vela_core::widget::mobile_bar::{
    MaximizeStop, MobileBarWiring, MobileStop, bar_actions, mobile_bar_stops, symbol_stop_text,
    timeframe_stop_text,
};
use vela_core::widget::more_drawer::{
    LayoutInputs, MainInputs, MainRow, MoreView, PanelRow, StyleChoice, alert_text, main_view,
    no_alerts_label, pick_style, style_checked, sync_label,
};
use vela_core::widget::price_scale_drawer::{SheetRow, TapAction, sheet_rows, tap_action};
use vela_core::widget::sidebar::sidebar_actions;
use vela_core::widget::topbar::{price_style_icon, price_style_label};
use vela_core::workspace::{Workspace, WorkspaceEventName, layout_glyph, layout_shape};
use wasm_bindgen::{JsCast, JsValue};
use web_sys::Element;

use crate::dom::{attr, prevent_default, stop_propagation};
use crate::workspace_ui::{Mount, Region};

use super::bind::{ChartSubs, chart_on};
use super::dock::Dock;
use super::kit::{
    Bag, Cx, Disposers, clear, contains, ev_key, event_target, focus, icon_el, markup_icon,
    set_data, set_hidden, set_title, set_value,
};
use super::{Intent, Intents, MobileHost};

// ── the bottom sheet ──

/// The kit's `Drawer`: a bottom sheet over the root.
pub struct Sheet {
    cx: Cx,
    me: Weak<Sheet>,
    intents: Rc<Intents>,
    backdrop: RefCell<Option<Element>>,
    pub body: Element,
    bag: Rc<Bag>,
    open: Cell<bool>,
    prev_focus: RefCell<Option<Element>>,
    sheet: Element,
    label: RefCell<String>,
    /// The drawer's title is shown above the body (the kit's `drawer-title`), outside the
    /// scrolling part.
    titled: bool,
}

impl Sheet {
    pub fn new(cx: &Cx, intents: &Rc<Intents>, label: &str) -> Rc<Sheet> {
        Self::build(cx, intents, label, false)
    }

    /// A sheet that also shows its title (the drawings and price-scale drawers).
    pub fn titled(cx: &Cx, intents: &Rc<Intents>, label: &str) -> Rc<Sheet> {
        Self::build(cx, intents, label, true)
    }

    fn build(cx: &Cx, intents: &Rc<Intents>, label: &str, titled: bool) -> Rc<Sheet> {
        let body = cx.el("div", "vela-sheet-body");
        let sheet = cx.el("div", "vela-sheet");
        Rc::new_cyclic(|me| Sheet {
            cx: cx.clone(),
            me: me.clone(),
            intents: intents.clone(),
            backdrop: RefCell::new(None),
            body,
            bag: Bag::new(),
            open: Cell::new(false),
            prev_focus: RefCell::new(None),
            sheet,
            label: RefCell::new(label.to_string()),
            titled,
        })
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    pub fn show(&self) {
        if self.open.replace(true) {
            return;
        }
        let cx = &self.cx;
        *self.prev_focus.borrow_mut() = cx.doc.active_element();
        let backdrop = cx.el("div", "vela-sheet-backdrop");
        let sheet = &self.sheet;
        clear(sheet);
        attr(sheet, "role", "dialog");
        attr(sheet, "aria-modal", "true");
        attr(sheet, "aria-label", &self.label.borrow());
        attr(sheet, "tabindex", "-1");
        let grab = cx.el("div", "vela-sheet-grab");
        let _ = sheet.append_child(&grab);
        if self.titled {
            let title = cx.text("div", "vela-sheet-title", &self.label.borrow());
            let _ = sheet.append_child(&title);
        }
        let _ = sheet.append_child(&self.body);
        let _ = backdrop.append_child(sheet);
        let _ = cx.root.element().append_child(&backdrop);
        let me = self.me.clone();
        let s = sheet.clone();
        self.bag.on(&backdrop, "pointerdown", move |ev: JsValue| {
            let outside = event_target(&ev).is_none_or(|t| !contains(&s, &t.into()));
            if outside && let Some(d) = me.upgrade() {
                d.hide();
            }
        });
        let me = self.me.clone();
        let (trap, doc) = (sheet.clone(), cx.doc.clone());
        self.bag.on(sheet, "keydown", move |ev: JsValue| {
            super::kit::trap_tab(&ev, &trap, &doc);
            if ev_key(&ev) == "Escape" {
                prevent_default(&ev);
                stop_propagation(&ev);
                if let Some(d) = me.upgrade() {
                    d.hide();
                }
            }
        });
        *self.backdrop.borrow_mut() = Some(backdrop);
        cx.root.keymap().push_scope("dialog");
        self.intents.dialog_opened(true);
        focus(sheet);
    }

    pub fn hide(&self) {
        if !self.open.replace(false) {
            return;
        }
        self.bag.retire(&self.cx);
        if let Some(b) = self.backdrop.borrow_mut().take() {
            b.remove();
        }
        self.cx.root.keymap().pop_scope("dialog");
        self.intents.dialog_opened(false);
        if let Some(p) = self.prev_focus.borrow_mut().take() {
            focus(&p);
        }
    }

    pub fn destroy(&self) {
        if self.open.get() {
            self.open.set(false);
            self.cx.root.keymap().pop_scope("dialog");
            self.intents.dialog_opened(false);
        }
        self.bag.clear();
        if let Some(b) = self.backdrop.borrow_mut().take() {
            b.remove();
        }
    }
}

// ── the parts present while the layout is mobile ──

struct Parts {
    /// `None` when the shell was built without a bottom bar (`bottombar: false`: the mobile
    /// bar is the bottom bar of the mobile size class, the same toggle governs both).
    bar: Option<Rc<Bar>>,
    pill: Rc<Pill>,
}

pub struct Mobile {
    cx: Cx,
    me: Weak<Mobile>,
    ws: vela_core::workspace::WeakWorkspace,
    intents: Rc<Intents>,
    dock: Weak<Dock>,
    host: RefCell<Option<Rc<dyn MobileHost>>>,
    parts: RefCell<Option<Parts>>,
    more: RefCell<Option<Rc<MoreDrawer>>>,
    drawings: RefCell<Option<Rc<DrawingsDrawer>>>,
    scale: RefCell<Option<Rc<PriceScaleSheet>>>,
    scale_subs: ChartSubs,
    contributions: Option<Contributions>,
    destroyed: Cell<bool>,
}

impl Mobile {
    pub fn new(
        cx: &Cx,
        ws: &vela_core::workspace::Workspace,
        intents: &Rc<Intents>,
        dock: &Rc<Dock>,
        disposers: &Disposers,
        contributions: Option<Contributions>,
    ) -> Rc<Mobile> {
        let m = Rc::new_cyclic(|me| Mobile {
            cx: cx.clone(),
            me: me.clone(),
            ws: ws.downgrade(),
            intents: intents.clone(),
            dock: Rc::downgrade(dock),
            host: RefCell::new(None),
            parts: RefCell::new(None),
            more: RefCell::new(None),
            drawings: RefCell::new(None),
            scale: RefCell::new(None),
            scale_subs: ChartSubs::default(),
            contributions,
            destroyed: Cell::new(false),
        });
        let me = m.me.clone();
        let off = cx.root.on_layout_mode(move |_| {
            if let Some(m) = me.upgrade() {
                m.sync();
            }
        });
        disposers.push(off);
        // The bar, the pill and the axis sheets follow the workspace.
        for name in [
            WorkspaceEventName::Projection,
            WorkspaceEventName::CellMaximized,
            WorkspaceEventName::LayoutChanged,
            WorkspaceEventName::CellCreated,
            WorkspaceEventName::CellDestroyed,
            WorkspaceEventName::CellActive,
        ] {
            let me = m.me.clone();
            let sub = ws.on(name, move |_| {
                if let Some(m) = me.upgrade() {
                    m.refresh();
                }
            });
            let w = ws.downgrade();
            disposers.push(move || {
                if let Some(ws) = w.upgrade() {
                    ws.off(sub);
                }
            });
        }
        let me = m.me.clone();
        let sub = ws.on(WorkspaceEventName::LocaleChanged, move |_| {
            if let Some(m) = me.upgrade() {
                m.relocalize();
            }
        });
        let w = ws.downgrade();
        disposers.push(move || {
            if let Some(ws) = w.upgrade() {
                ws.off(sub);
            }
        });
        m.sync();
        m
    }

    pub fn set_host(&self, host: Rc<dyn MobileHost>) {
        *self.host.borrow_mut() = Some(host);
    }

    fn ws(&self) -> Option<Workspace> {
        self.ws.upgrade()
    }

    fn is_mobile(&self) -> bool {
        self.cx.root.layout_mode() == vela_core::widget::layout_mode::LayoutMode::Mobile
    }

    /// Mount or remove the parts as the layout mode says.
    fn sync(&self) {
        if self.destroyed.get() {
            return;
        }
        let mobile = self.is_mobile();
        let has = self.parts.borrow().is_some();
        if mobile && !has {
            let bar = (self.cx.root.shell_options().bottombar != Some(false))
                .then(|| Bar::new(&self.cx, self.me.clone()));
            let pill = Pill::new(&self.cx);
            *self.parts.borrow_mut() = Some(Parts { bar, pill });
            self.refresh();
            self.bind_axes();
        } else if !mobile && has {
            self.tear_down_parts();
        }
    }

    fn tear_down_parts(&self) {
        self.scale_subs.clear();
        let taken = self.more.borrow_mut().take();
        if let Some(d) = taken {
            d.destroy();
        }
        let taken = self.drawings.borrow_mut().take();
        if let Some(d) = taken {
            d.destroy();
        }
        let taken = self.scale.borrow_mut().take();
        if let Some(d) = taken {
            d.destroy();
        }
        let taken = self.parts.borrow_mut().take();
        if let Some(p) = taken {
            if let Some(bar) = &p.bar {
                bar.destroy();
            }
            p.pill.destroy();
        }
    }

    /// A long press on a price axis opens the price-scale sheet of its pane.
    fn bind_axes(&self) {
        self.scale_subs.clear();
        let Some(ws) = self.ws() else { return };
        for cell in ws.cells() {
            let Some(chart) = cell.try_chart() else {
                continue;
            };
            let (me, id) = (self.me.clone(), cell.id().to_string());
            let off = chart.renderer().on_axis_long_press(Box::new(
                move |e: &vela_core::ports::chart_renderer::AxisLongPressEvent| {
                    let (Some(m), true) = (me.upgrade(), true) else {
                        return;
                    };
                    let y = e.y;
                    let axis_is_price = format!("{:?}", e.axis).to_lowercase().contains("price");
                    if axis_is_price {
                        m.open_price_scale(&id, y);
                    }
                },
            ));
            self.scale_subs.push(off);
        }
    }

    pub fn open_price_scale(&self, cell: &str, y: f64) {
        if !self.is_mobile() {
            return;
        }
        let sheet = {
            let mut s = self.scale.borrow_mut();
            s.get_or_insert_with(|| PriceScaleSheet::new(&self.cx, &self.intents, self.me.clone()))
                .clone()
        };
        sheet.open(cell, y);
    }

    /// Re-read what the bar and the pill show.
    pub fn refresh(&self) {
        let (bar, pill) = match self.parts.borrow().as_ref() {
            Some(p) => (p.bar.clone(), p.pill.clone()),
            None => return,
        };
        if let Some(bar) = bar {
            bar.sync();
        }
        if let Some(chart) = self.ws().and_then(|w| w.chart()) {
            pill.on_chart(&chart);
        }
        // Axis hooks follow the cells that exist now.
        self.bind_axes();
    }

    fn relocalize(&self) {
        let bar = self.parts.borrow().as_ref().and_then(|p| p.bar.clone());
        if let Some(b) = bar {
            b.sync();
        }
        let pill = self.parts.borrow().as_ref().map(|p| p.pill.clone());
        if let Some(p) = pill {
            p.sync();
        }
        // Drawers rebuild with the new words on their next open.
        let taken = self.more.borrow_mut().take();
        if let Some(d) = taken {
            d.destroy();
        }
        let taken = self.drawings.borrow_mut().take();
        if let Some(d) = taken {
            d.destroy();
        }
        // The price-scale sheet carries its title: rebuilt in the new language.
        let taken = self.scale.borrow_mut().take();
        if let Some(d) = taken {
            d.destroy();
        }
    }

    fn open_more(&self) {
        let d = {
            let mut m = self.more.borrow_mut();
            m.get_or_insert_with(|| MoreDrawer::new(&self.cx, &self.intents, self.me.clone()))
                .clone()
        };
        d.open();
    }

    fn open_drawings(&self) {
        let d = {
            let mut m = self.drawings.borrow_mut();
            m.get_or_insert_with(|| DrawingsDrawer::new(&self.cx, &self.intents, self.me.clone()))
                .clone()
        };
        d.open();
    }

    fn toggle_maximize(&self) {
        let Some(ws) = self.ws() else { return };
        let Some(cell) = ws.active() else { return };
        if ws.maximized_cell().is_some() {
            ws.maximize_cell(None);
        } else if let Some(chart) = cell.try_chart()
            && chart.panes().list().iter().any(|p| p.maximized)
        {
            chart.panes().maximize(None);
        } else {
            ws.maximize_cell(Some(cell.id()));
        }
    }

    fn context(&self) -> Option<Rc<dyn WidgetContext>> {
        // The root's one context, so the sidebar and topbar controls it hands are the live ones.
        let _ = self.ws()?;
        Some(self.cx.root.context() as Rc<dyn WidgetContext>)
    }

    pub fn destroy(&self) {
        if self.destroyed.replace(true) {
            return;
        }
        self.tear_down_parts();
    }
}

// ── the bar ──

pub struct Bar {
    cx: Cx,
    me: Weak<Bar>,
    owner: Weak<Mobile>,
    bag: Rc<Bag>,
    stops: RefCell<Vec<(MobileStop, Element, Mount)>>,
    actions: RefCell<Vec<Element>>,
    max: RefCell<MaximizeStop>,
}

impl Bar {
    fn new(cx: &Cx, owner: Weak<Mobile>) -> Rc<Bar> {
        let bar = Rc::new_cyclic(|me| Bar {
            cx: cx.clone(),
            me: me.clone(),
            owner,
            bag: Bag::new(),
            stops: RefCell::default(),
            actions: RefCell::default(),
            max: RefCell::new(MaximizeStop::default()),
        });
        bar.build();
        bar
    }

    fn build(&self) {
        let cx = &self.cx;
        // The indicators stop exists unless the picker was turned off and no action replaces
        // it; a hidden topbar item hides it again in `sync`.
        let overridden = self
            .owner
            .upgrade()
            .and_then(|m| m.contributions.clone())
            .is_some_and(|c| c.topbar_action_override("indicators").is_some());
        let wiring = MobileBarWiring {
            indicators: cx.root.shell_options().indicator_picker != Some(false) || overridden,
            drawings: self
                .owner
                .upgrade()
                .and_then(|m| m.ws())
                .and_then(|w| w.chart())
                .is_some_and(|c| c.drawings().supported()),
            maximize: true,
        };
        let tr = cx.tr();
        for stop in mobile_bar_stops(wiring) {
            let label = vela_core::i18n::text_of(tr.as_deref(), &stop.label(), &[]);
            let b = cx.el(
                "button",
                &format!(
                    "vela-mb-item vela-mb-{}",
                    match stop {
                        MobileStop::Symbol => "symbol",
                        MobileStop::Timeframe => "tf",
                        MobileStop::Indicators => "indicators",
                        MobileStop::Drawings => "drawings",
                        MobileStop::Maximize => "maximize",
                        MobileStop::More => "more",
                        MobileStop::Settings => "settings",
                    }
                ),
            );
            attr(&b, "type", "button");
            attr(&b, "aria-label", &label);
            set_data(&b, "stop", Some(stop.name()));
            if let Some(icon) = stop.icon() {
                let _ = b.append_child(&icon_el(&cx.doc, icon));
            }
            let me = self.owner.clone();
            self.bag.on(&b, "click", move |_| {
                if let Some(m) = me.upgrade() {
                    match stop {
                        MobileStop::Symbol => m.intents.emit(Intent::SymbolSearch(None)),
                        MobileStop::Timeframe => m.intents.emit(Intent::TimeframeDrawer),
                        MobileStop::Indicators => m.intents.emit(Intent::IndicatorPicker),
                        MobileStop::Drawings => m.open_drawings(),
                        MobileStop::Maximize => m.toggle_maximize(),
                        MobileStop::More => m.open_more(),
                        MobileStop::Settings => {
                            if let Some(chart) = m.ws().and_then(|w| w.chart()) {
                                chart.renderer().open_settings(None);
                            }
                        }
                    }
                }
            });
            let mount = cx.root.mount(Region::MobileBar, &b);
            self.stops.borrow_mut().push((stop, b, mount));
        }
    }

    /// Re-read the symbol, the timeframe, the maximize stop and the contributed actions.
    pub fn sync(&self) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let Some(ws) = owner.ws() else { return };
        let cell = ws.active();
        let tr = self.cx.tr();
        let (symbol, tf) = cell
            .as_ref()
            .map(|c| (c.symbol(), c.timeframe()))
            .unwrap_or_default();
        let multi = ws.cells().len() > 1;
        let pane_max = cell
            .as_ref()
            .and_then(|c| c.try_chart())
            .is_some_and(|c| c.panes().list().iter().any(|p| p.maximized));
        {
            let mut m = self.max.borrow_mut();
            m.set_active(ws.maximized_cell().is_some() || pane_max);
            m.set_visible(multi);
        }
        for (stop, el, _) in self.stops.borrow().iter() {
            match stop {
                MobileStop::Symbol => el.set_text_content(Some(&symbol_stop_text(&symbol))),
                MobileStop::Timeframe => {
                    el.set_text_content(Some(&timeframe_stop_text(&tf, tr.as_deref())))
                }
                MobileStop::Maximize => {
                    let m = self.max.borrow();
                    clear(el);
                    let _ = el.append_child(&icon_el(&self.cx.doc, m.icon()));
                    let label = vela_core::i18n::text_of(tr.as_deref(), &m.label(), &[]);
                    attr(el, "aria-label", &label);
                    set_hidden(el, m.hidden);
                    if m.active {
                        super::kit::add_class(el, "vela-mb-on");
                    } else {
                        super::kit::set_class(el, "vela-mb-item vela-mb-maximize");
                    }
                }
                other => {
                    let label = vela_core::i18n::text_of(tr.as_deref(), &other.label(), &[]);
                    attr(el, "aria-label", &label);
                    // The topbar's `indicators` item hidden (composition or a plugin) takes
                    // its mobile stop with it.
                    if *other == MobileStop::Indicators
                        && let Some(ctx) = owner.context()
                    {
                        set_hidden(
                            el,
                            !ctx.topbar().is_visible(
                                vela_core::widget::contributions::TopbarItemId::Indicators,
                            ),
                        );
                    }
                }
            }
        }
        self.render_actions(&owner);
    }

    /// Contributed topbar actions placed on the bar, as icon-only stops after the
    /// indicators stop.
    fn render_actions(&self, owner: &Rc<Mobile>) {
        for a in self.actions.borrow_mut().drain(..) {
            a.remove();
        }
        let (Some(contrib), Some(ctx)) = (owner.contributions.clone(), owner.context()) else {
            return;
        };
        let tr = self.cx.tr();
        let acts = bar_actions(&contrib, &*ctx, tr.as_deref());
        // The actions sit right after the indicators stop: insert before the next stop.
        let anchor = self
            .stops
            .borrow()
            .iter()
            .skip_while(|(s, _, _)| *s != MobileStop::Indicators)
            .nth(1)
            .map(|(_, el, _)| el.clone());
        for a in acts {
            let b = self.cx.el("button", "vela-mb-item");
            attr(&b, "type", "button");
            set_data(&b, "action-id", Some(&a.id));
            attr(&b, "aria-label", &a.label);
            match &a.icon {
                Some(i) => {
                    let _ = b.append_child(&icon_el(&self.cx.doc, i));
                }
                None => b.set_text_content(Some(&a.label)),
            }
            let (me, a2) = (self.owner.clone(), a.clone());
            self.bag.on(&b, "click", move |_| {
                if let Some(ctx) = me.upgrade().and_then(|m| m.context()) {
                    a2.run(&*ctx);
                }
            });
            let region = self.cx.root.region(Region::MobileBar);
            match &anchor {
                Some(next) => {
                    let _ = region.insert_before(&b, Some(next));
                }
                None => {
                    let _ = region.append_child(&b);
                }
            }
            self.actions.borrow_mut().push(b);
        }
    }

    fn destroy(&self) {
        self.bag.clear();
        for a in self.actions.borrow_mut().drain(..) {
            a.remove();
        }
        self.stops.borrow_mut().clear();
        let _ = &self.me;
    }
}

// ── the drawing pill ──

pub struct Pill {
    cx: Cx,
    me: Weak<Pill>,
    el: Element,
    tool: Element,
    magnet: Element,
    badge: Element,
    stay: Element,
    eraser: Element,
    bag: Rc<Bag>,
    chart: RefCell<Option<vela_core::vela::Vela>>,
    subs: ChartSubs,
    labels: RefCell<Vec<(Element, vela_core::i18n::MessageLike)>>,
}

impl Pill {
    fn new(cx: &Cx) -> Rc<Pill> {
        let el = cx.el("div", "vela-drawpill");
        set_hidden(&el, true);
        let tool = cx.el("span", "vela-drawpill-tool");
        let btn = |icon: &str| {
            let b = cx.el("button", "vela-drawpill-btn");
            attr(&b, "type", "button");
            let _ = b.append_child(&icon_el(&cx.doc, icon));
            b
        };
        let magnet = btn("magnet");
        let badge = cx.el("span", "vela-drawpill-badge");
        attr(&badge, "aria-hidden", "true");
        let _ = badge.append_child(&cx.el("i", ""));
        let _ = badge.append_child(&cx.el("i", ""));
        let _ = magnet.append_child(&badge);
        let stay = btn("pen-lock");
        let eraser = btn("eraser");
        let close = btn("close");
        for e in [&tool, &magnet, &stay, &eraser, &close] {
            let _ = el.append_child(e);
        }
        let _ = cx.root.grid().append_child(&el);
        let pill = Rc::new_cyclic(|me| Pill {
            cx: cx.clone(),
            me: me.clone(),
            el,
            tool,
            magnet: magnet.clone(),
            badge,
            stay: stay.clone(),
            eraser: eraser.clone(),
            bag: Bag::new(),
            chart: RefCell::new(None),
            subs: ChartSubs::default(),
            labels: RefCell::new(vec![
                (magnet.clone(), magnet_label()),
                (stay.clone(), stay_label()),
                (eraser.clone(), eraser_label()),
                (close.clone(), exit_label()),
            ]),
        });
        let on = |b: &Element, f: Rc<dyn Fn(&vela_core::vela::Vela)>| {
            let me = pill.me.clone();
            pill.bag.on(b, "click", move |_| {
                if let Some(p) = me.upgrade()
                    && let Some(chart) = p.chart.borrow().clone()
                {
                    f(&chart);
                }
            });
        };
        on(
            &magnet,
            Rc::new(|c| {
                let d = c.drawings();
                d.set_snap_mode(next_snap_mode(d.get_snap_mode()));
            }),
        );
        on(
            &stay,
            Rc::new(|c| {
                let d = c.drawings();
                d.set_stay_mode(!d.get_stay_mode());
            }),
        );
        on(
            &eraser,
            Rc::new(|c| {
                let d = c.drawings();
                d.set_mode(next_eraser_mode(d.get_mode()));
            }),
        );
        on(
            &close,
            Rc::new(|c| {
                let d = c.drawings();
                d.set_tool(None);
                d.set_mode(None);
            }),
        );
        pill
    }

    /// Rebind to another chart: the previous chart's subscriptions are dropped so only one
    /// chart drives the pill.
    fn on_chart(&self, chart: &vela_core::vela::Vela) {
        self.subs.clear();
        *self.chart.borrow_mut() = Some(chart.clone());
        for ev in [
            ChartEventName::DrawingTool,
            ChartEventName::DrawingSnap,
            ChartEventName::DrawingStay,
            ChartEventName::DrawingMode,
        ] {
            let me = self.me.clone();
            self.subs.push(chart_on(chart, ev, move |_: &ChartEvent| {
                if let Some(p) = me.upgrade() {
                    p.sync();
                }
            }));
        }
        self.sync();
    }

    fn sync(&self) {
        let tr = self.cx.tr();
        for (el, label) in self.labels.borrow().iter() {
            attr(
                el,
                "aria-label",
                &vela_core::i18n::text_of(tr.as_deref(), label, &[]),
            );
        }
        let Some(chart) = self.chart.borrow().clone() else {
            set_hidden(&self.el, true);
            return;
        };
        let d = chart.drawings();
        let state = drawing_pill_state(
            d.supported(),
            d.get_tool().as_deref(),
            d.get_mode(),
            d.get_snap_mode(),
            d.get_stay_mode(),
        );
        set_hidden(&self.el, state.hidden);
        if state.hidden {
            return;
        }
        let markup = match &state.glyph {
            Some(PillGlyph::Tool(key)) => self
                .cx
                .root
                .env()
                .registries
                .drawings
                .get(key)
                .map(|m| m.icon.clone())
                .unwrap_or_default(),
            _ => super::icons::icon("eraser").to_string(),
        };
        self.tool.set_inner_html(&markup);
        set_data(
            &self.magnet,
            "on",
            Some(if state.magnet_on { "1" } else { "" }),
        );
        set_data(&self.badge, "strength", state.magnet_strength);
        set_data(&self.stay, "on", Some(if state.stay_on { "1" } else { "" }));
        set_data(
            &self.eraser,
            "on",
            Some(if state.eraser_on { "1" } else { "" }),
        );
    }

    fn destroy(&self) {
        self.subs.clear();
        self.bag.clear();
        self.el.remove();
    }
}

// ── the three-dots drawer ──

struct MoreDrawer {
    cx: Cx,
    me: Weak<MoreDrawer>,
    owner: Weak<Mobile>,
    sheet: Rc<Sheet>,
    view: Cell<MoreView>,
    bag: Rc<Bag>,
    /// Action closures of the rows of the current render, by index.
    runs: RefCell<Vec<Rc<dyn Fn()>>>,
}

impl MoreDrawer {
    fn new(cx: &Cx, intents: &Rc<Intents>, owner: Weak<Mobile>) -> Rc<MoreDrawer> {
        let sheet = Sheet::new(cx, intents, &cx.m("widget.mobileBar.more", "More"));
        Rc::new_cyclic(|me| MoreDrawer {
            cx: cx.clone(),
            me: me.clone(),
            owner,
            sheet,
            view: Cell::new(MoreView::Main),
            bag: Bag::new(),
            runs: RefCell::default(),
        })
    }

    fn open(&self) {
        self.view.set(MoreView::Main);
        self.render();
        self.sheet.show();
    }

    fn destroy(&self) {
        self.bag.clear();
        self.sheet.destroy();
    }

    fn show(&self, view: MoreView) {
        self.view.set(view);
        self.render();
    }

    #[allow(clippy::too_many_arguments)]
    fn row(
        &self,
        label: &str,
        icon: Option<&str>,
        markup: Option<&str>,
        value: Option<&str>,
        chevron: bool,
        checked: bool,
        disabled: bool,
        title: Option<&str>,
        run: Rc<dyn Fn()>,
    ) -> Element {
        let cx = &self.cx;
        let el = cx.el("div", "vela-md-row");
        attr(&el, "role", "button");
        attr(&el, "tabindex", "0");
        if let Some(t) = title {
            set_title(&el, t);
        }
        if let Some(i) = icon {
            let ic = match markup {
                Some(m) => markup_icon(&cx.doc, m),
                None => icon_el(&cx.doc, i),
            };
            let _ = el.append_child(&ic);
        }
        let _ = el.append_child(&cx.text("span", "vela-md-row-label", label));
        if let Some(v) = value {
            let _ = el.append_child(&cx.text("span", "vela-md-row-value", v));
        }
        if checked {
            set_data(&el, "checked", Some("1"));
        }
        if chevron {
            let _ = el.append_child(&icon_el(&cx.doc, "chevron-right"));
        }
        if disabled {
            set_data(&el, "disabled", Some(""));
            attr(&el, "aria-disabled", "true");
            return el;
        }
        self.bag.on(&el, "click", move |_| run());
        let r2 = el.clone();
        self.bag.on(&el, "keydown", move |ev: JsValue| {
            if matches!(ev_key(&ev).as_str(), "Enter" | " ") {
                prevent_default(&ev);
                let _ = js_sys::Reflect::get(&r2, &JsValue::from_str("click"))
                    .ok()
                    .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
                    .map(|f| f.call0(&r2));
            }
        });
        el
    }

    fn render(&self) {
        self.bag.retire(&self.cx);
        let body = &self.sheet.body;
        clear(body);
        let view = self.view.get();
        let tr = self.cx.tr();
        if let Some(back) = view.back_label() {
            let b = self.cx.el("button", "vela-md-back");
            attr(&b, "type", "button");
            let _ = b.append_child(&icon_el(&self.cx.doc, "chevron-left"));
            let text = self.cx.t(&back);
            let _ = b.append_child(&self.cx.text("span", "", &text));
            let me = self.me.clone();
            self.bag.on(&b, "click", move |_| {
                if let Some(d) = me.upgrade() {
                    d.show(MoreView::Main);
                }
            });
            let _ = body.append_child(&b);
        }
        let _ = &tr;
        match view {
            MoreView::Main => self.render_main(),
            MoreView::Style => self.render_style(),
            MoreView::Layout => self.render_layout(),
            MoreView::Alerts => self.render_alerts(),
        }
    }

    fn hide_after(&self, f: impl Fn() + 'static) -> Rc<dyn Fn()> {
        let me = self.me.clone();
        Rc::new(move || {
            f();
            if let Some(d) = me.upgrade() {
                d.sheet.hide();
            }
        })
    }

    fn styles(&self, ws: &Workspace) -> Vec<(StyleChoice, Option<String>)> {
        let cell = ws.active();
        let unavailable: Vec<String> = cell
            .as_ref()
            .map(|c| c.unavailable_price_styles())
            .unwrap_or_default();
        let registry = self.cx.root.env().registries.chart_types.clone();
        let registry = registry.borrow();
        let tr = self.cx.tr();
        let why = vela_core::i18n::text_of(
            tr.as_deref(),
            &vela_core::chart_types::price_style_unavailable(),
            &[],
        );
        vela_core::render::core::chart_config::price_style_ids(&registry)
            .into_iter()
            .map(|id| {
                let icon = price_style_icon(&registry, &id, &|i| !super::icons::icon(i).is_empty());
                let blocked = unavailable.contains(&id);
                (
                    StyleChoice {
                        label: price_style_label(&registry, &id, tr.as_deref()),
                        icon: icon.as_ref().map(|i| i.id.clone()),
                        unavailable: blocked,
                        title: blocked.then(|| why.clone()),
                        id,
                    },
                    icon.and_then(|i| i.register),
                )
            })
            .collect()
    }

    fn render_main(&self) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let Some(ws) = owner.ws() else { return };
        let cx = &self.cx;
        let tr = cx.tr();
        let cell = ws.active();
        let styles = self.styles(&ws);
        let current_style = cell.as_ref().map(|c| c.price_style()).unwrap_or_default();
        let dock = owner.dock.upgrade();
        let panels: Vec<PanelRow> = dock
            .as_ref()
            .map(|d| {
                d.list()
                    .into_iter()
                    .map(|p| PanelRow {
                        id: p.id,
                        title: p.title,
                        icon: p.icon,
                    })
                    .collect()
            })
            .unwrap_or_default();
        // The contributed actions the menu lists, from one cluster: the topbar's menu-placed
        // ones (`left` are the primary rows right after Layout, `right` the rows at the
        // end) and, in the right cluster, the sidebar's actions that show there.
        let mut runs: Vec<Rc<dyn Fn()>> = Vec::new();
        let mut action_rows = Vec::new();
        let mut primary_rows = Vec::new();
        let ctx = owner.context();
        if let (Some(contrib), Some(ctx)) = (owner.contributions.clone(), ctx.clone()) {
            for (primary, rows) in [(true, &mut primary_rows), (false, &mut action_rows)] {
                for a in drawer_actions(&contrib, &*ctx, primary) {
                    rows.push(vela_core::widget::more_drawer::ActionRow {
                        label: vela_core::i18n::text_of(tr.as_deref(), &a.label, &[]),
                        icon: a.icon.clone(),
                    });
                    let o = owner.me.clone();
                    runs.push(Rc::new(move || {
                        if let Some(ctx) = o.upgrade().and_then(|m| m.context()) {
                            (a.run)(&*ctx);
                        }
                    }));
                }
            }
        }
        let layout = ws.layout();
        let defs = ws.layouts().layouts(tr.as_deref());
        let shape = layout_shape(&layout).map(|s| LayoutPickerShape {
            rows: s.rows,
            cols: s.cols,
        });
        let presets: Vec<PickerRow> = defs
            .iter()
            .filter(|l| layout_shape(l).is_none())
            .map(|l| PickerRow {
                id: l.id.clone(),
                label: l.label.clone(),
                checked: l.id == layout.id,
            })
            .collect();
        // The drawer is the mobile counterpart of the topbar's tools: an item hidden (by the
        // composition or a plugin) loses its drawer row too.
        let has = |id: TopbarItemId| ctx.as_ref().is_none_or(|c| c.topbar().is_visible(id));
        let inputs = MainInputs {
            has_undo: has(TopbarItemId::UndoRedo),
            has_redo: has(TopbarItemId::UndoRedo),
            can_undo: cell.as_ref().is_some_and(|c| c.history().can_undo()),
            can_redo: cell.as_ref().is_some_and(|c| c.history().can_redo()),
            has_screenshot: has(TopbarItemId::Screenshot),
            styles: styles.iter().map(|(s, _)| s.clone()).collect(),
            current_style,
            panels: if has(TopbarItemId::Panels) {
                panels
            } else {
                Vec::new()
            },
            alert_count: has(TopbarItemId::Alerts).then(|| ws.alerts().len()),
            actions: action_rows,
            primary_actions: primary_rows,
            layout: has(TopbarItemId::Layout).then(|| LayoutInputs {
                shape,
                presets,
                glyph: Some(layout_glyph(&layout)),
            }),
        };
        let view = main_view(&inputs, tr.as_deref());
        let body = &self.sheet.body;
        if !view.actions.is_empty() {
            let strip = cx.el("div", "vela-md-actions");
            for a in &view.actions {
                let b = cx.el("button", "vela-md-action");
                attr(&b, "type", "button");
                if !a.enabled {
                    super::kit::set_disabled(&b, true);
                }
                let _ = b.append_child(&icon_el(&cx.doc, a.icon));
                let _ = b.append_child(&cx.text("span", "", &a.label));
                let w = owner.ws.clone();
                let which = a.icon;
                let shot_override = owner
                    .contributions
                    .as_ref()
                    .and_then(|c| c.topbar_action_override("screenshot"));
                let o = owner.me.clone();
                let run = self.hide_after(move || {
                    let Some(ws) = w.upgrade() else { return };
                    match which {
                        "undo" => {
                            if let Some(c) = ws.active() {
                                c.history().undo();
                            }
                        }
                        "redo" => {
                            if let Some(c) = ws.active() {
                                c.history().redo();
                            }
                        }
                        _ if shot_override.is_some() => {
                            // A host's action that replaces the slot takes the row over.
                            if let (Some(a), Some(ctx)) =
                                (&shot_override, o.upgrade().and_then(|m| m.context()))
                            {
                                (a.run)(&*ctx);
                            }
                        }
                        _ => {
                            if let Some(url) = ws.screenshot() {
                                crate::workspace_ui::js::vela_ws_download(
                                    ws_doc(&ws).as_ref(),
                                    &url,
                                    "vela-layout.png",
                                );
                            }
                        }
                    }
                });
                self.bag.on(&b, "click", move |_| run());
                let _ = strip.append_child(&b);
            }
            let _ = body.append_child(&strip);
        }
        let list = cx.el("div", "vela-md-list");
        let mut action_i = 0usize;
        for row in view.rows {
            let el = match row {
                MainRow::ChartType { value } => {
                    let me = self.me.clone();
                    self.row(
                        &cx.t(&vela_core::widget::more_drawer::chart_type_label()),
                        Some("style-line"),
                        None,
                        value.as_deref(),
                        true,
                        false,
                        false,
                        None,
                        Rc::new(move || {
                            if let Some(d) = me.upgrade() {
                                d.show(MoreView::Style);
                            }
                        }),
                    )
                }
                MainRow::Layout { value, glyph } => {
                    let me = self.me.clone();
                    self.row(
                        &cx.t(&vela_core::widget::more_drawer::layout_label()),
                        Some("layout"),
                        glyph.as_deref(),
                        value.as_deref(),
                        true,
                        false,
                        false,
                        None,
                        Rc::new(move || {
                            if let Some(d) = me.upgrade() {
                                d.show(MoreView::Layout);
                            }
                        }),
                    )
                }
                MainRow::PrimaryAction(a) | MainRow::Action(a) => {
                    let run = runs.get(action_i).cloned();
                    action_i += 1;
                    let run = run.unwrap_or_else(|| Rc::new(|| {}));
                    self.row(
                        &a.label,
                        a.icon.as_deref(),
                        None,
                        None,
                        false,
                        false,
                        false,
                        None,
                        self.hide_after(move || run()),
                    )
                }
                MainRow::Panel(p) => {
                    let d = owner.dock.clone();
                    let id = p.id.clone();
                    self.row(
                        &p.title,
                        Some(&p.icon),
                        None,
                        None,
                        false,
                        false,
                        false,
                        None,
                        self.hide_after(move || {
                            if let Some(d) = d.upgrade() {
                                d.toggle(&id, None);
                            }
                        }),
                    )
                }
                MainRow::Alerts { value } => {
                    let me = self.me.clone();
                    self.row(
                        &cx.t(&vela_core::widget::more_drawer::alerts_label()),
                        Some("bell"),
                        None,
                        value.as_deref(),
                        true,
                        false,
                        false,
                        None,
                        Rc::new(move || {
                            if let Some(d) = me.upgrade() {
                                d.show(MoreView::Alerts);
                            }
                        }),
                    )
                }
            };
            let _ = list.append_child(&el);
        }
        let _ = body.append_child(&list);
        *self.runs.borrow_mut() = runs;
    }

    fn render_style(&self) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let Some(ws) = owner.ws() else { return };
        let current = ws.active().map(|c| c.price_style()).unwrap_or_default();
        let list = self.cx.el("div", "vela-md-list");
        for (style, register) in self.styles(&ws) {
            let id = pick_style(&style).map(str::to_string);
            let w = owner.ws.clone();
            let el = self.row(
                &style.label,
                style.icon.as_deref(),
                register.as_deref(),
                None,
                false,
                style_checked(&style, &current),
                style.unavailable,
                style.title.as_deref(),
                self.hide_after(move || {
                    if let (Some(id), Some(ws)) = (id.clone(), w.upgrade())
                        && let Some(c) = ws.active()
                    {
                        c.set_price_style(&id);
                    }
                }),
            );
            let _ = list.append_child(&el);
        }
        let _ = self.sheet.body.append_child(&list);
    }

    fn render_layout(&self) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let Some(ws) = owner.ws() else { return };
        let cx = &self.cx;
        let tr = cx.tr();
        let layout = ws.layout();
        let shape = layout_shape(&layout).map(|s| LayoutPickerShape {
            rows: s.rows,
            cols: s.cols,
        });
        let list = cx.el("div", "vela-md-list");
        let wrap = cx.el("div", "vela-md-gridwrap");
        let grid = cx.el("div", "vela-lp-grid");
        let lit = paint_layout_grid(shape);
        for (i, cell) in grid_cells(tr.as_deref()).into_iter().enumerate() {
            let sq = cx.el("button", "vela-lp-sq");
            attr(&sq, "type", "button");
            attr(&sq, "aria-label", &cell.label);
            set_data(&sq, "r", Some(&cell.row.to_string()));
            set_data(&sq, "c", Some(&cell.col.to_string()));
            if lit.get(i).copied().unwrap_or(false) {
                set_data(&sq, "on", Some("1"));
            }
            let (w, me, rows, cols) = (
                owner.ws.clone(),
                self.me.clone(),
                cell.row + 1,
                cell.col + 1,
            );
            self.bag.on(&sq, "click", move |_| {
                if let Some(ws) = w.upgrade() {
                    let tr = ws.i18n();
                    let def = ws
                        .layouts()
                        .layout_for_grid(rows as f64, cols as f64, Some(&*tr));
                    ws.set_layout_def(def);
                }
                if let Some(d) = me.upgrade() {
                    d.sheet.hide();
                }
            });
            let _ = grid.append_child(&sq);
        }
        let _ = wrap.append_child(&grid);
        let _ = list.append_child(&wrap);
        let defs = ws.layouts().layouts(tr.as_deref());
        for l in defs.iter().filter(|l| layout_shape(l).is_none()) {
            let (w, id) = (owner.ws.clone(), l.id.clone());
            let el = self.row(
                &l.label,
                None,
                None,
                None,
                false,
                l.id == layout.id,
                false,
                None,
                self.hide_after(move || {
                    if let Some(ws) = w.upgrade() {
                        let _ = ws.set_layout(&id);
                    }
                }),
            );
            let _ = list.append_child(&el);
        }
        let syncs = ws.sync_rows();
        if !syncs.is_empty() {
            let _ = list.append_child(&cx.text("div", "vela-md-section", &cx.t(&sync_label())));
            for s in syncs {
                let (w, me) = (owner.ws.clone(), self.me.clone());
                let id = s.id;
                let el = self.row(
                    &s.label,
                    None,
                    None,
                    None,
                    false,
                    s.checked,
                    false,
                    None,
                    Rc::new(move || {
                        if let Some(ws) = w.upgrade()
                            && let Some(kind) = sync_kind(id)
                        {
                            ws.toggle_sync(kind);
                        }
                        if let Some(d) = me.upgrade() {
                            d.render();
                        }
                    }),
                );
                let _ = list.append_child(&el);
            }
        }
        let _ = &layout_grid_label;
        let _ = self.sheet.body.append_child(&list);
    }

    fn render_alerts(&self) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let Some(ws) = owner.ws() else { return };
        let cx = &self.cx;
        let tr = cx.tr();
        let alerts = ws.alerts();
        if alerts.is_empty() {
            let _ = self.sheet.body.append_child(&cx.text(
                "div",
                "vela-md-empty",
                &cx.t(&no_alerts_label()),
            ));
            return;
        }
        let list = cx.el("div", "vela-md-list");
        for a in alerts {
            let value = vela_core::i18n::event_time_text(
                a.time as f64,
                tr.as_deref(),
                &*cx.root.env().intl,
            );
            let el = self.row(
                &alert_text(&a.title, &a.message, tr.as_deref()),
                None,
                None,
                Some(&value),
                false,
                false,
                false,
                None,
                Rc::new(|| {}),
            );
            let _ = list.append_child(&el);
        }
        let _ = self.sheet.body.append_child(&list);
    }
}

/// The contributed actions the three-dots menu lists from one cluster: the topbar's
/// menu-placed ones (`primary`: the left cluster, rows right after Layout; otherwise the
/// right cluster, rows at the end) and, in the right cluster, the sidebar's actions that
/// show there (composition and runtime hiding decide). Built-in ids are slot overrides: they
/// reach the menu through their slot's own row, never as one of these.
fn drawer_actions(
    contributions: &Contributions,
    ctx: &dyn WidgetContext,
    primary: bool,
) -> Vec<Rc<WidgetActionDescriptor>> {
    use vela_core::widget::contributions::ActionAlign;
    use vela_core::widget::topbar_composition::TOPBAR_BUILTIN_IDS;
    let mut out: Vec<Rc<WidgetActionDescriptor>> = contributions
        .widget_actions(WidgetActionTarget::Topbar, Some(ctx))
        .into_iter()
        .filter(|a| {
            mobile_placement(a.align, a.mobile)
                == vela_core::widget::contributions::MobilePlacement::Menu
                && (a.align == Some(ActionAlign::Left)) == primary
                && !TOPBAR_BUILTIN_IDS.contains(&a.id.as_str())
        })
        .collect();
    if !primary {
        out.extend(
            sidebar_actions(contributions, ctx)
                .into_iter()
                .filter(|a| ctx.sidebar().is_visible(&a.id)),
        );
    }
    out
}

fn ws_doc(ws: &Workspace) -> JsValue {
    let _ = ws;
    crate::dom::vela_document()
}

fn sync_kind(id: &str) -> Option<vela_core::state::SyncKind> {
    use vela_core::state::SyncKind;
    Some(match id {
        "symbol" => SyncKind::Symbol,
        "timeframe" => SyncKind::Timeframe,
        "crosshair" => SyncKind::Crosshair,
        "style" => SyncKind::Style,
        _ => return None,
    })
}

// ── the drawings drawer ──

struct DrawingsDrawer {
    cx: Cx,
    me: Weak<DrawingsDrawer>,
    owner: Weak<Mobile>,
    sheet: Rc<Sheet>,
    model: RefCell<DrawerModel>,
    bag: Rc<Bag>,
    list_host: RefCell<Option<Element>>,
    tabs_host: RefCell<Option<Element>>,
    search: RefCell<Option<Element>>,
    swipe: Cell<Option<f64>>,
}

impl DrawingsDrawer {
    fn new(cx: &Cx, intents: &Rc<Intents>, owner: Weak<Mobile>) -> Rc<DrawingsDrawer> {
        let sheet = Sheet::titled(cx, intents, &cx.t(&title_message()));
        Rc::new_cyclic(|me| DrawingsDrawer {
            cx: cx.clone(),
            me: me.clone(),
            owner,
            sheet,
            model: RefCell::new(DrawerModel::new()),
            bag: Bag::new(),
            list_host: RefCell::new(None),
            tabs_host: RefCell::new(None),
            search: RefCell::new(None),
            swipe: Cell::new(None),
        })
    }

    fn chart(&self) -> Option<vela_core::vela::Vela> {
        self.owner.upgrade()?.ws()?.chart()
    }

    fn open(&self) {
        // The toolbar is read at open.
        let registry = self.cx.root.env().registries.drawings.clone();
        let (source, _) = toolbar_source(&registry, None);
        self.model.borrow_mut().open(source);
        self.build();
        self.sheet.show();
    }

    fn build(&self) {
        self.bag.retire(&self.cx);
        let cx = &self.cx;
        let body = &self.sheet.body;
        clear(body);
        let tr = cx.tr();
        let sticky = cx.el("div", "vela-dd-sticky");
        let search = cx.el("input", "vela-dd-search");
        attr(&search, "type", "search");
        attr(
            &search,
            "placeholder",
            &vela_core::i18n::text_of(tr.as_deref(), &search_placeholder(), &[]),
        );
        attr(&search, "aria-label", &cx.t(&title_message()));
        let me = self.me.clone();
        let s2 = search.clone();
        self.bag.on(&search, "input", move |_| {
            if let Some(d) = me.upgrade() {
                d.model.borrow_mut().set_query(&super::kit::get_value(&s2));
                d.render_tabs();
                d.render_list();
            }
        });
        let _ = sticky.append_child(&search);
        let tabs = cx.el("div", "vela-dd-tabs");
        attr(&tabs, "role", "tablist");
        let _ = sticky.append_child(&tabs);
        // Search and tabs stay pinned while the tool list scrolls underneath.
        let _ = body.append_child(&sticky);
        let list = cx.el("div", "vela-dd-list");
        let _ = body.append_child(&list);
        // A horizontal swipe steps through the groups.
        let me = self.me.clone();
        self.bag.on(&list, "pointerdown", move |ev: JsValue| {
            if let Some(d) = me.upgrade() {
                d.swipe.set(Some(super::kit::ev_f64(&ev, "clientX")));
            }
        });
        let me = self.me.clone();
        self.bag.on(&list, "pointerup", move |ev: JsValue| {
            let Some(d) = me.upgrade() else { return };
            let Some(x0) = d.swipe.take() else { return };
            let dx = super::kit::ev_f64(&ev, "clientX") - x0;
            if dx.abs() > 50.0
                && d.model
                    .borrow_mut()
                    .step_group(if dx < 0.0 { 1 } else { -1 })
            {
                d.render_tabs();
                d.render_list();
            }
        });
        *self.search.borrow_mut() = Some(search);
        *self.tabs_host.borrow_mut() = Some(tabs);
        *self.list_host.borrow_mut() = Some(list);
        self.render_tabs();
        self.render_list();
    }

    fn render_tabs(&self) {
        let Some(host) = self.tabs_host.borrow().clone() else {
            return;
        };
        clear(&host);
        let tr = self.cx.tr();
        let (labels, active) = {
            let m = self.model.borrow();
            (m.tab_labels(tr.as_deref()), m.active_group().to_string())
        };
        let searching = !self.model.borrow().query().trim().is_empty();
        for (id, label) in labels {
            let t = self.cx.el("button", "vela-dd-tab");
            attr(&t, "type", "button");
            attr(&t, "role", "tab");
            attr(&t, "aria-label", &label);
            let on = !searching && id == active;
            attr(&t, "aria-selected", if on { "true" } else { "false" });
            set_data(&t, "group", Some(&id));
            t.set_text_content(Some(&label));
            let me = self.me.clone();
            self.bag.on(&t, "click", move |_| {
                let Some(d) = me.upgrade() else { return };
                d.model.borrow_mut().set_group(&id);
                if let Some(s) = d.search.borrow().as_ref() {
                    set_value(s, "");
                }
                d.render_tabs();
                d.render_list();
            });
            let _ = host.append_child(&t);
        }
    }

    fn render_list(&self) {
        let Some(host) = self.list_host.borrow().clone() else {
            return;
        };
        clear(&host);
        let tr = self.cx.tr();
        let chart = self.chart();
        let tool = chart.as_ref().and_then(|c| c.drawings().get_tool());
        let list = self.model.borrow().list(tool.as_deref(), tr.as_deref());
        match list {
            DrawerList::Empty(text) => {
                let _ = host.append_child(&self.cx.text("div", "vela-dd-empty", &text));
            }
            DrawerList::Sections(sections) => {
                for s in sections {
                    if let Some(t) = &s.title {
                        let _ = host.append_child(&self.cx.text("div", "vela-dd-section", t));
                    }
                    for row in s.rows {
                        let el = self.cx.el("div", "vela-dd-row");
                        attr(&el, "role", "button");
                        attr(&el, "tabindex", "0");
                        set_data(&el, "tool", Some(&row.type_key));
                        if row.active {
                            set_data(&el, "active", Some("1"));
                        }
                        let _ = el.append_child(&markup_icon(&self.cx.doc, &row.icon));
                        let _ = el.append_child(&self.cx.text("span", "vela-dd-label", &row.label));
                        // The favorite star.
                        let fav = chart
                            .as_ref()
                            .is_some_and(|c| c.drawings().is_favorite(&row.type_key));
                        let star = self.cx.el("button", "vela-dd-star");
                        attr(&star, "type", "button");
                        let label = favorite_label(&row.label, tr.as_deref());
                        attr(&star, "aria-label", &label);
                        attr(&star, "aria-pressed", if fav { "true" } else { "false" });
                        let _ = star.append_child(&icon_el(
                            &self.cx.doc,
                            if fav { "star-filled" } else { "star" },
                        ));
                        let (me, key) = (self.me.clone(), row.type_key.clone());
                        self.bag.on(&star, "click", move |ev: JsValue| {
                            stop_propagation(&ev);
                            let Some(d) = me.upgrade() else { return };
                            if let Some(c) = d.chart() {
                                let on = c.drawings().is_favorite(&key);
                                c.drawings().set_favorite(&key, !on);
                            }
                            d.render_list();
                        });
                        let _ = el.append_child(&star);
                        let (me, key) = (self.me.clone(), row.type_key.clone());
                        self.bag.on(&el, "click", move |_| {
                            let Some(d) = me.upgrade() else { return };
                            if let Some(c) = d.chart() {
                                c.drawings().set_tool(Some(&key));
                            }
                            d.sheet.hide();
                        });
                        let _ = host.append_child(&el);
                    }
                }
            }
        }
    }

    fn destroy(&self) {
        self.bag.clear();
        self.sheet.destroy();
    }
}

// ── the price-scale sheet ──

struct PriceScaleSheet {
    cx: Cx,
    me: Weak<PriceScaleSheet>,
    owner: Weak<Mobile>,
    sheet: Rc<Sheet>,
    bag: Rc<Bag>,
    state: RefCell<Option<(String, f64)>>,
}

impl PriceScaleSheet {
    fn new(cx: &Cx, intents: &Rc<Intents>, owner: Weak<Mobile>) -> Rc<PriceScaleSheet> {
        let sheet = Sheet::titled(
            cx,
            intents,
            &cx.t(&vela_core::widget::price_scale_drawer::title_message()),
        );
        Rc::new_cyclic(|me| PriceScaleSheet {
            cx: cx.clone(),
            me: me.clone(),
            owner,
            sheet,
            bag: Bag::new(),
            state: RefCell::new(None),
        })
    }

    fn open(&self, cell: &str, y: f64) {
        *self.state.borrow_mut() = Some((cell.to_string(), y));
        self.render();
        self.sheet.show();
    }

    fn chart(&self) -> Option<vela_core::vela::Vela> {
        let (cell, _) = self.state.borrow().clone()?;
        self.owner.upgrade()?.ws()?.cell(&cell)?.try_chart()
    }

    fn pane(&self) -> Option<vela_core::widget::context_menu_model::PaneScaleInfo> {
        let chart = self.chart()?;
        let (_, y) = self.state.borrow().clone()?;
        let panes = super::context_menu::ContextMenus::pane_scales_of(&chart)?;
        vela_core::widget::context_menu_model::pane_scale_at(&panes, y).cloned()
    }

    fn render(&self) {
        use vela_core::widget::context_menu_model::{
            PriceAxisState, price_axis_items, price_only_state, scale_choice_of,
        };
        self.bag.retire(&self.cx);
        let body = &self.sheet.body;
        clear(body);
        let Some(chart) = self.chart() else { return };
        let pane = self.pane();
        let r = chart.renderer();
        let flag = |f: &str| {
            r.get(f)
                .is_some_and(|v| v.as_bool().unwrap_or(v.as_f64().is_some_and(|n| n != 0.0)))
        };
        let (mode, log) = match &pane {
            Some(p) => (Some(p.mode.clone()), p.log),
            None => (
                r.get("scaleMode")
                    .and_then(|v| v.as_str().map(str::to_string)),
                flag("logScale"),
            ),
        };
        let tr = self.cx.tr();
        let items = price_axis_items(
            &PriceAxisState {
                auto: r.get("autoScale") != Some(serde_json::json!(false)),
                price_only: price_only_state(pane.as_ref(), flag("scalePriceOnly")),
                invert: pane
                    .as_ref()
                    .map_or_else(|| flag("invertScale"), |p| p.invert),
                choice: scale_choice_of(mode.as_deref(), log),
                axis_labels: flag("axisLabels"),
                price_label: flag("priceLabel"),
                countdown: flag("countdown"),
                price_line: flag("currentPriceLine"),
            },
            tr.as_deref(),
        );
        let list = self.cx.el("div", "vela-psd-list");
        for row in sheet_rows(&items) {
            match row {
                SheetRow::Section(s) => {
                    let _ = list.append_child(&self.cx.text("div", "vela-psd-section", &s));
                }
                SheetRow::Item {
                    id,
                    label,
                    checked,
                    separator,
                } => {
                    let el = self.cx.el("div", "vela-psd-row");
                    attr(&el, "role", "button");
                    attr(&el, "tabindex", "0");
                    set_data(&el, "item", Some(&id));
                    if checked {
                        set_data(&el, "checked", Some("1"));
                    }
                    if separator {
                        set_data(&el, "sep", Some("1"));
                    }
                    let _ = el.append_child(&self.cx.text("span", "vela-psd-row-label", &label));
                    if checked {
                        let _ = el.append_child(&icon_el(&self.cx.doc, "check"));
                    }
                    let me = self.me.clone();
                    self.bag.on(&el, "click", move |_| {
                        if let Some(s) = me.upgrade() {
                            s.tap(&id);
                        }
                    });
                    let _ = list.append_child(&el);
                }
            }
        }
        let _ = body.append_child(&list);
    }

    fn tap(&self, id: &str) {
        let Some(chart) = self.chart() else { return };
        let pane = self.pane();
        let r = chart.renderer();
        let flag = |f: &str| r.get(f).is_some_and(|v| v.as_bool().unwrap_or(false));
        let invert_now = pane
            .as_ref()
            .map_or_else(|| flag("invertScale"), |p| p.invert);
        let Some(action) = tap_action(id, invert_now, pane.as_ref()) else {
            return;
        };
        let mut close = false;
        match action {
            TapAction::ToggleFeature(f) => {
                let on = flag(&f);
                r.set(&f, serde_json::json!(!on));
            }
            TapAction::ToggleAutoScale => {
                let off = r.get("autoScale") == Some(serde_json::json!(false));
                r.set("autoScale", serde_json::json!(off));
            }
            TapAction::Write(writes) => {
                for (f, v) in writes {
                    r.set(&f, v);
                }
            }
            TapAction::OpenSettings(section) => {
                r.open_settings(section.as_deref());
                close = true;
            }
        }
        if close {
            self.sheet.hide();
        } else {
            // A tap stays in the sheet: it re-reads the pane's scales.
            self.render();
        }
    }

    fn destroy(&self) {
        self.bag.clear();
        self.sheet.destroy();
    }
}
