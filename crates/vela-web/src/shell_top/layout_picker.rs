//! The layout dropdown (`layout-picker.ts`): a bounded 4 x 4 grid canvas (hover previews
//! the rows x cols rectangle from the top-left, a press applies it), the registered
//! layouts the canvas cannot express as labeled rows, and the workspace SYNC switches
//! beside the grid.
//!
//! The grid, the hover state and the placement maths are
//! [`vela_core::widget::layout_picker`]; this is the anchored popover (an outside press or
//! Escape dismisses it) and its elements.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vela_core::widget::layout_picker::{
    GridHover, LayoutPickerShape, PickerRow, grid_cells, grid_hint, grid_selection, layout_heading,
    paint_layout_grid, popover_left, popover_top, sync_heading,
};
use wasm_bindgen::JsValue;
use web_sys::{Element, EventTarget};

use super::js::{holds, vst_closest_class, vst_key, vst_offset_width, vst_target};
use super::kit::{Env, Tooltip, clear, set_class};
use crate::dom::{Listener, attr, make, on, set_style, set_styles, stop_propagation};

pub struct LayoutPickerOpts {
    pub trigger: Element,
    pub shape: Rc<dyn Fn() -> Option<LayoutPickerShape>>,
    pub presets: Rc<dyn Fn() -> Vec<PickerRow>>,
    pub on_select_grid: Rc<dyn Fn(usize, usize)>,
    pub on_select_preset: Rc<dyn Fn(&str)>,
    pub syncs: Rc<dyn Fn() -> Vec<PickerRow>>,
    pub on_toggle_sync: Rc<dyn Fn(&str)>,
    pub on_open_change: Option<Rc<dyn Fn(bool)>>,
}

pub struct LayoutPicker {
    env: Env,
    me: Weak<LayoutPicker>,
    opts: LayoutPickerOpts,
    layer: Element,
    squares: Vec<Element>,
    presets_el: Element,
    sync_el: Element,
    heading: Element,
    sync_heading_el: Element,
    tip: Rc<Tooltip>,
    open: Cell<bool>,
    hover: RefCell<GridHover>,
    perm: RefCell<Vec<Listener>>,
    live: RefCell<Vec<Listener>>,
    rows_l: RefCell<Vec<Listener>>,
    sync_l: RefCell<Vec<Listener>>,
}

