//! The popup list of the side surfaces: the kit's `DropdownMenu` / `ContextMenu` list, built
//! from [`MenuItemDescriptor`]s (icons, hints, checks, switch rows that keep the menu open,
//! separators, nested submenus).
//!
//! `role="menu"` of `menuitem` / `menuitemcheckbox` buttons. Opening focuses the first item;
//! ArrowDown / ArrowUp / Home / End move through the enabled items of the level, ArrowRight
//! opens a submenu and ArrowLeft closes it, Enter and Space activate (they are buttons), Escape
//! closes and returns focus to the opener, a press outside closes. A pick closes the menu
//! first, then reports the id, so the handler may rebuild the chrome freely.

use std::cell::RefCell;
use std::rc::Rc;

use vela_core::ui::MenuItemDescriptor;
use wasm_bindgen::JsValue;
use web_sys::{Element, EventTarget};

use crate::dom::{Listener, attr, prevent_default, set_style, set_styles, stop_propagation};

use super::kit::{
    Bag, Cx, contains, ev_key, event_target, focus, html, icon_el, is_hidden, set_data, set_hidden,
};

/// The open menu: dropping or closing it removes the list.
pub struct MenuHandle {
    cx: Cx,
    el: Element,
    bag: Rc<Bag>,
    opener: Option<Element>,
    on_close: RefCell<Option<Rc<dyn Fn()>>>,
    closed: RefCell<bool>,
}

type Select = Rc<dyn Fn(&str)>;

/// Open a menu at the viewport point (`x`, `y`). `on_select` receives the picked id after the
/// menu closed; `on_close` runs whenever it closes (a pick, Escape, an outside press).
#[allow(clippy::too_many_arguments)]
pub fn open_menu(
    cx: &Cx,
    items: &[MenuItemDescriptor],
    x: f64,
    y: f64,
    label: &str,
    opener: Option<Element>,
    on_select: Select,
    on_close: Option<Rc<dyn Fn()>>,
) -> Option<Rc<MenuHandle>> {
    if items.is_empty() {
        return None;
    }
    let el = cx.el("div", "vela-smenu");
    attr(&el, "role", "menu");
    attr(&el, "aria-label", label);
    let handle = Rc::new(MenuHandle {
        cx: cx.clone(),
        el: el.clone(),
        bag: Bag::new(),
        opener,
        on_close: RefCell::new(on_close),
        closed: RefCell::new(false),
    });
    let first = fill(&handle, &el, items, &on_select);
    // Root coordinates: the root is the positioned ancestor.
    let rect = cx.root.element().get_bounding_client_rect();
    set_styles(
        &el,
        &[
            ("left", &format!("{}px", x - rect.left())),
            ("top", &format!("{}px", y - rect.top())),
        ],
    );
    let _ = cx.root.element().append_child(&el);
    // Clamp into the root once the list has a size.
    let (w, h) = (
        f64::from(html(&el).offset_width()),
        f64::from(html(&el).offset_height()),
    );
    if w > 0.0 && rect.width() > 0.0 {
        set_style(
            &el,
            "left",
            &format!(
                "{}px",
                (x - rect.left()).min(rect.width() - w - 4.0).max(0.0)
            ),
        );
    }
    if h > 0.0 && rect.height() > 0.0 {
        set_style(
            &el,
            "top",
            &format!(
                "{}px",
                (y - rect.top()).min(rect.height() - h - 4.0).max(0.0)
            ),
        );
    }
    if let Some(first) = first {
        focus(&first);
    }
    // A press outside, and Escape from anywhere, close it.
    let target: EventTarget = cx.doc.clone().into();
    {
        let weak = Rc::downgrade(&handle);
        handle.bag.push(Listener::new(
            &target,
            "pointerdown",
            true,
            true,
            move |ev: JsValue| {
                let Some(h) = weak.upgrade() else { return };
                let t = crate::dom::get_prop(&ev, "target");
                if !contains(&h.el, &t) {
                    h.close(false);
                }
            },
        ));
    }
    {
        let weak = Rc::downgrade(&handle);
        handle.bag.push(Listener::new(
            &target,
            "keydown",
            true,
            false,
            move |ev: JsValue| {
                let Some(h) = weak.upgrade() else { return };
                if ev_key(&ev) == "Escape" {
                    prevent_default(&ev);
                    stop_propagation(&ev);
                    h.close(true);
                }
            },
        ));
    }
    Some(handle)
}

impl MenuHandle {
    pub fn element(&self) -> &Element {
        &self.el
    }

    pub fn is_open(&self) -> bool {
        !*self.closed.borrow()
    }

    /// Close the menu; `refocus` returns the focus to the element that opened it.
    pub fn close(&self, refocus: bool) {
        if self.closed.replace(true) {
            return;
        }
        self.el.remove();
        if refocus && let Some(opener) = &self.opener {
            focus(opener);
        }
        self.bag.retire(&self.cx);
        let cb = self.on_close.borrow_mut().take();
        if let Some(cb) = cb {
            cb();
        }
    }
}

