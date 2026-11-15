//! The indicator picker dialog (`indicator-picker.ts`): a search field, the "On chart"
//! card with a trash per live instance, and the library grouped by category.
//!
//! The layout (matching, grouping, the empty states) is
//! [`vela_core::widget::indicator_picker::indicator_picker_view`]; this is its view and
//! the click routing: a row adds a library entry (repeatable), the trash removes an
//! instance, both followed by a refresh.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vela_core::widget::indicator_picker::{
    IndexedRow, IndicatorPickerView, IndicatorRow, beta_label, indicator_picker_view,
    instance_badge, remove_label, search_placeholder, title_message,
};
use wasm_bindgen::JsValue;
use web_sys::Element;

use super::js::{vst_closest_class, vst_set_value, vst_target, vst_value};
use super::kit::{Dialog, DialogOpts, Env, clear, icon_el};
use crate::dom::{Listener, attr, make, on, vela_focus};

pub struct IndicatorPickerOpts {
    /// The manifest library: clicking a row ADDS an instance (repeatable).
    pub library: Rc<dyn Fn() -> Vec<IndicatorRow>>,
    /// The live instances on the chart: the trash removes one.
    pub on_chart: Rc<dyn Fn() -> Vec<IndicatorRow>>,
    pub on_add: Rc<dyn Fn(usize)>,
    pub on_remove: Rc<dyn Fn(usize)>,
    pub on_open_change: Rc<dyn Fn(bool)>,
}

pub struct IndicatorPicker {
    env: Env,
    me: Weak<IndicatorPicker>,
    opts: IndicatorPickerOpts,
    dialog: Rc<Dialog>,
    search: Element,
    list: Element,
    open: Cell<bool>,
    perm: RefCell<Vec<Listener>>,
}

