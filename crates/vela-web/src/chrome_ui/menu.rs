//! The small popup menu of the legend (the row's "move to" menu and its right-click menu),
//! standing in for the kit's `DropdownMenu` / `ContextMenu`.
//!
//! `role="menu"` of `menuitem` / `menuitemcheckbox` buttons. Opening focuses the first item;
//! ArrowDown / ArrowUp / Home / End move through the enabled items, Enter and Space activate
//! (they are buttons), Escape or Tab closes and returns focus to the opener, a press outside
//! closes. A pick closes the menu first, then reports the id, so the handler may rebuild the
//! chrome freely.

use std::rc::Rc;

use wasm_bindgen::prelude::*;
use web_sys::{Element, EventTarget, HtmlElement};

use super::ChromeUi;
use crate::dom::{
    Listener, attr, get_prop, get_str, make, on, prevent_default, set_style, set_styles,
    stop_propagation, vela_contains, vela_focus,
};

/// One menu entry.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuItem {
    pub id: String,
    pub label: String,
    /// `Some`: a checkbox row.
    pub checked: Option<bool>,
    pub separator_before: bool,
}

/// The open menu.
pub struct Menu {
    el: Element,
    _listeners: Vec<Listener>,
    opener: Option<Element>,
}

impl ChromeUi {
    /// Close the open menu (if any). `refocus` returns the focus to the element that opened
    /// it.
    fn close_menu_with(&self, refocus: bool) {
        let menu = self.menu.borrow_mut().take();
        let Some(menu) = menu else { return };
        menu.el.remove();
        if refocus && let Some(opener) = &menu.opener {
            vela_focus(opener);
        }
        self.retire(Box::new(menu));
    }

    pub(crate) fn close_menu(&self) {
        self.close_menu_with(false);
    }

    /// Open a menu at (`x`, `y`) in plot coordinates. `on_select` gets the picked item's id.
    pub(crate) fn open_menu(
        self: &Rc<Self>,
        items: &[MenuItem],
        x: f64,
        y: f64,
        label: &str,
        opener: Option<Element>,
        on_select: Rc<dyn Fn(&str)>,
    ) {
        self.close_menu();
        if items.is_empty() {
            return;
        }
        let el = make(&self.doc, "div", "vela-cmenu");
        attr(&el, "role", "menu");
        attr(&el, "aria-label", label);
        let mut listeners: Vec<Listener> = Vec::new();
        let mut buttons: Vec<Element> = Vec::new();
        for item in items {
            if item.separator_before && !buttons.is_empty() {
                let sep = make(&self.doc, "div", "vela-cmenu-sep");
                attr(&sep, "role", "separator");
                let _ = el.append_child(&sep);
            }
            let button = make(&self.doc, "button", "vela-cmenu-item");
            attr(&button, "type", "button");
            attr(&button, "tabindex", "-1");
            attr(&button, "data-vela-item", &item.id);
            match item.checked {
                Some(checked) => {
                    attr(&button, "role", "menuitemcheckbox");
                    attr(
                        &button,
                        "aria-checked",
                        if checked { "true" } else { "false" },
                    );
                }
                None => attr(&button, "role", "menuitem"),
            }
            let check = make(&self.doc, "span", "vela-cmenu-check");
            attr(&check, "aria-hidden", "true");
            if item.checked == Some(true) {
                check.set_inner_html(&super::icons::icon_at("check", 14));
            }
            let text = make(&self.doc, "span", "vela-cmenu-label");
            text.set_text_content(Some(&item.label));
            if item.checked.is_some() {
                let _ = button.append_child(&check);
            }
            let _ = button.append_child(&text);
            let weak = self.weak();
            let id = item.id.clone();
            let on_select = on_select.clone();
            listeners.push(on(&button, "click", move |ev: JsValue| {
                stop_propagation(&ev);
                if let Some(ui) = weak.upgrade() {
                    ui.close_menu_with(true);
                }
                on_select(&id);
            }));
            let _ = el.append_child(&button);
            buttons.push(button);
        }
        {
            let weak = self.weak();
            let nav = buttons.clone();
            listeners.push(on(&el, "keydown", move |ev: JsValue| {
                let key = get_str(&ev, "key");
                match key.as_str() {
                    "Escape" => {
                        prevent_default(&ev);
                        stop_propagation(&ev);
                        if let Some(ui) = weak.upgrade() {
                            ui.close_menu_with(true);
                        }
                    }
                    "Tab" => {
                        if let Some(ui) = weak.upgrade() {
                            ui.close_menu_with(true);
                        }
                    }
                    "ArrowDown" | "ArrowUp" | "Home" | "End" => {
                        prevent_default(&ev);
                        let doc_active = weak.upgrade().and_then(|ui| ui.doc.active_element());
                        let at = doc_active
                            .and_then(|a| nav.iter().position(|b| b == &a))
                            .unwrap_or(usize::MAX);
                        let n = nav.len();
                        let next = match key.as_str() {
                            "Home" => 0,
                            "End" => n - 1,
                            "ArrowDown" => {
                                if at == usize::MAX {
                                    0
                                } else {
                                    (at + 1) % n
                                }
                            }
                            _ => {
                                if at == usize::MAX || at == 0 {
                                    n - 1
                                } else {
                                    at - 1
                                }
                            }
                        };
                        vela_focus(&nav[next]);
                    }
                    _ => {}
                }
            }));
        }
        {
            // A press outside the menu closes it (the press itself still reaches its target).
            let weak = self.weak();
            let menu_el = el.clone();
            let target: &EventTarget = self.doc.as_ref();
            listeners.push(Listener::new(
                target,
                "pointerdown",
                true,
                true,
                move |ev: JsValue| {
                    let inside = vela_contains(&menu_el, &get_prop(&ev, "target"));
                    if inside {
                        return;
                    }
                    if let Some(ui) = weak.upgrade() {
                        ui.close_menu_with(false);
                    }
                },
            ));
        }
        set_styles(
            &el,
            &[("left", &format!("{x}px")), ("top", &format!("{y}px"))],
        );
        let _ = self.root.append_child(&el);
        // Clamp into the plot once it has a size.
        let (root_w, root_h) = self.root_size();
        let w = f64::from(el.unchecked_ref::<HtmlElement>().offset_width());
        let h = f64::from(el.unchecked_ref::<HtmlElement>().offset_height());
        if w > 0.0 && root_w > 0.0 {
            set_style(
                &el,
                "left",
                &format!("{}px", x.min(root_w - w - 4.0).max(0.0)),
            );
        }
        if h > 0.0 && root_h > 0.0 {
            set_style(
                &el,
                "top",
                &format!("{}px", y.min(root_h - h - 4.0).max(0.0)),
            );
        }
        if let Some(first) = buttons.first() {
            vela_focus(first);
        }
        *self.menu.borrow_mut() = Some(Menu {
            el,
            _listeners: listeners,
            opener,
        });
    }
}
