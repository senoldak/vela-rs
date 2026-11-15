//! The topbar (`topbar.ts`): the symbol button, the timeframe cluster (favorite chips and the
//! caret dropdown), the chart-style dropdown, the layout dropdown, the indicators button,
//! the contributed actions, undo / redo, the alerts bell, the side-panel toggles and the
//! screenshot tool, assembled per the host's composition.
//!
//! What each control says and lists is [`vela_core::widget::topbar`] (labels, the two
//! dropdowns' rows, the chip cluster) and the composition is
//! [`vela_core::widget::topbar_composition`]; this is the element tree, the dropdown
//! wiring, the hairline separators (one device pixel wide, snapped to the pixel grid), the
//! contributed-action buttons and the runtime hiding of items.
//!
//! The bar's content sits in a `display: contents` wrapper mounted into
//! `Region::Topbar`, whose element already is the original's `.vela-widget-topbar` (the
//! flex row): the children lay out as they do in the original.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use vela_core::chart_types::ChartTypeRegistry;
use vela_core::i18n::{MessageArg, MessageLike, text_of};
use vela_core::ui::MenuItemDescriptor;
use vela_core::widget::contributions::{
    ActionAlign, ContextFactory, Contributions, SidePanelButton, WidgetActionDescriptor,
    WidgetActionTarget,
};
use vela_core::widget::layout_picker::{LayoutPickerShape, PickerRow};
use vela_core::widget::timeframe::timeframe_label;
use vela_core::widget::topbar::{
    TimeframeChips, alerts_message, chart_style_message, current_layout_message,
    indicators_message, layout_message, price_style_icon, price_style_label, redo_message,
    screenshot_message, style_button_label, style_menu_items, symbol_button_text, timeframe_chips,
    timeframe_menu_items, timeframe_message, timeframes_message, undo_message,
};
use vela_core::widget::topbar_composition::{
    ResolvedTopbarComposition, TOPBAR_BUILTIN_IDS, TopbarComposition, resolve_topbar_composition,
    topbar_has,
};
use web_sys::Element;

use super::js::{vst_dpr, vst_set_html};
use super::kit::{Env, Menu, MenuButton, MenuOpts, Observer, Tooltip, clear, icon_el, set_flag};
use super::layout_picker::{LayoutPicker, LayoutPickerOpts};
use crate::dom::{Listener, attr, make, on, set_style};
use wasm_bindgen::JsValue;

/// The layout block of the bar (absent in single-chart mode).
pub struct TopbarLayout {
    pub current: String,
    pub shape: Rc<dyn Fn() -> Option<LayoutPickerShape>>,
    pub presets: Rc<dyn Fn() -> Vec<(String, String)>>,
    pub on_select_grid: Rc<dyn Fn(usize, usize)>,
    pub on_select_preset: Rc<dyn Fn(&str)>,
    pub syncs: Rc<dyn Fn() -> Vec<PickerRow>>,
    pub on_toggle_sync: Rc<dyn Fn(&str)>,
    /// The current layout's 16px glyph (`<svg>` markup) for the button.
    pub glyph: Rc<dyn Fn() -> String>,
}

pub struct TopbarOpts {
    pub timeframes: Vec<String>,
    pub timeframe_favorites: Vec<String>,
    pub composition: Option<TopbarComposition>,
    pub on_symbol_click: Rc<dyn Fn()>,
    /// No callback, no button: a host hiding the built-in picker must not show a dead entry.
    pub on_indicators_click: Option<Rc<dyn Fn()>>,
    pub on_undo: Rc<dyn Fn()>,
    pub on_redo: Rc<dyn Fn()>,
    pub on_screenshot: Rc<dyn Fn()>,
    pub on_alerts: Rc<dyn Fn(&Element)>,
    pub on_timeframe: Rc<dyn Fn(&str)>,
    /// Omitted: the dropdown carries no stars and the chips never render.
    pub on_timeframe_favorite: Option<Rc<dyn Fn(&str, bool)>>,
    pub on_price_style: Rc<dyn Fn(&str)>,
    pub layout: Option<TopbarLayout>,
    pub contributions: Option<(Contributions, ContextFactory)>,
}

/// One pinned contributed-action slot.
struct Pin {
    host: Element,
    left: bool,
}

