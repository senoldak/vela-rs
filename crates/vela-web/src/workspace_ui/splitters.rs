//! The splitter strips: the DOM half of `splitters.ts` (`SplitterLayer`).
//!
//! The math is core's ([`vela_core::workspace::splitters`]); the layer owns the strip
//! elements over the grid and reads geometry from the container on every layout pass. One
//! strip per seam SEGMENT of each internal column and row boundary (never the boundary's
//! full length: a cell spanning the boundary keeps the pointer there), each a `div` with
//! `class="vela-ws-splitter"`, `data-axis` (`cols` / `rows`), `data-index`, absolutely
//! positioned at `z-index: 30` with a `col-resize` / `row-resize` cursor.
//!
//! - A pointer press on a strip starts a drag; moves trade weight between the two
//!   neighboring tracks through [`Workspace::apply_tracks`]; release or cancel ends it.
//!   The strip takes pointer capture so the drag survives leaving the strip.
//! - A double click on a strip evens the axis out ([`Workspace::reset_tracks`]).
//!
//! The original has no keyboard handling on the strips, and neither does this port.
//!
//! Every entry point here runs from a DOM event or a deferred task, never from inside a
//! [`WorkspaceHost`](vela_core::workspace::WorkspaceHost) method, so calling the workspace
//! is allowed.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use vela_core::workspace::{Axis, SplitterDrag, Workspace};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use web_sys::{Document, Element, PointerEvent};

use crate::dom::{Listener, attr, make, set_styles, vela_capture_pointer, vela_release_pointer};

/// How the layer reaches the workspace (it exists only after the host did).
pub type WorkspaceLink = Rc<dyn Fn() -> Option<Workspace>>;

struct Strip {
    el: Element,
    _listeners: Vec<Listener>,
}

struct Drag {
    inner: SplitterDrag,
    pointer: i32,
}

/// The strips over one grid container.
pub struct SplitterLayer {
    me: Weak<SplitterLayer>,
    doc: Document,
    container: Element,
    ws: WorkspaceLink,
    strips: RefCell<Vec<Strip>>,
    drag: RefCell<Option<Drag>>,
}

fn axis_of(el: &Element) -> Option<(Axis, usize)> {
    let axis = match el.get_attribute("data-axis")?.as_str() {
        "cols" => Axis::Cols,
        "rows" => Axis::Rows,
        _ => return None,
    };
    let index = el.get_attribute("data-index")?.parse::<usize>().ok()?;
    Some((axis, index))
}

impl SplitterLayer {
    pub fn new(doc: &Document, container: &Element, ws: WorkspaceLink) -> Rc<Self> {
        Rc::new_cyclic(|me| Self {
            me: me.clone(),
            doc: doc.clone(),
            container: container.clone(),
            ws,
            strips: RefCell::default(),
            drag: RefCell::default(),
        })
    }

    /// The strip elements, in order (tests and shell components read them).
    pub fn elements(&self) -> Vec<Element> {
        self.strips.borrow().iter().map(|s| s.el.clone()).collect()
    }

    /// Rebuild and reposition the strips for the workspace's current tracks and the
    /// container's current size.
    pub fn layout(&self) {
        let Some(ws) = (self.ws)() else { return };
        let rect = self.container.get_bounding_client_rect();
        let placements = ws.splitter_strips(rect.width(), rect.height());
        let mut strips = self.strips.borrow_mut();
        while strips.len() > placements.len() {
            if let Some(s) = strips.pop() {
                s.el.remove();
            }
        }
        while strips.len() < placements.len() {
            let strip = self.make_strip();
            strips.push(strip);
        }
        for (strip, p) in strips.iter().zip(&placements) {
            attr(&strip.el, "data-axis", p.axis.as_str());
            attr(&strip.el, "data-index", &p.index.to_string());
            let cursor = match p.axis {
                Axis::Cols => "col-resize",
                Axis::Rows => "row-resize",
            };
            set_styles(
                &strip.el,
                &[
                    ("position", "absolute"),
                    ("z-index", "30"),
                    ("left", &format!("{}px", p.left)),
                    ("top", &format!("{}px", p.top)),
                    ("width", &format!("{}px", p.width)),
                    ("height", &format!("{}px", p.height)),
                    ("cursor", cursor),
                ],
            );
        }
    }

    /// Remove every strip and end any drag.
    pub fn destroy(&self) {
        for s in self.strips.borrow_mut().drain(..) {
            s.el.remove();
        }
        self.drag.borrow_mut().take();
    }

    fn make_strip(&self) -> Strip {
        let el = make(&self.doc, "div", "vela-ws-splitter");
        set_styles(&el, &[("position", "absolute"), ("z-index", "30")]);
        let target: &web_sys::EventTarget = el.as_ref();
        let mut listeners = Vec::new();
        {
            let (me, el) = (self.me.clone(), el.clone());
            listeners.push(Listener::new(
                target,
                "pointerdown",
                false,
                false,
                move |ev: JsValue| {
                    if let Some(layer) = me.upgrade() {
                        layer.on_down(&el, ev.unchecked_ref::<PointerEvent>());
                    }
                },
            ));
        }
        {
            let me = self.me.clone();
            listeners.push(Listener::new(
                target,
                "pointermove",
                false,
                true,
                move |ev: JsValue| {
                    if let Some(layer) = me.upgrade() {
                        layer.on_move(ev.unchecked_ref::<PointerEvent>());
                    }
                },
            ));
        }
        for name in ["pointerup", "pointercancel"] {
            let (me, el) = (self.me.clone(), el.clone());
            listeners.push(Listener::new(target, name, false, true, move |_| {
                if let Some(layer) = me.upgrade() {
                    layer.on_up(&el);
                }
            }));
        }
        {
            let (me, el) = (self.me.clone(), el.clone());
            listeners.push(Listener::new(target, "dblclick", false, true, move |_| {
                let Some(layer) = me.upgrade() else { return };
                if let (Some((axis, _)), Some(ws)) = (axis_of(&el), (layer.ws)()) {
                    ws.reset_tracks(axis);
                }
            }));
        }
        let _ = self.container.append_child(&el);
        Strip {
            el,
            _listeners: listeners,
        }
    }

    fn on_down(&self, el: &Element, e: &PointerEvent) {
        let Some((axis, index)) = axis_of(el) else {
            return;
        };
        let Some(ws) = (self.ws)() else { return };
        e.prevent_default();
        let rect = self.container.get_bounding_client_rect();
        let (cols, rows) = ws.tracks();
        let (weights, size, start) = match axis {
            Axis::Cols => (cols, rect.width(), f64::from(e.client_x())),
            Axis::Rows => (rows, rect.height(), f64::from(e.client_y())),
        };
        let pointer = e.pointer_id();
        *self.drag.borrow_mut() = Some(Drag {
            inner: SplitterDrag::begin(axis, index, start, &weights, size, ws.gap_px()),
            pointer,
        });
        vela_capture_pointer(el, pointer);
    }

    fn on_move(&self, e: &PointerEvent) {
        let (axis, weights) = {
            let drag = self.drag.borrow();
            let Some(d) = drag.as_ref() else { return };
            let px = match d.inner.axis() {
                Axis::Cols => f64::from(e.client_x()),
                Axis::Rows => f64::from(e.client_y()),
            };
            (d.inner.axis(), d.inner.update(px))
        };
        if let Some(ws) = (self.ws)() {
            ws.apply_tracks(axis, weights);
        }
    }

    fn on_up(&self, el: &Element) {
        if let Some(d) = self.drag.borrow_mut().take() {
            vela_release_pointer(el, d.pointer);
        }
    }
}
