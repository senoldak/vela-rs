//! The DOM half of the original's `InputController` and `KeyboardController`,
//! and the observers that keep the canvas pile sized (`ResizeObserver`, the
//! `devicePixelRatio` media-query watcher).
//!
//! The listeners translate pointer, wheel, touch and key events into the
//! renderer's gesture entry points (`NativeRenderer::pointer_move`, `click`,
//! `apply_viewport`, `zoom_to`, `fling`, `begin_price_scale`, ...). What a
//! gesture MEANS (regions, wheel intent, drag arithmetic, fling, tap and
//! double-tap rules, key mapping) is decided by
//! [`vela_core::render::core::input_logic`]; this module only keeps the press
//! state machine (`onDown` / `onMove` / `onUp`) and the DOM plumbing.
//!
//! The user-drawings claims of the original controller are wired to
//! [`UserDrawingController`] (see its "Input" section): `clear_transient` on
//! every press, `claim` on a primary press, `begin_measure_at` on Shift and
//! `begin_marquee_at` on Ctrl/Cmd over the empty data area, middle `delete_at`,
//! right `cancel_placement`, hover `pointer_move` and `cursor_at`, `dbl_click`
//! and the keyboard pre-empt (`handle_key`), plus the modifier re-shape of the
//! placing ghost. A press the drawings layer does not claim stays a pan, a click
//! or an axis gesture.
//!
//! # Re-entrancy
//!
//! Every handler runs from a browser event, never from inside a host member.
//! No `RefCell` of this module is borrowed across a renderer call, because a
//! renderer subscriber may call back into the chart (for instance destroy it).

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::Function;
use vela_core::drawings::SnapMode;
use vela_core::options::FreePanEvent;
use vela_core::ports::chart_renderer::ChartRenderer;
use vela_core::ports::chart_renderer::{Axis, AxisLongPressEvent};
use vela_core::render::core::ViewportState;
use vela_core::render::core::coordinate_system::CoordinateSystem;
use vela_core::render::core::input_logic::{
    DoubleActivation, DragRegion, InputSettings, KeyInput, LONG_PRESS_MS, TOUCH_SLOP, WheelInput,
    WheelIntent, classify_wheel, double_activate, drag_pan_right_offset, drag_slop,
    effective_snap_mode, fling_velocity, is_tap_release, matches_free_pan, pairs_as_double_tap,
    pinch_bar_spacing, pinch_pinned_right_offset, region_at, smooth_velocity,
    time_axis_zoom_spacing,
};
use vela_core::render::drawings::{DrawingKeyEvent, UserDrawingController, is_editing_text};
use vela_core::render::native::NativeRenderer;
use vela_core::runtime::{Platform, TimerId};
use wasm_bindgen::prelude::*;
use web_sys::{
    Element, EventTarget, HtmlCanvasElement, KeyboardEvent, MouseEvent, PointerEvent, WheelEvent,
};

use crate::dom::{
    Listener, default_view, get_bool, get_str, remove_style, set_style, vela_capture_pointer,
    vela_disconnect, vela_observe_size, vela_release_pointer, vela_watch_dpr,
};
use crate::host::{BrowserHost, DataWatcher};

/// The press state of the controller (the original's private fields).
struct Press {
    dragging: bool,
    moved: bool,
    region: DragRegion,
    /// The data-area drag also pans price (the pane is in manual-scale mode).
    vertical_pan: bool,
    /// The data-area drag unlocks manual price panning once past the slop.
    free_pending: bool,
    start_x: f64,
    start_y: f64,
    start_right_offset: f64,
    start_bar_spacing: f64,
    // velocity tracking for the inertial flick
    last_x: f64,
    last_t: f64,
    vx: f64,
    // touch
    touches: Vec<(i32, (f64, f64))>,
    long_press: Option<TimerId>,
    pinch_start_dist: f64,
    pinch_start_bar_spacing: f64,
    pinch_anchor_logical: f64,
    last_tap_t: f64,
    last_tap_x: f64,
    last_tap_y: f64,
    // user drawings
    /// The last middle press deleted a drawing (suppress autoscroll / paste).
    middle_deleted: bool,
    /// The last right press cancelled a placement (suppress the context menu).
    right_cancelled: bool,
    /// The last cursor position over the plot (`None` once it leaves): lets a
    /// modifier press / release re-shape the drawings preview while the pointer rests.
    cursor: Option<(f64, f64)>,
    /// Whether the last two primary presses were claimed by the drawings layer: a
    /// double-click is the last press plus the one before it.
    last_press_drawing: bool,
    prev_press_drawing: bool,
}

impl Press {
    fn new() -> Self {
        Self {
            dragging: false,
            moved: false,
            region: DragRegion::Data,
            vertical_pan: false,
            free_pending: false,
            start_x: 0.0,
            start_y: 0.0,
            start_right_offset: 0.0,
            start_bar_spacing: 0.0,
            last_x: 0.0,
            last_t: 0.0,
            vx: 0.0,
            touches: Vec::new(),
            long_press: None,
            pinch_start_dist: 1.0,
            pinch_start_bar_spacing: 0.0,
            pinch_anchor_logical: 0.0,
            last_tap_t: 0.0,
            last_tap_x: 0.0,
            last_tap_y: 0.0,
            middle_deleted: false,
            right_cancelled: false,
            cursor: None,
            last_press_drawing: false,
            prev_press_drawing: false,
        }
    }
}

