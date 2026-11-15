//! The small kit the top shell stands on: the original's `vela-ui` dialog, dropdown menu,
//! tooltip, tabs and drawer as native elements (the kit is not ported). Structure, ARIA,
//! dismissal and keyboard rules follow the kit components; the look is `css.rs`.
//!
//! Closing a surface from inside one of its own handlers frees the listener that is
//! running: every close hands its listeners to [`Env::retire`], which drops them on a fresh
//! stack.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vela_core::i18n::{MessageArg, MessageLike, Translator, text_of};
use vela_core::runtime::{Platform, TimerId};
use vela_core::ui::MenuItemDescriptor;
use wasm_bindgen::JsValue;
use web_sys::{Document, Element, EventTarget};

use super::icons;
use super::js::{
    holds, vst_active, vst_attached, vst_ev_num, vst_key, vst_prevent, vst_scroll_into_view,
    vst_set_html, vst_stop, vst_target,
};
use crate::dom::{Listener, attr, make, on, set_style, set_styles, vela_focus};
use crate::workspace_ui::WidgetRoot;

/// What every surface needs: the document, the root (weak), the platform.
#[derive(Clone)]
pub struct Env {
    pub root: Weak<WidgetRoot>,
    pub doc: Document,
    pub platform: Rc<dyn Platform>,
}

impl Env {
    pub fn root(&self) -> Option<Rc<WidgetRoot>> {
        self.root.upgrade()
    }

    /// The element floating layers append to (the widget root).
    pub fn host(&self) -> Option<Element> {
        Some(self.root()?.element().clone())
    }

    pub fn translator(&self) -> Option<Rc<dyn Translator>> {
        self.root()?.translator()
    }

    /// A message in the shell's language (English before the workspace exists).
    pub fn t(&self, m: &MessageLike) -> String {
        self.tv(m, &[])
    }

    pub fn tv(&self, m: &MessageLike, args: &[(&str, MessageArg)]) -> String {
        let tr = self.translator();
        text_of(tr.as_deref(), m, args)
    }

    /// Drop `thing` once the current call stack is gone.
    pub fn retire(&self, thing: Box<dyn Any>) {
        self.platform
            .set_timeout(0.0, Box::new(move || drop(thing)));
    }

    /// An icon element of the registry (`span.vela-icon`).
    pub fn icon(&self, id: &str) -> Element {
        icon_el(&self.doc, id)
    }

    /// Whether the chrome is in the mobile size class.
    pub fn mobile(&self) -> bool {
        self.root()
            .is_some_and(|r| r.layout_mode() == vela_core::widget::layout_mode::LayoutMode::Mobile)
    }

    pub fn after(&self, ms: f64, f: impl FnOnce() + 'static) -> TimerId {
        self.platform.set_timeout(ms, Box::new(f))
    }
}

/// `span.vela-icon` holding the registry's markup of `id`.
pub fn icon_el(doc: &Document, id: &str) -> Element {
    let el = make(doc, "span", "vela-icon");
    attr(&el, "aria-hidden", "true");
    vst_set_html(&el, icons::icon(id));
    el
}

/// Icon markup at `px` pixels (the registry's are `1em`).
pub fn icon_html_px(id: &str, px: u32) -> String {
    icons::icon(id)
        .replace("width=\"1em\"", &format!("width=\"{px}\""))
        .replace("height=\"1em\"", &format!("height=\"{px}\""))
}

pub fn clear(el: &Element) {
    while let Some(child) = el.first_child() {
        match wasm_bindgen::JsCast::dyn_into::<Element>(child) {
            Ok(c) => c.remove(),
            Err(_) => break,
        }
    }
    el.set_text_content(Some(""));
}

pub fn set_class(el: &Element, class: &str, on: bool) {
    let current = el.get_attribute("class").unwrap_or_default();
    let has = current.split_whitespace().any(|c| c == class);
    if on == has {
        return;
    }
    let next: Vec<&str> = current
        .split_whitespace()
        .filter(|c| *c != class)
        .chain(on.then_some(class))
        .collect();
    attr(el, "class", &next.join(" ")); // not `className`: read-only on SVG elements
}

pub fn set_flag(el: &Element, name: &str, on: bool) {
    if on {
        attr(el, name, "");
    } else {
        let _ = el.remove_attribute(name);
    }
}

fn doc_target(doc: &Document) -> EventTarget {
    doc.clone().into()
}

// ── dialog ──

pub struct DialogOpts {
    pub title: String,
    /// The body owns its layout (a flex column, no padding).
    pub flush: bool,
    /// The card moves by its header (the original's `draggable`: the indicator picker and
    /// the timeframe entry; the symbol search stays put).
    pub draggable: bool,
    pub on_open_change: Option<Rc<dyn Fn(bool)>>,
}

