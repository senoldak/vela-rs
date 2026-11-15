//! The floating card shared by the legend's callout panels and the timeline-mark popup,
//! standing in for the kit's `Popover`.
//!
//! A `role="dialog"` card inside the chrome root, placed under an anchor rectangle (flipping
//! above when the plot's bottom edge is too close) and clamped into the plot. A press outside
//! or Escape reports a dismissal; the owner decides what closes.

use std::rc::Rc;

use wasm_bindgen::prelude::*;
use web_sys::{Element, EventTarget, HtmlElement};

use super::ChromeUi;
use crate::dom::{Listener, attr, get_prop, get_str, make, set_styles, vela_contains};

/// Space kept between the card and the plot's edges, px.
const EDGE: f64 = 4.0;

/// A rectangle in plot coordinates: x, y, width, height.
pub type Rect = (f64, f64, f64, f64);

/// How the card sits against its anchor.
#[derive(Clone, Copy)]
pub struct Placement {
    /// Center on the anchor (the mark glyph) instead of aligning to its start (the bubble).
    pub center: bool,
    /// Gap between the anchor and the card, px.
    pub gap: f64,
}

/// An open card. Dropping it frees its listeners; remove the element with [`Popover::remove`].
pub struct Popover {
    pub el: Element,
    placement: Placement,
    _listeners: Vec<Listener>,
}

impl Popover {
    pub fn remove(&self) {
        self.el.remove();
    }
}

impl ChromeUi {
    /// Build a card (`class` is added to `vela-pop`), fill it with `build`, and listen for
    /// dismissals. `ignore` is an element whose presses do not count as outside (the
    /// anchor, which toggles the card itself). `on_dismiss` runs on an outside press or
    /// Escape.
    pub(crate) fn open_popover(
        self: &Rc<Self>,
        class: &str,
        label: &str,
        placement: Placement,
        ignore: Option<Element>,
        build: impl FnOnce(&Element),
        on_dismiss: Rc<dyn Fn()>,
    ) -> Popover {
        let el = make(&self.doc, "div", &format!("vela-pop {class}"));
        attr(&el, "role", "dialog");
        attr(&el, "aria-label", label);
        build(&el);
        let _ = self.root.append_child(&el);
        let doc_target: &EventTarget = self.doc.as_ref();
        let mut listeners = Vec::new();
        {
            let card = el.clone();
            let on_dismiss = on_dismiss.clone();
            listeners.push(Listener::new(
                doc_target,
                "pointerdown",
                true,
                true,
                move |ev: JsValue| {
                    let target = get_prop(&ev, "target");
                    if vela_contains(&card, &target) {
                        return;
                    }
                    if let Some(anchor) = &ignore
                        && vela_contains(anchor, &target)
                    {
                        return;
                    }
                    on_dismiss();
                },
            ));
        }
        listeners.push(Listener::new(
            doc_target,
            "keydown",
            false,
            false,
            move |ev: JsValue| {
                if get_str(&ev, "key") == "Escape" {
                    on_dismiss();
                }
            },
        ));
        Popover {
            el,
            placement,
            _listeners: listeners,
        }
    }

    /// Place (or re-place) a card against its anchor rectangle.
    pub(crate) fn place_popover(&self, pop: &Popover, anchor: Rect) {
        let (ax, ay, aw, ah) = anchor;
        let (root_w, root_h) = self.root_size();
        let html = pop.el.unchecked_ref::<HtmlElement>();
        let w = f64::from(html.offset_width());
        let h = f64::from(html.offset_height());
        let gap = pop.placement.gap;
        let mut left = if pop.placement.center {
            ax + aw / 2.0 - w / 2.0
        } else {
            ax
        };
        let mut top = ay + ah + gap;
        if root_h > 0.0 && h > 0.0 && top + h > root_h - EDGE && ay - gap - h >= EDGE {
            top = ay - gap - h; // flip above
        }
        if root_w > 0.0 && w > 0.0 {
            left = left.min(root_w - w - EDGE);
        }
        left = left.max(EDGE);
        set_styles(
            &pop.el,
            &[("left", &format!("{left}px")), ("top", &format!("{top}px"))],
        );
    }
}