/// What the handlers share.
struct Shared {
    renderer: Rc<NativeRenderer>,
    host: Rc<BrowserHost>,
    /// The data canvas the listeners sit on: it follows the host's `Data` canvas when the
    /// geometry backend swaps it (WebGL2 given up for canvas2d).
    el: RefCell<HtmlCanvasElement>,
    platform: Rc<dyn Platform>,
    press: RefCell<Press>,
}

fn cursor_of(region: DragRegion) -> &'static str {
    match region {
        DragRegion::Price => "ns-resize",
        DragRegion::Time => "ew-resize",
        DragRegion::Separator => "row-resize",
        _ => "",
    }
}

/// Whether the key event targets an input, textarea or content-editable field.
fn key_target_is_text(e: &KeyboardEvent) -> bool {
    let Some(target) = e.target() else {
        return false;
    };
    let target = JsValue::from(target);
    let tag = get_str(&target, "tagName");
    is_editing_text(
        (!tag.is_empty()).then_some(tag.as_str()),
        get_bool(&target, "isContentEditable"),
    )
}

impl Shared {
    /// The data canvas now.
    fn el(&self) -> HtmlCanvasElement {
        self.el.borrow().clone()
    }

    /// Event position relative to the data canvas (the plot).
    fn local(&self, client_x: f64, client_y: f64) -> (f64, f64) {
        let rect = self.el().get_bounding_client_rect();
        (client_x - rect.left(), client_y - rect.top())
    }

    fn local_of_mouse(&self, e: &MouseEvent) -> (f64, f64) {
        self.local(f64::from(e.client_x()), f64::from(e.client_y()))
    }

    fn region_at(
        &self,
        coords: &CoordinateSystem,
        settings: InputSettings,
        x: f64,
        y: f64,
    ) -> DragRegion {
        region_at(coords.width(), coords.height(), x, y, settings, &|sy| {
            self.renderer.pane_separator_at(sy)
        })
    }

    /// The user-drawings controller, while it exists.
    fn drawings(&self) -> Option<Rc<UserDrawingController>> {
        self.renderer.user_drawings_controller()
    }

    /// The controller, only while the `drawingsInteractive` feature is on (the browser
    /// asks it nothing otherwise; a gesture already under way still finishes).
    fn drawings_on(&self) -> Option<Rc<UserDrawingController>> {
        if self.renderer.drawings_interactive() {
            self.drawings()
        } else {
            None
        }
    }

    /// The magnet mode in effect: Ctrl/Cmd forces strong, else the toolbar's sticky mode.
    fn snap_of(&self, ctrl_or_meta: bool) -> SnapMode {
        effective_snap_mode(ctrl_or_meta, self.renderer.snap_mode())
    }

    fn cancel_long_press(&self) {
        let timer = self.press.borrow_mut().long_press.take();
        if let Some(t) = timer {
            self.platform.clear_timeout(t);
        }
    }

    fn end_gesture(&self, pointer_id: i32) {
        {
            let mut p = self.press.borrow_mut();
            p.dragging = false;
            p.free_pending = false;
        }
        vela_release_pointer(&self.el(), pointer_id);
    }

    // ── pointer down ──

