//! The timeline-mark popup (`MarkPopover.ts`): a card anchored on a glyph of the canvas lane,
//! listing every mark of the clicked cluster as a scrollable run of sections, each its title
//! over its content (escaped text, allowlisted HTML, or a structured panel of fields and
//! buttons).
//!
//! The core sends directives (`MarkPopoverAction`): open on a glyph, track it after a chrome
//! frame, close. The glyph is canvas pixels, so the card is placed from the rectangle the
//! directive carries. Marks are read through the source given to
//! [`ChromeUi::set_mark_source`](super::ChromeUi::set_mark_source) (the directive carries only
//! ids); without one nothing opens.
//!
//! Differences from the original: lazy (function) content does not exist in the port's
//! `MarkContent`, so there are no "Loading…" sections. HTML goes through
//! [`sanitize`](super::sanitize), a port of the original's `sanitize-html.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use vela_core::marks::{MarkContent, MarkPanelItem, TimelineMark};
use vela_core::render::native::MarkPopoverAction;
use wasm_bindgen::prelude::*;
use web_sys::{Document, Element};

use super::ChromeUi;
use super::popover::{Placement, Popover};
use super::sanitize::sanitize_html;
use crate::dom::{Listener, attr, make, on};

struct OpenMark {
    key: String,
    pop: Popover,
    _held: Rc<RefCell<Vec<Listener>>>,
}

#[derive(Default)]
pub struct MarkPopover {
    open: Option<OpenMark>,
    /// The cluster whose popup was open when the current pointer press began: the press
    /// closes the popup, and the click derived from the same release must read as "close",
    /// not reopen that cluster.
    press_key: Option<String>,
    press: Vec<Listener>,
}

impl MarkPopover {
    pub fn new() -> Self {
        Self::default()
    }
}

fn mark_section(
    doc: &Document,
    mark: &TimelineMark,
    held: &Rc<RefCell<Vec<Listener>>>,
    ui: &Rc<ChromeUi>,
) -> Element {
    let section = make(doc, "section", "vela-marks-section");
    if let Some(title) = mark.title.as_deref().filter(|t| !t.is_empty()) {
        let t = make(doc, "div", "vela-callout-title");
        attr(&t, "data-slot", "callout-bubble-title");
        t.set_text_content(Some(title));
        let _ = section.append_child(&t);
    }
    match &mark.content {
        Some(MarkContent::Text(text)) => {
            let t = make(doc, "div", "vela-callout-text");
            attr(&t, "data-slot", "callout-bubble-text");
            t.set_text_content(Some(text));
            let _ = section.append_child(&t);
        }
        Some(MarkContent::Html(html)) => {
            let h = make(doc, "div", "vela-marks-html");
            let clean = sanitize_html(doc, html);
            let _ = h.append_child(&clean);
            let _ = section.append_child(&h);
        }
        Some(MarkContent::Panel(items)) => {
            let mut actions: Option<Element> = None;
            for item in items {
                match item {
                    MarkPanelItem::Button {
                        label,
                        primary,
                        close,
                        run,
                    } => {
                        let row = actions.get_or_insert_with(|| {
                            let a = make(doc, "div", "vela-callout-actions");
                            attr(&a, "data-slot", "callout-bubble-actions");
                            let _ = section.append_child(&a);
                            a
                        });
                        let is_primary = *primary == Some(true);
                        let b = make(
                            doc,
                            "button",
                            if is_primary {
                                "vela-callout-btn vela-callout-btn-primary"
                            } else {
                                "vela-callout-btn"
                            },
                        );
                        attr(&b, "type", "button");
                        attr(&b, "data-slot", "callout-bubble-button");
                        if is_primary {
                            attr(&b, "data-primary", "");
                        }
                        b.set_text_content(Some(label));
                        let run = run.clone();
                        let closes = *close != Some(false);
                        let weak = ui.weak();
                        held.borrow_mut().push(on(&b, "click", move |_ev: JsValue| {
                            run.run();
                            if closes && let Some(ui) = weak.upgrade() {
                                ui.marks_close(true);
                            }
                        }));
                        let _ = row.append_child(&b);
                    }
                    MarkPanelItem::Text { text } => {
                        actions = None;
                        let t = make(doc, "div", "vela-callout-text");
                        attr(&t, "data-slot", "callout-bubble-text");
                        t.set_text_content(Some(text));
                        let _ = section.append_child(&t);
                    }
                    MarkPanelItem::Field { label, value } => {
                        actions = None;
                        let row = make(doc, "div", "vela-marks-field");
                        let l = make(doc, "span", "vela-marks-field-label");
                        l.set_text_content(Some(label));
                        let v = make(doc, "span", "vela-marks-field-value");
                        v.set_text_content(Some(value));
                        let _ = row.append_child(&l);
                        let _ = row.append_child(&v);
                        let _ = section.append_child(&row);
                    }
                }
            }
        }
        None => {}
    }
    section
}