/// A modal card over a transparent backdrop: Escape, the close button and a press outside
/// the card close it; focus goes back to the element that had it.
pub struct Dialog {
    env: Env,
    me: Weak<Dialog>,
    backdrop: Element,
    positioner: Element,
    panel: Element,
    title: Element,
    body: Element,
    open: Cell<bool>,
    perm: RefCell<Vec<Listener>>,
    live: RefCell<Vec<Listener>>,
    opener: RefCell<Option<Element>>,
    on_open_change: Option<Rc<dyn Fn(bool)>>,
}

impl Dialog {
    pub fn new(env: &Env, opts: DialogOpts) -> Rc<Dialog> {
        let doc = &env.doc;
        let opts_draggable = opts.draggable;
        let backdrop = make(doc, "div", "vela-st-backdrop");
        let positioner = make(doc, "div", "vela-st-positioner");
        let panel = make(doc, "div", "vela-st-panel");
        attr(&panel, "role", "dialog");
        attr(&panel, "aria-modal", "true");
        attr(&panel, "tabindex", "-1");
        let header = make(doc, "div", "vela-st-header");
        let title = make(doc, "div", "vela-st-title");
        title.set_id(&format!(
            "vela-st-title-{}",
            env.platform.random().to_bits() % 1_000_000
        ));
        attr(&panel, "aria-labelledby", &title.id());
        title.set_text_content(Some(&opts.title));
        let close = make(doc, "button", "vela-st-close");
        attr(&close, "type", "button");
        attr(
            &close,
            "aria-label",
            &env.t(&vela_core::i18n::msg("ui.dialog.close", "Close")),
        );
        close.append_child(&icon_el(doc, "close")).ok();
        let body = make(doc, "div", "vela-st-body");
        if opts.flush {
            attr(&body, "data-flush", "");
        }
        header.append_child(&title).ok();
        header.append_child(&close).ok();
        panel.append_child(&header).ok();
        panel.append_child(&body).ok();
        positioner.append_child(&panel).ok();
        let this = Rc::new_cyclic(|me| Dialog {
            env: env.clone(),
            me: me.clone(),
            backdrop: backdrop.clone(),
            positioner,
            panel,
            title,
            body,
            open: Cell::new(false),
            perm: RefCell::new(Vec::new()),
            live: RefCell::new(Vec::new()),
            opener: RefCell::new(None),
            on_open_change: opts.on_open_change,
        });
        let mut perm = Vec::new();
        {
            let me = this.me.clone();
            perm.push(on(&close, "click", move |_| {
                if let Some(d) = me.upgrade() {
                    d.hide();
                }
            }));
            // The backdrop catches interact-outside; the positioner is click-through.
            let me = this.me.clone();
            perm.push(on(&backdrop, "pointerdown", move |_| {
                if let Some(d) = me.upgrade() {
                    d.hide();
                }
            }));
        }
        if opts_draggable {
            set_style(&header, "cursor", "move");
            // (start x, start y, offset x, offset y, dragging): the pointer is captured on the
            // header, so the card follows even when the pointer outruns it.
            let state = Rc::new(Cell::new((0.0f64, 0.0f64, 0.0f64, 0.0f64, false)));
            let (st, hdr) = (state.clone(), header.clone());
            perm.push(on(&header, "pointerdown", move |ev| {
                if !super::js::vst_closest_class(&vst_target(&ev), "vela-st-close").is_null() {
                    return;
                }
                let (_, _, ox, oy, _) = st.get();
                st.set((
                    vst_ev_num(&ev, "clientX") - ox,
                    vst_ev_num(&ev, "clientY") - oy,
                    ox,
                    oy,
                    true,
                ));
                super::js::vst_capture(&hdr, vst_ev_num(&ev, "pointerId") as i32);
            }));
            let (st, panel) = (state.clone(), this.panel.clone());
            perm.push(on(&header, "pointermove", move |ev| {
                let (sx, sy, _, _, dragging) = st.get();
                if !dragging {
                    return;
                }
                let (ox, oy) = (
                    vst_ev_num(&ev, "clientX") - sx,
                    vst_ev_num(&ev, "clientY") - sy,
                );
                st.set((sx, sy, ox, oy, true));
                set_style(&panel, "transform", &format!("translate({ox}px, {oy}px)"));
            }));
            let st = state;
            perm.push(on(&header, "pointerup", move |_| {
                let (sx, sy, ox, oy, _) = st.get();
                st.set((sx, sy, ox, oy, false));
            }));
        }
        *this.perm.borrow_mut() = perm;
        this
    }

    pub fn body(&self) -> &Element {
        &self.body
    }

    pub fn panel(&self) -> &Element {
        &self.panel
    }