pub struct Topbar {
    env: Env,
    me: Weak<Topbar>,
    opts: TopbarOpts,
    wrap: Element,
    symbol_el: Element,
    chips_host: Element,
    tf_caret: Element,
    style_button: Element,
    layout_button: Option<Element>,
    layout_picker: RefCell<Option<Rc<LayoutPicker>>>,
    layout_id: RefCell<String>,
    tf_menu: RefCell<Option<Rc<MenuButton>>>,
    style_menu: RefCell<Option<Rc<MenuButton>>>,
    actions_host: Element,
    left_actions_host: Element,
    left_actions_sep: Element,
    panels_host: Element,
    undo_btn: Element,
    redo_btn: Element,
    alerts_btn: Element,
    alerts_badge: Element,
    comp: ResolvedTopbarComposition,
    item_els: RefCell<HashMap<String, Vec<Element>>>,
    pinned: RefCell<HashMap<String, Pin>>,
    flowing_overrides: RefCell<HashSet<String>>,
    tooltips: RefCell<Vec<(Rc<Tooltip>, MessageLike)>>,
    action_tips: RefCell<Vec<Rc<Tooltip>>>,
    panel_tips: RefCell<Vec<Rc<Tooltip>>>,
    panel_btns: RefCell<HashMap<String, Element>>,
    listeners: RefCell<Vec<Listener>>,
    dyn_listeners: RefCell<Vec<Listener>>,
    panel_listeners: RefCell<Vec<Listener>>,
    action_listeners: RefCell<Vec<Listener>>,
    chip_listeners: RefCell<Vec<Listener>>,
    labels: RefCell<Vec<Box<dyn Fn()>>>,
    plugin_icons: RefCell<HashMap<String, String>>,
    timeframe: RefCell<String>,
    tf_favs: RefCell<Vec<String>>,
    price_style: RefCell<String>,
    unavailable: RefCell<HashSet<String>>,
    hairline_pending: Cell<bool>,
    destroyed: Cell<bool>,
    indicators_btn: Option<Element>,
    screenshot_btn: Option<Element>,
    /// Re-snaps the hairlines when the bar's region resizes (the original's ResizeObserver).
    observer: RefCell<Option<Observer>>,
    /// The window's `resize` (device pixel ratio changes with page zoom).
    resize_listener: RefCell<Option<Listener>>,
}

fn registry_of(env: &Env) -> Option<Rc<std::cell::RefCell<ChartTypeRegistry>>> {
    Some(env.root()?.env().registries.chart_types.clone())
}

impl Topbar {
    pub fn new(env: &Env, opts: TopbarOpts) -> Rc<Topbar> {
        let doc = &env.doc;
        let comp = resolve_topbar_composition(opts.composition.as_ref());
        let vis = |id: &str| topbar_has(&comp, id);
        let overrides = |id: &str| -> Option<Rc<WidgetActionDescriptor>> {
            opts.contributions.as_ref()?.0.topbar_action_override(id)
        };
        let wrap = make(doc, "div", "vela-topbar-content");
        set_style(&wrap, "display", "contents");
        let symbol_el = make(doc, "button", "vela-widget-symbol");
        attr(&symbol_el, "type", "button");
        let chips_host = make(doc, "span", "vela-widget-tf-chips");
        let tf_caret = make(doc, "button", "vela-widget-tf-caret");
        attr(&tf_caret, "type", "button");
        let tf_group = make(doc, "span", "vela-widget-tf-group");
        tf_group.append_child(&chips_host).ok();
        tf_group.append_child(&tf_caret).ok();
        let style_button = make(doc, "button", "vela-widget-style");
        attr(&style_button, "type", "button");

        let indicators_btn =
            if opts.on_indicators_click.is_some() && overrides("indicators").is_none() {
                let b = make(doc, "button", "vela-widget-indicators");
                attr(&b, "type", "button");
                b.append_child(&icon_el(doc, "indicators")).ok();
                let label = make(doc, "span", "vela-widget-label");
                label.set_text_content(Some(&env.t(&indicators_message())));
                b.append_child(&label).ok();
                Some(b)
            } else {
                None
            };

        let actions_host = make(doc, "span", "vela-widget-actions");
        let left_actions_host = make(doc, "span", "vela-widget-actions-left");
        let panels_host = make(doc, "span", "vela-widget-panels");
        let tool = |cls: &str, icon: &str| -> Element {
            let b = make(doc, "button", &format!("vela-widget-tool {cls}"));
            attr(&b, "type", "button");
            b.append_child(&icon_el(doc, icon)).ok();
            b
        };
        // Hidden tools get a DETACHED bare button: the state setters stay safe no-ops and no
        // tooltip ever binds to chrome the composition removed.
        let (undo_btn, redo_btn) = if vis("undo-redo") {
            (
                tool("vela-widget-undo", "undo"),
                tool("vela-widget-redo", "redo"),
            )
        } else {
            (make(doc, "button", ""), make(doc, "button", ""))
        };
        let screenshot_btn = (vis("screenshot") && overrides("screenshot").is_none())
            .then(|| tool("vela-widget-screenshot", "camera"));
        let alerts_btn = if vis("alerts") {
            tool("vela-widget-alerts", "bell")
        } else {
            make(doc, "button", "")
        };
        set_style(&alerts_btn, "position", "relative");
        let alerts_badge = make(doc, "span", "vela-alerts-badge");
        set_style(&alerts_badge, "display", "none");
        alerts_btn.append_child(&alerts_badge).ok();

        let layout_button = (opts.layout.is_some() && vis("layout")).then(|| {
            let b = make(doc, "button", "vela-widget-style");
            attr(&b, "type", "button");
            b
        });
        let layout_id = opts
            .layout
            .as_ref()
            .map(|l| l.current.clone())
            .unwrap_or_default();
        let left_actions_sep = make(doc, "span", "vela-sep");
        set_flag(&left_actions_sep, "hidden", true);
        set_style(&left_actions_sep, "display", "none");

        let this = Rc::new_cyclic(|me| Topbar {
            env: env.clone(),
            me: me.clone(),
            wrap: wrap.clone(),
            symbol_el: symbol_el.clone(),
            chips_host,
            tf_caret: tf_caret.clone(),
            style_button: style_button.clone(),
            layout_button,
            layout_picker: RefCell::new(None),
            layout_id: RefCell::new(layout_id),
            tf_menu: RefCell::new(None),
            style_menu: RefCell::new(None),
            actions_host,
            left_actions_host,
            left_actions_sep,
            panels_host,
            undo_btn,
            redo_btn,
            alerts_btn,
            alerts_badge,
            comp,
            item_els: RefCell::new(HashMap::new()),
            pinned: RefCell::new(HashMap::new()),
            flowing_overrides: RefCell::new(HashSet::new()),
            tooltips: RefCell::new(Vec::new()),
            action_tips: RefCell::new(Vec::new()),
            panel_tips: RefCell::new(Vec::new()),
            panel_btns: RefCell::new(HashMap::new()),
            listeners: RefCell::new(Vec::new()),
            dyn_listeners: RefCell::new(Vec::new()),
            panel_listeners: RefCell::new(Vec::new()),
            action_listeners: RefCell::new(Vec::new()),
            chip_listeners: RefCell::new(Vec::new()),
            labels: RefCell::new(Vec::new()),
            plugin_icons: RefCell::new(HashMap::new()),
            timeframe: RefCell::new("60".to_string()),
            tf_favs: RefCell::new(opts.timeframe_favorites.clone()),
            price_style: RefCell::new("candles".to_string()),
            unavailable: RefCell::new(HashSet::new()),
            hairline_pending: Cell::new(false),
            destroyed: Cell::new(false),
            indicators_btn: indicators_btn.clone(),
            screenshot_btn: screenshot_btn.clone(),
            observer: RefCell::new(None),
            resize_listener: RefCell::new(None),
            opts,
        });
        this.build(tf_group, indicators_btn, screenshot_btn);
        this
    }

