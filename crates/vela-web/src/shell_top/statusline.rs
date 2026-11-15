//! The in-chart status line (`statusline.ts`): the symbol with its venue and timeframe, the
//! market-status badge and the OHLC / change of the bar under the crosshair, falling back to
//! the latest bar.
//!
//! The text and the decisions (the readout cells and change at a ladder level, the segment
//! visibility of the parts, the width ladder, the right-click menu rows, the market
//! vocabulary) are [`vela_core::widget::statusline`] and
//! [`vela_core::widget::statusline_model`]; this is the element tree and the fitting: on
//! every resize it walks the ladder (the values move under the symbol, then shed O/H/L, then
//! the absolute change) until the chip no longer overflows, and in fit mode (multi-chart
//! cells) hides whole segments, least important first.
//!
//! Differences from the original: the market badge is a plain tinted circle holding the
//! icon (the kit's callout bubble without a panel), and each readout cell is a `span` that
//! holds its label in an inner `span` (the label and the value are two elements, not a text
//! node and an element).

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vela_core::widget::statusline::{
    BarLike, IsUp, MarketStatus, StatuslineInk, StatuslineReadout, base_of_ticker, meta_text,
    replay_label, statusline_values, width_key,
};
use vela_core::widget::statusline_model::{
    STATUSLINE_LADDER, StatuslineLayout, StatuslineLevel, StatuslinePart, StatuslineParts,
    segment_visibility, show_chart_message, statusline_menu_items,
};
use vela_core::widget::symbol_icon::IconFailures;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::Element;

use super::js::{vst_client_width, vst_ev_num, vst_prevent, vst_scroll_width, vst_stop};
use super::kit::{
    Env, Menu, MenuOpts, Observer, Tooltip, clear, icon_el, icon_html_px, set_class, set_flag,
};
use super::symbol_picker::ticker_icon_el;
use crate::dom::{Listener, attr, make, on, set_style};

/// What the right-click menu reaches: part toggles route through the host (so its
/// persistence and style-link mirroring follow), the chart toggle reaches the renderer.
pub struct StatuslineMenuHooks {
    pub set_part: Rc<dyn Fn(StatuslinePart, bool)>,
    pub chart_visible: Rc<dyn Fn() -> bool>,
    pub set_chart_visible: Rc<dyn Fn(bool)>,
}

const REPLAY_BADGE_SVG: &str = "<svg viewBox=\"0 0 16 16\" width=\"16\" height=\"16\" aria-hidden=\"true\"><circle cx=\"8\" cy=\"8\" r=\"8\" style=\"fill: var(--vela-selected-bg)\"/><path d=\"M11.7 4.75v6.5L7.2 8zM7.2 4.75v6.5L2.7 8z\" style=\"fill: var(--vela-selected-fg); stroke: var(--vela-selected-fg); stroke-width: 0.8; stroke-linejoin: round\"/></svg>";

pub struct Statusline {
    env: Env,
    me: Weak<Statusline>,
    host: Element,
    pub el: Element,
    identity_row: Element,
    values_row: Element,
    avatar: RefCell<Element>,
    avatar_l: RefCell<Option<Listener>>,
    symbol_el: Element,
    meta_el: Element,
    market_el: Element,
    bubble: Element,
    replay_badge: Element,
    ohlc_el: Element,
    change_el: Element,
    eye_el: Element,
    market_tip: Rc<Tooltip>,
    eye_tip: Rc<Tooltip>,
    parts: Cell<StatuslineParts>,
    chart_hidden: Cell<bool>,
    market_status: Cell<MarketStatus>,
    replaying: Cell<bool>,
    last_bar: Cell<Option<BarLike>>,
    hover_bar: Cell<Option<BarLike>>,
    ink: RefCell<StatuslineInk>,
    fit_mode: Cell<bool>,
    layout: Cell<StatuslineLayout>,
    fit_key: RefCell<Option<String>>,
    meta: RefCell<Option<(String, String)>>,
    symbol: RefCell<String>,
    icon_for: Rc<dyn Fn(&str) -> Option<String>>,
    failures: IconFailures,
    hooks: RefCell<Option<StatuslineMenuHooks>>,
    menu: RefCell<Option<Rc<Menu>>>,
    listeners: RefCell<Vec<Listener>>,
    observer: RefCell<Option<Observer>>,
    /// Web fonts landing after the last fit change every width without a resize.
    fonts: RefCell<Option<FontsWatch>>,
}