    pub fn set_title(&self, title: &str) {
        self.title.set_text_content(Some(title));
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    pub fn show(&self) {
        if self.open.replace(true) {
            return;
        }
        let Some(host) = self.env.host() else {
            self.open.set(false);
            return;
        };
        let active = vst_active(&JsValue::from(self.env.doc.clone()));
        *self.opener.borrow_mut() = if active.is_null() || active.is_undefined() {
            None
        } else {
            wasm_bindgen::JsCast::dyn_into::<Element>(active).ok()
        };
        host.append_child(&self.backdrop).ok();
        host.append_child(&self.positioner).ok();
        let me = self.me.clone();
        let target = doc_target(&self.env.doc);
        self.live
            .borrow_mut()
            .push(Listener::new(&target, "keydown", true, false, move |ev| {
                if vst_key(&ev) == "Escape"
                    && let Some(d) = me.upgrade()
                {
                    vst_prevent(&ev);
                    vst_stop(&ev);
                    d.hide();
                }
            }));
        if let Some(f) = &self.on_open_change {
            f(true);
        }
    }

    pub fn hide(&self) {
        if !self.open.replace(false) {
            return;
        }
        self.backdrop.remove();
        self.positioner.remove();
        let live: Vec<Listener> = self.live.borrow_mut().drain(..).collect();
        self.env.retire(Box::new(live));
        let opener = self.opener.borrow_mut().take();
        if let Some(o) = opener
            && vst_attached(&o)
        {
            vela_focus(&o);
        }
        if let Some(f) = &self.on_open_change {
            f(false);
        }
    }

    /// Hide and drop everything.
    pub fn destroy(&self) {
        self.hide();
        self.backdrop.remove();
        self.positioner.remove();
        let perm: Vec<Listener> = self.perm.borrow_mut().drain(..).collect();
        drop(perm);
    }
}

// ── dropdown menu ──

pub struct MenuOpts {
    pub min_width: Option<String>,
    pub icon_badges: bool,
    /// Open above the anchor (the bottom bar's zone menu).
    pub side_top: bool,
    /// Align the menu's end edge to the anchor's end edge.
    pub align_end: bool,
    pub on_select: Rc<dyn Fn(&str)>,
    pub on_favorite: Option<Rc<dyn Fn(&str, bool)>>,
    pub on_close: Option<Rc<dyn Fn()>>,
}

/// One open menu.
pub struct Menu {
    env: Env,
    me: Weak<Menu>,
    el: Element,
    anchor: Option<Element>,
    items: RefCell<Vec<MenuItemDescriptor>>,
    rows: RefCell<Vec<Element>>,
    highlighted: Cell<Option<usize>>,
    opts: MenuOpts,
    row_listeners: RefCell<Vec<Listener>>,
    listeners: RefCell<Vec<Listener>>,
    closed: Cell<bool>,
}

fn enabled(items: &[MenuItemDescriptor], i: usize) -> bool {
    items.get(i).is_some_and(|it| !it.disabled)
}

impl Menu {
    /// Open a menu on `anchor` (placed under it, or over it with `side_top`), or at the
    /// viewport point `at` when there is no anchor.
    pub fn open(
        env: &Env,
        anchor: Option<&Element>,
        at: Option<(f64, f64)>,
        items: Vec<MenuItemDescriptor>,
        opts: MenuOpts,
    ) -> Option<Rc<Menu>> {
        let host = env.host()?;
        let el = make(&env.doc, "div", "vela-st-menu");
        attr(&el, "role", "menu");
        attr(&el, "tabindex", "-1");
        if opts.icon_badges {
            attr(&el, "data-badges", "");
        }
        if let Some(w) = &opts.min_width {
            set_style(&el, "min-width", w);
        }
        let this = Rc::new_cyclic(|me| Menu {
            env: env.clone(),
            me: me.clone(),
            el: el.clone(),
            anchor: anchor.cloned(),
            items: RefCell::new(items),
            rows: RefCell::new(Vec::new()),
            highlighted: Cell::new(None),
            opts,
            row_listeners: RefCell::new(Vec::new()),
            listeners: RefCell::new(Vec::new()),
            closed: Cell::new(false),
        });
        this.render();
        set_style(&el, "visibility", "hidden");
        host.append_child(&el).ok();
        this.position(&host, at);
        set_style(&el, "visibility", "");
        this.wire();
        // The first enabled row takes the highlight; the menu itself takes focus so the
        // arrows work from the first keystroke.
        let first = {
            let items = this.items.borrow();
            (0..items.len()).find(|i| enabled(&items, *i))
        };
        this.highlight(first);
        vela_focus(&el);
        Some(this)
    }

    pub fn element(&self) -> &Element {
        &self.el
    }

    pub fn is_closed(&self) -> bool {
        self.closed.get()
    }

