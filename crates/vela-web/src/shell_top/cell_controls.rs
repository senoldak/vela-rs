//! The per-cell view controls (`cell-controls.ts`): a hover cluster pinned to the
//! bottom-center of a cell's price plot with a drag handle (trade slots with another cell),
//! zoom out / zoom in, maximize-or-restore and reset. Revealed by cursor proximity.
//!
//! The proximity maths, the button set and its gating, the reveal state and the grip drag are
//! [`vela_core::widget::cell_controls`]; the zoom buttons drive a
//! [`Glider`](vela_core::widget::glide::Glider) of their own on THIS cell's chart (they act
//! on the cell they live in, whatever the active cell is).

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vela_core::vela::Vela;
use vela_core::widget::cell_controls::{
    CLUSTER_LEFT_CSS, CLUSTER_PILL, CONTROLS_BOTTOM_PX, CellControlAction, ClusterReveal, GripDrag,
    GripEffect, Gutters, cell_control_buttons, grip_press_starts_drag,
};
use vela_core::widget::glide::{GlideSchedule, Glider, ZOOM_IN, ZOOM_OUT};
use wasm_bindgen::JsValue;
use web_sys::Element;

use super::js::{vst_capture, vst_ev_flag, vst_ev_num, vst_prevent, vst_stop, vst_var_px};
use super::kit::{Env, clear};
use crate::dom::{Listener, attr, make, on, set_styles};

/// What the cluster asks of its cell and the grid.
pub struct CellControlsDeps {
    /// The cell's LIVE chart (`None` once the cell is destroyed).
    pub chart: Rc<dyn Fn() -> Option<Vela>>,
    pub reset: Rc<dyn Fn()>,
    /// Whether the grid holds more than one cell: gates maximize and the drag handle.
    pub multi_cell: Rc<dyn Fn() -> bool>,
    pub is_maximized: Rc<dyn Fn() -> bool>,
    pub toggle_maximize: Rc<dyn Fn()>,
    /// The OTHER live cell under a viewport point (`None` over this cell or outside).
    pub drag_target_at: Rc<dyn Fn(f64, f64) -> Option<String>>,
    pub preview_drop: Rc<dyn Fn(Option<&str>)>,
    pub drop_on: Rc<dyn Fn(&str)>,
}

pub struct CellControls {
    env: Env,
    me: Weak<CellControls>,
    host: Element,
    root: Element,
    deps: CellControlsDeps,
    glider: RefCell<Glider>,
    frame: Cell<Option<vela_core::runtime::TimerId>>,
    reveal: RefCell<ClusterReveal>,
    buttons_l: RefCell<Vec<Listener>>,
    host_l: RefCell<Vec<Listener>>,
    drag: RefCell<Option<(GripDrag, Vec<Listener>)>>,
}

impl CellControls {
    pub fn new(env: &Env, host: &Element, deps: CellControlsDeps) -> Rc<CellControls> {
        let root = make(&env.doc, "div", "vela-cell-controls");
        attr(&root, "role", "group");
        set_styles(
            &root,
            &[
                ("position", "absolute"),
                ("left", CLUSTER_LEFT_CSS),
                ("bottom", &format!("{CONTROLS_BOTTOM_PX}px")),
                ("transform", "translateX(-50%)"),
                ("z-index", "6"),
                ("display", "none"), // revealed by cursor proximity
                ("gap", "2px"),
                ("padding", "2px"),
                ("border-radius", "var(--vela-radius-md)"),
                ("background", CLUSTER_PILL),
                ("pointer-events", "auto"),
            ],
        );
        let this = Rc::new_cyclic(|me| CellControls {
            env: env.clone(),
            me: me.clone(),
            host: host.clone(),
            root: root.clone(),
            deps,
            glider: RefCell::new(Glider::new()),
            frame: Cell::new(None),
            reveal: RefCell::new(ClusterReveal::new()),
            buttons_l: RefCell::new(Vec::new()),
            host_l: RefCell::new(Vec::new()),
            drag: RefCell::new(None),
        });
        // Hover on the HOST (not a canvas child) so moving the cursor onto the cluster, a
        // host child above the canvases, keeps it revealed.
        let me = this.me.clone();
        let l1 = on(host, "pointermove", move |ev| {
            if let Some(c) = me.upgrade() {
                c.on_host_move(&ev);
            }
        });
        let me = this.me.clone();
        let l2 = on(host, "pointerleave", move |_| {
            if let Some(c) = me.upgrade() {
                let changed = c.reveal.borrow_mut().on_host_leave();
                if changed {
                    c.paint_reveal();
                }
            }
        });
        *this.host_l.borrow_mut() = vec![l1, l2];
        host.append_child(&root).ok();
        this.refresh();
        this
    }