/// A `document.fonts` `loadingdone` subscription that ends when dropped.
struct FontsWatch {
    unsub: JsValue,
    _cb: wasm_bindgen::prelude::Closure<dyn FnMut()>,
}

impl Drop for FontsWatch {
    fn drop(&mut self) {
        if let Some(f) = self.unsub.dyn_ref::<js_sys::Function>() {
            let _ = f.call0(&JsValue::NULL);
        }
    }
}

impl Statusline {
    pub fn new(
        env: &Env,
        host: &Element,
        symbol: &str,
        icon_for: Rc<dyn Fn(&str) -> Option<String>>,
    ) -> Rc<Statusline> {
        let doc = &env.doc;
        // Scopes the price-legend shift to THIS host.
        set_flag(host, "data-vela-has-statusline", true);
        let el = make(doc, "div", "vela-statusline");
        // Opts into the renderer's PNG export: the status line is part of what is on screen.
        attr(&el, "data-vela-screenshot", "1");
        let failures = IconFailures::new();
        let ticker = vela_core::data::provider_registry::parse_symbol(symbol).ticker;
        let url = icon_for(symbol);
        let (avatar, avatar_l) = ticker_icon_el(
            env,
            &failures,
            &base_of_ticker(&ticker),
            &ticker,
            "vela-sl-avatar",
            url.as_deref(),
        );
        let symbol_el = make(doc, "span", "vela-sl-symbol");
        symbol_el.set_text_content(Some(&ticker));
        let meta_el = make(doc, "span", "vela-sl-meta");
        let bubble = make(doc, "span", "vela-sl-bubble");
        let replay_badge = make(doc, "span", "vela-sl-replay-badge");
        super::js::vst_set_html(&replay_badge, REPLAY_BADGE_SVG);
        set_style(&replay_badge, "display", "none");
        let market_el = make(doc, "span", "vela-sl-market");
        market_el.append_child(&bubble).ok();
        market_el.append_child(&replay_badge).ok();
        let ohlc_el = make(doc, "span", "vela-sl-ohlc");
        let change_el = make(doc, "span", "vela-sl-change");
        attr(&change_el, "dir", "ltr");
        // The show-chart eye takes the readout's place while the chart is hidden.
        let eye_el = make(doc, "button", "vela-sl-eye");
        attr(&eye_el, "type", "button");
        super::js::vst_set_html(&eye_el, &icon_html_px("eye-off", 14));
        let eye_label = env.t(&show_chart_message());
        attr(&eye_el, "aria-label", &eye_label);
        set_style(&eye_el, "display", "none");
        let identity_row = make(doc, "span", "vela-sl-row vela-sl-identity");
        for c in [&avatar, &symbol_el, &meta_el, &market_el] {
            identity_row.append_child(c).ok();
        }
        let values_row = make(doc, "span", "vela-sl-row vela-sl-values");
        for c in [&ohlc_el, &change_el, &eye_el] {
            values_row.append_child(c).ok();
        }
        el.append_child(&identity_row).ok();
        el.append_child(&values_row).ok();
        host.append_child(&el).ok();
        let market_tip = Tooltip::new(env, &market_el, &env.t(&MarketStatus::Open.label()));
        let eye_tip = Tooltip::new(env, &eye_el, &eye_label);
        let this = Rc::new_cyclic(|me| Statusline {
            env: env.clone(),
            me: me.clone(),
            host: host.clone(),
            el: el.clone(),
            identity_row,
            values_row,
            avatar: RefCell::new(avatar),
            avatar_l: RefCell::new(avatar_l),
            symbol_el,
            meta_el,
            market_el,
            bubble,
            replay_badge,
            ohlc_el,
            change_el,
            eye_el: eye_el.clone(),
            market_tip,
            eye_tip,
            parts: Cell::new(StatuslineParts::default()),
            chart_hidden: Cell::new(false),
            market_status: Cell::new(MarketStatus::Open),
            replaying: Cell::new(false),
            last_bar: Cell::new(None),
            hover_bar: Cell::new(None),
            ink: RefCell::new(StatuslineInk {
                up: None,
                down: None,
                is_up: None,
                readout: StatuslineReadout::Ohlc,
            }),
            fit_mode: Cell::new(false),
            layout: Cell::new(STATUSLINE_LADDER[0]),
            fit_key: RefCell::new(None),
            meta: RefCell::new(None),
            symbol: RefCell::new(symbol.to_string()),
            icon_for,
            failures,
            hooks: RefCell::new(None),
            menu: RefCell::new(None),
            listeners: RefCell::new(Vec::new()),
            observer: RefCell::new(None),
            fonts: RefCell::new(None),
        });
        let mut l = Vec::new();
        let me = this.me.clone();
        l.push(on(&eye_el, "click", move |ev| {
            vst_stop(&ev); // never bubbles into the chip's own handlers
            if let Some(s) = me.upgrade() {
                if let Some(h) = s.hooks.borrow().as_ref() {
                    (h.set_chart_visible)(true);
                }
                s.set_chart_hidden(false);
            }
        }));
        let me = this.me.clone();
        l.push(on(&el, "contextmenu", move |ev| {
            if let Some(s) = me.upgrade() {
                s.context_menu(&ev);
            }
        }));
        *this.listeners.borrow_mut() = l;
        let me = this.me.clone();
        *this.observer.borrow_mut() = Some(Observer::new(host, move || {
            if let Some(s) = me.upgrade() {
                s.fit();
            }
        }));
        let me = this.me.clone();
        let cb = wasm_bindgen::prelude::Closure::<dyn FnMut()>::new(move || {
            if let Some(s) = me.upgrade() {
                s.fit();
            }
        });
        let unsub = super::js::vst_fonts_listen(
            &JsValue::from(env.doc.clone()),
            wasm_bindgen::JsCast::unchecked_ref(cb.as_ref()),
        );
        *this.fonts.borrow_mut() = Some(FontsWatch { unsub, _cb: cb });
        this.dress_badge();
        this.render();
        this
    }