    fn position(&self, host: &Element, at: Option<(f64, f64)>) {
        let hr = host.get_bounding_client_rect();
        let width = {
            let w = super::js::vst_offset_width(&self.el);
            if w > 0.0 { w } else { 180.0 }
        };
        let (left, top) = if let Some(a) = &self.anchor {
            let r = a.get_bounding_client_rect();
            let mut left = if self.opts.align_end {
                r.right() - hr.left() - width
            } else {
                r.left() - hr.left()
            };
            if left + width > hr.width() - 4.0 {
                left = (hr.width() - 4.0 - width).max(4.0);
            }
            let top = if self.opts.side_top {
                let h = super::js::vst_offset_width(&self.el).min(0.0);
                let height = (self.items.borrow().len() as f64 * 32.0 + 12.0).max(h);
                r.top() - hr.top() - height - 8.0
            } else {
                r.bottom() - hr.top() + 8.0
            };
            (left, top.max(4.0))
        } else if let Some((x, y)) = at {
            (x - hr.left(), y - hr.top())
        } else {
            (0.0, 0.0)
        };
        set_styles(
            &self.el,
            &[("left", &format!("{left}px")), ("top", &format!("{top}px"))],
        );
    }

    fn render(&self) {
        clear(&self.el);
        self.row_listeners.borrow_mut().clear();
        let doc = &self.env.doc;
        let items = self.items.borrow().clone();
        let mut rows = Vec::new();
        let mut listeners = Vec::new();
        for (i, it) in items.iter().enumerate() {
            if it.separator_before {
                let sep = make(doc, "div", "vela-st-menu-sep");
                attr(&sep, "role", "separator");
                self.el.append_child(&sep).ok();
            }
            let row = make(doc, "button", "vela-st-menu-item");
            attr(&row, "type", "button");
            attr(
                &row,
                "role",
                if it.checked.is_some() {
                    "menuitemradio"
                } else {
                    "menuitem"
                },
            );
            attr(&row, "data-id", &it.id);
            if let Some(c) = it.checked {
                attr(&row, "aria-checked", if c { "true" } else { "false" });
                if c {
                    attr(&row, "data-checked", "");
                }
            }
            if it.disabled {
                attr(&row, "data-disabled", "");
                attr(&row, "aria-disabled", "true");
            }
            if let Some(t) = &it.title {
                attr(&row, "title", t);
            }
            if self.opts.icon_badges {
                let badge = make(doc, "span", "vela-st-menu-badge");
                if let Some(icon) = &it.icon {
                    badge.append_child(&icon_el(doc, icon)).ok();
                }
                row.append_child(&badge).ok();
            } else if let Some(icon) = &it.icon {
                let ic = make(doc, "span", "vela-st-menu-icon");
                ic.append_child(&icon_el(doc, icon)).ok();
                row.append_child(&ic).ok();
            }
            let label = make(doc, "span", "vela-st-menu-label");
            label.set_text_content(Some(&it.label));
            row.append_child(&label).ok();
            if let Some(h) = &it.hint {
                let hint = make(doc, "span", "vela-st-menu-hint");
                hint.set_text_content(Some(h));
                row.append_child(&hint).ok();
            }
            if let Some(fav) = it.favorite
                && self.opts.on_favorite.is_some()
            {
                let star = make(doc, "span", "vela-st-menu-star");
                attr(&star, "role", "button");
                attr(&star, "aria-label", if fav { "Unstar" } else { "Star" });
                attr(&star, "aria-pressed", if fav { "true" } else { "false" });
                if fav {
                    attr(&star, "data-fav", "");
                }
                star.append_child(&icon_el(doc, if fav { "star-filled" } else { "star" }))
                    .ok();
                let (me, id) = (self.me.clone(), it.id.clone());
                listeners.push(on(&star, "click", move |ev| {
                    vst_stop(&ev);
                    vst_prevent(&ev);
                    let Some(menu) = me.upgrade() else { return };
                    if let Some(f) = &menu.opts.on_favorite {
                        f(&id, !fav);
                    }
                }));
                row.append_child(&star).ok();
            }
            let (me, id, disabled) = (self.me.clone(), it.id.clone(), it.disabled);
            listeners.push(on(&row, "click", move |_| {
                if disabled {
                    return;
                }
                let Some(menu) = me.upgrade() else { return };
                menu.pick(&id);
            }));
            let me = self.me.clone();
            listeners.push(on(&row, "pointerenter", move |_| {
                if let Some(menu) = me.upgrade()
                    && !menu.closed.get()
                    && enabled(&menu.items.borrow(), i)
                {
                    menu.highlight(Some(i));
                }
            }));
            self.el.append_child(&row).ok();
            rows.push(row);
        }
        *self.rows.borrow_mut() = rows;
        *self.row_listeners.borrow_mut() = listeners;
    }

    /// Replace the rows while the menu stays open (starring a timeframe, a language change).
    pub fn set_items(&self, items: Vec<MenuItemDescriptor>) {
        if self.closed.get() {
            return;
        }
        let keep = self.highlighted.get();
        let old: Vec<Listener> = self.row_listeners.borrow_mut().drain(..).collect();
        *self.items.borrow_mut() = items;
        self.render();
        self.env.retire(Box::new(old));
        let len = self.items.borrow().len();
        self.highlight(keep.filter(|i| *i < len));
    }