    fn on_down(self: &Rc<Self>, e: &PointerEvent) {
        let (x, y) = self.local_of_mouse(e);
        let coords = self.renderer.coordinates();
        let settings = self.renderer.input_settings();
        let region = self.region_at(&coords, settings, x, y);
        let button = e.button();
        let touch = e.pointer_type() == "touch";
        let free_match = region == DragRegion::Data
            && self.renderer.free_pan_trigger().is_some_and(|trigger| {
                matches_free_pan(
                    &trigger,
                    &FreePanEvent {
                        button: i32::from(button),
                        alt_key: e.alt_key(),
                        shift_key: e.shift_key(),
                        ctrl_key: e.ctrl_key(),
                        meta_key: e.meta_key(),
                        pointer_type: e.pointer_type(),
                    },
                )
            });
        // Middle-click deletes the drawing under the cursor; the flag lets the mousedown /
        // auxclick companions suppress autoscroll and paste for THIS press only.
        if button == 1 && !free_match {
            let deleted = self
                .drawings_on()
                .is_some_and(|ctl| ctl.delete_at(x, y, true));
            self.press.borrow_mut().middle_deleted = deleted;
            if deleted {
                e.prevent_default();
            }
            return;
        }
        // Right-click cancels or disarms the active drawing tool, ruler or eraser; the
        // flag lets the contextmenu companion suppress the host's menu for THIS press only.
        if button == 2 && !free_match {
            let cancelled = self.drawings_on().is_some_and(|ctl| ctl.cancel_placement());
            self.press.borrow_mut().right_cancelled = cancelled;
            if cancelled {
                e.prevent_default();
            }
            return;
        }
        // Any other non-primary press is only ever a free-pan chord.
        if button != 0 && !free_match {
            return;
        }
        if button != 0 {
            if button == 1 {
                self.press.borrow_mut().middle_deleted = true;
            }
            e.prevent_default();
        }
        if touch {
            // A second finger during a pan (or long-press crosshair) escalates to a
            // pinch; beyond two contacts, or during an axis gesture, extra fingers
            // are ignored so they cannot corrupt the gesture in flight.
            let escalate = {
                let mut p = self.press.borrow_mut();
                if p.touches.len() == 1
                    && p.dragging
                    && matches!(p.region, DragRegion::Data | DragRegion::Crosshair)
                {
                    p.touches.push((e.pointer_id(), (x, y)));
                    Some(true)
                } else if !p.touches.is_empty() {
                    Some(false)
                } else {
                    p.touches.push((e.pointer_id(), (x, y)));
                    None
                }
            };
            match escalate {
                Some(true) => return self.begin_pinch(e, &coords),
                Some(false) => return,
                None => {}
            }
        }
        // A finished ruler vanishes on the next press (the pan still proceeds); an open
        // inline edit ends.
        if let Some(ctl) = self.drawings() {
            ctl.clear_transient();
        }
        {
            let mut p = self.press.borrow_mut();
            p.dragging = true;
            p.moved = false;
            p.start_x = x;
            p.start_y = y;
            p.prev_press_drawing = p.last_press_drawing;
            p.last_press_drawing = false;
        }
        let shift = e.shift_key();
        let ctrl_or_meta = e.ctrl_key() || e.meta_key();
        let on_data = region == DragRegion::Data;
        // The drawings layer gets first refusal: with a tool armed, or over a drawing or
        // one of its handles, it claims the WHOLE gesture (no pan, no fling).
        if let Some(ctl) = self.drawings_on() {
            let snap = self.snap_of(ctrl_or_meta);
            if button == 0 && ctl.claim(x, y) {
                self.claimed(e);
                ctl.pointer_down(x, y, snap, shift, ctrl_or_meta);
                return;
            }
            // Shift+press on the empty plot starts the measure ruler in one gesture (a
            // press over a drawing keeps the additive-select meaning of Shift, via the
            // claim above).
            if !free_match && shift && on_data && ctl.begin_measure_at(x, y, snap) {
                self.claimed(e);
                return;
            }
            // Ctrl/Cmd+press on the empty plot sweeps a selection box.
            if !free_match && ctrl_or_meta && on_data && ctl.begin_marquee_at(x, y) {
                self.claimed(e);
                return;
            }
        }
        // Freeze any in-flight zoom or fling at its current position before grabbing.
        let vp = coords.viewport();
        self.renderer.apply_viewport(vp);
        let (mut vertical_pan, mut free_pending) = (false, false);
        match region {
            DragRegion::Price => self.renderer.begin_price_scale(x, y),
            DragRegion::Separator => self.renderer.begin_pane_resize(y),
            DragRegion::Data => {
                vertical_pan = self.renderer.begin_price_pan(y, false);
                free_pending = !vertical_pan && free_match;
            }
            _ => {}
        }
        {
            let mut p = self.press.borrow_mut();
            p.start_right_offset = vp.right_offset;
            p.start_bar_spacing = vp.bar_spacing;
            p.region = region;
            p.vertical_pan = vertical_pan;
            p.free_pending = free_pending;
            p.last_x = x;
            p.last_t = e.time_stamp();
            p.vx = 0.0;
        }
        // A stationary touch hold in the plot becomes crosshair inspection (a finger
        // cannot hover); on an axis strip the same hold raises an axis long-press.
        if touch
            && matches!(
                region,
                DragRegion::Data | DragRegion::Price | DragRegion::Time
            )
        {
            self.start_long_press(x, y, region);
        }
        vela_capture_pointer(&self.el(), e.pointer_id());
    }

    /// The drawings layer took the press: it owns the rest of the gesture.
    fn claimed(&self, e: &PointerEvent) {
        {
            let mut p = self.press.borrow_mut();
            p.region = DragRegion::Drawing;
            p.last_press_drawing = true;
            p.free_pending = false;
            p.vertical_pan = false;
        }
        vela_capture_pointer(&self.el(), e.pointer_id());
    }

    fn start_long_press(self: &Rc<Self>, x: f64, y: f64, region: DragRegion) {
        self.cancel_long_press();
        let me = Rc::downgrade(self);
        let id = self.platform.set_timeout(
            LONG_PRESS_MS,
            Box::new(move || {
                if let Some(s) = me.upgrade() {
                    s.on_long_press(x, y, region);
                }
            }),
        );
        self.press.borrow_mut().long_press = Some(id);
    }

    fn on_long_press(&self, x: f64, y: f64, region: DragRegion) {
        {
            let mut p = self.press.borrow_mut();
            p.long_press = None;
            if !p.dragging || p.region != region {
                return;
            }
            if region == DragRegion::Data {
                p.region = DragRegion::Crosshair;
            } else {
                // Freeze the gesture so a later wobble cannot rescale or zoom.
                p.region = DragRegion::AxisSheet;
                p.moved = true;
            }
        }
        match region {
            DragRegion::Data => self.renderer.pointer_move(x, y),
            DragRegion::Price | DragRegion::Time => {
                self.renderer.axis_long_press(AxisLongPressEvent {
                    axis: if region == DragRegion::Price {
                        Axis::Price
                    } else {
                        Axis::Time
                    },
                    x,
                    y,
                });
            }
            _ => {}
        }
    }