impl IndicatorPicker {
    pub fn new(env: &Env, opts: IndicatorPickerOpts) -> Rc<IndicatorPicker> {
        let doc = &env.doc;
        let search = make(doc, "input", "vela-ip-search");
        attr(&search, "type", "text");
        attr(&search, "spellcheck", "false");
        attr(&search, "autocomplete", "off");
        attr(&search, "placeholder", &env.t(&search_placeholder()));
        attr(&search, "aria-label", &env.t(&title_message()));
        let row = make(doc, "div", "vela-ip-searchrow");
        row.append_child(&icon_el(doc, "search")).ok();
        row.append_child(&search).ok();
        let list = make(doc, "div", "vela-ip-list");
        let this = Rc::new_cyclic(|me: &Weak<IndicatorPicker>| {
            let m = me.clone();
            let on_open = opts.on_open_change.clone();
            let dialog = Dialog::new(
                env,
                DialogOpts {
                    title: env.t(&title_message()),
                    flush: false,
                    draggable: true,
                    on_open_change: Some(Rc::new(move |open| {
                        if let Some(p) = m.upgrade() {
                            p.opened(open);
                        }
                        on_open(open);
                    })),
                },
            );
            IndicatorPicker {
                env: env.clone(),
                me: me.clone(),
                opts,
                dialog,
                search: search.clone(),
                list: list.clone(),
                open: Cell::new(false),
                perm: RefCell::new(Vec::new()),
            }
        });
        this.dialog.body().append_child(&row).ok();
        this.dialog.body().append_child(&list).ok();
        let me = this.me.clone();
        let l1 = on(&search, "input", move |_| {
            if let Some(p) = me.upgrade() {
                p.refresh();
            }
        });
        let me = this.me.clone();
        let l2 = on(&list, "click", move |ev| {
            if let Some(p) = me.upgrade() {
                p.clicked(&ev);
            }
        });
        // The rows are focusable (below): Enter / Space on a row itself acts like its click.
        let me = this.me.clone();
        let l3 = on(&list, "keydown", move |ev| {
            let k = super::js::vst_key(&ev);
            if k != "Enter" && k != " " {
                return;
            }
            let target = vst_target(&ev);
            let row = vst_closest_class(&target, "vela-ip-row");
            if row.is_null() || row != target {
                return; // a key on the trash button is that button's own click
            }
            super::js::vst_prevent(&ev);
            if let Some(p) = me.upgrade() {
                p.clicked(&ev);
            }
        });
        *this.perm.borrow_mut() = vec![l1, l2, l3];
        this
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    pub fn open(&self) {
        self.dialog.show();
    }

    pub fn close(&self) {
        self.dialog.hide();
    }

    /// Re-render the lists if the dialog is open (async catalog updates land late).
    pub fn sync(&self) {
        if self.open.get() {
            self.refresh();
        }
    }

    /// The dialog's fixed text in the shell's language, re-read on every open.
    pub fn relabel(&self) {
        self.dialog.set_title(&self.env.t(&title_message()));
        attr(
            &self.search,
            "placeholder",
            &self.env.t(&search_placeholder()),
        );
        attr(&self.search, "aria-label", &self.env.t(&title_message()));
    }

    fn opened(&self, open: bool) {
        self.open.set(open);
        if open {
            vst_set_value(&self.search, "");
            self.relabel();
            self.refresh();
            // Desktop only: focusing the field on a touch device would pop the keyboard
            // over the just-opened fullscreen picker.
            if !self.env.mobile() {
                let me = self.me.clone();
                self.env.after(0.0, move || {
                    if let Some(p) = me.upgrade()
                        && p.open.get()
                    {
                        vela_focus(&p.search);
                    }
                });
            }
        }
    }

    fn clicked(&self, ev: &JsValue) {
        let target = vst_target(ev);
        let row = vst_closest_class(&target, "vela-ip-row");
        if row.is_null() {
            return;
        }
        let trash = !vst_closest_class(&target, "vela-ip-trash").is_null();
        let row: Element = wasm_bindgen::JsCast::unchecked_into(row);
        if trash {
            if let Some(i) = row
                .get_attribute("data-instance")
                .and_then(|v| v.parse().ok())
            {
                (self.opts.on_remove)(i);
            }
        } else if let Some(i) = row
            .get_attribute("data-library")
            .and_then(|v| v.parse().ok())
        {
            (self.opts.on_add)(i);
        }
        self.refresh();
    }

    fn row_el(&self, r: &IndexedRow, library: bool) -> Element {
        let doc = &self.env.doc;
        let row = make(doc, "div", "vela-ip-row");
        if library {
            // A library row adds its indicator; an on-chart row's action is its trash.
            attr(&row, "role", "button");
            attr(&row, "tabindex", "0");
        }
        attr(
            &row,
            if library {
                "data-library"
            } else {
                "data-instance"
            },
            &r.index.to_string(),
        );
        if r.row.native {
            attr(&row, "data-native", "1");
        }
        let name = make(doc, "span", "vela-ip-name");
        name.set_text_content(Some(&r.row.name));
        row.append_child(&name).ok();
        if r.row.beta {
            let beta = make(doc, "span", "vela-ip-badge");
            beta.set_text_content(Some(&self.env.t(&beta_label())));
            row.append_child(&beta).ok();
        }
        if !library {
            if let Some(text) = instance_badge(&r.row) {
                let badge = make(doc, "span", "vela-ip-badge");
                badge.set_text_content(Some(text));
                row.append_child(&badge).ok();
            }
            let trash = make(doc, "button", "vela-ip-trash");
            attr(&trash, "type", "button");
            let label = self.env.t(&remove_label());
            attr(&trash, "title", &label);
            attr(&trash, "aria-label", &label);
            trash.append_child(&icon_el(doc, "trash")).ok();
            row.append_child(&trash).ok();
        }
        row
    }

    fn refresh(&self) {
        let query = vst_value(&self.search);
        let tr = self.env.translator();
        let view: IndicatorPickerView = indicator_picker_view(
            &(self.opts.library)(),
            &(self.opts.on_chart)(),
            &query,
            tr.as_deref(),
        );
        let doc = &self.env.doc;
        clear(&self.list);
        if let Some(card) = &view.on_chart {
            let el = make(doc, "div", "vela-ip-oncard");
            let title = make(doc, "div", "vela-ip-group");
            title.set_text_content(Some(&card.title));
            el.append_child(&title).ok();
            for r in &card.rows {
                el.append_child(&self.row_el(r, false)).ok();
            }
            self.list.append_child(&el).ok();
        }
        for group in &view.groups {
            let title = make(doc, "div", "vela-ip-group");
            title.set_text_content(Some(&group.title));
            self.list.append_child(&title).ok();
            for r in &group.rows {
                self.list.append_child(&self.row_el(r, true)).ok();
            }
        }
        if let Some(text) = &view.empty {
            let empty = make(doc, "div", "vela-ip-empty");
            empty.set_text_content(Some(text));
            self.list.append_child(&empty).ok();
        }
    }

    pub fn destroy(&self) {
        self.dialog.destroy();
        let l: Vec<Listener> = self.perm.borrow_mut().drain(..).collect();
        drop(l);
    }
}