    /// The wrapper to mount into `Region::Topbar`.
    pub fn element(&self) -> &Element {
        &self.wrap
    }

    fn t(&self, m: &MessageLike) -> String {
        self.env.t(m)
    }

    fn build(
        self: &Rc<Self>,
        tf_group: Element,
        indicators_btn: Option<Element>,
        screenshot_btn: Option<Element>,
    ) {
        let doc = self.env.doc.clone();
        let opts = &self.opts;
        // The bar boots empty: the active cell's projection fills it.
        self.symbol_el.set_text_content(Some(""));
        let mut listeners = Vec::new();
        {
            let f = opts.on_symbol_click.clone();
            listeners.push(on(&self.symbol_el, "click", move |_| f()));
        }
        if let (Some(b), Some(f)) = (&indicators_btn, &opts.on_indicators_click) {
            let f = f.clone();
            listeners.push(on(b, "click", move |_| f()));
            let label = b.last_element_child();
            let me = self.me.clone();
            self.labels.borrow_mut().push(Box::new(move || {
                if let (Some(t), Some(l)) = (me.upgrade(), &label) {
                    l.set_text_content(Some(&t.t(&indicators_message())));
                }
            }));
        }
        // tool buttons: label, tooltip, click
        let wire_tool = |btn: &Element,
                         tip: MessageLike,
                         f: Option<Rc<dyn Fn()>>,
                         listeners: &mut Vec<Listener>| {
            if btn.get_attribute("class").unwrap_or_default().is_empty() {
                return; // a detached placeholder: no tooltip, no click
            }
            attr(btn, "aria-label", &self.t(&tip));
            self.tooltips
                .borrow_mut()
                .push((Tooltip::new(&self.env, btn, &self.t(&tip)), tip));
            if let Some(f) = f {
                listeners.push(on(btn, "click", move |_| f()));
            }
        };
        wire_tool(
            &self.undo_btn,
            undo_message(),
            Some(opts.on_undo.clone()),
            &mut listeners,
        );
        wire_tool(
            &self.redo_btn,
            redo_message(),
            Some(opts.on_redo.clone()),
            &mut listeners,
        );
        if let Some(b) = &screenshot_btn {
            wire_tool(
                b,
                screenshot_message(),
                Some(opts.on_screenshot.clone()),
                &mut listeners,
            );
        }
        wire_tool(&self.alerts_btn, alerts_message(), None, &mut listeners);
        {
            let (me, f) = (self.me.clone(), opts.on_alerts.clone());
            let btn = self.alerts_btn.clone();
            let _ = me;
            listeners.push(on(&self.alerts_btn, "click", move |_| f(&btn)));
        }
        self.set_history_state(false, false);

        // ── assemble per the composition ──
        let primaries: HashSet<&str> = ["symbol", "timeframes", "style", "layout", "indicators"]
            .into_iter()
            .collect();
        let builtin: HashSet<&str> = TOPBAR_BUILTIN_IDS.into_iter().collect();
        let sep = || make(&doc, "span", "vela-sep");
        let overridden = |id: &str, left: bool| -> Option<Vec<Element>> {
            let ov = opts.contributions.as_ref()?.0.topbar_action_override(id)?;
            let side_declared = if left {
                opts.composition.as_ref().is_some_and(|c| c.left.is_some())
            } else {
                opts.composition.as_ref().is_some_and(|c| c.right.is_some())
            };
            if !side_declared && ov.order.is_some() {
                self.flowing_overrides.borrow_mut().insert(id.to_string());
                return Some(Vec::new());
            }
            Some(vec![self.pin_slot(id, left)])
        };
        let elements_for =
            |id: &str, left: bool| -> Vec<Element> {
                match id {
                    "symbol" => vec![self.symbol_el.clone()],
                    "timeframes" => vec![tf_group.clone()],
                    "style" => vec![self.style_button.clone()],
                    "layout" => self.layout_button.iter().cloned().collect(),
                    "indicators" => overridden(id, left)
                        .unwrap_or_else(|| indicators_btn.iter().cloned().collect()),
                    "actions" => {
                        if left {
                            vec![
                                self.left_actions_host.clone(),
                                self.left_actions_sep.clone(),
                            ]
                        } else {
                            vec![self.actions_host.clone()]
                        }
                    }
                    "undo-redo" => vec![self.undo_btn.clone(), self.redo_btn.clone()],
                    "alerts" => vec![self.alerts_btn.clone()],
                    "panels" => vec![self.panels_host.clone()],
                    "screenshot" => overridden(id, left)
                        .unwrap_or_else(|| screenshot_btn.iter().cloned().collect()),
                    _ => vec![self.pin_slot(id, left)],
                }
            };
        let side_els = |list: &[String], left: bool| -> Vec<Element> {
            let mut out = Vec::new();
            for (i, id) in list.iter().enumerate() {
                let els = elements_for(id, left);
                if els.is_empty() {
                    continue;
                }
                let mut item = els;
                if primaries.contains(id.as_str()) && i + 1 < list.len() {
                    item.push(sep());
                }
                out.extend(item.iter().cloned());
                if builtin.contains(id.as_str()) && id != "actions" {
                    self.item_els.borrow_mut().insert(id.clone(), item);
                }
            }
            out
        };
        let right = make(&doc, "span", "vela-topbar-right");
        for el in side_els(&self.comp.right, false) {
            right.append_child(&el).ok();
        }
        for el in side_els(&self.comp.left, true) {
            self.wrap.append_child(&el).ok();
        }
        self.wrap.append_child(&right).ok();

        // labels and tooltips of the dropdown triggers
        self.render_style_button();
        self.render_tf_chips();
        if let Some(lb) = &self.layout_button {
            self.render_layout_button();
            let tip = layout_message();
            self.tooltips
                .borrow_mut()
                .push((Tooltip::new(&self.env, lb, &self.t(&tip)), tip));
        }
        for (el, tip) in [
            (&self.tf_caret, timeframe_message()),
            (&self.style_button, chart_style_message()),
        ] {
            self.tooltips
                .borrow_mut()
                .push((Tooltip::new(&self.env, el, &self.t(&tip)), tip));
        }
        *self.listeners.borrow_mut() = listeners;
        self.sync_hairlines_later();
        self.render_actions();
        self.build_menus();
    }