    /// Two fingers down: freeze the view and re-base the gesture as an anchored pinch.
    fn begin_pinch(&self, e: &PointerEvent, coords: &CoordinateSystem) {
        self.cancel_long_press();
        if self.press.borrow().region == DragRegion::Crosshair {
            self.renderer.pointer_leave(); // inspection ends; the pinch owns the gesture
        }
        let vp = coords.viewport();
        self.renderer.apply_viewport(vp); // freeze any in-flight fling or zoom
        {
            let mut p = self.press.borrow_mut();
            let (a, b) = (p.touches[0].1, p.touches[1].1);
            p.pinch_start_dist = (a.0 - b.0).hypot(a.1 - b.1).max(1.0);
            p.pinch_start_bar_spacing = vp.bar_spacing;
            p.pinch_anchor_logical = coords.x_to_logical((a.0 + b.0) / 2.0);
            p.region = DragRegion::Pinch;
            p.dragging = true;
            p.moved = true; // a pinch is never a click
            p.vx = 0.0;
        }
        vela_capture_pointer(&self.el(), e.pointer_id());
    }

    /// Track both fingers: the distance ratio zooms, the live midpoint pans.
    fn apply_pinch(&self) {
        let (a, b, start_dist, start_bs, anchor) = {
            let p = self.press.borrow();
            if p.touches.len() < 2 {
                return;
            }
            (
                p.touches[0].1,
                p.touches[1].1,
                p.pinch_start_dist,
                p.pinch_start_bar_spacing,
                p.pinch_anchor_logical,
            )
        };
        let coords = self.renderer.coordinates();
        let bar_spacing = pinch_bar_spacing(start_bs, start_dist, (a.0 - b.0).hypot(a.1 - b.1));
        // The spacing multiplier is constant through the gesture: derive it from the
        // live effective pitch so the pin math matches logical_to_x exactly.
        let vp = coords.viewport();
        let pitch_scale = if vp.bar_spacing > 0.0 {
            coords.px_per_bar() / vp.bar_spacing
        } else {
            1.0
        };
        let mid_x = (a.0 + b.0) / 2.0;
        self.renderer.apply_viewport(ViewportState {
            bar_spacing,
            right_offset: pinch_pinned_right_offset(
                anchor,
                coords.bar_count(),
                coords.width(),
                mid_x,
                bar_spacing * pitch_scale,
            ),
        });
    }

    // ── pointer move ──

    fn on_move(&self, e: &PointerEvent) {
        let (x, y) = self.local_of_mouse(e);
        let touch = e.pointer_type() == "touch";
        let (dragging, region) = {
            let mut p = self.press.borrow_mut();
            p.cursor = Some((x, y));
            if touch && let Some(t) = p.touches.iter_mut().find(|(id, _)| *id == e.pointer_id()) {
                t.1 = (x, y);
            }
            (p.dragging, p.region)
        };
        if dragging {
            match region {
                DragRegion::Pinch => {
                    self.apply_pinch();
                    return; // no crosshair while pinching: two fingers name no single point
                }
                DragRegion::Crosshair => {
                    self.renderer.pointer_move(x, y); // the finger drives the crosshair
                    return;
                }
                DragRegion::AxisSheet => return, // sheet already raised
                _ => {}
            }
        }
        // Once a touch travels past the wobble slop it is a pan, not a nascent long-press.
        let (start_x, start_y, pending_press) = {
            let p = self.press.borrow();
            (p.start_x, p.start_y, p.long_press.is_some())
        };
        if pending_press && (x - start_x).hypot(y - start_y) > TOUCH_SLOP {
            self.cancel_long_press();
        }
        let slop = drag_slop(touch);
        if dragging {
            self.drag(e, x, y, region, slop);
        } else {
            // Cursor affordance: a lane glyph reads as clickable, else the draggable
            // axis-strip cursors.
            let coords = self.renderer.coordinates();
            let r = self.region_at(&coords, self.renderer.input_settings(), x, y);
            let drawings = self.drawings_on();
            let cursor = match drawings.as_ref().and_then(|ctl| ctl.cursor_at(x, y)) {
                Some(c) => c,
                None if self.renderer.mark_glyph_at(x, y) => "pointer",
                None => cursor_of(r),
            };
            if cursor.is_empty() {
                remove_style(&self.el(), "cursor");
            } else {
                set_style(&self.el(), "cursor", cursor);
            }
            // Hover moves feed the drawings layer so a placing ghost advances (placing is
            // click-based: the cursor follows with no button down) and handles show.
            if let Some(ctl) = drawings {
                let ctrl_or_meta = e.ctrl_key() || e.meta_key();
                ctl.pointer_move(
                    x,
                    y,
                    self.snap_of(ctrl_or_meta),
                    e.shift_key(),
                    ctrl_or_meta,
                );
            }
        }
        // Hover crosshair is a MOUSE affordance: a touch never hovers, its crosshair
        // comes only from the long-press path above.
        if !touch {
            self.renderer.pointer_move(x, y);
        }
    }

