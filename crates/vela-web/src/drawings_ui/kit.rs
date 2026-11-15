//! The widgets of the settings panel: its blocks, rows, segmented buttons, chips, folds and
//! the thin wrappers that put the dialogs' native controls (`crate::dialogs::controls`) on
//! them. The original builds these from `vela-ui`'s field kit and its own `row` / `seg` /
//! `chip` / `fold` helpers (`DrawingSettingsPanel.ts`); here they are native elements over
//! the same shared stylesheet.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use serde_json::Value;
use vela_core::i18n::MessageLike;
use web_sys::Element;

use super::glyphs;
use super::model::Snap;
use super::session::Core;
use super::util;
use crate::dialogs::controls::{self, Control, NumOpts, SelOpt};
use crate::dialogs::{Cx, Tr};
use crate::dom::{Listener, attr};

thread_local! {
    /// Which folds are open, per tool: remembered for the session so a panel reopens as it
    /// was left.
    static OPEN_FOLDS: RefCell<HashMap<String, BTreeSet<String>>> = RefCell::new(HashMap::new());
}

/// One button of a segmented control.
pub struct SegOpt {
    pub value: String,
    pub html: String,
    pub title: Option<String>,
}

impl SegOpt {
    pub fn new(value: impl Into<String>, html: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            html: html.into(),
            title: None,
        }
    }

    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
}

/// The mutable bits one panel build shares with its controls: the original's `Build`.
#[derive(Clone)]
pub struct Build {
    pub core: Rc<Core>,
    /// The listener bag of this build (freed on a rebuild).
    pub cx: Cx,
    syncs: Rc<RefCell<Vec<Rc<dyn Fn()>>>>,
    bags: Rc<RefCell<Vec<Rc<RefCell<Vec<Listener>>>>>>,
}

impl Build {
    pub fn new(
        core: Rc<Core>,
        cx: Cx,
        syncs: Rc<RefCell<Vec<Rc<dyn Fn()>>>>,
        bags: Rc<RefCell<Vec<Rc<RefCell<Vec<Listener>>>>>>,
    ) -> Self {
        Self {
            core,
            cx,
            syncs,
            bags,
        }
    }

    /// A context with a listener bag of its own, freed with the build (a part of the panel that
    /// rebuilds itself, like the level grid).
    pub fn sub_cx(&self) -> Cx {
        let cx = self.cx.sub();
        self.bags.borrow_mut().push(cx.bag.clone());
        cx
    }

    pub fn tr(&self) -> Tr {
        self.cx.tr.clone()
    }

    pub fn t(&self, id: &'static str, en: &'static str) -> String {
        self.cx.tr.t(id, en)
    }

    pub fn m(&self, message: &MessageLike) -> String {
        self.cx.tr.m(message)
    }

    /// Read the live drawing (every edit replaces it, so a control reads it per use).
    pub fn live<R>(&self, f: impl FnOnce(&Snap) -> R) -> Option<R> {
        self.core.with_first(f)
    }

    /// Register a refresher, run now and after every model change.
    pub fn watch(&self, f: impl Fn() + 'static) {
        f();
        self.syncs.borrow_mut().push(Rc::new(f));
    }

    /// Register a control's own re-read, skipped while its field has focus (a field being
    /// typed in is not overwritten).
    pub fn watch_control(&self, control: &Control) {
        let (sync, el) = (control.sync.clone(), control.el.clone());
        self.syncs.borrow_mut().push(Rc::new(move || {
            if !super::popup::active_is(&el) {
                sync();
            }
        }));
    }

    /// Apply a settings patch; whatever reads from the drawing refreshes from a timer.
    pub fn edit(&self, items: &[(&str, Value)]) {
        self.core.patch(items);
    }

    pub fn uid(&self, prefix: &str) -> String {
        self.cx.uid(prefix)
    }

    // ── structure ──

    /// A titled group of rows.
    pub fn block(&self, title: Option<&str>, rows: Vec<Element>) -> Element {
        let el = self.cx.el("div", "vela-dsp-block");
        if let Some(title) = title {
            let h = self.cx.text("div", "vela-dsp-h", title);
            attr(&h, "role", "heading");
            attr(&h, "aria-level", "3");
            let _ = el.append_child(&h);
        }
        for r in rows {
            let _ = el.append_child(&r);
        }
        el
    }

    /// A label and its controls on one line.
    pub fn row(&self, label: &str, controls: &[Element]) -> Element {
        let el = self.cx.el("div", "vela-dsp-row");
        let lab = self.cx.text("span", "vela-dsp-lab", label);
        attr(&lab, "title", label);
        let _ = el.append_child(&lab);
        for c in controls {
            let _ = el.append_child(c);
        }
        el
    }

    pub fn chips(&self, items: &[Element]) -> Element {
        let el = self.cx.el("div", "vela-dsp-chips");
        attr(&el, "role", "group");
        for i in items {
            let _ = el.append_child(i);
        }
        el
    }