    fn pin_slot(&self, id: &str, left: bool) -> Element {
        let slot = make(&self.env.doc, "span", "vela-widget-action-pin");
        self.pinned.borrow_mut().insert(
            id.to_string(),
            Pin {
                host: slot.clone(),
                left,
            },
        );
        slot
    }

    fn build_menus(self: &Rc<Self>) {
        let env = &self.env;
        // timeframe dropdown
        let (me, me2) = (self.me.clone(), self.me.clone());
        let tf = MenuButton::new(
            env,
            &self.tf_caret,
            move || me.upgrade().map(|t| t.tf_items()).unwrap_or_default(),
            move || {
                let (a, b) = (me2.clone(), me2.clone());
                MenuOpts {
                    min_width: Some("84px".to_string()),
                    icon_badges: false,
                    side_top: false,
                    align_end: false,
                    on_select: Rc::new(move |id| {
                        if let Some(t) = a.upgrade() {
                            (t.opts.on_timeframe)(id);
                        }
                    }),
                    on_favorite: b
                        .upgrade()
                        .and_then(|t| t.opts.on_timeframe_favorite.clone()),
                    on_close: None,
                }
            },
        );
        *self.tf_menu.borrow_mut() = Some(tf);
        // style dropdown
        let (me, me2) = (self.me.clone(), self.me.clone());
        let style = MenuButton::new(
            env,
            &self.style_button,
            move || me.upgrade().map(|t| t.style_items()).unwrap_or_default(),
            move || {
                let a = me2.clone();
                MenuOpts {
                    min_width: None,
                    icon_badges: true,
                    side_top: false,
                    align_end: false,
                    on_select: Rc::new(move |id| {
                        if let Some(t) = a.upgrade() {
                            (t.opts.on_price_style)(id);
                        }
                    }),
                    on_favorite: None,
                    on_close: None,
                }
            },
        );
        *self.style_menu.borrow_mut() = Some(style);
        // layout picker
        if let (Some(lb), Some(layout)) = (&self.layout_button, &self.opts.layout) {
            let me = self.me.clone();
            let presets = layout.presets.clone();
            let picker = LayoutPicker::new(
                env,
                LayoutPickerOpts {
                    trigger: lb.clone(),
                    shape: layout.shape.clone(),
                    presets: Rc::new(move || {
                        let current = me
                            .upgrade()
                            .map(|t| t.layout_id.borrow().clone())
                            .unwrap_or_default();
                        presets()
                            .into_iter()
                            .map(|(id, label)| PickerRow {
                                checked: id == current,
                                id,
                                label,
                            })
                            .collect()
                    }),
                    on_select_grid: layout.on_select_grid.clone(),
                    on_select_preset: layout.on_select_preset.clone(),
                    syncs: layout.syncs.clone(),
                    on_toggle_sync: layout.on_toggle_sync.clone(),
                    on_open_change: None,
                },
            );
            *self.layout_picker.borrow_mut() = Some(picker);
        }
    }

