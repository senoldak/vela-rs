//! The shortcuts panel (`shortcuts-help.ts`): a dialog listing every registered keymap
//! binding grouped by category with platform-formatted chords, then the pointer gestures and
//! the type-to-act note. The content is `shortcuts_help_sections` over the live keymap (so a
//! plugin's bindings appear), read again each time the dialog opens.
//!
//! The dialog frame is the kit's `Dialog` rebuilt natively: a backdrop over the root with a
//! titled panel (`role=dialog`, `aria-modal`, `aria-labelledby`), a close button, Escape and
//! an outside press close it, and focus returns to what had it. While open it pushes the
//! keymap's `dialog` scope so chart shortcuts stand down.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vela_core::widget::shortcuts_help::{shortcuts_help_sections, title_message};
use wasm_bindgen::JsValue;
use web_sys::Element;

use crate::dom::{attr, prevent_default, stop_propagation};

use super::Intents;
use super::kit::{Bag, Cx, clear, contains, ev_key, event_target, focus, icon_el, set_title};

pub struct ShortcutsHelp {
    cx: Cx,
    me: Weak<ShortcutsHelp>,
    backdrop: RefCell<Option<Element>>,
    bag: Rc<Bag>,
    prev_focus: RefCell<Option<Element>>,
    open: Cell<bool>,
    intents: Rc<Intents>,
}

impl ShortcutsHelp {
    pub fn new(cx: &Cx, intents: &Rc<Intents>) -> Rc<ShortcutsHelp> {
        Rc::new_cyclic(|me| ShortcutsHelp {
            cx: cx.clone(),
            me: me.clone(),
            backdrop: RefCell::new(None),
            bag: Bag::new(),
            prev_focus: RefCell::new(None),
            open: Cell::new(false),
            intents: intents.clone(),
        })
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    pub fn open(&self) {
        if self.open.replace(true) {
            return;
        }
        let cx = &self.cx;
        *self.prev_focus.borrow_mut() = cx.doc.active_element();
        let title = cx.t(&title_message());
        let backdrop = cx.el("div", "vela-sc-backdrop");
        let dialog = cx.el("div", "vela-sc");
        attr(&dialog, "role", "dialog");
        attr(&dialog, "aria-modal", "true");
        attr(&dialog, "aria-labelledby", "vela-sc-title");
        attr(&dialog, "tabindex", "-1");
        let head = cx.el("div", "vela-sc-head");
        let title_el = cx.text("span", "vela-sc-title", &title);
        attr(&title_el, "id", "vela-sc-title");
        let close = cx.button("vela-sc-close", &cx.m("ui.dialog.close", "Close"));
        let _ = close.append_child(&icon_el(&cx.doc, "close"));
        let _ = head.append_child(&title_el);
        let _ = head.append_child(&close);
        let body = cx.el("div", "vela-sc-body");
        self.fill(&body);
        let _ = dialog.append_child(&head);
        let _ = dialog.append_child(&body);
        let _ = backdrop.append_child(&dialog);
        let _ = cx.root.element().append_child(&backdrop);
        let me = self.me.clone();
        self.bag.on(&close, "click", move |_| {
            if let Some(h) = me.upgrade() {
                h.close();
            }
        });
        // Escape closes; a press on the backdrop (outside the panel) closes.
        let me = self.me.clone();
        let (trap, doc) = (dialog.clone(), cx.doc.clone());
        self.bag.on(&dialog, "keydown", move |ev: JsValue| {
            super::kit::trap_tab(&ev, &trap, &doc);
            if ev_key(&ev) == "Escape" {
                prevent_default(&ev);
                stop_propagation(&ev);
                if let Some(h) = me.upgrade() {
                    h.close();
                }
            }
        });
        let me = self.me.clone();
        let panel = dialog.clone();
        self.bag.on(&backdrop, "pointerdown", move |ev: JsValue| {
            let outside = event_target(&ev).is_none_or(|t| !contains(&panel, &t.into()));
            if outside && let Some(h) = me.upgrade() {
                h.close();
            }
        });
        set_title(&close, &cx.m("ui.dialog.close", "Close"));
        *self.backdrop.borrow_mut() = Some(backdrop);
        cx.root.keymap().push_scope("dialog");
        self.intents.dialog_opened(true);
        focus(&dialog);
    }

    fn fill(&self, list: &Element) {
        clear(list);
        let cx = &self.cx;
        let tr = cx.tr();
        let view = shortcuts_help_sections(cx.root.keymap(), tr.as_deref());
        for cat in view.categories {
            let _ = list.append_child(&cx.text("div", "vela-sh-cat", &cat.title));
            for row in cat.rows {
                let r = cx.el("div", "vela-sh-row");
                let _ = r.append_child(&cx.text("span", "vela-sh-label", &row.label));
                let keys = cx.el("span", "vela-sh-keys");
                for k in row.keys {
                    let _ = keys.append_child(&cx.text("span", "vela-sh-key", &k));
                }
                let _ = r.append_child(&keys);
                let _ = list.append_child(&r);
            }
        }
        let _ = list.append_child(&cx.text("div", "vela-sh-static", &view.typing_note));
    }

    pub fn close(&self) {
        if !self.open.replace(false) {
            return;
        }
        self.bag.retire(&self.cx);
        if let Some(b) = self.backdrop.borrow_mut().take() {
            b.remove();
        }
        self.cx.root.keymap().pop_scope("dialog");
        self.intents.dialog_opened(false);
        let prev = self.prev_focus.borrow_mut().take();
        if let Some(p) = prev {
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