    fn drag(&self, e: &PointerEvent, x: f64, y: f64, region: DragRegion, slop: f64) {
        let (start_x, start_y, start_ro, start_bs) = {
            let p = self.press.borrow();
            (
                p.start_x,
                p.start_y,
                p.start_right_offset,
                p.start_bar_spacing,
            )
        };
        match region {
            DragRegion::Price => {
                if (y - start_y).abs() > slop {
                    self.press.borrow_mut().moved = true;
                }
                self.renderer.price_scale_by(y - start_y);
            }
            DragRegion::Separator => {
                if (y - start_y).abs() > slop {
                    self.press.borrow_mut().moved = true;
                }
                self.renderer.pane_resize_by(y - start_y);
            }
            DragRegion::Time => {
                if (x - start_x).abs() > slop {
                    self.press.borrow_mut().moved = true;
                }
                // Pin the right edge (keep right_offset): only the bar spacing changes.
                self.renderer.apply_viewport(ViewportState {
                    bar_spacing: time_axis_zoom_spacing(start_bs, start_x, x),
                    right_offset: start_ro,
                });
            }
            DragRegion::Drawing => {
                if (x - start_x).abs() > slop || (y - start_y).abs() > slop {
                    self.press.borrow_mut().moved = true;
                }
                if let Some(ctl) = self.drawings() {
                    let ctrl_or_meta = e.ctrl_key() || e.meta_key();
                    ctl.pointer_move(
                        x,
                        y,
                        self.snap_of(ctrl_or_meta),
                        e.shift_key(),
                        ctrl_or_meta,
                    );
                }
            }
            _ => {
                let coords = self.renderer.coordinates();
                let vp = coords.viewport();
                let dx = x - start_x;
                let (free_pending, moved_now) = {
                    let mut p = self.press.borrow_mut();
                    if dx.abs() > slop {
                        p.moved = true;
                    }
                    (p.free_pending, p.moved)
                };
                if free_pending && (moved_now || (y - start_y).abs() > slop) {
                    let vertical = self.renderer.begin_price_pan(start_y, true);
                    let mut p = self.press.borrow_mut();
                    p.free_pending = false;
                    p.moved = true;
                    p.vertical_pan = vertical;
                }
                // Drag right reveals earlier bars: right_offset decreases, tracked by the
                // effective pitch so the chart follows the cursor 1:1.
                self.renderer.apply_viewport(ViewportState {
                    bar_spacing: vp.bar_spacing,
                    right_offset: drag_pan_right_offset(start_ro, dx, coords.px_per_bar()),
                });
                let vertical = self.press.borrow().vertical_pan;
                if vertical {
                    if (y - start_y).abs() > slop {
                        self.press.borrow_mut().moved = true;
                    }
                    self.renderer.price_pan_by(y - start_y);
                }
                // A low-passed pointer velocity for the release flick.
                let mut p = self.press.borrow_mut();
                let dt = e.time_stamp() - p.last_t;
                if let Some(v) = smooth_velocity(p.vx, x, p.last_x, dt) {
                    p.vx = v;
                    p.last_x = x;
                    p.last_t = e.time_stamp();
                }
            }
        }
    }

    // ── pointer up / cancel / leave ──

    fn on_up(&self, e: &PointerEvent) {
        let touch = e.pointer_type() == "touch";
        if touch {
            self.press
                .borrow_mut()
                .touches
                .retain(|(id, _)| *id != e.pointer_id());
        }
        self.cancel_long_press();
        let (dragging, region) = {
            let p = self.press.borrow();
            (p.dragging, p.region)
        };
        if dragging && region == DragRegion::Pinch {
            let rest = {
                let p = self.press.borrow();
                (p.touches.len() == 1).then(|| p.touches[0].1)
            };
            if let Some((rx, ry)) = rest {
                // One finger lifted: the survivor continues as a plain pan, re-based at
                // its current position so the view does not jump.
                let vp = self.renderer.coordinates().viewport();
                let mut p = self.press.borrow_mut();
                p.region = DragRegion::Data;
                p.vertical_pan = false;
                p.start_x = rx;
                p.start_y = ry;
                p.start_right_offset = vp.right_offset;
                p.start_bar_spacing = vp.bar_spacing;
                p.last_x = rx;
                p.last_t = e.time_stamp();
                p.vx = 0.0;
                return; // still dragging with the remaining finger
            }
            self.end_gesture(e.pointer_id());
            return;
        }
        if dragging && region == DragRegion::Crosshair {
            self.renderer.pointer_leave(); // lifting the finger ends the readout
            self.end_gesture(e.pointer_id());
            return;
        }
        if dragging && region == DragRegion::AxisSheet {
            self.end_gesture(e.pointer_id());
            return;
        }
        let (x, y) = self.local_of_mouse(e);
        let (start_x, start_y, moved, last_t, vx) = {
            let p = self.press.borrow();
            (p.start_x, p.start_y, p.moved, p.last_t, p.vx)
        };
        let tap_release = is_tap_release(dragging, moved, touch, (x - start_x).hypot(y - start_y));
        if dragging && region == DragRegion::Drawing {
            if let Some(ctl) = self.drawings() {
                ctl.pointer_up(x, y, self.snap_of(e.ctrl_key() || e.meta_key()));
            }
        } else if dragging && region == DragRegion::Separator {
            self.renderer.end_pane_resize();
        } else if tap_release && region == DragRegion::Data && e.button() == 0 {
            self.renderer.click(x, y);
        } else if dragging && region == DragRegion::Data && touch {
            // Only a finger flicks: a mouse release stops dead, so a scrub lands exactly
            // where the pointer let go.
            let pitch = self.renderer.coordinates().px_per_bar();
            if let Some(v) = fling_velocity(vx, e.time_stamp() - last_t, pitch, touch) {
                self.renderer.fling(v);
            }
        }
        // A clean touch release is a tap: feed the double-tap pairing (touch has no
        // native dblclick here).
        let was_tap = touch && tap_release;
        self.end_gesture(e.pointer_id());
        // A finger leaves no resting cursor behind.
        if touch {
            self.renderer.pointer_leave();
        }
        if was_tap {
            self.register_tap(e.time_stamp(), x, y);
        }
    }

