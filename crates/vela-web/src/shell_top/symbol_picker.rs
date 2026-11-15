//! The symbol search dialog (`symbol-picker.ts`) and the ticker icon (`symbol-icon.ts`).
//!
//! The logic (ranking, venue scopes, tabs, paging, groups, the highlight, the choice a pick
//! commits) is [`vela_core::widget::symbol_picker::SymbolPickerModel`]; this is its view: a
//! dialog with a search field, the asset-class tabs and the result list, the field
//! upper-casing what is typed, ArrowUp / ArrowDown / Enter on the field, the infinite
//! scroll, and a row per symbol with its icon, ticker, description, venue badge and (for a
//! futures root) the contracts expander.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use vela_core::ports::data_provider::SymbolDescriptor;
use vela_core::widget::contributions::SymbolRankingHook;
use vela_core::widget::symbol_icon::{IconFailures, TickerIcon, ticker_icon};
use vela_core::widget::symbol_picker::{
    SymbolPickerModel, SymbolSource, SymbolTab, avatar_color, search_placeholder,
    show_contracts_label, title_message,
};
use wasm_bindgen::JsValue;
use web_sys::Element;

use super::js::{
    is_el, vst_client_height, vst_closest_class, vst_ev_flag, vst_key, vst_prevent,
    vst_scroll_height, vst_scroll_into_view, vst_scroll_top, vst_set_value, vst_target, vst_value,
};
use super::kit::{Dialog, DialogOpts, Env, Tabs, clear, icon_el};
use crate::dom::{Listener, attr, make, on, set_style, vela_caret_end, vela_focus};

/// A ticker icon: the provider-resolved image (a failed one degrades to initials for good),
/// or a colored initials badge. `class` is the avatar class. The returned listener belongs
/// to an image's `error` event: keep it as long as the element.
pub fn ticker_icon_el(
    env: &Env,
    failures: &IconFailures,
    base: &str,
    name: &str,
    class: &str,
    icon_url: Option<&str>,
) -> (Element, Option<Listener>) {
    let doc = &env.doc;
    let el = make(doc, "span", class);
    attr(&el, "aria-hidden", "true");
    match ticker_icon(failures, base, name, icon_url) {
        TickerIcon::Image { url } => {
            let img = make(doc, "img", "");
            attr(&img, "src", &url);
            attr(&img, "crossorigin", "anonymous");
            attr(&img, "alt", "");
            attr(&img, "draggable", "false");
            el.append_child(&img).ok();
            let (failures, el2, base2, name2) = (
                failures.clone(),
                el.clone(),
                base.to_string(),
                name.to_string(),
            );
            let env2 = env.clone();
            let l = on(&img, "error", move |_| {
                failures.mark_failed(&url);
                clear(&el2);
                if let TickerIcon::Initials { text, background } =
                    vela_core::widget::symbol_icon::initials_badge(&base2, &name2)
                {
                    set_style(&el2, "background", background);
                    el2.set_text_content(Some(&text));
                }
                let _ = &env2;
            });
            (el, Some(l))
        }
        TickerIcon::Initials { text, background } => {
            set_style(&el, "background", background);
            el.set_text_content(Some(&text));
            (el, None)
        }
    }
}

pub struct SymbolPickerOpts {
    /// Called with the CHOSEN symbol, `VENUE:`-prefixed when the row named a venue.
    pub on_select: Rc<dyn Fn(&str)>,
    pub on_open_change: Rc<dyn Fn(bool)>,
    pub source: SymbolSource,
    /// The row's icon URL, routed to the symbol's owning provider.
    pub icon_for: Rc<dyn Fn(&SymbolDescriptor) -> Option<String>>,
    pub ranking: Rc<dyn Fn() -> Option<SymbolRankingHook>>,
}