    fn highlight(&self, to: Option<usize>) {
        self.highlighted.set(to);
        for (i, row) in self.rows.borrow().iter().enumerate() {
            if Some(i) == to {
                attr(row, "data-highlighted", "");
                vst_scroll_into_view(row);
            } else {
                let _ = row.remove_attribute("data-highlighted");
            }
        }
    }

    fn step(&self, delta: i64) {
        let items = self.items.borrow();
        let n = items.len() as i64;
        if n == 0 {
            return;
        }
        let mut at = match self.highlighted.get() {
            Some(i) => i as i64,
            None => {
                if delta > 0 {
                    -1
                } else {
                    n
                }
            }
        };
        for _ in 0..n {
            at = (at + delta).rem_euclid(n);
            if enabled(&items, at as usize) {
                drop(items);
                self.highlight(Some(at as usize));
                return;
            }
        }
    }

    fn pick(&self, id: &str) {
        let toggle = self
            .items
            .borrow()
            .iter()
            .find(|i| i.id == id)
            .is_some_and(|i| i.toggle);
        let select = self.opts.on_select.clone();
        if toggle {
            select(id);
            return;
        }
        // Close first, then report: the handler may rebuild the chrome freely.
        self.close();
        select(id);
    }

    fn wire(&self) {
        let mut l = Vec::new();
        let me = self.me.clone();
        l.push(on(&self.el, "keydown", move |ev| {
            let Some(menu) = me.upgrade() else { return };
            match vst_key(&ev).as_str() {
                "ArrowDown" => {
                    vst_prevent(&ev);
                    menu.step(1);
                }
                "ArrowUp" => {
                    vst_prevent(&ev);
                    menu.step(-1);
                }
                "Home" => {
                    vst_prevent(&ev);
                    let first = {
                        let items = menu.items.borrow();
                        (0..items.len()).find(|i| enabled(&items, *i))
                    };
                    menu.highlight(first);
                }
                "End" => {
                    vst_prevent(&ev);
                    let last = {
                        let items = menu.items.borrow();
                        (0..items.len()).rev().find(|i| enabled(&items, *i))
                    };
                    menu.highlight(last);
                }
                "Enter" | " " => {
                    vst_prevent(&ev);
                    let id = menu
                        .highlighted
                        .get()
                        .and_then(|i| menu.items.borrow().get(i).map(|it| it.id.clone()));
                    if let Some(id) = id {
                        menu.pick(&id);
                    }
                }
                "Tab" => {
                    menu.close();
                    if let Some(a) = &menu.anchor {
                        vela_focus(a);
                    }
                }
                _ => {}
            }
        }));
        // Dismissal on the document in the capture phase, so the page's own handlers
        // cannot swallow it.
        let target = doc_target(&self.env.doc);
        let (me, el, anchor) = (self.me.clone(), self.el.clone(), self.anchor.clone());
        l.push(Listener::new(
            &target,
            "pointerdown",
            true,
            true,
            move |ev| {
                let node = vst_target(&ev);
                if holds(&el, &node) || anchor.as_ref().is_some_and(|a| holds(a, &node)) {
                    return;
                }
                if let Some(menu) = me.upgrade() {
                    menu.close();
                }
            },
        ));
        let (me, anchor) = (self.me.clone(), self.anchor.clone());
        l.push(Listener::new(&target, "keydown", true, false, move |ev| {
            if vst_key(&ev) != "Escape" {
                return;
            }
            vst_prevent(&ev);
            vst_stop(&ev);
            if let Some(menu) = me.upgrade() {
                menu.close();
            }
            if let Some(a) = &anchor {
                vela_focus(a);
            }
        }));
        *self.listeners.borrow_mut() = l;
    }

    /// Close the menu (its listeners are freed on a fresh stack).
    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        self.el.remove();
        let a: Vec<Listener> = self.listeners.borrow_mut().drain(..).collect();
        let b: Vec<Listener> = self.row_listeners.borrow_mut().drain(..).collect();
        self.env.retire(Box::new((a, b)));
        if let Some(f) = &self.opts.on_close {
            f();
        }
    }
}

/// A trigger button with a dropdown menu: click toggles it, `ArrowDown` opens it, the
/// rows are re-read on every open and on [`refresh`](MenuButton::refresh).
pub struct MenuButton {
    env: Env,
    me: Weak<MenuButton>,
    trigger: Element,
    items: Box<dyn Fn() -> Vec<MenuItemDescriptor>>,
    make_opts: Box<dyn Fn() -> MenuOpts>,
    menu: RefCell<Option<Rc<Menu>>>,
    listeners: RefCell<Vec<Listener>>,
}

