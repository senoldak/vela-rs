//! Tooltips of the chrome: the control tip (`attachChromeTooltip` of `chrome-tooltip.ts`)
//! and the hover tip of canvas labels and marks (`LabelTooltip.ts`).
//!
//! Both are themed `div`s inside the chrome root (`role="tooltip"`, pointer-transparent),
//! opened after a hover delay by a MOUSE only: a tap fires a synthetic enter with no leave
//! to follow, so a touch-armed tip would stick. Every anchor also carries an `aria-label`:
//! the tip is a sighted-mouse aid, not the accessible name.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vela_core::render::native::NativeRenderer;
use vela_core::runtime::TimerId;
use wasm_bindgen::prelude::*;
use web_sys::{Element, HtmlElement};

use super::ChromeUi;
use crate::dom::{Listener, attr, get_str, make, on, set_style, set_styles};

/// Hover delay of a control tip, ms.
const CONTROL_DELAY_MS: f64 = 700.0;
/// Hover delay of a label tip, ms.
const LABEL_DELAY_MS: f64 = 350.0;

/// Where a control tip opens relative to its anchor.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Below,
    Above,
}

struct TipState {
    ui: Weak<ChromeUi>,
    anchor: Element,
    text: Box<dyn Fn() -> String>,
    placement: Placement,
    wrap: bool,
    timer: Cell<Option<TimerId>>,
    el: RefCell<Option<Element>>,
}

impl TipState {
    fn clear(&self) {
        if let Some(timer) = self.timer.take()
            && let Some(ui) = self.ui.upgrade()
        {
            ui.clear_timeout(timer);
        }
        let taken_1 = self.el.borrow_mut().take();
        if let Some(el) = taken_1 {
            el.remove();
        }
    }

    fn show(&self) {
        self.timer.set(None);
        let Some(ui) = self.ui.upgrade() else { return };
        if ui.is_disposed() {
            return;
        }
        let text = (self.text)();
        if text.is_empty() || self.el.borrow().is_some() {
            return;
        }
        let tip = make(&ui.doc, "div", "vela-chrome-tip");
        attr(&tip, "role", "tooltip");
        if self.wrap {
            attr(&tip, "data-wrap", "1");
        }
        tip.set_text_content(Some(&text));
        let _ = ui.root.append_child(&tip);
        let (ax, ay, aw, ah) = ui.rect_in_root(&self.anchor);
        let (root_w, _) = ui.root_size();
        let tip_w = f64::from(tip.unchecked_ref::<HtmlElement>().offset_width());
        let tip_h = f64::from(tip.unchecked_ref::<HtmlElement>().offset_height());
        // Left-aligned, clamped so it never spills out of the plot.
        let left = ax.min((root_w - tip_w - 4.0).max(0.0)).max(0.0);
        let top = match self.placement {
            Placement::Above => (ay - tip_h - 6.0).max(0.0),
            Placement::Below => ay + ah + 6.0,
        };
        let _ = aw;
        set_styles(
            &tip,
            &[("left", &format!("{left}px")), ("top", &format!("{top}px"))],
        );
        *self.el.borrow_mut() = Some(tip);
    }
}

/// A tooltip attached to an anchor. Dropping it removes its listeners and any open tip.
pub struct Tip {
    state: Rc<TipState>,
    _listeners: Vec<Listener>,
}

impl Tip {
    /// Close an open tip and cancel a pending one (the anchor is going away).
    pub fn dismiss(&self) {
        self.state.clear();
    }
}

impl Drop for Tip {
    fn drop(&mut self) {
        self.state.clear();
    }
}