impl LayoutPicker {
    pub fn new(env: &Env, opts: LayoutPickerOpts) -> Rc<LayoutPicker> {
        let doc = &env.doc;
        let layer = make(doc, "div", "vela-lp-layer");
        set_style(&layer, "display", "none");
        let panel = make(doc, "div", "vela-lp");
        attr(&panel, "role", "dialog");
        layer.append_child(&panel).ok();
        let cols = make(doc, "div", "vela-lp-cols");
        panel.append_child(&cols).ok();

        let layout_col = make(doc, "div", "vela-lp-layout");
        let heading_row = make(doc, "div", "vela-lp-heading");
        let heading = make(doc, "span", "");
        heading.set_text_content(Some(&env.t(&layout_heading())));
        let badge = make(doc, "span", "vela-lp-badge");
        badge.set_text_content(Some("?"));
        heading_row.append_child(&heading).ok();
        heading_row.append_child(&badge).ok();
        layout_col.append_child(&heading_row).ok();
        let tip = Tooltip::new(env, &badge, &env.t(&grid_hint()));

        let grid = make(doc, "div", "vela-lp-grid");
        let mut squares = Vec::new();
        for cell in grid_cells(env.translator().as_deref()) {
            let sq = make(doc, "button", "vela-lp-sq");
            attr(&sq, "type", "button");
            attr(&sq, "data-r", &cell.row.to_string());
            attr(&sq, "data-c", &cell.col.to_string());
            attr(&sq, "aria-label", &cell.label);
            grid.append_child(&sq).ok();
            squares.push(sq);
        }
        layout_col.append_child(&grid).ok();
        let presets_el = make(doc, "div", "vela-lp-presets");
        layout_col.append_child(&presets_el).ok();
        cols.append_child(&layout_col).ok();
        cols.append_child(&make(doc, "div", "vela-lp-vsep")).ok();
        let sync_col = make(doc, "div", "");
        let sync_heading_el = make(doc, "div", "vela-lp-heading");
        sync_heading_el.set_text_content(Some(&env.t(&sync_heading())));
        let sync_el = make(doc, "div", "vela-lp-sync");
        sync_col.append_child(&sync_heading_el).ok();
        sync_col.append_child(&sync_el).ok();
        cols.append_child(&sync_col).ok();

        let trigger = opts.trigger.clone();
        let this = Rc::new_cyclic(|me| LayoutPicker {
            env: env.clone(),
            me: me.clone(),
            opts,
            layer,
            squares,
            presets_el,
            sync_el,
            heading,
            sync_heading_el,
            tip,
            open: Cell::new(false),
            hover: RefCell::new(GridHover::default()),
            perm: RefCell::new(Vec::new()),
            live: RefCell::new(Vec::new()),
            rows_l: RefCell::new(Vec::new()),
            sync_l: RefCell::new(Vec::new()),
        });
        let mut perm = Vec::new();
        let me = this.me.clone();
        perm.push(on(&grid, "pointerdown", move |ev| {
            let Some(p) = me.upgrade() else { return };
            let Some((r, c)) = p.square_at(&ev) else {
                return;
            };
            crate::dom::prevent_default(&ev);
            p.close();
            let (rows, cols) = grid_selection(r, c);
            (p.opts.on_select_grid)(rows, cols);
        }));
        // A square also acts from the keyboard (Enter / Space fire a click with the picker
        // still open); a pointer press has already applied and closed it by then.
        let me = this.me.clone();
        perm.push(on(&grid, "click", move |ev| {
            let Some(p) = me.upgrade() else { return };
            if !p.open.get() {
                return;
            }
            let Some((r, c)) = p.square_at(&ev) else {
                return;
            };
            p.close();
            let (rows, cols) = grid_selection(r, c);
            (p.opts.on_select_grid)(rows, cols);
        }));
        let me = this.me.clone();
        perm.push(on(&grid, "pointermove", move |ev| {
            let Some(p) = me.upgrade() else { return };
            let Some((r, c)) = p.square_at(&ev) else {
                return;
            };
            if p.hover.borrow_mut().on_square(r, c) {
                p.render();
            }
        }));
        let me = this.me.clone();
        perm.push(on(&grid, "pointerleave", move |_| {
            if let Some(p) = me.upgrade()
                && p.hover.borrow_mut().on_leave()
            {
                p.render();
            }
        }));
        let me = this.me.clone();
        perm.push(on(&trigger, "click", move |ev| {
            if let Some(p) = me.upgrade() {
                stop_propagation(&ev);
                p.toggle();
            }
        }));
        attr(&trigger, "aria-haspopup", "true");
        attr(&trigger, "aria-expanded", "false");
        *this.perm.borrow_mut() = perm;
        this
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    pub fn toggle(&self) {
        if self.open.get() {
            self.close();
        } else {
            self.open();
        }
    }

    pub fn open(&self) {
        if self.open.replace(true) {
            return;
        }
        *self.hover.borrow_mut() = GridHover::default();
        self.relabel();
        self.refresh();
        // The layer joins the root only while open: an idle shell leaves the root as it was.
        if let Some(host) = self.env.host() {
            host.append_child(&self.layer).ok();
        }
        set_style(&self.layer, "display", "");
        self.position();
        attr(&self.opts.trigger, "aria-expanded", "true");
        let target: EventTarget = self.env.doc.clone().into();
        let (me, layer, trigger) = (
            self.me.clone(),
            self.layer.clone(),
            self.opts.trigger.clone(),
        );
        let down = Listener::new(&target, "pointerdown", true, true, move |ev| {
            let node = vst_target(&ev);
            if holds(&layer, &node) || holds(&trigger, &node) {
                return;
            }
            if let Some(p) = me.upgrade() {
                p.close();
            }
        });
        let me = self.me.clone();
        let key = Listener::new(&target, "keydown", true, false, move |ev| {
            if vst_key(&ev) == "Escape"
                && let Some(p) = me.upgrade()
            {
                p.close();
            }
        });
        *self.live.borrow_mut() = vec![down, key];
        if let Some(f) = &self.opts.on_open_change {
            f(true);
        }
    }

    pub fn close(&self) {
        if !self.open.replace(false) {
            return;
        }
        set_style(&self.layer, "display", "none");
        self.layer.remove();
        attr(&self.opts.trigger, "aria-expanded", "false");
        let live: Vec<Listener> = self.live.borrow_mut().drain(..).collect();
        self.env.retire(Box::new(live));
        if let Some(f) = &self.opts.on_open_change {
            f(false);
        }
    }

    /// Re-read shape, presets and syncs and re-render (no-op while closed: `open` re-reads).
    pub fn refresh(&self) {
        if !self.open.get() {
            return;
        }
        self.render_presets();
        self.render_syncs();
        self.render();
    }

    /// Fixed text and square names in the current language.
    pub fn relabel(&self) {
        self.heading
            .set_text_content(Some(&self.env.t(&layout_heading())));
        self.sync_heading_el
            .set_text_content(Some(&self.env.t(&sync_heading())));
        self.tip.set_content(&self.env.t(&grid_hint()));
        let tr = self.env.translator();
        for (sq, cell) in self.squares.iter().zip(grid_cells(tr.as_deref())) {
            attr(sq, "aria-label", &cell.label);
        }
    }

    fn square_at(&self, ev: &JsValue) -> Option<(usize, usize)> {
        let sq = vst_closest_class(&vst_target(ev), "vela-lp-sq");
        if sq.is_null() {
            return None;
        }
        let sq: Element = wasm_bindgen::JsCast::unchecked_into(sq);
        Some((
            sq.get_attribute("data-r")?.parse().ok()?,
            sq.get_attribute("data-c")?.parse().ok()?,
        ))
    }

    /// Paint the hover preview, else the current layout's rectangle.
    fn render(&self) {
        let shown = self.hover.borrow().shown((self.opts.shape)());
        let lit = paint_layout_grid(shown);
        for (sq, on) in self.squares.iter().zip(lit) {
            if on {
                attr(sq, "data-on", "1");
            } else {
                let _ = sq.remove_attribute("data-on");
            }
        }
    }

    fn render_presets(&self) {
        let old: Vec<Listener> = self.rows_l.borrow_mut().drain(..).collect();
        clear(&self.presets_el);
        let presets = (self.opts.presets)();
        set_style(
            &self.presets_el,
            "display",
            if presets.is_empty() { "none" } else { "" },
        );
        let mut l = Vec::new();
        for p in presets {
            let b = make(&self.env.doc, "button", "vela-lp-preset");
            attr(&b, "type", "button");
            b.set_text_content(Some(&p.label));
            if p.checked {
                attr(&b, "data-checked", "1");
            }
            let (me, id) = (self.me.clone(), p.id.clone());
            l.push(on(&b, "click", move |_| {
                if let Some(pk) = me.upgrade() {
                    pk.close();
                    (pk.opts.on_select_preset)(&id);
                }
            }));
            self.presets_el.append_child(&b).ok();
        }
        self.rows_l.borrow_mut().extend(l);
        self.env.retire(Box::new(old));
    }

    fn render_syncs(&self) {
        // The switch rows live in their own bag so a toggle can rebuild them from inside
        // their own handler.
        let old: Vec<Listener> = self.sync_l.borrow_mut().drain(..).collect();
        self.env.retire(Box::new(old));
        clear(&self.sync_el);
        for s in (self.opts.syncs)() {
            let row = make(&self.env.doc, "button", "vela-lp-sync-row");
            attr(&row, "type", "button");
            attr(&row, "role", "switch");
            attr(
                &row,
                "aria-checked",
                if s.checked { "true" } else { "false" },
            );
            attr(&row, "data-sync", &s.id);
            let label = make(&self.env.doc, "span", "vela-lp-label");
            label.set_text_content(Some(&s.label));
            let pill = make(&self.env.doc, "span", "vela-lp-switch");
            attr(&pill, "aria-hidden", "true");
            set_class(&pill, "on", s.checked);
            row.append_child(&label).ok();
            row.append_child(&pill).ok();
            let (me, id) = (self.me.clone(), s.id.clone());
            let l = on(&row, "click", move |_| {
                if let Some(p) = me.upgrade() {
                    (p.opts.on_toggle_sync)(&id);
                    // Switches stay open: flip, see, flip again.
                    p.render_syncs();
                }
            });
            self.sync_l.borrow_mut().push(l);
            self.sync_el.append_child(&row).ok();
        }
    }

    /// Anchor under the trigger, clamped to the host's right edge.
    fn position(&self) {
        let Some(host) = self.env.host() else { return };
        let hr = host.get_bounding_client_rect();
        let tr = self.opts.trigger.get_bounding_client_rect();
        let width = vst_offset_width(&self.layer);
        let left = popover_left(tr.left(), hr.left(), width, hr.width());
        let top = popover_top(tr.bottom(), hr.top());
        set_styles(
            &self.layer,
            &[("left", &format!("{left}px")), ("top", &format!("{top}px"))],
        );
    }

    pub fn destroy(&self) {
        self.close();
        self.tip.destroy();
        self.layer.remove();
        let l: Vec<Listener> = self.perm.borrow_mut().drain(..).collect();
        drop(l);
        let l: Vec<Listener> = self.rows_l.borrow_mut().drain(..).collect();
        drop(l);
        let l: Vec<Listener> = self.sync_l.borrow_mut().drain(..).collect();
        drop(l);
    }
}