impl MenuButton {
    pub fn new(
        env: &Env,
        trigger: &Element,
        items: impl Fn() -> Vec<MenuItemDescriptor> + 'static,
        make_opts: impl Fn() -> MenuOpts + 'static,
    ) -> Rc<MenuButton> {
        attr(trigger, "aria-haspopup", "menu");
        attr(trigger, "aria-expanded", "false");
        let this = Rc::new_cyclic(|me| MenuButton {
            env: env.clone(),
            me: me.clone(),
            trigger: trigger.clone(),
            items: Box::new(items),
            make_opts: Box::new(make_opts),
            menu: RefCell::new(None),
            listeners: RefCell::new(Vec::new()),
        });
        let mut l = Vec::new();
        let me = this.me.clone();
        l.push(on(trigger, "click", move |_| {
            if let Some(b) = me.upgrade() {
                b.toggle();
            }
        }));
        let me = this.me.clone();
        l.push(on(trigger, "keydown", move |ev| {
            if vst_key(&ev) == "ArrowDown"
                && let Some(b) = me.upgrade()
            {
                vst_prevent(&ev);
                b.open();
            }
        }));
        *this.listeners.borrow_mut() = l;
        this
    }

    pub fn is_open(&self) -> bool {
        self.menu.borrow().as_ref().is_some_and(|m| !m.is_closed())
    }

    pub fn toggle(&self) {
        if self.is_open() {
            self.close();
        } else {
            self.open();
        }
    }

    pub fn open(&self) {
        if self.is_open() {
            return;
        }
        let mut opts = (self.make_opts)();
        let me = self.me.clone();
        let user_close = opts.on_close.take();
        opts.on_close = Some(Rc::new(move || {
            if let Some(b) = me.upgrade() {
                attr(&b.trigger, "aria-expanded", "false");
            }
            if let Some(f) = &user_close {
                f();
            }
        }));
        let menu = Menu::open(&self.env, Some(&self.trigger), None, (self.items)(), opts);
        if menu.is_some() {
            attr(&self.trigger, "aria-expanded", "true");
        }
        *self.menu.borrow_mut() = menu;
    }

    pub fn close(&self) {
        let menu = self.menu.borrow_mut().take();
        if let Some(m) = menu {
            m.close();
        }
    }

    /// Re-read the rows into the open menu (no-op while closed).
    pub fn refresh(&self) {
        if let Some(m) = self.menu.borrow().as_ref() {
            m.set_items((self.items)());
        }
    }

    pub fn destroy(&self) {
        self.close();
        let l: Vec<Listener> = self.listeners.borrow_mut().drain(..).collect();
        drop(l);
    }
}

// ── tooltip ──

/// The kit tooltip: a small label under the trigger after a short hover, or on keyboard
/// focus; gone on leave, blur, press and destroy.
pub struct Tooltip {
    env: Env,
    me: Weak<Tooltip>,
    trigger: Element,
    text: RefCell<String>,
    tip: RefCell<Option<Element>>,
    timer: Cell<Option<TimerId>>,
    listeners: RefCell<Vec<Listener>>,
}

impl Tooltip {
    pub fn new(env: &Env, trigger: &Element, text: &str) -> Rc<Tooltip> {
        let this = Rc::new_cyclic(|me| Tooltip {
            env: env.clone(),
            me: me.clone(),
            trigger: trigger.clone(),
            text: RefCell::new(text.to_string()),
            tip: RefCell::new(None),
            timer: Cell::new(None),
            listeners: RefCell::new(Vec::new()),
        });
        let mut l = Vec::new();
        for (name, show) in [
            ("pointerenter", true),
            ("focus", true),
            ("pointerleave", false),
            ("blur", false),
            ("pointerdown", false),
        ] {
            let me = this.me.clone();
            l.push(on(trigger, name, move |_| {
                if let Some(t) = me.upgrade() {
                    if show {
                        t.arm();
                    } else {
                        t.hide();
                    }
                }
            }));
        }
        *this.listeners.borrow_mut() = l;
        this
    }

    pub fn set_content(&self, text: &str) {
        *self.text.borrow_mut() = text.to_string();
        if let Some(tip) = self.tip.borrow().as_ref() {
            tip.set_text_content(Some(text));
        }
    }

    fn arm(&self) {
        if self.tip.borrow().is_some() || self.timer.get().is_some() {
            return;
        }
        let me = self.me.clone();
        let id = self.env.after(400.0, move || {
            if let Some(t) = me.upgrade() {
                t.timer.set(None);
                t.show();
            }
        });
        self.timer.set(Some(id));
    }

    fn show(&self) {
        let Some(host) = self.env.host() else { return };
        let text = self.text.borrow().clone();
        if text.is_empty() {
            return;
        }
        let tip = make(&self.env.doc, "div", "vela-st-tip");
        attr(&tip, "role", "tooltip");
        tip.set_text_content(Some(&text));
        let hr = host.get_bounding_client_rect();
        let r = self.trigger.get_bounding_client_rect();
        set_styles(
            &tip,
            &[
                ("left", &format!("{}px", r.left() - hr.left())),
                ("top", &format!("{}px", r.bottom() - hr.top() + 6.0)),
            ],
        );
        host.append_child(&tip).ok();
        *self.tip.borrow_mut() = Some(tip);
    }