    // ── controls ──

    /// A segmented control: one pressed button reads the current value.
    pub fn seg(
        &self,
        label: &str,
        options: Vec<SegOpt>,
        read: Rc<dyn Fn() -> Option<String>>,
        pick: Rc<dyn Fn(&str)>,
        full: bool,
    ) -> Element {
        let el = self.cx.el("div", "vela-dsp-seg");
        attr(&el, "role", "group");
        attr(&el, "aria-label", label);
        if full {
            attr(&el, "data-full", "");
        }
        let mut buttons: Vec<(String, Element)> = Vec::new();
        let sync: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        for o in options {
            let btn = self.cx.el("button", "");
            attr(&btn, "type", "button");
            // A glyph is markup of ours; a label is text (never parsed as markup).
            if o.html.starts_with("<svg") {
                btn.set_inner_html(&o.html);
            } else {
                btn.set_text_content(Some(&o.html));
            }
            if let Some(title) = &o.title {
                attr(&btn, "title", title);
                attr(&btn, "aria-label", title);
            }
            let (value, pick, sync) = (o.value.clone(), pick.clone(), sync.clone());
            self.cx.on(&btn, "click", move |_| {
                pick(&value);
                if let Some(s) = sync.borrow().clone() {
                    s();
                }
            });
            let _ = el.append_child(&btn);
            buttons.push((o.value, btn));
        }
        let paint: Rc<dyn Fn()> = Rc::new(move || {
            let cur = read();
            for (value, btn) in &buttons {
                util::set_pressed(btn, cur.as_deref() == Some(value.as_str()));
            }
        });
        *sync.borrow_mut() = Some(paint.clone());
        self.watch(move || paint());
        el
    }

    /// A toggle chip (arrows, bold, italic, the level flags).
    pub fn chip(
        &self,
        text: &str,
        read: Rc<dyn Fn() -> bool>,
        set: Rc<dyn Fn(bool)>,
        title: Option<&str>,
    ) -> Element {
        let btn = self.cx.text("button", "vela-dsp-chip", text);
        attr(&btn, "type", "button");
        if let Some(title) = title {
            attr(&btn, "title", title);
            attr(&btn, "aria-label", title);
        }
        if text == "B" {
            crate::dom::set_style(&btn, "font-weight", "700");
        }
        if text == "I" {
            crate::dom::set_style(&btn, "font-style", "italic");
        }
        let read2 = read.clone();
        self.cx.on(&btn, "click", move |_| set(!read2()));
        let el = btn.clone();
        self.watch(move || util::set_pressed(&el, read()));
        btn
    }

    /// A color field (swatch and opacity), committing on release.
    pub fn swatch(&self, title: &str, read: Rc<dyn Fn() -> String>, path: String) -> Element {
        let core = self.core.clone();
        let c = controls::color_field(
            &self.cx,
            &self.uid("vela-dsp-color"),
            title,
            true,
            read,
            Rc::new(move |v| core.patch(&[(path.as_str(), Value::String(v))])),
        );
        self.watch_control(&c);
        c.el
    }

    /// A color field that reports through `on_change` (a level's color).
    pub fn color_with(
        &self,
        title: &str,
        read: Rc<dyn Fn() -> String>,
        on_change: Rc<dyn Fn(String)>,
    ) -> Element {
        let c = controls::color_field(
            &self.cx,
            &self.uid("vela-dsp-color"),
            title,
            true,
            read,
            on_change,
        );
        self.watch_control(&c);
        c.el
    }

    /// An on / off switch at the row's end.
    pub fn switch(&self, aria: &str, read: Rc<dyn Fn() -> bool>, set: Rc<dyn Fn(bool)>) -> Element {
        let c = controls::switch(&self.cx, &self.uid("vela-dsp-switch"), aria, read, set);
        self.watch_control(&c);
        let wrap = self.cx.el("div", "vela-dsp-end");
        let _ = wrap.append_child(&c.el);
        wrap
    }

    /// A bare switch with no wrapper (a level's on / off).
    pub fn bare_switch(
        &self,
        aria: &str,
        read: Rc<dyn Fn() -> bool>,
        set: Rc<dyn Fn(bool)>,
    ) -> Control {
        let c = controls::switch(&self.cx, &self.uid("vela-dsp-switch"), aria, read, set);
        self.watch_control(&c);
        c
    }

    /// A number field committing on blur or Enter. `step` and `integer` are the field's own:
    /// without them it steps by 0.1 and keeps decimals (a price must not be rounded).
    #[allow(clippy::too_many_arguments)]
    pub fn number(
        &self,
        title: &str,
        read: Rc<dyn Fn() -> f64>,
        min: Option<f64>,
        max: Option<f64>,
        step: Option<f64>,
        integer: bool,
        on_change: Rc<dyn Fn(f64)>,
    ) -> Control {
        let c = controls::number(
            &self.cx,
            &self.uid("vela-dsp-num"),
            NumOpts {
                min,
                max,
                step,
                integer,
                aria: title.to_string(),
                ..Default::default()
            },
            read,
            on_change,
        );
        self.watch_control(&c);
        c
    }