impl ChromeUi {
    fn ensure_press_listeners(self: &Rc<Self>) {
        if !self.marks.borrow().press.is_empty() {
            return;
        }
        let target: &web_sys::EventTarget = self.doc.as_ref();
        let mut ls = Vec::new();
        let weak = self.weak();
        ls.push(Listener::new(
            target,
            "pointerdown",
            true,
            true,
            move |_e: JsValue| {
                let Some(ui) = weak.upgrade() else { return };
                let mut m = ui.marks.borrow_mut();
                if let Some(open) = &m.open {
                    m.press_key = Some(open.key.clone());
                }
            },
        ));
        for event in ["pointerup", "pointercancel"] {
            let weak = self.weak();
            ls.push(Listener::new(
                target,
                event,
                true,
                true,
                move |_e: JsValue| {
                    let Some(ui) = weak.upgrade() else { return };
                    let again = ui.weak();
                    // Cleared once the release has dispatched (the click follows it).
                    ui.set_timeout(0.0, move || {
                        if let Some(ui) = again.upgrade() {
                            ui.marks.borrow_mut().press_key = None;
                        }
                    });
                },
            ));
        }
        self.marks.borrow_mut().press = ls;
    }

    /// A directive from the core (called with the renderer borrowed: DOM only).
    pub(crate) fn mark_popover_action(self: &Rc<Self>, action: &MarkPopoverAction) {
        match action {
            MarkPopoverAction::Open {
                key,
                mark_ids,
                x,
                y,
                size,
            } => self.marks_open(key, mark_ids, (*x, *y, *size)),
            MarkPopoverAction::Close => self.marks_close(false),
            MarkPopoverAction::Track(Some(glyph)) => {
                let m = self.marks.borrow();
                if let Some(open) = &m.open {
                    self.place_popover(&open.pop, glyph_rect(*glyph));
                }
            }
            MarkPopoverAction::Track(None) => self.marks_close(false),
        }
    }

    fn marks_open(self: &Rc<Self>, key: &str, ids: &[String], glyph: (f64, f64, f64)) {
        if self.marks.borrow().press_key.as_deref() == Some(key) {
            return; // this press already closed that popup: the click toggles it off
        }
        self.ensure_press_listeners();
        self.marks_close(false);
        let source = self.mark_source.borrow().clone();
        let marks: Vec<TimelineMark> = source
            .map(|s| ids.iter().filter_map(|id| s(id)).collect())
            .unwrap_or_default();
        if marks.is_empty() {
            // Nothing to show: tell the core the popup is not open, from a fresh stack.
            let weak = self.weak();
            self.set_timeout(0.0, move || {
                if let Some(r) = weak.upgrade().and_then(|ui| ui.renderer()) {
                    r.mark_popover_closed();
                }
            });
            return;
        }
        let held: Rc<RefCell<Vec<Listener>>> = Rc::new(RefCell::new(Vec::new()));
        let held_in = held.clone();
        let doc = self.doc.clone();
        let ui = self.clone();
        let label = marks
            .iter()
            .find_map(|m| m.title.clone())
            .unwrap_or_default();
        let weak = self.weak();
        let dismiss: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(ui) = weak.upgrade() {
                ui.marks_close(true);
            }
        });
        let pop = self.open_popover(
            "vela-marks-pop",
            &label,
            Placement {
                center: true,
                gap: 8.0,
            },
            None,
            move |card| {
                let root = make(&doc, "div", "vela-callout-panel vela-marks-panel");
                attr(&root, "data-slot", "callout-bubble-panel");
                for mark in &marks {
                    let section = mark_section(&doc, mark, &held_in, &ui);
                    if section.child_element_count() > 0 {
                        let _ = root.append_child(&section);
                    }
                }
                let _ = card.append_child(&root);
            },
            dismiss,
        );
        self.place_popover(&pop, glyph_rect(glyph));
        self.marks.borrow_mut().open = Some(OpenMark {
            key: key.to_string(),
            pop,
            _held: held,
        });
    }

    /// Close the popup. `notify` tells the core (a dismissal on the host's side); a close the
    /// core asked for needs no answer.
    pub(crate) fn marks_close(&self, notify: bool) {
        let open = self.marks.borrow_mut().open.take();
        let Some(open) = open else { return };
        open.pop.remove();
        self.retire(Box::new(open));
        if notify && let Some(r) = self.renderer() {
            r.mark_popover_closed();
        }
    }

    pub(crate) fn marks_dispose(&self) {
        self.marks_close(false);
        self.marks.borrow_mut().press.clear();
    }
}

fn glyph_rect((x, y, size): (f64, f64, f64)) -> (f64, f64, f64, f64) {
    (x - size / 2.0, y - size / 2.0, size, size)
}