    fn hide(&self) {
        if let Some(id) = self.timer.take() {
            self.env.platform.clear_timeout(id);
        }
        if let Some(tip) = self.tip.borrow_mut().take() {
            tip.remove();
        }
    }

    pub fn destroy(&self) {
        self.hide();
        let l: Vec<Listener> = self.listeners.borrow_mut().drain(..).collect();
        drop(l);
    }
}

// ── tabs ──

/// A tab strip (the kit's filled tabs without panels): `role=tablist` of `role=tab`.
pub struct Tabs {
    pub el: Element,
    env: Env,
    me: Weak<Tabs>,
    ids: RefCell<Vec<String>>,
    buttons: RefCell<Vec<Element>>,
    selected: RefCell<String>,
    on_change: Rc<dyn Fn(&str)>,
    listeners: RefCell<Vec<Listener>>,
}

impl Tabs {
    pub fn new(
        env: &Env,
        class: &str,
        items: &[(String, String)],
        selected: &str,
        on_change: impl Fn(&str) + 'static,
    ) -> Rc<Tabs> {
        let el = make(&env.doc, "div", &format!("vela-st-tabs {class}"));
        attr(&el, "role", "tablist");
        let this = Rc::new_cyclic(|me| Tabs {
            el,
            env: env.clone(),
            me: me.clone(),
            ids: RefCell::new(Vec::new()),
            buttons: RefCell::new(Vec::new()),
            selected: RefCell::new(selected.to_string()),
            on_change: Rc::new(on_change),
            listeners: RefCell::new(Vec::new()),
        });
        this.set_items(items);
        this
    }

    pub fn set_items(&self, items: &[(String, String)]) {
        let old: Vec<Listener> = self.listeners.borrow_mut().drain(..).collect();
        clear(&self.el);
        let mut buttons = Vec::new();
        let mut ids = Vec::new();
        let mut listeners = Vec::new();
        for (id, label) in items {
            let b = make(&self.env.doc, "button", "vela-st-tab");
            attr(&b, "type", "button");
            attr(&b, "role", "tab");
            attr(&b, "data-value", id);
            b.set_text_content(Some(label));
            let (me, id2) = (self.me.clone(), id.clone());
            listeners.push(on(&b, "click", move |_| {
                if let Some(t) = me.upgrade() {
                    t.select(&id2, true);
                }
            }));
            let (me, at) = (self.me.clone(), ids.len());
            listeners.push(on(&b, "keydown", move |ev| {
                let Some(t) = me.upgrade() else { return };
                let n = t.ids.borrow().len();
                let to = match vst_key(&ev).as_str() {
                    "ArrowRight" => (at + 1) % n,
                    "ArrowLeft" => (at + n - 1) % n,
                    _ => return,
                };
                vst_prevent(&ev);
                let id = t.ids.borrow()[to].clone();
                t.select(&id, true);
                if let Some(b) = t.buttons.borrow().get(to) {
                    vela_focus(b);
                }
            }));
            self.el.append_child(&b).ok();
            buttons.push(b);
            ids.push(id.clone());
        }
        *self.buttons.borrow_mut() = buttons;
        *self.ids.borrow_mut() = ids;
        *self.listeners.borrow_mut() = listeners;
        self.env.retire(Box::new(old));
        self.paint();
    }

    pub fn selected(&self) -> String {
        self.selected.borrow().clone()
    }

    pub fn select(&self, id: &str, notify: bool) {
        if *self.selected.borrow() == id {
            return;
        }
        *self.selected.borrow_mut() = id.to_string();
        self.paint();
        if notify {
            (self.on_change)(id);
        }
    }

    fn paint(&self) {
        let sel = self.selected.borrow().clone();
        let ids = self.ids.borrow();
        for (i, b) in self.buttons.borrow().iter().enumerate() {
            let on = ids.get(i).is_some_and(|id| *id == sel);
            attr(b, "aria-selected", if on { "true" } else { "false" });
            attr(b, "tabindex", if on { "0" } else { "-1" });
        }
    }

    pub fn destroy(&self) {
        let l: Vec<Listener> = self.listeners.borrow_mut().drain(..).collect();
        drop(l);
    }
}

// ── drawer (bottom sheet) ──

/// The mobile bottom sheet: a backdrop, a sheet with a handle, an optional title and a
/// body. A press on the backdrop or Escape closes it.
pub struct Drawer {
    env: Env,
    me: Weak<Drawer>,
    backdrop: Element,
    sheet: Element,
    title: Element,
    body: Element,
    open: Cell<bool>,
    perm: RefCell<Vec<Listener>>,
    live: RefCell<Vec<Listener>>,
    on_open_change: Option<Rc<dyn Fn(bool)>>,
}