    fn t_part_visible(&self) -> StatuslineParts {
        self.parts.get()
    }

    pub fn part_visible(&self, part: StatuslinePart) -> bool {
        self.t_part_visible().get(part)
    }

    pub fn set_symbol(&self, symbol: &str) {
        *self.symbol.borrow_mut() = symbol.to_string();
        let ticker = vela_core::data::provider_registry::parse_symbol(symbol).ticker;
        self.symbol_el.set_text_content(Some(&ticker));
        let url = (self.icon_for)(symbol);
        let (fresh, l) = ticker_icon_el(
            &self.env,
            &self.failures,
            &base_of_ticker(&ticker),
            &ticker,
            "vela-sl-avatar",
            url.as_deref(),
        );
        let old = self.avatar.replace(fresh.clone());
        self.identity_row.insert_before(&fresh, Some(&old)).ok();
        old.remove();
        *self.avatar_l.borrow_mut() = l;
        self.sync_parts(); // the fresh avatar must inherit a hidden logo part
        self.fit();
    }

    /// Multi-chart cells: the same width ladder, plus a last resort for cells too narrow for
    /// the bottom rung: whatever still overflows is hidden outright, least important first.
    pub fn set_fit_mode(&self, on: bool) {
        if self.fit_mode.replace(on) != on {
            self.fit();
        }
    }

    fn seg(&self) -> vela_core::widget::statusline_model::SegmentVisibility {
        segment_visibility(&self.parts.get(), self.chart_hidden.get())
    }