    // ── state in ──

    pub fn set_symbol(&self, symbol: &str) {
        self.symbol_el
            .set_text_content(Some(&symbol_button_text(symbol)));
    }

    pub fn set_timeframe(&self, tf: &str) {
        *self.timeframe.borrow_mut() = tf.to_string();
        self.refresh_tf_menu();
        self.render_tf_chips();
    }

    /// Reflect the favorite-timeframe set: the quick-switch chips and the dropdown stars.
    pub fn set_timeframe_favorites(&self, favs: &[String]) {
        *self.tf_favs.borrow_mut() = favs.to_vec();
        self.render_tf_chips();
        self.refresh_tf_menu();
    }

    fn refresh_tf_menu(&self) {
        if let Some(m) = self.tf_menu.borrow().as_ref() {
            m.refresh();
        }
    }

    /// Reflect the style the chart draws and the styles its data cannot draw.
    pub fn set_price_style(&self, style: &str, unavailable: &[String]) {
        *self.price_style.borrow_mut() = style.to_string();
        *self.unavailable.borrow_mut() = unavailable.iter().cloned().collect();
        self.render_style_button();
        if let Some(m) = self.style_menu.borrow().as_ref() {
            m.refresh();
        }
    }

    /// Reflect the current workspace layout (no-op without the layout dropdown).
    pub fn set_layout(&self, id: &str) {
        if self.layout_button.is_none() {
            return;
        }
        *self.layout_id.borrow_mut() = id.to_string();
        self.render_layout_button();
        if let Some(p) = self.layout_picker.borrow().as_ref() {
            p.refresh();
        }
    }

    pub fn set_indicator_count(&self, _n: usize) {
        // The count badge is intentionally hidden: kept so hosts can keep calling it.
    }

    /// Hide or show a rendered built-in item (and its trailing hairline).
    pub fn set_item_visible(&self, id: &str, visible: bool) {
        if let Some(els) = self.item_els.borrow().get(id) {
            for el in els {
                if visible {
                    let _ = el.remove_attribute("data-topbar-hidden");
                } else {
                    attr(el, "data-topbar-hidden", "1");
                }
            }
        }
    }

    pub fn set_history_state(&self, can_undo: bool, can_redo: bool) {
        vst_set_flag(&self.undo_btn, !can_undo);
        vst_set_flag(&self.redo_btn, !can_redo);
    }

    pub fn set_alert_count(&self, n: usize) {
        self.alerts_badge.set_text_content(Some(&if n > 9 {
            "9+".to_string()
        } else {
            n.to_string()
        }));
        set_style(
            &self.alerts_badge,
            "display",
            if n > 0 { "" } else { "none" },
        );
    }

    /// Replace the side-panel toggle group: one icon button per docked panel.
    pub fn set_panel_buttons(&self, buttons: &[SidePanelButton], on_click: Rc<dyn Fn(&str)>) {
        let old_l: Vec<Listener> = self.panel_listeners.borrow_mut().drain(..).collect();
        let old_t: Vec<Rc<Tooltip>> = self.panel_tips.borrow_mut().drain(..).collect();
        for t in &old_t {
            t.destroy();
        }
        self.panel_btns.borrow_mut().clear();
        clear(&self.panels_host);
        let mut l = Vec::new();
        for b in buttons {
            let el = make(
                &self.env.doc,
                "button",
                &format!("vela-widget-tool vela-widget-panel-{}", b.id),
            );
            attr(&el, "type", "button");
            el.append_child(&icon_el(&self.env.doc, &b.icon)).ok();
            attr(&el, "aria-label", &b.title);
            self.panel_tips
                .borrow_mut()
                .push(Tooltip::new(&self.env, &el, &b.title));
            let (f, id) = (on_click.clone(), b.id.clone());
            l.push(on(&el, "click", move |_| f(&id)));
            self.panels_host.append_child(&el).ok();
            self.panel_btns.borrow_mut().insert(b.id.clone(), el);
        }
        *self.panel_listeners.borrow_mut() = l;
        self.env.retire(Box::new((old_l, old_t)));
    }