pub struct SymbolPicker {
    env: Env,
    me: Weak<SymbolPicker>,
    opts: SymbolPickerOpts,
    model: SymbolPickerModel,
    dialog: Rc<Dialog>,
    input: Element,
    list: Element,
    tabs: Rc<Tabs>,
    failures: IconFailures,
    perm: RefCell<Vec<Listener>>,
    rows_l: RefCell<Vec<Listener>>,
}

fn tab_items(env: &Env) -> Vec<(String, String)> {
    SymbolTab::ALL
        .iter()
        .map(|t| (t.id().to_string(), env.t(&t.label())))
        .collect()
}

impl SymbolPicker {
    pub fn new(env: &Env, opts: SymbolPickerOpts) -> Rc<SymbolPicker> {
        let doc = &env.doc;
        let input = make(doc, "input", "vela-sp-input");
        attr(&input, "type", "text");
        attr(&input, "spellcheck", "false");
        attr(&input, "autocomplete", "off");
        attr(&input, "aria-label", &env.t(&title_message()));
        attr(&input, "placeholder", &env.t(&search_placeholder()));
        let search_row = make(doc, "div", "vela-sp-searchrow");
        search_row.append_child(&icon_el(doc, "search")).ok();
        search_row.append_child(&input).ok();
        let list = make(doc, "div", "vela-sp-list");
        attr(&list, "role", "listbox");
        let head = make(doc, "div", "vela-sp-head");
        let this = Rc::new_cyclic(|me: &Weak<SymbolPicker>| {
            let m = me.clone();
            let model =
                SymbolPickerModel::new(env.platform.clone(), opts.ranking.clone(), move || {
                    if let Some(p) = m.upgrade() {
                        p.paint();
                    }
                });
            model.set_source(opts.source.clone());
            let m = me.clone();
            let tabs = Tabs::new(
                env,
                "vela-sp-tabs",
                &tab_items(env),
                SymbolTab::All.id(),
                move |id| {
                    if let (Some(p), Some(tab)) = (m.upgrade(), SymbolTab::from_id(id)) {
                        p.model.set_tab(tab);
                        p.paint();
                    }
                },
            );
            let m = me.clone();
            let on_open = opts.on_open_change.clone();
            let dialog = Dialog::new(
                env,
                DialogOpts {
                    title: env.t(&title_message()),
                    flush: true,
                    draggable: false,
                    on_open_change: Some(Rc::new(move |open| {
                        if let Some(p) = m.upgrade() {
                            p.opened(open);
                        }
                        on_open(open);
                    })),
                },
            );
            SymbolPicker {
                env: env.clone(),
                me: me.clone(),
                opts,
                model,
                dialog,
                input: input.clone(),
                list: list.clone(),
                tabs,
                failures: IconFailures::new(),
                perm: RefCell::new(Vec::new()),
                rows_l: RefCell::new(Vec::new()),
            }
        });
        head.append_child(&search_row).ok();
        head.append_child(&this.tabs.el).ok();
        this.dialog.body().append_child(&head).ok();
        this.dialog.body().append_child(&list).ok();
        let mut perm = Vec::new();
        let me = this.me.clone();
        perm.push(on(&input, "input", move |_| {
            let Some(p) = me.upgrade() else { return };
            let value = vst_value(&p.input);
            let upper = value.to_uppercase();
            if upper != value {
                // Writing the value moves the caret to the end: put it back (the original
                // keeps the selection range across the upper-casing).
                let sel = super::js::selection_of(&p.input);
                vst_set_value(&p.input, &upper);
                if let Some((start, end)) = sel {
                    super::js::vst_set_selection(&p.input, start, end);
                }
            }
            p.model.set_query(&upper);
            p.paint();
        }));
        let me = this.me.clone();
        perm.push(on(&input, "keydown", move |ev| {
            let Some(p) = me.upgrade() else { return };
            match vst_key(&ev).as_str() {
                "ArrowDown" => {
                    vst_prevent(&ev);
                    p.model.move_highlight(1);
                    p.paint_highlight();
                }
                "ArrowUp" => {
                    vst_prevent(&ev);
                    p.model.move_highlight(-1);
                    p.paint_highlight();
                }
                "Enter" => {
                    if vst_ev_flag(&ev, "isComposing") {
                        return;
                    }
                    if let Some(symbol) = p.model.pick_highlighted() {
                        p.commit(&symbol);
                    }
                }
                _ => {}
            }
        }));
        let me = this.me.clone();
        perm.push(on(&list, "click", move |ev| {
            let Some(p) = me.upgrade() else { return };
            p.clicked(&ev);
        }));
        // Infinite scroll: nearing the bottom grows the page and APPENDS the new rows.
        let me = this.me.clone();
        perm.push(on(&list, "scroll", move |_| {
            let Some(p) = me.upgrade() else { return };
            if p.model.exhausted() {
                return;
            }
            if vst_scroll_top(&p.list) + vst_client_height(&p.list)
                < vst_scroll_height(&p.list) - 200.0
            {
                return;
            }
            let more = p.model.grow();
            p.append_rows(&more);
        }));
        *this.perm.borrow_mut() = perm;
        this
    }