    /// Project the parts config onto the segments (the baseline `fit` prunes from).
    fn sync_parts(&self) {
        let seg = self.seg();
        let show = |el: &Element, on: bool| set_style(el, "display", if on { "" } else { "none" });
        show(&self.avatar.borrow(), seg.avatar);
        show(&self.symbol_el, seg.symbol);
        show(&self.meta_el, seg.meta);
        show(&self.market_el, seg.market);
        show(&self.ohlc_el, seg.ohlc);
        show(&self.change_el, seg.change);
        set_style(
            &self.eye_el,
            "display",
            if seg.eye { "inline-flex" } else { "none" },
        );
        // An empty values row must not hold a gap or padding of its own on the line.
        show(&self.values_row, seg.ohlc || seg.change || seg.eye);
    }

    fn apply_layout(&self, layout: StatuslineLayout) {
        self.layout.set(layout);
        set_class(&self.el, "vela-sl-stacked", layout.stacked);
        set_flag(&self.host, "data-vela-sl-stacked-host", layout.stacked);
        self.render_values();
    }

    fn overflows(&self) -> bool {
        vst_scroll_width(&self.el) > vst_client_width(&self.el)
    }

    /// Walk the width ladder until the chip fits its max-width (see the module doc). Without
    /// layout (a detached host, a test) nothing overflows, so the widest rung stays.
    pub fn fit(&self) {
        *self.fit_key.borrow_mut() = Some(self.width_key());
        self.sync_parts(); // start from the full (parts-allowed) row, then descend
        let seg = self.seg();
        // Only a value readout can stack or shed: with none showing the chip stays one line.
        let has_values = seg.ohlc || seg.change;
        for rung in STATUSLINE_LADDER {
            self.apply_layout(rung);
            if !has_values || !self.overflows() {
                break;
            }
        }
        if !self.fit_mode.get() {
            return;
        }
        let order: [(&Element, bool); 3] = [
            (&self.values_row, has_values),
            (&self.meta_el, seg.meta),
            (&self.market_el, seg.market),
        ];
        // The values row is already out when the parts leave it nothing to show.
        let mut values_hidden = !(seg.ohlc || seg.change || seg.eye);
        for (i, (el, shown)) in order.into_iter().enumerate() {
            if !self.overflows() {
                break;
            }
            if shown {
                set_style(el, "display", "none");
                if i == 0 {
                    values_hidden = true;
                }
            }
        }
        // With the values row gone the chip is one line again: un-stack so the legend shift
        // drops back to a single line's.
        if values_hidden {
            self.apply_layout(StatuslineLayout {
                stacked: false,
                ..self.layout.get()
            });
        }
    }

    /// Shape and color the value readout after the active price style.
    pub fn set_direction_colors(&self, ink: StatuslineInk) {
        *self.ink.borrow_mut() = ink;
        self.render();
    }

    /// The `· BINANCE · 1h` segment after the symbol: venue first, then resolution.
    pub fn set_meta(&self, timeframe: &str, provider: &str) {
        *self.meta.borrow_mut() = Some((timeframe.to_string(), provider.to_string()));
        let tr = self.env.translator();
        self.meta_el
            .set_text_content(Some(&meta_text(timeframe, provider, tr.as_deref())));
        self.fit();
    }

    /// Re-read every label and value in the chart's language.
    pub fn relocalize(&self) {
        let label = self.env.t(&show_chart_message());
        attr(&self.eye_el, "aria-label", &label);
        self.eye_tip.set_content(&label);
        self.dress_badge();
        let meta = self.meta.borrow().clone();
        if let Some((tf, provider)) = meta {
            self.set_meta(&tf, &provider);
        }
        *self.fit_key.borrow_mut() = None;
        self.fit();
    }

    /// Dress the market badge for a session state.
    pub fn set_market_status(&self, status: MarketStatus) {
        self.market_status.set(status);
        self.dress_badge();
    }

    /// The chart entered or left a bar replay: the badge shows the replay mode meanwhile.
    pub fn set_replaying(&self, replaying: bool) {
        if self.replaying.replace(replaying) != replaying {
            self.dress_badge();
        }
    }

    pub fn market_status(&self) -> MarketStatus {
        self.market_status.get()
    }