    /// Reflect a docked side panel's open state on its button.
    pub fn set_panel_active(&self, id: &str, open: bool) {
        if let Some(btn) = self.panel_btns.borrow().get(id) {
            if open {
                attr(btn, "data-active", "1");
            } else {
                let _ = btn.remove_attribute("data-active");
            }
        }
    }

    /// The elements of the alerts bell (the dropdown anchors on it).
    pub fn alerts_button(&self) -> &Element {
        &self.alerts_btn
    }

    // ── rendering ──

    fn tf_items(&self) -> Vec<MenuItemDescriptor> {
        let stars = self.opts.on_timeframe_favorite.is_some();
        let favs = self.tf_favs.borrow().clone();
        let tr = self.env.translator();
        timeframe_menu_items(
            &self.opts.timeframes,
            &self.timeframe.borrow(),
            stars.then_some(favs.as_slice()),
            tr.as_deref(),
        )
    }

    /// The icon id of a price style: the registry's, else a plugin chart type's own markup.
    fn style_icon_id(&self, id: &str) -> Option<String> {
        let registry = registry_of(&self.env)?;
        let registry = registry.borrow();
        let known = |icon_id: &str| {
            !super::icons::icon(icon_id).is_empty()
                || self.plugin_icons.borrow().contains_key(icon_id)
        };
        let icon = price_style_icon(&registry, id, &known)?;
        if let Some(svg) = icon.register {
            self.plugin_icons.borrow_mut().insert(icon.id.clone(), svg);
        }
        Some(icon.id)
    }

    fn style_icon_el(&self, icon_id: &str) -> Element {
        if super::icons::icon(icon_id).is_empty()
            && let Some(svg) = self.plugin_icons.borrow().get(icon_id)
        {
            let el = make(&self.env.doc, "span", "vela-icon");
            attr(&el, "aria-hidden", "true");
            vst_set_html(&el, svg);
            return el;
        }
        icon_el(&self.env.doc, icon_id)
    }

    fn style_items(&self) -> Vec<MenuItemDescriptor> {
        let Some(registry) = registry_of(&self.env) else {
            return Vec::new();
        };
        let tr = self.env.translator();
        let me = self.me.clone();
        style_menu_items(
            &registry.borrow(),
            &self.price_style.borrow(),
            &self.unavailable.borrow(),
            &move |id| me.upgrade().and_then(|t| t.style_icon_id(id)),
            tr.as_deref(),
        )
    }

    fn render_style_button(&self) {
        clear(&self.style_button);
        let style = self.price_style.borrow().clone();
        let tr = self.env.translator();
        let Some(registry) = registry_of(&self.env) else {
            return;
        };
        let label = price_style_label(&registry.borrow(), &style, tr.as_deref());
        match self.style_icon_id(&style) {
            Some(icon) => {
                self.style_button
                    .append_child(&self.style_icon_el(&icon))
                    .ok();
            }
            None => self.style_button.set_text_content(Some(&label)),
        }
        attr(
            &self.style_button,
            "aria-label",
            &style_button_label(&registry.borrow(), &style, tr.as_deref()),
        );
    }

    fn render_layout_button(&self) {
        let Some(btn) = &self.layout_button else {
            return;
        };
        clear(btn);
        let id = self.layout_id.borrow().clone();
        let glyph = self
            .opts
            .layout
            .as_ref()
            .map(|l| (l.glyph)())
            .filter(|g| !g.is_empty());
        match glyph {
            Some(svg) => {
                let el = make(&self.env.doc, "span", "vela-icon");
                attr(&el, "aria-hidden", "true");
                vst_set_html(&el, &svg);
                btn.append_child(&el).ok();
            }
            None => btn.set_text_content(Some(&id)),
        }
        attr(
            btn,
            "aria-label",
            &self.env.tv(
                &current_layout_message(),
                &[("layout", MessageArg::from(id))],
            ),
        );
    }