    fn register_tap(&self, t: f64, x: f64, y: f64) {
        let paired = {
            let p = self.press.borrow();
            pairs_as_double_tap(p.last_tap_t, p.last_tap_x, p.last_tap_y, t, x, y)
        };
        if paired {
            // Route by the FIRST tap: that one was aimed; the second is the sloppier
            // confirmation and can drift onto a neighboring region.
            let (fx, fy) = {
                let mut p = self.press.borrow_mut();
                p.last_tap_t = 0.0; // a triple tap starts a fresh pair, not two doubles
                (p.last_tap_x, p.last_tap_y)
            };
            self.double_activate(fx, fy);
            return;
        }
        let mut p = self.press.borrow_mut();
        p.last_tap_t = t;
        p.last_tap_x = x;
        p.last_tap_y = y;
    }

    /// The browser took the pointer back (system gesture, palm rejection): drop
    /// the gesture where it is, with no click, no fling and no stuck `dragging`.
    fn on_cancel(&self, e: &PointerEvent) {
        let touch = e.pointer_type() == "touch";
        if touch {
            self.press
                .borrow_mut()
                .touches
                .retain(|(id, _)| *id != e.pointer_id());
        }
        self.cancel_long_press();
        let (dragging, region) = {
            let p = self.press.borrow();
            (p.dragging, p.region)
        };
        if !dragging {
            return;
        }
        // A cancelled drawings gesture lands where the pointer last was.
        if region == DragRegion::Drawing {
            let cursor = self.press.borrow().cursor;
            if let (Some((cx, cy)), Some(ctl)) = (cursor, self.drawings()) {
                ctl.pointer_up(cx, cy, self.snap_of(e.ctrl_key() || e.meta_key()));
            }
        }
        // A cancelled separator drag keeps the sizes it reached, so it still reports them.
        if region == DragRegion::Separator {
            self.renderer.end_pane_resize();
        }
        if region == DragRegion::Crosshair || touch {
            self.renderer.pointer_leave();
        }
        self.end_gesture(e.pointer_id());
    }

    fn double_activate(&self, x: f64, y: f64) {
        // A drawing double-click opens its settings (or edits its text): no view reset.
        if let Some(ctl) = self.drawings_on()
            && ctl.dbl_click(x, y)
        {
            return;
        }
        let coords = self.renderer.coordinates();
        let region = self.region_at(&coords, self.renderer.input_settings(), x, y);
        let (last, prev) = {
            let p = self.press.borrow();
            (p.last_press_drawing, p.prev_press_drawing)
        };
        match double_activate(region, last, prev) {
            DoubleActivation::ResetPriceScale => self.renderer.reset_price_scale(x, y),
            DoubleActivation::ResetPaneSize => self.renderer.reset_pane_size(y),
            DoubleActivation::ResetView => self.renderer.reset_view(),
            DoubleActivation::DataDblClick => self.renderer.data_double_click(x, y),
            DoubleActivation::Ignore => {}
        }
    }

    // ── wheel ──

    fn on_wheel(&self, e: &WheelEvent) {
        e.prevent_default();
        if let Some(ctl) = self.drawings() {
            ctl.clear_transient(); // a finished ruler vanishes on zoom and pan
        }
        let (x, y) = self.local_of_mouse(e);
        let coords = self.renderer.coordinates();
        let intent = classify_wheel(
            &coords,
            &WheelInput {
                x,
                y,
                delta_x: e.delta_x(),
                delta_y: e.delta_y(),
                shift: e.shift_key(),
                ctrl_or_meta: e.ctrl_key() || e.meta_key(),
            },
            self.renderer.input_settings(),
            &|sy| self.renderer.pane_separator_at(sy),
        );
        match intent {
            // Each notch is an independent micro-drag: grab, rescale, release.
            WheelIntent::PriceScale { x, y, drag_px } => {
                self.renderer.begin_price_scale(x, y);
                self.renderer.price_scale_by(drag_px);
            }
            WheelIntent::Pan(vp) => self.renderer.apply_viewport(vp),
            WheelIntent::Zoom {
                target_bar_spacing,
                anchor,
            } => self
                .renderer
                .zoom_to(target_bar_spacing, anchor.logical, anchor.x),
        }
    }

    // ── keyboard ──