    fn dress_badge(&self) {
        let replaying = self.replaying.get();
        set_style(&self.bubble, "display", if replaying { "none" } else { "" });
        set_style(
            &self.replay_badge,
            "display",
            if replaying { "" } else { "none" },
        );
        if replaying {
            attr(&self.market_el, "data-status", "replay");
            self.market_tip.set_content(&self.env.t(&replay_label()));
            attr(&self.market_el, "aria-label", &self.env.t(&replay_label()));
            return;
        }
        let status = self.market_status.get();
        attr(&self.market_el, "data-status", status.as_str());
        let label = self.env.t(&status.label());
        clear(&self.bubble);
        self.bubble
            .append_child(&icon_el(&self.env.doc, &status.icon()))
            .ok();
        set_style(&self.bubble, "background", &status.badge_background());
        set_style(&self.bubble, "color", status.ink());
        self.market_tip.set_content(&label);
        attr(&self.market_el, "aria-label", &label);
    }

    /// Show or hide one part (the settings dialog's Status line tab and the right-click menu).
    pub fn set_part_visible(&self, part: StatuslinePart, visible: bool) {
        let mut parts = self.parts.get();
        match part {
            StatuslinePart::Logo => parts.logo = visible,
            StatuslinePart::Name => parts.name = visible,
            StatuslinePart::Market => parts.market = visible,
            StatuslinePart::Ohlc => parts.ohlc = visible,
            StatuslinePart::Change => parts.change = visible,
        }
        self.set_parts(parts);
    }

    pub fn set_parts(&self, parts: StatuslineParts) {
        if self.parts.get() == parts {
            return;
        }
        self.parts.set(parts);
        self.sync_parts();
        self.fit();
    }

    pub fn parts(&self) -> StatuslineParts {
        self.parts.get()
    }

    /// Mirror the chart's visibility: dim the line, drop the value readout and put the
    /// show-chart eye out in its place. Idempotent.
    pub fn set_chart_hidden(&self, hidden: bool) {
        if self.chart_hidden.replace(hidden) == hidden {
            return;
        }
        set_class(&self.el, "vela-sl-chart-hidden", hidden);
        self.sync_parts();
        self.fit();
    }

    /// Wire the right-click action menu (later calls swap the hooks).
    pub fn attach_menu(&self, hooks: StatuslineMenuHooks) {
        *self.hooks.borrow_mut() = Some(hooks);
    }

    fn context_menu(&self, ev: &JsValue) {
        let chart_visible = {
            let hooks = self.hooks.borrow();
            let Some(h) = hooks.as_ref() else { return };
            (h.chart_visible)()
        };
        // This right-click is the status line's: keep the chart's own context menu closed.
        vst_prevent(ev);
        vst_stop(ev);
        self.set_chart_hidden(!chart_visible); // opportunistic re-sync with the live renderer
        let tr = self.env.translator();
        let items = statusline_menu_items(&self.parts.get(), chart_visible, tr.as_deref());
        if let Some(old) = self.menu.borrow_mut().take() {
            old.close();
        }
        let me = self.me.clone();
        let at = (vst_ev_num(ev, "clientX"), vst_ev_num(ev, "clientY"));
        let menu = Menu::open(
            &self.env,
            None,
            Some(at),
            items,
            MenuOpts {
                min_width: None,
                icon_badges: false,
                side_top: false,
                align_end: false,
                on_select: Rc::new(move |id| {
                    if let Some(s) = me.upgrade() {
                        s.run_menu_item(id);
                    }
                }),
                on_favorite: None,
                on_close: None,
            },
        );
        *self.menu.borrow_mut() = menu;
    }

    fn run_menu_item(&self, id: &str) {
        let hooks = self.hooks.borrow();
        let Some(h) = hooks.as_ref() else { return };
        if let Some(name) = id.strip_prefix("part:") {
            if let Some(part) = StatuslinePart::ALL.into_iter().find(|p| p.as_str() == name) {
                (h.set_part)(part, !self.parts.get().get(part));
            }
        } else if id == "chart" {
            let next = !(h.chart_visible)();
            (h.set_chart_visible)(next);
            drop(hooks);
            self.set_chart_hidden(!next);
        }
    }