    /// Rebuild the quick-switch chips and the caret (current value or favorite set changed).
    fn render_tf_chips(&self) {
        let old: Vec<Listener> = self.chip_listeners.borrow_mut().drain(..).collect();
        clear(&self.chips_host);
        let current = self.timeframe.borrow().clone();
        let tr = self.env.translator();
        let model: TimeframeChips = timeframe_chips(
            &self.tf_favs.borrow(),
            &current,
            self.opts.on_timeframe_favorite.is_some(),
            tr.as_deref(),
        );
        let mut l = Vec::new();
        for chip in &model.chips {
            let b = make(&self.env.doc, "button", "vela-widget-tf");
            attr(&b, "type", "button");
            b.set_text_content(Some(&chip.label));
            if chip.current {
                attr(&b, "data-current", "1");
                attr(&b, "aria-current", "true");
            } else {
                if let Some(a) = &chip.aria_label {
                    attr(&b, "aria-label", a);
                }
                let (f, tf) = (self.opts.on_timeframe.clone(), chip.timeframe.clone());
                l.push(on(&b, "click", move |_| f(&tf)));
            }
            self.chips_host.append_child(&b).ok();
        }
        *self.chip_listeners.borrow_mut() = l;
        clear(&self.tf_caret);
        if model.caret_solo {
            attr(&self.tf_caret, "data-solo", "1");
            if let Some(text) = &model.caret_text {
                let label = make(&self.env.doc, "span", "vela-widget-tf-label");
                label.set_text_content(Some(text));
                self.tf_caret.append_child(&label).ok();
            }
        } else {
            let _ = self.tf_caret.remove_attribute("data-solo");
        }
        self.tf_caret
            .append_child(&icon_el(&self.env.doc, "chevron-down"))
            .ok();
        attr(&self.tf_caret, "aria-label", &model.caret_aria_label);
        self.env.retire(Box::new(old));
        self.sync_hairlines_later();
    }

    /// Re-project the contributed topbar actions (call after registrations change).
    pub fn render_actions(&self) {
        let old_l: Vec<Listener> = self.action_listeners.borrow_mut().drain(..).collect();
        let old_t: Vec<Rc<Tooltip>> = self.action_tips.borrow_mut().drain(..).collect();
        for t in &old_t {
            t.destroy();
        }
        clear(&self.actions_host);
        clear(&self.left_actions_host);
        for pin in self.pinned.borrow().values() {
            clear(&pin.host);
        }
        self.env.retire(Box::new((old_l, old_t)));
        let Some((contrib, factory)) = &self.opts.contributions else {
            set_style(&self.left_actions_sep, "display", "none");
            return;
        };
        let ctx = factory();
        let flow_left = self.comp.left.iter().any(|x| x == "actions");
        let flow_right = self.comp.right.iter().any(|x| x == "actions");
        let builtin: HashSet<&str> = TOPBAR_BUILTIN_IDS.into_iter().collect();
        let tr = self.env.translator();
        for action in contrib.widget_actions(WidgetActionTarget::Topbar, Some(ctx.as_ref())) {
            let pinned = self.pinned.borrow();
            let pin = pinned.get(&action.id);
            if pin.is_none()
                && builtin.contains(action.id.as_str())
                && !self.flowing_overrides.borrow().contains(&action.id)
            {
                continue;
            }
            let left = pin.map_or(action.align == Some(ActionAlign::Left), |p| p.left);
            if pin.is_none() && !(if left { flow_left } else { flow_right }) {
                continue;
            }
            let icon_only = action.icon_only && action.icon.is_some();
            let b = make(
                &self.env.doc,
                "button",
                if left {
                    "vela-widget-action-left"
                } else if icon_only {
                    "vela-widget-tool"
                } else {
                    "vela-widget-action"
                },
            );
            attr(&b, "type", "button");
            attr(&b, "data-action-id", &action.id);
            if let Some(icon) = &action.icon {
                b.append_child(&icon_el(&self.env.doc, icon)).ok();
            }
            let label = text_of(tr.as_deref(), &action.label, &[]);
            if icon_only {
                attr(&b, "aria-label", &label);
                self.action_tips
                    .borrow_mut()
                    .push(Tooltip::new(&self.env, &b, &label));
            } else {
                let text = make(&self.env.doc, "span", "vela-widget-label");
                text.set_text_content(Some(&label));
                b.append_child(&text).ok();
            }
            let (run, factory) = (action.run.clone(), factory.clone());
            self.action_listeners
                .borrow_mut()
                .push(on(&b, "click", move |_| {
                    let ctx = factory();
                    run(ctx.as_ref());
                }));
            match pin {
                Some(p) => p.host.append_child(&b).ok(),
                None if left => self.left_actions_host.append_child(&b).ok(),
                None => self.actions_host.append_child(&b).ok(),
            };
        }
        let any_left = self.left_actions_host.first_element_child().is_some();
        set_style(
            &self.left_actions_sep,
            "display",
            if any_left { "" } else { "none" },
        );
        self.sync_hairlines_later();
    }

    /// Re-read every label, tooltip and menu in the current language.
    pub fn relocalize(&self) {
        for relabel in self.labels.borrow().iter() {
            relabel();
        }
        for (tip, m) in self.tooltips.borrow().iter() {
            let text = self.t(m);
            tip.set_content(&text);
        }
        for (btn, m) in [
            (&self.undo_btn, undo_message()),
            (&self.redo_btn, redo_message()),
            (&self.alerts_btn, alerts_message()),
        ] {
            if !btn.get_attribute("class").unwrap_or_default().is_empty() {
                attr(btn, "aria-label", &self.t(&m));
            }
        }
        if let Some(b) = &self.screenshot_btn {
            attr(b, "aria-label", &self.t(&screenshot_message()));
        }
        self.render_tf_chips();
        self.render_style_button();
        if self.layout_button.is_some() {
            self.render_layout_button();
        }
        if let Some(m) = self.tf_menu.borrow().as_ref() {
            m.refresh();
        }
        if let Some(m) = self.style_menu.borrow().as_ref() {
            m.refresh();
        }
        if let Some(p) = self.layout_picker.borrow().as_ref() {
            p.relabel();
        }
        self.render_actions();
        let _ = (&self.indicators_btn, timeframes_message);
    }