    /// A native dropdown.
    pub fn select(
        &self,
        aria: &str,
        options: &[SelOpt],
        read: Rc<dyn Fn() -> String>,
        on_change: Rc<dyn Fn(String)>,
    ) -> Control {
        let c = controls::select(
            &self.cx,
            &self.uid("vela-dsp-select"),
            aria,
            options,
            read,
            on_change,
        );
        self.watch_control(&c);
        c
    }

    /// A multi-line text field that commits when it loses focus (one edit for the words, not
    /// one per key) and grows with its content. Typing never reaches the chart's shortcuts
    /// (Escape still cancels the panel).
    pub fn text_area(&self, placeholder: &str, rows: u32, read: Rc<dyn Fn() -> String>) -> Element {
        let core = self.core.clone();
        let c = controls::textarea(
            &self.cx,
            &self.uid("vela-dsp-text"),
            placeholder,
            read,
            Rc::new(move |v| core.patch(&[("text.value", Value::String(v))])),
        );
        attr(&c.el, "rows", &rows.to_string());
        attr(&c.el, "placeholder", placeholder);
        crate::dom::set_style(&c.el, "min-height", &format!("{}px", 14 + rows * 18));
        util::auto_grow(&c.el, 6);
        {
            let el = c.el.clone();
            self.cx.on(&c.el, "input", move |_| util::auto_grow(&el, 6));
        }
        self.cx.on(&c.el, "keydown", move |ev| {
            if crate::dom::get_str(&ev, "key") != "Escape" {
                crate::dom::stop_propagation(&ev);
            }
        });
        {
            // A refresh re-reads the words (never under the field being typed in) and re-fits.
            let (sync, el) = (c.sync.clone(), c.el.clone());
            self.syncs.borrow_mut().push(Rc::new(move || {
                if !super::popup::active_is(&el) {
                    sync();
                    util::auto_grow(&el, 6);
                }
            }));
        }
        c.el
    }

    /// A fold: a one-line header saying what is set, opening to its rows (built on first
    /// open). Open state is remembered per tool for the session.
    pub fn fold(
        &self,
        key: &'static str,
        title: &str,
        summary: Rc<dyn Fn() -> String>,
        body: impl FnOnce(&Build) -> Vec<Element> + 'static,
    ) -> Element {
        let tool = self.live(|s| s.type_key().to_string()).unwrap_or_default();
        let is_open = {
            let tool = tool.clone();
            move || OPEN_FOLDS.with(|m| m.borrow().get(&tool).is_some_and(|s| s.contains(key)))
        };
        let wrap = self.cx.el("div", "vela-dsp-fold");
        let head = self.cx.el("button", "vela-dsp-fold-h");
        attr(&head, "type", "button");
        let t = self.cx.text("span", "vela-dsp-fold-t", title);
        let s = self.cx.el("span", "vela-dsp-fold-s");
        let chev = self.cx.el("span", "vela-dsp-chev");
        chev.set_inner_html(glyphs::CHEVRON);
        let _ = head.append_child(&t);
        let _ = head.append_child(&s);
        let _ = head.append_child(&chev);
        let content = self.cx.el("div", "vela-dsp-fold-b");
        let body: Rc<RefCell<Option<Box<dyn FnOnce(&Build) -> Vec<Element>>>>> =
            Rc::new(RefCell::new(Some(Box::new(body))));
        let set_open: Rc<dyn Fn(bool)> = {
            let (wrap, head, content, b, tool) = (
                wrap.clone(),
                head.clone(),
                content.clone(),
                self.clone(),
                tool.clone(),
            );
            Rc::new(move |on: bool| {
                OPEN_FOLDS.with(|m| {
                    let mut m = m.borrow_mut();
                    let set = m.entry(tool.clone()).or_default();
                    if on {
                        set.insert(key.to_string());
                    } else {
                        set.remove(key);
                    }
                });
                util::set_flag(&wrap, "data-open", on);
                attr(&head, "aria-expanded", if on { "true" } else { "false" });
                if on && let Some(build) = body.borrow_mut().take() {
                    for el in build(&b) {
                        let _ = content.append_child(&el);
                    }
                }
            })
        };
        {
            let (set_open, is_open) = (set_open.clone(), is_open.clone());
            self.cx.on(&head, "click", move |_| set_open(!is_open()));
        }
        {
            let s = s.clone();
            self.watch(move || s.set_text_content(Some(&summary())));
        }
        set_open(is_open());
        let _ = wrap.append_child(&head);
        let _ = wrap.append_child(&content);
        wrap
    }
}
