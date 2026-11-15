//! The floating shell the popup's menus, its color popover and the panel's style menu share:
//! the original's `hostFloat` over the kit's `Popover`.
//!
//! A shell is a card inside the chart's wrapper, anchored on a trigger: below it (flipping
//! above when the chart's bottom is too near), aligned to its start or end, clamped into
//! the chart. One shell is open per host at a time; opening another closes the first, and
//! a close frees the shell's listeners from a timer (the shell may be closing from inside
//! one of its own handlers).

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use wasm_bindgen::JsValue;
use web_sys::Element;

use super::DrawingsUi;
use super::model;
use super::util;
use crate::dialogs::Cx;
use crate::dom::{
    Listener, attr, get_prop, get_str, prevent_default, set_style, set_styles, stop_propagation,
    vela_contains, vela_focus,
};

const FALLBACK: (f64, f64) = (150.0, 120.0);

struct Open {
    el: Element,
    anchor: Element,
    bag: Rc<RefCell<Vec<Listener>>>,
    on_close: Option<Box<dyn FnOnce()>>,
}

/// Where shells open and what frees their listeners.
pub struct FloatHost {
    me: Weak<FloatHost>,
    ui: Weak<DrawingsUi>,
    cx: Cx,
    wrapper: Element,
    current: RefCell<Option<Open>>,
}

impl FloatHost {
    pub fn new(ui: &Rc<DrawingsUi>, cx: &Cx) -> Rc<Self> {
        Rc::new_cyclic(|me| Self {
            me: me.clone(),
            ui: Rc::downgrade(ui),
            cx: cx.clone(),
            wrapper: ui.wrapper.clone(),
            current: RefCell::new(None),
        })
    }

    pub fn is_open(&self) -> bool {
        self.current.borrow().is_some()
    }

    /// Whether `node` is inside the open shell.
    pub fn contains(&self, node: &JsValue) -> bool {
        self.current
            .borrow()
            .as_ref()
            .is_some_and(|o| vela_contains(&o.el, node))
    }

    /// Whether the open shell hangs off `anchor`.
    pub fn owned_by(&self, anchor: &Element) -> bool {
        self.current
            .borrow()
            .as_ref()
            .is_some_and(|o| JsValue::from(o.anchor.clone()) == JsValue::from(anchor.clone()))
    }

    /// Open a shell on `anchor` (closing any other). `fill` adds its content with a bag of
    /// its own; `on_close` runs when the shell closes.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &self,
        anchor: &Element,
        align_end: bool,
        z: u32,
        class: &str,
        padding: &str,
        on_close: Option<Box<dyn FnOnce()>>,
        fill: impl FnOnce(&Element, &Cx),
    ) {
        self.close();
        let cx = self.cx.sub();
        let el = cx.el("div", &format!("vela-dpop-float vela-dsp-scope {class}"));
        set_styles(
            &el,
            &[
                ("z-index", &z.to_string()),
                ("padding", padding),
                ("visibility", "hidden"),
            ],
        );
        fill(&el, &cx);
        // Escape closes the shell and hands focus back to its trigger.
        {
            let (me, trigger) = (self.me.clone(), anchor.clone());
            cx.on(&el, "keydown", move |ev| {
                if get_str(&ev, "key") == "Escape" {
                    prevent_default(&ev);
                    stop_propagation(&ev);
                    if let Some(host) = me.upgrade() {
                        host.close();
                    }
                    vela_focus(&trigger);
                }
            });
        }
        // The kit popover's dismissal, on the document in the capture phase so the page's own
        // handlers cannot swallow it: a press outside the shell (and off its trigger, whose
        // click toggles it) closes it, and so does Escape, from wherever focus is.
        {
            let target: &web_sys::EventTarget = self.cx.doc.as_ref();
            let (me, shell, trigger) = (self.me.clone(), el.clone(), anchor.clone());
            cx.bag.borrow_mut().push(Listener::new(
                target,
                "pointerdown",
                true,
                true,
                move |ev| {
                    let node = get_prop(&ev, "target");
                    if vela_contains(&shell, &node) || vela_contains(&trigger, &node) {
                        return;
                    }
                    if let Some(host) = me.upgrade() {
                        host.close();
                    }
                },
            ));
            let (me, trigger) = (self.me.clone(), anchor.clone());
            cx.bag
                .borrow_mut()
                .push(Listener::new(target, "keydown", true, false, move |ev| {
                    if get_str(&ev, "key") != "Escape" {
                        return;
                    }
                    prevent_default(&ev);
                    stop_propagation(&ev);
                    if let Some(host) = me.upgrade() {
                        host.close();
                    }
                    vela_focus(&trigger);
                }));
        }
        let _ = self.wrapper.append_child(&el);
        let anchor_box = util::rect_in(anchor, &self.wrapper);
        let host = self.wrapper.get_bounding_client_rect();
        let r = el.get_bounding_client_rect();
        let size = if r.width() > 0.0 && r.height() > 0.0 {
            (r.width(), r.height())
        } else {
            FALLBACK
        };
        let (left, top) =
            model::float_origin(anchor_box, size, (host.width(), host.height()), align_end);
        util::place(&el, left, top);
        set_style(&el, "visibility", "");
        attr(anchor, "aria-expanded", "true");
        *self.current.borrow_mut() = Some(Open {
            el,
            anchor: anchor.clone(),
            bag: cx.bag.clone(),
            on_close,
        });
    }

    /// Close the open shell; its listeners are freed from a timer.
    pub fn close(&self) {
        let listeners = self.close_now();
        if !listeners.is_empty()
            && let Some(ui) = self.ui.upgrade()
        {
            ui.retire(listeners);
        }
    }

    /// Close the open shell and hand its listeners back.
    pub fn close_now(&self) -> Vec<Listener> {
        let Some(open) = self.current.borrow_mut().take() else {
            return Vec::new();
        };
        let _ = open.anchor.set_attribute("aria-expanded", "false");
        open.el.remove();
        let listeners = std::mem::take(&mut *open.bag.borrow_mut());
        if let Some(f) = open.on_close {
            f();
        }
        listeners
    }
}