    // ── hairlines ──

    fn sync_hairlines_later(&self) {
        if self.hairline_pending.replace(true) || self.destroyed.get() {
            return;
        }
        let me = self.me.clone();
        self.env.after(0.0, move || {
            if let Some(t) = me.upgrade() {
                t.hairline_pending.set(false);
                t.sync_hairlines();
            }
        });
    }

    /// Paint each `.vela-sep` as exactly one device pixel, snapped to the pixel grid.
    fn sync_hairlines(&self) {
        if self.destroyed.get() {
            return;
        }
        self.watch_resizes();
        let dpr = vst_dpr(&JsValue::from(self.env.doc.clone()));
        let mut seps = Vec::new();
        collect_seps(&self.wrap, &mut seps);
        for el in seps {
            set_style(&el, "width", &format!("{}px", 1.0 / dpr));
            set_style(&el, "transform", "");
            let left = el.get_bounding_client_rect().left();
            let dx = (left * dpr).round() / dpr - left;
            if dx != 0.0 {
                set_style(&el, "transform", &format!("translateX({dx}px)"));
            }
        }
    }

    /// Observe the bar's region and the window once the bar is mounted (the original
    /// observes its element and listens to the window's `resize`).
    fn watch_resizes(&self) {
        if self.observer.borrow().is_none()
            && let Some(parent) = self.wrap.parent_element()
        {
            let me = self.me.clone();
            *self.observer.borrow_mut() = Some(Observer::new(&parent, move || {
                if let Some(t) = me.upgrade() {
                    t.sync_hairlines_later();
                }
            }));
        }
        if self.resize_listener.borrow().is_none()
            && let Some(win) = crate::dom::default_view(&self.env.doc)
        {
            let me = self.me.clone();
            *self.resize_listener.borrow_mut() =
                Some(Listener::new(&win, "resize", false, true, move |_| {
                    if let Some(t) = me.upgrade() {
                        t.sync_hairlines_later();
                    }
                }));
        }
    }

    /// Call after the root resized: hairlines re-snap.
    pub fn on_resize(&self) {
        self.sync_hairlines_later();
    }

    pub fn destroy(&self) {
        if self.destroyed.replace(true) {
            return;
        }
        if let Some(m) = self.tf_menu.borrow_mut().take() {
            m.destroy();
        }
        if let Some(m) = self.style_menu.borrow_mut().take() {
            m.destroy();
        }
        if let Some(p) = self.layout_picker.borrow_mut().take() {
            p.destroy();
        }
        for (t, _) in self.tooltips.borrow_mut().drain(..) {
            t.destroy();
        }
        for t in self.action_tips.borrow_mut().drain(..) {
            t.destroy();
        }
        for t in self.panel_tips.borrow_mut().drain(..) {
            t.destroy();
        }
        self.observer.borrow_mut().take();
        self.resize_listener.borrow_mut().take();
        self.listeners.borrow_mut().clear();
        self.chip_listeners.borrow_mut().clear();
        self.panel_listeners.borrow_mut().clear();
        self.action_listeners.borrow_mut().clear();
        self.dyn_listeners.borrow_mut().clear();
        self.wrap.remove();
    }
}

fn vst_set_flag(el: &Element, disabled: bool) {
    super::js::vst_set_flag(el, "disabled", disabled);
    if disabled {
        attr(el, "disabled", "");
    } else {
        let _ = el.remove_attribute("disabled");
    }
}

fn collect_seps(el: &Element, out: &mut Vec<Element>) {
    let mut child = el.first_element_child();
    while let Some(c) = child {
        if c.get_attribute("class")
            .unwrap_or_default()
            .split_whitespace()
            .any(|x| x == "vela-sep")
        {
            out.push(c.clone());
        }
        collect_seps(&c, out);
        child = c.next_element_sibling();
    }
}

/// The label of a timeframe for tests and the shell: the cell's value as the bar shows it.
pub fn timeframe_text(env: &Env, tf: &str) -> String {
    let tr = env.translator();
    timeframe_label(tf, tr.as_deref())
}

/// The alerts bell's dropdown rows are the shell's (see `mod.rs`).
pub fn open_menu_at(
    env: &Env,
    at: (f64, f64),
    items: Vec<MenuItemDescriptor>,
    on_select: Rc<dyn Fn(&str)>,
) -> Option<Rc<Menu>> {
    Menu::open(
        env,
        None,
        Some(at),
        items,
        MenuOpts {
            min_width: None,
            icon_badges: false,
            side_top: false,
            align_end: false,
            on_select,
            on_favorite: None,
            on_close: None,
        },
    )
}