    pub fn is_open(&self) -> bool {
        self.dialog.is_open()
    }

    /// Open the dialog, its field seeded with `query`.
    pub fn open(&self, query: &str) {
        self.model.set_seed(query);
        self.dialog.show();
    }

    pub fn close(&self) {
        self.dialog.hide();
    }

    /// Text typed at the picker that never reached its field: closed, it opens seeded with
    /// the text; open, it extends the query.
    pub fn type_text(&self, text: &str) {
        if !self.is_open() {
            self.open(text);
            return;
        }
        let next = self.model.append_typed(text);
        vst_set_value(&self.input, &next);
        self.paint();
    }

    /// The dialog's fixed text in the shell's language.
    pub fn relabel(&self) {
        self.dialog.set_title(&self.env.t(&title_message()));
        attr(
            &self.input,
            "placeholder",
            &self.env.t(&search_placeholder()),
        );
        attr(&self.input, "aria-label", &self.env.t(&title_message()));
        self.tabs.set_items(&tab_items(&self.env));
    }

    fn opened(&self, open: bool) {
        if open {
            self.relabel();
            let query = self.model.on_open();
            vst_set_value(&self.input, &query);
            self.paint();
            let me = self.me.clone();
            self.env.after(0.0, move || {
                if let Some(p) = me.upgrade()
                    && p.is_open()
                {
                    vela_focus(&p.input);
                    vela_caret_end(&p.input);
                }
            });
        } else {
            super::js::vst_blur(&self.input);
        }
    }

    fn commit(&self, symbol: &str) {
        self.close();
        (self.opts.on_select)(symbol);
    }

    fn clicked(&self, ev: &JsValue) {
        let target = vst_target(ev);
        let row = vst_closest_class(&target, "vela-sp-row");
        if row.is_null() {
            return;
        }
        let row: Element = wasm_bindgen::JsCast::unchecked_into(row);
        let Some(i) = row
            .get_attribute("data-i")
            .and_then(|v| v.parse::<usize>().ok())
        else {
            return;
        };
        let Some(s) = self.model.rows().get(i).cloned() else {
            return;
        };
        let expander = vst_closest_class(&target, "vela-sp-expander");
        if !expander.is_null() {
            self.model.toggle_expand(&s);
            let top = vst_scroll_top(&self.list);
            self.paint_rows();
            super::js::vst_set_scroll_top(&self.list, top);
        } else {
            let symbol = self.model.pick(&s);
            self.commit(&symbol);
        }
    }

    /// Repaint the list from the model (a new query, tab or ranked pool).
    fn paint(&self) {
        self.paint_rows();
    }

    fn paint_rows(&self) {
        let old: Vec<Listener> = self.rows_l.borrow_mut().drain(..).collect();
        clear(&self.list);
        self.env.retire(Box::new(old));
        let rows = self.model.rows();
        if rows.is_empty() {
            let empty = make(&self.env.doc, "div", "vela-sp-empty");
            let tr = self.env.translator();
            empty.set_text_content(Some(&self.model.empty_message(tr.as_deref())));
            self.list.append_child(&empty).ok();
            return;
        }
        self.append_rows_from(&rows, 0);
        self.paint_highlight();
    }