impl Drawer {
    pub fn new(
        env: &Env,
        title: Option<&str>,
        on_open_change: Option<Rc<dyn Fn(bool)>>,
    ) -> Rc<Drawer> {
        let doc = &env.doc;
        let backdrop = make(doc, "div", "vela-st-drawer-backdrop");
        let sheet = make(doc, "div", "vela-st-drawer");
        attr(&sheet, "role", "dialog");
        attr(&sheet, "aria-modal", "true");
        let handle = make(doc, "div", "vela-st-drawer-handle");
        let title_el = make(doc, "div", "vela-st-drawer-title");
        let body = make(doc, "div", "vela-st-drawer-body");
        sheet.append_child(&handle).ok();
        if let Some(t) = title {
            title_el.set_text_content(Some(t));
            attr(&sheet, "aria-label", t);
            sheet.append_child(&title_el).ok();
        }
        sheet.append_child(&body).ok();
        let this = Rc::new_cyclic(|me| Drawer {
            env: env.clone(),
            me: me.clone(),
            backdrop: backdrop.clone(),
            sheet,
            title: title_el,
            body,
            open: Cell::new(false),
            perm: RefCell::new(Vec::new()),
            live: RefCell::new(Vec::new()),
            on_open_change,
        });
        let me = this.me.clone();
        *this.perm.borrow_mut() = vec![on(&backdrop, "pointerdown", move |_| {
            if let Some(d) = me.upgrade() {
                d.hide();
            }
        })];
        this
    }

    pub fn body(&self) -> &Element {
        &self.body
    }

    pub fn set_title(&self, title: &str) {
        self.title.set_text_content(Some(title));
        attr(&self.sheet, "aria-label", title);
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    pub fn show(&self) {
        if self.open.replace(true) {
            return;
        }
        let Some(host) = self.env.host() else {
            self.open.set(false);
            return;
        };
        host.append_child(&self.backdrop).ok();
        host.append_child(&self.sheet).ok();
        let me = self.me.clone();
        let target = doc_target(&self.env.doc);
        self.live
            .borrow_mut()
            .push(Listener::new(&target, "keydown", true, false, move |ev| {
                if vst_key(&ev) == "Escape"
                    && let Some(d) = me.upgrade()
                {
                    vst_prevent(&ev);
                    d.hide();
                }
            }));
        if let Some(f) = &self.on_open_change {
            f(true);
        }
    }

    pub fn hide(&self) {
        if !self.open.replace(false) {
            return;
        }
        self.backdrop.remove();
        self.sheet.remove();
        let live: Vec<Listener> = self.live.borrow_mut().drain(..).collect();
        self.env.retire(Box::new(live));
        if let Some(f) = &self.on_open_change {
            f(false);
        }
    }

    pub fn destroy(&self) {
        self.hide();
        self.backdrop.remove();
        self.sheet.remove();
        let perm: Vec<Listener> = self.perm.borrow_mut().drain(..).collect();
        drop(perm);
    }
}

// ── resize observer ──

/// A `ResizeObserver` on one element that disconnects when dropped.
pub struct Observer {
    handle: JsValue,
    _cb: wasm_bindgen::prelude::Closure<dyn FnMut()>,
}

impl Observer {
    pub fn new(el: &Element, f: impl FnMut() + 'static) -> Observer {
        let cb = wasm_bindgen::prelude::Closure::<dyn FnMut()>::new(f);
        let handle = crate::workspace_ui::js::vela_ws_observe(
            el,
            wasm_bindgen::JsCast::unchecked_ref(cb.as_ref()),
        );
        Observer { handle, _cb: cb }
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        crate::workspace_ui::js::vela_ws_disconnect(&self.handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_icons_are_resized_to_pixels() {
        let svg = icon_html_px("eye-off", 14);
        assert!(svg.contains("width=\"14\"") && svg.contains("height=\"14\""));
        assert!(!svg.contains("1em"));
        assert_eq!(icon_html_px("no-such-icon", 14), "");
    }

    #[test]
    fn the_registry_has_every_icon_the_shell_draws() {
        for id in [
            "search",
            "chevron-down",
            "chevron-right",
            "indicators",
            "undo",
            "redo",
            "camera",
            "bell",
            "gear",
            "trash",
            "check",
            "eye-off",
            "minus",
            "plus",
            "restore",
            "maximize",
            "reset",
            "grip",
            "replay",
            "star",
            "star-filled",
            "close",
            "market-open",
            "market-pre",
            "market-post",
            "market-extended",
            "market-closed",
            "market-holiday",
            "style-candles",
            "style-line",
        ] {
            assert!(!icons::icon(id).is_empty(), "icon {id} is missing");
        }
    }
}