    fn chart(&self) -> Option<Vela> {
        (self.deps.chart)()
    }

    /// Rebuild the buttons (the multi-cell gate, the maximized state or the language changed).
    pub fn refresh(&self) {
        let old: Vec<Listener> = self.buttons_l.borrow_mut().drain(..).collect();
        clear(&self.root);
        let multi = (self.deps.multi_cell)();
        let maximized = multi && (self.deps.is_maximized)();
        let mut l = Vec::new();
        for b in cell_control_buttons(multi, maximized) {
            let doc = &self.env.doc;
            let el = make(doc, "button", "vela-cc-btn");
            attr(&el, "type", "button");
            let title = self.env.t(&b.label);
            attr(&el, "title", &title);
            attr(&el, "aria-label", &title);
            attr(
                &el,
                "data-action",
                match b.action {
                    CellControlAction::Grip => "grip",
                    CellControlAction::ZoomOut => "zoom-out",
                    CellControlAction::ZoomIn => "zoom-in",
                    CellControlAction::ToggleMaximize => "maximize",
                    CellControlAction::Reset => "reset",
                },
            );
            if b.selected {
                attr(&el, "class", "vela-cc-btn vela-cc-on");
                attr(&el, "aria-pressed", "true");
            }
            super::js::vst_set_html(&el, &super::kit::icon_html_px(b.icon, 12));
            let me = self.me.clone();
            match b.action {
                CellControlAction::Grip => {
                    attr(&el, "class", "vela-cc-btn vela-cc-grip");
                    let btn = el.clone();
                    l.push(on(&el, "pointerdown", move |ev| {
                        if let Some(c) = me.upgrade() {
                            c.grip_down(&btn, &ev);
                        }
                    }));
                }
                action => {
                    l.push(on(&el, "click", move |ev| {
                        vst_stop(&ev); // a click on the cluster never reaches the chart
                        if let Some(c) = me.upgrade() {
                            c.press(action);
                        }
                    }));
                }
            }
            self.root.append_child(&el).ok();
        }
        *self.buttons_l.borrow_mut() = l;
        self.env.retire(Box::new(old));
    }

    fn press(&self, action: CellControlAction) {
        match action {
            CellControlAction::ZoomOut => self.zoom(ZOOM_OUT),
            CellControlAction::ZoomIn => self.zoom(ZOOM_IN),
            CellControlAction::ToggleMaximize => (self.deps.toggle_maximize)(),
            CellControlAction::Reset => {
                self.stop_glide(); // a running zoom glide must not fight the reset
                (self.deps.reset)();
            }
            CellControlAction::Grip => {}
        }
    }

    fn zoom(&self, factor: f64) {
        let chart = self.chart();
        let schedule = self.glider.borrow_mut().zoom(chart.as_ref(), factor);
        if schedule == GlideSchedule::NextFrame {
            self.schedule_frame();
        }
    }

    fn schedule_frame(&self) {
        let me = self.me.clone();
        let id = self.env.after(16.0, move || {
            let Some(c) = me.upgrade() else { return };
            c.frame.set(None);
            let chart = c.chart();
            let more = c.glider.borrow_mut().frame(chart.as_ref());
            if more {
                c.schedule_frame();
            }
        });
        self.frame.set(Some(id));
    }