/// Build one level into `list`; returns its first enabled button.
fn fill(
    handle: &Rc<MenuHandle>,
    list: &Element,
    items: &[MenuItemDescriptor],
    on_select: &Select,
) -> Option<Element> {
    let cx = &handle.cx;
    let mut buttons: Vec<Element> = Vec::new();
    for item in items {
        if item.separator_before && !buttons.is_empty() {
            let sep = cx.el("div", "vela-smenu-sep");
            attr(&sep, "role", "separator");
            let _ = list.append_child(&sep);
        }
        let wrap = cx.el(
            "div",
            if item.submenu.is_some() {
                "vela-smenu-sub"
            } else {
                ""
            },
        );
        let button = cx.el("button", "vela-smenu-item");
        attr(&button, "type", "button");
        attr(&button, "tabindex", "-1");
        attr(&button, "data-vela-item", &item.id);
        if let Some(title) = &item.title {
            super::kit::set_title(&button, title);
        }
        match item.checked {
            Some(c) => {
                attr(&button, "role", "menuitemcheckbox");
                attr(&button, "aria-checked", if c { "true" } else { "false" });
            }
            None => attr(&button, "role", "menuitem"),
        }
        if item.disabled {
            attr(&button, "aria-disabled", "true");
        }
        if item.checked.is_some() {
            let check = cx.el("span", "vela-smenu-check");
            attr(&check, "aria-hidden", "true");
            if item.checked == Some(true) {
                check.set_inner_html(super::icons::icon("check"));
            }
            let _ = button.append_child(&check);
        }
        if let Some(icon) = &item.icon {
            let ic = icon_el(&cx.doc, icon);
            super::kit::add_class(&ic, "vela-smenu-icon");
            let _ = button.append_child(&ic);
        }
        let label = cx.text("span", "vela-smenu-label", &item.label);
        let _ = button.append_child(&label);
        if let Some(hint) = &item.hint {
            let _ = button.append_child(&cx.text("span", "vela-smenu-hint", hint));
        }
        let _ = wrap.append_child(&button);
        if let Some(sub) = &item.submenu {
            attr(&button, "aria-haspopup", "menu");
            attr(&button, "aria-expanded", "false");
            let chev = icon_el(&cx.doc, "chevron-right");
            super::kit::add_class(&chev, "vela-smenu-icon");
            let _ = button.append_child(&chev);
            let nested = cx.el("div", "vela-smenu");
            attr(&nested, "role", "menu");
            set_hidden(&nested, true);
            fill(handle, &nested, sub, on_select);
            let _ = wrap.append_child(&nested);
            let (b2, n2) = (button.clone(), nested.clone());
            let toggle = move |open: bool| {
                set_hidden(&n2, !open);
                attr(&b2, "aria-expanded", if open { "true" } else { "false" });
                set_data(&b2, "open", if open { Some("1") } else { None });
            };
            let toggle = Rc::new(toggle);
            {
                let toggle = toggle.clone();
                let n3 = nested.clone();
                handle.bag.on(&button, "click", move |ev: JsValue| {
                    stop_propagation(&ev);
                    toggle(is_hidden(&n3));
                });
            }
            {
                let toggle = toggle.clone();
                let n3 = nested.clone();
                handle.bag.on(&button, "keydown", move |ev: JsValue| {
                    if ev_key(&ev) == "ArrowRight" {
                        prevent_default(&ev);
                        stop_propagation(&ev);
                        toggle(true);
                        if let Some(first) = super::kit::kids(&n3).first().and_then(|w| {
                            if super::kit::has_class(w, "vela-smenu-sub") {
                                super::kit::kids(w).first().cloned()
                            } else {
                                Some(w.clone())
                            }
                        }) {
                            focus(&first);
                        }
                    }
                });
            }
            // ArrowLeft inside the nested list closes it and returns to the trigger.
            {
                let toggle = toggle.clone();
                let b = button.clone();
                handle.bag.on(&nested, "keydown", move |ev: JsValue| {
                    if ev_key(&ev) == "ArrowLeft" {
                        prevent_default(&ev);
                        stop_propagation(&ev);
                        toggle(false);
                        focus(&b);
                    }
                });
            }
            let _ = list.append_child(&wrap);
            if !item.disabled {
                buttons.push(button);
            }
            continue;
        }
        {
            let weak = Rc::downgrade(handle);
            let id = item.id.clone();
            let disabled = item.disabled;
            let keep_open = item.toggle;
            let on_select = on_select.clone();
            handle.bag.on(&button, "click", move |ev: JsValue| {
                stop_propagation(&ev);
                if disabled {
                    return;
                }
                if keep_open {
                    on_select(&id);
                    return;
                }
                if let Some(h) = weak.upgrade() {
                    h.close(true);
                }
                on_select(&id);
            });
        }
        let _ = list.append_child(&wrap);
        if !item.disabled {
            buttons.push(button);
        }
    }
    // Arrow navigation within this level.
    {
        let nav = buttons.clone();
        let cx2 = cx.clone();
        handle.bag.on(list, "keydown", move |ev: JsValue| {
            let key = ev_key(&ev);
            if !matches!(key.as_str(), "ArrowDown" | "ArrowUp" | "Home" | "End") || nav.is_empty() {
                return;
            }
            // Only this level's own buttons handle it (a nested list handles its own).
            let tgt = event_target(&ev);
            if tgt.as_ref().is_some_and(|t| !nav.iter().any(|b| b == t)) {
                return;
            }
            prevent_default(&ev);
            stop_propagation(&ev);
            let active = cx2.doc.active_element();
            let at = active
                .and_then(|a| nav.iter().position(|b| b == &a))
                .or_else(|| tgt.and_then(|t| nav.iter().position(|b| b == &t)));
            let n = nav.len();
            let next = match key.as_str() {
                "Home" => 0,
                "End" => n - 1,
                "ArrowDown" => at.map_or(0, |i| (i + 1) % n),
                _ => at.map_or(n - 1, |i| if i == 0 { n - 1 } else { i - 1 }),
            };
            focus(&nav[next]);
        });
    }
    buttons.first().cloned()
}