    /// A modifier press or release with the pointer stationary: re-issue the last
    /// cursor position so the placing ghost or drag preview reflects the new state at
    /// once (Shift's angle lock, Ctrl/Cmd's momentary strong magnet).
    fn on_modifier(&self, ev: &JsValue) {
        let key = get_str(ev, "key");
        if get_bool(ev, "repeat") || !matches!(key.as_str(), "Shift" | "Control" | "Meta") {
            return;
        }
        let (cursor, dragging, region) = {
            let p = self.press.borrow();
            (p.cursor, p.dragging, p.region)
        };
        let Some((x, y)) = cursor else { return };
        let live = if dragging {
            region == DragRegion::Drawing
        } else {
            self.renderer.drawings_interactive()
        };
        if !live {
            return; // nothing of the drawings' to re-shape
        }
        let ctrl_or_meta = get_bool(ev, "ctrlKey") || get_bool(ev, "metaKey");
        if let Some(ctl) = self.drawings() {
            ctl.pointer_move(
                x,
                y,
                self.snap_of(ctrl_or_meta),
                get_bool(ev, "shiftKey"),
                ctrl_or_meta,
            );
        }
    }

    fn on_key(&self, e: &KeyboardEvent) {
        if !self.host.keyboard_enabled() {
            return;
        }
        let key = e.key();
        // The drawings layer gets the key first (Escape, undo / redo, copy / paste,
        // delete, nudge); it stands down while a text field is focused.
        if let Some(ctl) = self.drawings() {
            let event = DrawingKeyEvent {
                key: key.clone(),
                ctrl_key: e.ctrl_key(),
                meta_key: e.meta_key(),
                shift_key: e.shift_key(),
            };
            if ctl.handle_key(&event, key_target_is_text(e)) {
                e.prevent_default();
                return;
            }
        }
        let input = KeyInput {
            key: &key,
            shift: e.shift_key(),
            alt: e.alt_key(),
            ctrl: e.ctrl_key(),
            meta: e.meta_key(),
        };
        if self.renderer.handle_key(&input) {
            e.prevent_default();
        }
    }
}

/// The event listeners of one chart. Dropping it removes every listener.
pub struct InputWiring {
    shared: Rc<Shared>,
    /// The listeners on the data canvas, replaced when the canvas is.
    _canvas_listeners: Rc<RefCell<Vec<Listener>>>,
    /// The host's data-canvas subscription (it holds this weakly).
    _follow: Rc<DataWatcher>,
    _listeners: Vec<Listener>,
}

/// The listeners that sit on the data canvas `el` (and the styles that hand it the touch
/// stream). Built again for the new canvas when the host replaces the data canvas.
fn bind_canvas(shared: &Rc<Shared>, el: &HtmlCanvasElement) -> Vec<Listener> {
    // Own the touch stream: without this the browser turns a drag into page
    // scroll and a pinch into page zoom before the chart sees coherent events.
    crate::dom::set_styles(
        el,
        &[
            ("touch-action", "none"),
            ("user-select", "none"),
            ("-webkit-user-select", "none"),
            ("-webkit-touch-callout", "none"),
        ],
    );
    let target: &EventTarget = el.as_ref();
    let mut listeners = Vec::new();

    macro_rules! on {
        ($name:literal, $capture:expr, $passive:expr, $ev:ty, $method:ident) => {{
            let s = shared.clone();
            listeners.push(Listener::new(
                target,
                $name,
                $capture,
                $passive,
                move |ev: JsValue| s.$method(ev.unchecked_ref::<$ev>()),
            ));
        }};
    }
    on!("pointerdown", false, true, PointerEvent, on_down);
    on!("pointermove", false, true, PointerEvent, on_move);
    on!("pointerup", false, true, PointerEvent, on_up);
    on!("pointercancel", false, true, PointerEvent, on_cancel);
    on!("wheel", false, false, WheelEvent, on_wheel);
    on!("keydown", false, false, KeyboardEvent, on_key);
    {
        let s = shared.clone();
        listeners.push(Listener::new(
            target,
            "pointerleave",
            false,
            true,
            move |_| {
                s.press.borrow_mut().cursor = None;
                s.renderer.pointer_leave();
                if let Some(ctl) = s.drawings() {
                    ctl.clear_hover_now(); // the pointer no longer reaches the drawings
                }
            },
        ));
    }
    // Companions of a middle or right press that the drawings layer took: they fire
    // AFTER pointerdown, so they can veto autoscroll / paste and the context menu.
    for name in ["mousedown", "auxclick"] {
        let s = shared.clone();
        listeners.push(Listener::new(
            target,
            name,
            false,
            false,
            move |ev: JsValue| {
                if ev.unchecked_ref::<MouseEvent>().button() == 1 && s.press.borrow().middle_deleted
                {
                    ev.unchecked_ref::<web_sys::Event>().prevent_default();
                }
            },
        ));
    }
    {
        let s = shared.clone();
        listeners.push(Listener::new(
            target,
            "contextmenu",
            false,
            false,
            move |ev: JsValue| {
                let cancelled = std::mem::take(&mut s.press.borrow_mut().right_cancelled);
                if cancelled {
                    let event: &web_sys::Event = ev.unchecked_ref();
                    event.prevent_default();
                    event.stop_propagation();
                }
            },
        ));
    }
    {
        let s = shared.clone();
        listeners.push(Listener::new(
            target,
            "dblclick",
            false,
            true,
            move |ev: JsValue| {
                let (x, y) = s.local_of_mouse(ev.unchecked_ref::<MouseEvent>());
                s.double_activate(x, y);
            },
        ));
    }
    listeners
}