    fn stop_glide(&self) {
        self.glider.borrow_mut().stop();
        if let Some(id) = self.frame.take() {
            self.env.platform.clear_timeout(id);
        }
    }

    /// Mobile flips the cluster off entirely (and hides it if currently revealed).
    pub fn set_suspended(&self, on: bool) {
        let changed = self.reveal.borrow_mut().set_suspended(on);
        if changed {
            self.paint_reveal();
        }
    }

    pub fn visible(&self) -> bool {
        self.reveal.borrow().visible()
    }

    fn paint_reveal(&self) {
        let v = self.reveal.borrow().visible();
        set_styles(&self.root, &[("display", if v { "flex" } else { "none" })]);
    }

    fn on_host_move(&self, ev: &JsValue) {
        let rect = self.host.get_bounding_client_rect();
        let gutters = Gutters {
            toolbar: vst_var_px(&self.host, "--vela-toolbar-gutter"),
            scale: vst_var_px(&self.host, "--vela-scale-gutter"),
        };
        let changed = self.reveal.borrow_mut().on_host_move(
            vst_ev_num(ev, "clientX") - rect.left(),
            vst_ev_num(ev, "clientY") - rect.top(),
            rect.width(),
            rect.height(),
            gutters,
        );
        if changed {
            self.paint_reveal();
        }
    }

    /// The drag handle: press and drag onto another cell to trade slots with it. The preview
    /// highlight follows the pointer; releasing outside any other cell cancels.
    fn grip_down(&self, btn: &Element, ev: &JsValue) {
        let is_mouse = ev_str(ev, "pointerType") == "mouse" || ev_str(ev, "pointerType").is_empty();
        if !grip_press_starts_drag(vst_ev_num(ev, "button") as i32, is_mouse) {
            return;
        }
        vst_prevent(ev);
        vst_stop(ev);
        vst_capture(btn, vst_ev_num(ev, "pointerId") as i32);
        self.reveal.borrow_mut().set_dragging(true);
        let mut l = Vec::new();
        let me = self.me.clone();
        l.push(on(btn, "pointermove", move |e| {
            let Some(c) = me.upgrade() else { return };
            let target =
                (c.deps.drag_target_at)(vst_ev_num(&e, "clientX"), vst_ev_num(&e, "clientY"));
            let effect = c.drag.borrow_mut().as_mut().map(|(d, _)| d.on_move(target));
            if let Some(effect) = effect {
                c.run_effect(effect);
            }
        }));
        for (name, commit) in [("pointerup", true), ("pointercancel", false)] {
            let me = self.me.clone();
            l.push(on(btn, name, move |_| {
                let Some(c) = me.upgrade() else { return };
                c.finish_drag(commit);
            }));
        }
        *self.drag.borrow_mut() = Some((GripDrag::start(), l));
    }

    fn run_effect(&self, effect: GripEffect) {
        match effect {
            GripEffect::Preview(id) => (self.deps.preview_drop)(id.as_deref()),
            GripEffect::Drop(id) => (self.deps.drop_on)(&id),
        }
    }

    fn finish_drag(&self, commit: bool) {
        let taken = self.drag.borrow_mut().take();
        self.reveal.borrow_mut().set_dragging(false);
        let Some((drag, listeners)) = taken else {
            return;
        };
        // The listeners of the gesture are freed on a fresh stack: this runs inside one.
        self.env.retire(Box::new(listeners));
        for effect in drag.finish(commit) {
            self.run_effect(effect);
        }
    }

    pub fn destroy(&self) {
        self.stop_glide();
        if self.drag.borrow().is_some() {
            self.finish_drag(false);
        }
        self.host_l.borrow_mut().clear();
        self.buttons_l.borrow_mut().clear();
        self.root.remove();
    }
}

fn ev_str(ev: &JsValue, name: &str) -> String {
    crate::dom::get_str(ev, name)
}

#[allow(dead_code)]
fn unused(ev: &JsValue) -> bool {
    vst_ev_flag(ev, "x")
}