    fn append_rows(&self, more: &[SymbolDescriptor]) {
        let at = self.model.rows().len().saturating_sub(more.len());
        self.append_rows_from(more, at);
    }

    fn append_rows_from(&self, rows: &[SymbolDescriptor], first: usize) {
        for (j, s) in rows.iter().enumerate() {
            let (el, l) = self.row_el(s, first + j);
            self.list.append_child(&el).ok();
            if let Some(l) = l {
                self.rows_l.borrow_mut().push(l);
            }
        }
    }

    fn paint_highlight(&self) {
        let at = self.model.highlighted();
        let mut i = 0;
        let mut child = self.list.first_element_child();
        while let Some(el) = child {
            if i == at {
                attr(&el, "data-highlighted", "");
                attr(&el, "aria-selected", "true");
                vst_scroll_into_view(&el);
            } else {
                let _ = el.remove_attribute("data-highlighted");
                let _ = el.remove_attribute("aria-selected");
            }
            i += 1;
            child = el.next_element_sibling();
        }
    }

    fn row_el(&self, s: &SymbolDescriptor, i: usize) -> (Element, Option<Listener>) {
        let doc = &self.env.doc;
        let row = make(doc, "div", "vela-sp-row");
        attr(&row, "role", "option");
        attr(&row, "data-i", &i.to_string());
        attr(&row, "data-ticker", &s.ticker);
        let venue = s.prefix.as_deref().or(s.provider.as_deref());
        if let Some(v) = venue {
            attr(&row, "data-venue", v);
        }
        let base = vela_core::data::base_of(s);
        let url = (self.opts.icon_for)(s);
        let (avatar, listener) = ticker_icon_el(
            &self.env,
            &self.failures,
            &base,
            &s.ticker,
            "vela-sp-avatar",
            url.as_deref(),
        );
        let _ = avatar_color;
        let main = make(doc, "span", "vela-sp-main");
        let t = make(doc, "span", "vela-sp-ticker");
        t.set_text_content(Some(&s.ticker));
        let d = make(doc, "span", "vela-sp-desc");
        d.set_text_content(Some(
            s.description.as_deref().or(s.kind.as_deref()).unwrap_or(""),
        ));
        main.append_child(&t).ok();
        main.append_child(&d).ok();
        row.append_child(&avatar).ok();
        row.append_child(&main).ok();
        if vela_core::data::is_group_row(s) {
            attr(&row, "data-group", "1");
            let expander = make(doc, "button", "vela-sp-expander");
            attr(&expander, "type", "button");
            attr(
                &expander,
                "aria-label",
                &self.env.t(&show_contracts_label()),
            );
            let open = self.model.is_expanded(s);
            attr(
                &expander,
                "aria-expanded",
                if open { "true" } else { "false" },
            );
            expander
                .append_child(&icon_el(
                    doc,
                    if open {
                        "chevron-down"
                    } else {
                        "chevron-right"
                    },
                ))
                .ok();
            row.append_child(&expander).ok();
        } else if s.group.is_some() && self.model.is_expanded(s) {
            attr(&row, "data-member", "1");
        }
        if let Some(v) = venue {
            let badge = make(doc, "span", "vela-sp-badge");
            attr(&badge, "data-p", s.provider.as_deref().unwrap_or(v));
            badge.set_text_content(Some(v));
            row.append_child(&badge).ok();
        }
        (row, listener)
    }

    pub fn destroy(&self) {
        self.dialog.destroy();
        self.tabs.destroy();
        let l: Vec<Listener> = self.perm.borrow_mut().drain(..).collect();
        drop(l);
        let l: Vec<Listener> = self.rows_l.borrow_mut().drain(..).collect();
        drop(l);
        let _ = is_el;
    }
}