impl Shared {
    /// The host replaced its `Data` canvas with `el`: move the listeners over. DOM
    /// plumbing only (this runs from inside a host call, with the renderer borrowed).
    fn follow(self: &Rc<Self>, el: &HtmlCanvasElement, slot: &RefCell<Vec<Listener>>) {
        self.cancel_long_press();
        // A gesture on the old canvas ends with it.
        *self.press.borrow_mut() = Press::new();
        *self.el.borrow_mut() = el.clone();
        let fresh = bind_canvas(self, el);
        // The old listeners drop here, after the new ones are in place.
        *slot.borrow_mut() = fresh;
    }
}

impl InputWiring {
    /// Wire the data canvas of `host` (the renderer must be mounted). `None`
    /// when there is no data canvas. The listeners follow the host's data canvas if it
    /// is replaced later (the WebGL2 backend giving up for canvas2d).
    pub fn attach(
        renderer: Rc<NativeRenderer>,
        host: Rc<BrowserHost>,
        platform: Rc<dyn Platform>,
    ) -> Option<Self> {
        let el = host.data_canvas()?;
        let shared = Rc::new(Shared {
            renderer,
            host: host.clone(),
            el: RefCell::new(el.clone()),
            platform,
            press: RefCell::new(Press::new()),
        });
        let canvas_listeners = Rc::new(RefCell::new(bind_canvas(&shared, &el)));
        let follow: Rc<DataWatcher> = {
            let weak = Rc::downgrade(&shared);
            let slot = canvas_listeners.clone();
            Rc::new(move |el: &HtmlCanvasElement| {
                if let Some(shared) = weak.upgrade() {
                    shared.follow(el, &slot);
                }
            })
        };
        host.watch_data_canvas(Rc::downgrade(&follow));
        let mut listeners = Vec::new();
        // A touch long-press must never open a context menu over the chart (the hold
        // means crosshair): swallow the synthesized event at the window capture phase
        // while a touch gesture is live. Modifier keys re-shape the drawings preview the
        // moment they change, with the pointer resting (window level: no chart focus
        // needed).
        if let Some(view) = default_view(&host.document().clone()) {
            for name in ["keydown", "keyup"] {
                let s = shared.clone();
                listeners.push(Listener::new(
                    &view,
                    name,
                    false,
                    true,
                    move |ev: JsValue| s.on_modifier(&ev),
                ));
            }
            let s = shared.clone();
            listeners.push(Listener::new(
                &view,
                "contextmenu",
                true,
                false,
                move |ev: JsValue| {
                    if s.press.borrow().touches.is_empty() {
                        return;
                    }
                    let event: &web_sys::Event = ev.unchecked_ref();
                    let inside = event
                        .target()
                        .and_then(|t| t.dyn_into::<web_sys::Node>().ok())
                        .is_some_and(|n| s.el().contains(Some(&n)));
                    if inside {
                        event.prevent_default();
                        event.stop_immediate_propagation();
                    }
                },
            ));
        }
        Some(Self {
            shared,
            _canvas_listeners: canvas_listeners,
            _follow: follow,
            _listeners: listeners,
        })
    }
}

impl Drop for InputWiring {
    fn drop(&mut self) {
        self.shared.cancel_long_press();
        remove_style(&self.shared.el(), "cursor");
    }
}

// ── size and dpr observers ──

/// The `ResizeObserver` (with `devicePixelContentBoxSize` where the browser has
/// it) and the `devicePixelRatio` media-query watcher: both end in
/// `renderer.resize()`.
pub struct SizeWatch {
    observer: JsValue,
    stop_dpr: Function,
    _on_size: Closure<dyn FnMut(bool, f64, f64)>,
    _on_dpr: Closure<dyn FnMut()>,
}

impl SizeWatch {
    pub fn start(renderer: Rc<NativeRenderer>, host: Rc<BrowserHost>) -> Self {
        let on_size = {
            let renderer = renderer.clone();
            let host = host.clone();
            Closure::<dyn FnMut(bool, f64, f64)>::new(move |has: bool, w: f64, h: f64| {
                // A report describes the CURRENT box only; the renderer rejects a stale one.
                if has {
                    host.set_device_size(Some((w, h)));
                }
                renderer.resize();
            })
        };
        let wrapper: &Element = host.wrapper();
        let plot: &Element = host.plot();
        let observer = vela_observe_size(wrapper, plot, on_size.as_ref().unchecked_ref());
        // A browser zoom or a move to a monitor with another scale changes the
        // pixel ratio without a CSS resize, which the observer misses.
        let on_dpr = Closure::<dyn FnMut()>::new(move || renderer.resize());
        let stop_dpr = vela_watch_dpr(on_dpr.as_ref().unchecked_ref());
        Self {
            observer,
            stop_dpr,
            _on_size: on_size,
            _on_dpr: on_dpr,
        }
    }
}

impl Drop for SizeWatch {
    fn drop(&mut self) {
        vela_disconnect(&self.observer);
        let _ = self.stop_dpr.call0(&JsValue::NULL);
    }
}