    /// The bar the chart's newest tick settled (the resting readout).
    pub fn set_last_bar(&self, bar: Option<BarLike>) {
        self.last_bar.set(bar);
        if self.hover_bar.get().is_none() {
            self.render();
        }
    }

    /// The bar under the crosshair (`None` off any bar).
    pub fn set_hover_bar(&self, bar: Option<BarLike>) {
        self.hover_bar.set(bar);
        self.render();
    }

    /// Drop the resting and the hover readout (a new market: the old one's values must not
    /// linger until the next tick). The original does this by re-binding to its chart.
    pub fn clear_bars(&self) {
        self.last_bar.set(None);
        self.hover_bar.set(None);
        self.render();
    }

    fn bar(&self) -> Option<BarLike> {
        self.hover_bar.get().or(self.last_bar.get())
    }

    fn width_key(&self) -> String {
        width_key(self.bar().as_ref(), self.ink.borrow().readout)
    }

    fn render(&self) {
        // Chart visibility has no change event of its own: re-derive it from the live
        // renderer on every readout refresh (bar ticks, crosshair moves), so a toggle made
        // anywhere (the object tree's eye) reaches the status line.
        let probe = self
            .hooks
            .borrow()
            .as_ref()
            .map(|h| h.chart_visible.clone());
        if let Some(visible) = probe {
            self.set_chart_hidden(!visible());
        }
        // Crosshair moves land here at pointer rate. Walking the ladder forces a layout per
        // rung, so only re-walk it when the readout's width can have changed.
        if self.fit_key.borrow().as_deref() == Some(self.width_key().as_str()) {
            self.render_values();
        } else {
            self.fit();
        }
    }

    /// Write the value readout for the current bar at the current ladder level.
    fn render_values(&self) {
        let Some(bar) = self.bar() else {
            clear(&self.ohlc_el);
            self.change_el.set_text_content(Some(""));
            return;
        };
        let ink = self.ink.borrow();
        let tr = self.env.translator();
        let values = statusline_values(
            &bar,
            self.layout.get().level,
            ink.readout,
            ink.is_up.as_ref(),
            tr.as_deref(),
        );
        // ONE ink for the whole readout: OHLC values and the change share it.
        let color = if values.up {
            ink.up
                .clone()
                .unwrap_or_else(|| "var(--vela-up)".to_string())
        } else {
            ink.down
                .clone()
                .unwrap_or_else(|| "var(--vela-down)".to_string())
        };
        let doc = &self.env.doc;
        clear(&self.ohlc_el);
        for cell in &values.cells {
            let s = make(doc, "span", "");
            if !cell.label.is_empty() {
                let k = make(doc, "span", "vela-sl-key");
                k.set_text_content(Some(&format!("{} ", cell.label)));
                s.append_child(&k).ok();
            }
            let b = make(doc, "b", "");
            attr(&b, "dir", "ltr");
            b.set_text_content(Some(&cell.text));
            set_style(&b, "color", &color);
            s.append_child(&b).ok();
            self.ohlc_el.append_child(&s).ok();
        }
        self.change_el.set_text_content(Some(&values.change));
        attr(
            &self.change_el,
            "data-dir",
            if values.up { "up" } else { "down" },
        );
        set_style(&self.change_el, "color", &color);
    }

    pub fn destroy(&self) {
        self.observer.borrow_mut().take();
        self.fonts.borrow_mut().take();
        if let Some(m) = self.menu.borrow_mut().take() {
            m.close();
        }
        self.market_tip.destroy();
        self.eye_tip.destroy();
        self.listeners.borrow_mut().clear();
        self.avatar_l.borrow_mut().take();
        set_flag(&self.host, "data-vela-has-statusline", false);
        set_flag(&self.host, "data-vela-sl-stacked-host", false);
        self.el.remove();
    }
}

/// The IsUp rule type, re-exported for the cell glue.
pub type Rule = IsUp;

/// Whether the ladder has a level (kept for the glue's tests).
pub fn ladder_levels() -> Vec<(bool, StatuslineLevel)> {
    STATUSLINE_LADDER
        .iter()
        .map(|l| (l.stacked, l.level))
        .collect()
}