/// Attach a control tip to `anchor`. `text` is read when the tip OPENS (a dynamic label);
/// empty text shows nothing.
pub fn attach(
    ui: &Rc<ChromeUi>,
    anchor: &Element,
    text: impl Fn() -> String + 'static,
    placement: Placement,
    wrap: bool,
) -> Tip {
    let state = Rc::new(TipState {
        ui: ui.weak(),
        anchor: anchor.clone(),
        text: Box::new(text),
        placement,
        wrap,
        timer: Cell::new(None),
        el: RefCell::new(None),
    });
    let mut listeners = Vec::new();
    {
        let state = state.clone();
        listeners.push(on(anchor, "pointerenter", move |ev: JsValue| {
            if get_str(&ev, "pointerType") != "mouse" {
                return;
            }
            state.clear();
            let Some(ui) = state.ui.upgrade() else { return };
            let again = state.clone();
            let timer = ui.set_timeout(CONTROL_DELAY_MS, move || again.show());
            state.timer.set(Some(timer));
        }));
    }
    for event in ["pointerleave", "pointerdown"] {
        // A click answers the question the tip poses.
        let state = state.clone();
        listeners.push(on(anchor, event, move |_ev: JsValue| state.clear()));
    }
    Tip {
        state,
        _listeners: listeners,
    }
}

/// The hover tip of Pine labels and timeline marks: the renderer hit-tests the label rects
/// and mark glyphs it painted, this shows the text near the pointer.
pub struct LabelTooltip {
    /// Text of the open OR armed tip: dedupes moves inside one label.
    current: RefCell<Option<String>>,
    timer: Cell<Option<TimerId>>,
    el: RefCell<Option<Element>>,
}

impl LabelTooltip {
    pub fn new() -> Self {
        Self {
            current: RefCell::new(None),
            timer: Cell::new(None),
            el: RefCell::new(None),
        }
    }

    /// The pointer moved over the plot (plot-relative px; mouse only).
    pub fn moved(&self, ui: &Rc<ChromeUi>, renderer: &NativeRenderer, x: f64, y: f64) {
        let Some(text) = renderer.label_tooltip_at(x, y) else {
            self.clear(ui);
            return;
        };
        if self.current.borrow().as_deref() == Some(text.as_str()) {
            return; // still inside the same label
        }
        self.clear(ui);
        *self.current.borrow_mut() = Some(text.clone());
        let weak = ui.weak();
        let timer = ui.set_timeout(LABEL_DELAY_MS, move || {
            if let Some(ui) = weak.upgrade() {
                ui.labels.show(&ui, &text, x, y);
            }
        });
        self.timer.set(Some(timer));
    }

    fn show(&self, ui: &ChromeUi, text: &str, x: f64, y: f64) {
        self.timer.set(None);
        if ui.is_disposed() || self.el.borrow().is_some() {
            return;
        }
        let tip = make(&ui.doc, "div", "vela-chrome-tip");
        attr(&tip, "role", "tooltip");
        attr(&tip, "data-wrap", "1");
        tip.set_text_content(Some(text));
        let _ = ui.root.append_child(&tip);
        // Below-right of the cursor, clamped so the tip never spills out of the plot.
        let (pw, ph) = ui.root_size();
        let tip_w = f64::from(tip.unchecked_ref::<HtmlElement>().offset_width());
        let tip_h = f64::from(tip.unchecked_ref::<HtmlElement>().offset_height());
        let left = (x + 12.0).min(pw - tip_w - 4.0).max(0.0);
        let top = (y + 16.0).min(ph - tip_h - 4.0).max(0.0);
        set_style(&tip, "left", &format!("{left}px"));
        set_style(&tip, "top", &format!("{top}px"));
        *self.el.borrow_mut() = Some(tip);
    }

    /// Cancel a pending tip and close an open one.
    pub fn clear(&self, ui: &ChromeUi) {
        if let Some(timer) = self.timer.take() {
            ui.clear_timeout(timer);
        }
        let taken_2 = self.el.borrow_mut().take();
        if let Some(el) = taken_2 {
            el.remove();
        }
        *self.current.borrow_mut() = None;
    }

    pub fn dispose(&self, ui: &ChromeUi) {
        self.clear(ui);
    }
}
