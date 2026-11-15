//! The user-drawing interaction state machine, ported from the original
//! `DrawingInteraction.ts`: arm, place (clicks anchor, live ghost), press a drawing, then
//! release without moving (a click, which opens the settings) or drag (the whole body or one
//! handle) to move or resize.
//!
//! Modifiers at the press: Shift or Ctrl/Cmd over a drawing make the click a selection toggle
//! (multi-select); Ctrl/Cmd plus a body drag DUPLICATES (the copies follow the cursor and are
//! committed on release as one `clone` intent, so the drag is one undo step and cancelling it
//! leaves nothing behind); Ctrl/Cmd-drag on the empty plot sweeps a marquee that adds the
//! drawings it touches to the selection; a body drag of a drawing in a multi-selection moves
//! (or copies) the whole selection. `locked` drawings open settings but never drag.
//!
//! # Ownership
//!
//! The original holds a deps object with closures over the controller. Here every method
//! takes the host as `&mut dyn InteractionDeps` for the duration of the call, so the machine
//! owns only its own state and can be tested without a canvas or a renderer. The live
//! drawings belong to the caller (the controller's projection of the core's snapshot): a drag
//! translates them IN PLACE through [`InteractionDeps::drawing_mut`], and the release hands
//! the final shape back as ONE `edit` / `edit-many` / `clone` intent. The core store is never
//! touched by a drag, so its history gets one entry per gesture.

use std::collections::HashSet;

use crate::drawings::{
    Bounds, Drawing, DrawingInit, DrawingPoint, DrawingRegistry, DrawingToolDefaults,
    PlacementMode, Projector, SnapMode, apply_tool_defaults,
};
use crate::indicators::classics::math::js_round;
use crate::ports::chart_renderer::DrawingIntent;

use super::hit_tester::{HIT_TOLERANCE, top_drawing_at};
use super::interaction_logic::{
    ANGLE_SNAP_STEP, exceeds_drag_slop, freehand_should_sample, is_angle_snap_type, marquee_rect,
    marquee_selection, marquee_selects, placement_anchor, snap_moved, thin_freehand_anchors,
};

/// What the interaction needs from its host (the original `InteractionDeps`).
pub trait InteractionDeps {
    fn projector(&self) -> &dyn Projector;
    /// The currently armed tool, or `None` in select / idle mode.
    fn active_tool(&self) -> Option<String>;
    /// The drawings currently shown, ascending paint order.
    fn drawings(&self) -> Vec<&dyn Drawing>;
    /// A shown drawing by id, for in-place mutation by a drag.
    fn drawing_mut(&mut self, id: &str) -> Option<&mut dyn Drawing>;
    /// The drawing under the cursor (host-owned): its handles are showing.
    fn hovered_id(&self) -> Option<String>;
    /// The selected drawings (host-owned), in selection order: their handles are showing
    /// and grabbable off-body.
    fn selected_ids(&self) -> Vec<String>;
    /// Emit a renderer-to-core intent.
    fn emit(&mut self, intent: DrawingIntent);
    /// Request a drawings-layer repaint (ghost, drag or state changed).
    fn changed(&mut self);
    /// A click (press and release, no drag) landed on a drawing: open its settings popup.
    fn open_settings(&mut self, id: &str, x: f64, y: f64);
    /// Snap a data point to the nearest candle, per the magnet mode and the cursor pixel.
    fn snap(
        &self,
        point: DrawingPoint,
        pane_id: &str,
        mode: SnapMode,
        cursor_px: Option<(f64, f64)>,
    ) -> DrawingPoint;
    /// The armed tool's remembered settings: they seed the placement draft so its ghost
    /// previews them (color, width, fib levels, ...), not the type default.
    fn tool_defaults(&self) -> Option<DrawingToolDefaults>;
    /// The catalog drafts and drag copies are built from.
    fn registry(&self) -> &DrawingRegistry;
}

/// A drawing riding along with a body drag, with its anchors as they were at the press.
#[derive(Clone, Debug)]
struct Rider {
    id: String,
    orig: Vec<DrawingPoint>,
}

/// A drawing a body drag moves (a source or its transient copy), with the anchors to
/// translate from.
struct DragTarget {
    drawing: Box<dyn Drawing>,
    orig: Vec<DrawingPoint>,
}

enum State {
    Idle,
    Placing {
        draft: Box<dyn Drawing>,
        need: usize,
        cursor: Option<DrawingPoint>,
    },
    Pressed {
        id: String,
        /// `None` = the body, `Some(i)` = a handle.
        handle: Option<usize>,
        orig: Vec<DrawingPoint>,
        px: f64,
        py: f64,
        moved: bool,
        /// The OTHER selected drawings a body drag carries along (a multi-selection moves as one).
        riders: Vec<Rider>,
        /// Ctrl/Cmd was held at the press: a click toggles the selection, a body drag moves copies.
        modifier: bool,
        /// The transient copies a Ctrl-drag moves (built at drag start); the sources stay put.
        clones: Option<Vec<DragTarget>>,
    },
    /// Ctrl/Cmd-drag on the empty plot: a rubber-band box from the press to the cursor.
    Marquee {
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
    },
}

/// The magnet target the ring marks: the snapped point and its pane.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapMarker {
    pub point: DrawingPoint,
    pub pane_id: String,
}

/// The single user-drawing interaction state machine. See the module documentation.
pub struct DrawingInteraction {
    state: State,
    snap_at: Option<SnapMarker>,
}

impl Default for DrawingInteraction {
    fn default() -> Self {
        Self::new()
    }
}

fn clone_anchors(d: &dyn Drawing) -> Vec<DrawingPoint> {
    d.base().anchors.clone()
}

impl DrawingInteraction {
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            snap_at: None,
        }
    }

    /// The current magnet target (snapped point and pane) to mark with a ring, or `None`.
    pub fn snap_marker(&self) -> Option<&SnapMarker> {
        self.snap_at.as_ref()
    }

    /// Control-circle positions for the anchors placed so far while PLACING, so the user
    /// always sees where each clicked point landed (a pitchfork's pivot), even before the
    /// shape has enough anchors to render a ghost. `None` when not placing.
    pub fn placing_markers(&self, proj: &dyn Projector) -> Option<Vec<(f64, f64)>> {
        let State::Placing { draft, .. } = &self.state else {
            return None;
        };
        if draft.placement_mode() == PlacementMode::Freehand {
            return None; // a captured stroke shows no per-point circles
        }
        let base = draft.base();
        let points: Vec<(f64, f64)> = base
            .anchors
            .iter()
            .filter_map(|a| {
                proj.y_of(a.price, &base.pane_id)
                    .map(|y| (proj.x_of(a.time), y))
            })
            .collect();
        (!points.is_empty()).then_some(points)
    }

    /// Resolve a pixel to a data point, snapping to a candle per the magnet `mode`. The
    /// snap ring marker is set only when a snap actually happened (so `Weak` outside its
    /// radius shows nothing).
    fn resolve(
        &mut self,
        deps: &dyn InteractionDeps,
        x: f64,
        y: f64,
        pane_id: &str,
        mode: SnapMode,
    ) -> DrawingPoint {
        let raw = deps.projector().px_to_point(x, y, pane_id);
        if mode == SnapMode::Off {
            self.snap_at = None;
            return raw;
        }
        let snapped = deps.snap(raw, pane_id, mode, Some((x, y)));
        self.snap_at = snap_moved(raw, snapped).then(|| SnapMarker {
            point: snapped,
            pane_id: pane_id.to_string(),
        });
        snapped
    }

    /// Resolve a cursor pixel through the magnet and return the snapped pixel: the same
    /// conversion drawing placement uses. Updates the snap-ring marker. The measure ruler
    /// goes through this so its endpoints follow weak / strong / Ctrl magnet too.
    pub fn snap_cursor(
        &mut self,
        deps: &dyn InteractionDeps,
        x: f64,
        y: f64,
        mode: SnapMode,
    ) -> (f64, f64) {
        let proj = deps.projector();
        let pane_id = proj.pane_id_at_y(y).unwrap_or_else(|| "price".to_string());
        let point = self.resolve(deps, x, y, &pane_id, mode);
        let sy = proj.y_of(point.price, &pane_id);
        (proj.x_of(point.time), sy.unwrap_or(y))
    }

    /// Drop the snap-ring marker (a transient mode ended without going through `up`).
    pub fn clear_snap_marker(&mut self) {
        self.snap_at = None;
    }

    /// Resolve a pixel to a data point with the segment angle locked to 45 degree steps
    /// around `pivot` (Shift held on a line tool). It works in PIXEL space: the user reasons
    /// about the angle they see, not about time and price units. The magnet is bypassed:
    /// snapping the result to a candle afterwards would break the exact angle just enforced.
    fn resolve_angle_snapped(
        &mut self,
        deps: &dyn InteractionDeps,
        pivot: DrawingPoint,
        pane_id: &str,
        x: f64,
        y: f64,
    ) -> DrawingPoint {
        let proj = deps.projector();
        let px = proj.x_of(pivot.time);
        let py = proj.y_of(pivot.price, pane_id);
        self.snap_at = None;
        let Some(py) = py else {
            return proj.px_to_point(x, y, pane_id);
        };
        let r = (x - px).hypot(y - py);
        if r < 1.0 {
            return proj.px_to_point(x, y, pane_id); // too close to define an angle
        }
        let angle = js_round((y - py).atan2(x - px) / ANGLE_SNAP_STEP) * ANGLE_SNAP_STEP;
        proj.px_to_point(px + r * angle.cos(), py + r * angle.sin(), pane_id)
    }

    /// The pivot the free endpoint angle-snaps around while placing `draft`, or `None` when
    /// the tool is not a two-point segment (or no anchor is committed yet).
    fn placing_pivot(draft: &dyn Drawing) -> Option<DrawingPoint> {
        if !is_angle_snap_type(draft.type_key()) {
            return None;
        }
        draft.base().anchors.last().copied()
    }

    /// Should the drawing layer win this press (vs pan)? Armed, placing, pressed, or over a
    /// drawing or handle.
    pub fn claim(&self, deps: &dyn InteractionDeps, x: f64, y: f64) -> bool {
        if deps.active_tool().is_some() || !matches!(self.state, State::Idle) {
            return true;
        }
        self.hit_id_at(deps, x, y).is_some()
    }

    /// A cursor hint for hovering (pointer over a drawing or handle), or `None` off any
    /// drawing.
    pub fn cursor_at(&self, deps: &dyn InteractionDeps, x: f64, y: f64) -> Option<&'static str> {
        if deps.active_tool().is_some() {
            return None; // armed: keep the crosshair
        }
        self.hit_id_at(deps, x, y).map(|_| "pointer")
    }

    pub fn is_placing(&self) -> bool {
        matches!(self.state, State::Placing { .. })
    }

    pub fn is_dragging(&self) -> bool {
        matches!(self.state, State::Pressed { moved: true, .. })
    }

    /// Whether anything is in flight (placing, pressed or sweeping).
    pub fn is_idle(&self) -> bool {
        matches!(self.state, State::Idle)
    }

    /// The drawing being actively dragged (so its handles stay shown mid-drag), or `None`.
    pub fn active_drag_id(&self) -> Option<&str> {
        match &self.state {
            State::Pressed {
                id, moved: true, ..
            } => Some(id),
            _ => None,
        }
    }

    /// The store drawings a live drag is moving (the pressed one and its riders), or none
    /// before the drag passes the slop. A Ctrl-drag moves copies, so its sources stay put.
    pub fn moving_ids(&self) -> HashSet<String> {
        match &self.state {
            State::Pressed {
                id,
                riders,
                moved: true,
                clones: None,
                ..
            } => std::iter::once(id.clone())
                .chain(riders.iter().map(|r| r.id.clone()))
                .collect(),
            _ => HashSet::new(),
        }
    }

    /// The drawing under a press that has not been released yet (drag or click undecided).
    pub fn pressed_id(&self) -> Option<&str> {
        match &self.state {
            State::Pressed { id, .. } => Some(id),
            _ => None,
        }
    }

    /// The copies a Ctrl-drag is moving (transient, not yet in the store), or `None`.
    pub fn drag_clones(&self) -> Option<Vec<&dyn Drawing>> {
        match &self.state {
            State::Pressed {
                moved: true,
                clones: Some(clones),
                ..
            } => Some(clones.iter().map(|c| c.drawing.as_ref()).collect()),
            _ => None,
        }
    }

    /// The marquee box being swept, normalized (or `None` when none is in progress).
    pub fn marquee_rect(&self) -> Option<Bounds> {
        match self.state {
            State::Marquee { x0, y0, x1, y1 } => Some(marquee_rect(x0, y0, x1, y1)),
            _ => None,
        }
    }

    /// Ctrl/Cmd-press on the empty plot: start sweeping a selection box. `false` when a tool
    /// is armed or a gesture is in flight (that path owns the press).
    pub fn begin_marquee(&mut self, deps: &dyn InteractionDeps, x: f64, y: f64) -> bool {
        if !matches!(self.state, State::Idle) || deps.active_tool().is_some() {
            return false;
        }
        self.state = State::Marquee {
            x0: x,
            y0: y,
            x1: x,
            y1: y,
        };
        true
    }

    /// A press: place the next anchor, or grab a drawing or handle (resolved as click or drag
    /// on release). `shift` over a drawing toggles it in or out of the selection
    /// (multi-select) instead of dragging; while placing a line tool it locks the segment
    /// angle to 45 degree steps. `modifier` (Ctrl/Cmd) over a drawing also toggles on a
    /// click, but a body drag then moves a COPY.
    pub fn down(
        &mut self,
        deps: &mut dyn InteractionDeps,
        x: f64,
        y: f64,
        mode: SnapMode,
        shift: bool,
        modifier: bool,
    ) {
        if self.is_placing() {
            self.down_placing(deps, x, y, mode, shift);
            return;
        }
        if let Some(tool) = deps.active_tool() {
            let paneid = deps
                .projector()
                .pane_id_at_y(y)
                .unwrap_or_else(|| "price".to_string());
            let anchor = self.resolve(deps, x, y, &paneid, mode);
            let init = DrawingInit {
                pane_id: paneid,
                anchors: vec![anchor],
                ..DrawingInit::default()
            };
            let Some(mut draft) = deps.registry().create(&tool, init) else {
                return;
            };
            apply_tool_defaults(draft.as_mut(), deps.tool_defaults().as_ref());
            let need = draft.anchor_schema().max;
            let ready = draft.placement_mode() == PlacementMode::Click
                && draft.base().anchors.len() >= need;
            self.state = State::Placing {
                draft,
                need,
                cursor: None,
            };
            // Fixed tools (max == min) finalize as soon as the count is met; variable and
            // freehand tools wait for a gesture.
            if ready {
                self.finalize(deps);
            } else {
                deps.changed();
            }
            return;
        }
        // Press mode: a press grabs the drawing or handle under the cursor; release without
        // moving is a click (settings), release after moving is a drag.
        let Some(hit_id) = self.hit_id_at(deps, x, y) else {
            return; // empty space: the popup self-dismisses and hover already cleared handles
        };
        if shift {
            deps.emit(DrawingIntent::Select {
                ids: vec![hit_id],
                additive: true,
            }); // shift-click toggles selection
            return;
        }
        let selected = deps.selected_ids();
        let (handle, orig, riders) = {
            let shown = deps.drawings();
            let Some(hit) = shown.iter().find(|d| d.base().id == hit_id) else {
                return;
            };
            let handle = hit.hit_handle(x, y, deps.projector(), HIT_TOLERANCE);
            // A body drag of a SELECTED drawing carries the rest of the selection along; a
            // handle drag reshapes just the one, and an unselected drawing drags alone.
            let mut riders = Vec::new();
            if handle.is_none() && selected.contains(&hit_id) {
                for id in &selected {
                    if *id == hit_id {
                        continue;
                    }
                    if let Some(d) = shown.iter().find(|d| d.base().id == *id)
                        && !d.base().locked
                        && d.base().visible
                    {
                        riders.push(Rider {
                            id: id.clone(),
                            orig: clone_anchors(*d),
                        });
                    }
                }
            }
            (handle, clone_anchors(*hit), riders)
        };
        self.state = State::Pressed {
            id: hit_id,
            handle,
            orig,
            px: x,
            py: y,
            moved: false,
            riders,
            modifier,
            clones: None,
        };
    }

    fn down_placing(
        &mut self,
        deps: &mut dyn InteractionDeps,
        x: f64,
        y: f64,
        mode: SnapMode,
        shift: bool,
    ) {
        let (pane_id, pivot) = {
            let State::Placing { draft, .. } = &self.state else {
                return;
            };
            if draft.placement_mode() != PlacementMode::Click {
                return; // drag / freehand finalize on release, not extra clicks
            }
            let pivot = if shift {
                Self::placing_pivot(draft.as_ref())
            } else {
                None
            };
            (draft.base().pane_id.clone(), pivot)
        };
        let point = match pivot {
            Some(p) => self.resolve_angle_snapped(deps, p, &pane_id, x, y),
            None => self.resolve(deps, x, y, &pane_id, mode),
        };
        let full = {
            let State::Placing { draft, need, .. } = &mut self.state else {
                return;
            };
            let anchor = placement_anchor(draft.type_key(), &draft.base().anchors, point);
            draft.base_mut().anchors.push(anchor);
            draft.base().anchors.len() >= *need
        };
        if full {
            self.finalize(deps);
        } else {
            deps.changed();
        }
    }

    /// Move the pressed drawing's body (and its riders), or on a Ctrl-drag their copies, to
    /// the cursor. Each drawing resolves the delta on its own pane, so drawings riding along
    /// on another pane move by the same PIXELS, not the same price. The copies are built on
    /// the first move (so a Ctrl-click never creates anything), from sources that have not
    /// moved yet, so their own anchors are the origin to translate from.
    fn drag_body(&mut self, deps: &mut dyn InteractionDeps, x: f64, y: f64) {
        self.snap_at = None; // whole-body moves stay smooth (no snap)
        let (sources, press, modifier, has_clones) = match &self.state {
            State::Pressed {
                id,
                orig,
                riders,
                modifier,
                clones,
                px,
                py,
                ..
            } => {
                let mut sources = vec![(id.clone(), orig.clone())];
                sources.extend(riders.iter().map(|r| (r.id.clone(), r.orig.clone())));
                (sources, (*px, *py), *modifier, clones.is_some())
            }
            _ => return,
        };
        if modifier && !has_clones {
            let mut built = Vec::new();
            for (sid, _) in &sources {
                let doc = deps
                    .drawings()
                    .into_iter()
                    .find(|d| d.base().id == *sid)
                    .map(|d| d.serialize());
                if let Some(copy) = doc.and_then(|doc| deps.registry().deserialize(&doc)) {
                    let orig = clone_anchors(copy.as_ref());
                    built.push(DragTarget {
                        drawing: copy,
                        orig,
                    });
                }
            }
            if let State::Pressed { clones, .. } = &mut self.state {
                *clones = Some(built);
            }
        }
        let proj = deps.projector();
        let delta = |pane_id: &str| {
            let from = proj.px_to_point(press.0, press.1, pane_id);
            let to = proj.px_to_point(x, y, pane_id);
            (to.time - from.time, to.price - from.price)
        };
        if let State::Pressed {
            clones: Some(clones),
            ..
        } = &mut self.state
        {
            for t in clones.iter_mut() {
                let (dt, dp) = delta(&t.drawing.base().pane_id);
                let next = t.drawing.translate_body(dt, dp, &t.orig); // a callout pins its tip
                t.drawing.base_mut().anchors = next;
            }
            return;
        }
        let mut moved: Vec<(String, Vec<DrawingPoint>)> = Vec::new();
        for (sid, orig) in &sources {
            if let Some(d) = deps.drawings().into_iter().find(|d| d.base().id == *sid) {
                let (dt, dp) = delta(&d.base().pane_id);
                moved.push((sid.clone(), d.translate_body(dt, dp, orig)));
            }
        }
        for (sid, anchors) in moved {
            if let Some(d) = deps.drawing_mut(&sid) {
                d.base_mut().anchors = anchors;
            }
        }
    }

    /// Pointer move: advance the placing ghost, or live-preview a drag once past the slop.
    /// `shift` on a line tool locks the segment angle to 45 degree steps (placing and handle
    /// drags).
    pub fn move_to(
        &mut self,
        deps: &mut dyn InteractionDeps,
        x: f64,
        y: f64,
        mode: SnapMode,
        shift: bool,
    ) {
        match &self.state {
            State::Idle => {
                // Armed but not yet placing: preview where the FIRST anchor will magnet-snap.
                let tool = deps.active_tool();
                let had = self.snap_at.is_some();
                if tool.is_some() && mode != SnapMode::Off {
                    let pane = deps
                        .projector()
                        .pane_id_at_y(y)
                        .unwrap_or_else(|| "price".to_string());
                    self.resolve(deps, x, y, &pane, mode);
                } else {
                    self.snap_at = None;
                }
                if self.snap_at.is_some() || had {
                    deps.changed();
                }
            }
            State::Placing { draft, .. } => {
                if draft.placement_mode() == PlacementMode::Freehand {
                    self.capture_freehand(deps, x, y);
                    return;
                }
                let pane_id = draft.base().pane_id.clone();
                let pivot = if shift {
                    Self::placing_pivot(draft.as_ref())
                } else {
                    None
                };
                let point = match pivot {
                    Some(p) => self.resolve_angle_snapped(deps, p, &pane_id, x, y),
                    None => self.resolve(deps, x, y, &pane_id, mode),
                };
                if let State::Placing { cursor, .. } = &mut self.state {
                    *cursor = Some(point);
                }
                deps.changed();
            }
            State::Marquee { .. } => {
                if let State::Marquee { x1, y1, .. } = &mut self.state {
                    *x1 = x;
                    *y1 = y;
                }
                deps.changed();
            }
            State::Pressed { .. } => self.move_pressed(deps, x, y, mode, shift),
        }
    }

    fn move_pressed(
        &mut self,
        deps: &mut dyn InteractionDeps,
        x: f64,
        y: f64,
        mode: SnapMode,
        shift: bool,
    ) {
        let State::Pressed {
            id,
            handle,
            px,
            py,
            moved,
            ..
        } = &self.state
        else {
            return;
        };
        let (id, handle, px, py, was_moved) = (id.clone(), *handle, *px, *py, *moved);
        let (locked, type_key, pane_id, anchors) = {
            let Some(d) = deps.drawings().into_iter().find(|d| d.base().id == id) else {
                return;
            };
            (
                d.base().locked,
                d.type_key(),
                d.base().pane_id.clone(),
                d.base().anchors.clone(),
            )
        };
        if locked {
            return; // locked: never drags (stays a click)
        }
        if !was_moved && !exceeds_drag_slop(x, y, px, py) {
            return;
        }
        if let State::Pressed { moved, .. } = &mut self.state {
            *moved = true;
        }
        match handle {
            None => self.drag_body(deps, x, y),
            Some(h) => {
                // Shift on a two-point line re-locks the dragged endpoint's angle around the
                // other one.
                let pivot = (shift && is_angle_snap_type(type_key) && anchors.len() == 2)
                    .then(|| anchors.get(1 - h.min(1)).copied())
                    .flatten();
                let point = match pivot {
                    Some(p) => self.resolve_angle_snapped(deps, p, &pane_id, x, y),
                    None => self.resolve(deps, x, y, &pane_id, mode), // a handle snaps to the candle
                };
                if let Some(d) = deps.drawing_mut(&id) {
                    d.drag_handle(h, point);
                    d.constrain_handle_drag(h); // a position flips its reward/stop to stay opposed
                }
            }
        }
        deps.changed();
    }

    /// Pointer release: finish a freehand stroke, end a polyline click (no-op), commit a
    /// drag, or treat a no-move press as a click that opens the settings.
    pub fn up(&mut self, deps: &mut dyn InteractionDeps, x: f64, y: f64) {
        self.snap_at = None;
        match &self.state {
            State::Placing { draft, .. } => {
                let mode = draft.placement_mode();
                let min = draft.anchor_schema().min;
                let have = draft.base().anchors.len();
                self.up_placing(deps, mode, min, have);
            }
            State::Marquee { .. } => {
                let rect = self.marquee_rect();
                self.state = State::Idle;
                if let Some(rect) = rect
                    && marquee_selects(&rect)
                {
                    self.select_within(deps, &rect);
                }
                deps.changed();
            }
            State::Pressed { .. } => self.up_pressed(deps, x, y),
            State::Idle => {}
        }
    }

    fn up_placing(
        &mut self,
        deps: &mut dyn InteractionDeps,
        mode: PlacementMode,
        min: usize,
        have: usize,
    ) {
        match mode {
            PlacementMode::Drag => {
                // press-drag-release: the release point is the second anchor (a box's far
                // corner)
                let mut have = have;
                if let State::Placing { draft, cursor, .. } = &mut self.state
                    && let Some(c) = *cursor
                {
                    draft.base_mut().anchors.push(c);
                    have += 1;
                }
                if have >= min {
                    self.finalize(deps);
                } else {
                    self.discard_placing(deps); // a bare click with no drag
                }
            }
            PlacementMode::Freehand => {
                if have >= min {
                    self.finalize(deps);
                } else {
                    self.discard_placing(deps);
                }
            }
            // click placement: releasing a click adds nothing (the press already placed the
            // anchor)
            PlacementMode::Click => {}
        }
    }

    fn up_pressed(&mut self, deps: &mut dyn InteractionDeps, x: f64, y: f64) {
        let State::Pressed {
            id,
            moved,
            riders,
            modifier,
            clones,
            ..
        } = std::mem::replace(&mut self.state, State::Idle)
        else {
            return;
        };
        if !moved {
            // A click: Ctrl/Cmd toggles the drawing in or out of the selection; plain opens
            // its settings.
            if modifier {
                deps.emit(DrawingIntent::Select {
                    ids: vec![id],
                    additive: true,
                });
            } else {
                deps.open_settings(&id, x, y);
            }
            return;
        }
        if let Some(clones) = clones {
            let docs = clones.iter().map(|c| c.drawing.serialize()).collect();
            deps.emit(DrawingIntent::Clone { docs });
        } else {
            let find = |deps: &dyn InteractionDeps, id: &str| {
                deps.drawings()
                    .into_iter()
                    .find(|d| d.base().id == id)
                    .map(|d| d.serialize())
            };
            if let Some(doc) = find(deps, &id) {
                let mut docs = vec![doc];
                for r in &riders {
                    if let Some(rd) = find(deps, &r.id) {
                        docs.push(rd);
                    }
                }
                if docs.len() > 1 {
                    deps.emit(DrawingIntent::EditMany { docs });
                } else if let Some(doc) = docs.pop() {
                    deps.emit(DrawingIntent::Edit { doc });
                }
            }
        }
        deps.changed();
    }

    /// Add every visible drawing whose on-screen bounds touch `rect` to the selection (a
    /// union, so successive sweeps accumulate). No change when the box touches nothing new.
    fn select_within(&mut self, deps: &mut dyn InteractionDeps, rect: &Bounds) {
        let selected = deps.selected_ids();
        let ids = {
            let shown = deps.drawings();
            marquee_selection(&selected, &shown, deps.projector(), rect)
        };
        if let Some(ids) = ids {
            deps.emit(DrawingIntent::Select {
                ids,
                additive: false,
            });
        }
    }

    /// Cancel an in-progress placement or drag (Escape). Returns whether anything was
    /// cancelled.
    pub fn cancel(&mut self, deps: &mut dyn InteractionDeps) -> bool {
        self.snap_at = None;
        match &self.state {
            State::Placing { draft, .. } => {
                let type_key = draft.type_key().to_string();
                self.state = State::Idle;
                deps.emit(DrawingIntent::ToolFinished { type_key });
                deps.changed();
                true
            }
            State::Pressed { .. } => {
                // A Ctrl-drag moved copies only: dropping them is the whole rollback.
                if let State::Pressed {
                    id,
                    orig,
                    riders,
                    moved: true,
                    clones: None,
                    ..
                } = &self.state
                {
                    let mut restore: Vec<(String, Vec<DrawingPoint>)> =
                        vec![(id.clone(), orig.clone())];
                    restore.extend(riders.iter().map(|r| (r.id.clone(), r.orig.clone())));
                    for (rid, anchors) in restore {
                        if let Some(d) = deps.drawing_mut(&rid) {
                            d.base_mut().anchors = anchors;
                        }
                    }
                }
                self.state = State::Idle;
                deps.changed();
                true
            }
            State::Marquee { .. } => {
                self.state = State::Idle;
                deps.changed();
                true
            }
            State::Idle => false,
        }
    }

    /// Disarm cleanup when the active tool is cleared externally.
    pub fn on_tool_cleared(&mut self, deps: &mut dyn InteractionDeps) {
        if self.is_placing() {
            self.state = State::Idle;
            deps.changed();
        }
    }

    /// The transient drawing to paint while placing (committed anchors plus the cursor), or
    /// `None`. With no cursor yet only an `hline` with an anchor shows (itself).
    pub fn ghost(&self, registry: &DrawingRegistry) -> Option<Box<dyn Drawing>> {
        let State::Placing { draft, cursor, .. } = &self.state else {
            return None;
        };
        let doc = draft.serialize();
        let Some(cursor) = cursor else {
            if !draft.base().anchors.is_empty() && draft.type_key() == "hline" {
                return registry.deserialize(&doc);
            }
            return None;
        };
        let mut anchors = doc.anchors.clone();
        anchors.push(placement_anchor(
            draft.type_key(),
            &draft.base().anchors,
            *cursor,
        ));
        registry.create(
            draft.type_key(),
            DrawingInit {
                pane_id: doc.pane_id,
                anchors,
                style: Some(doc.style.to_partial()),
                text: doc.text,
                props: doc.props,
                ..DrawingInit::default()
            },
        )
    }

    /// Sample a freehand point while dragging (raw, unsnapped), capped at the path max.
    fn capture_freehand(&mut self, deps: &mut dyn InteractionDeps, x: f64, y: f64) {
        self.snap_at = None;
        let proj = deps.projector();
        let State::Placing {
            draft,
            need,
            cursor,
        } = &mut self.state
        else {
            return;
        };
        let pane_id = draft.base().pane_id.clone();
        let last = draft.base().anchors.last().copied();
        let lx = last.map_or(f64::NAN, |l| proj.x_of(l.time));
        let ly = last
            .and_then(|l| proj.y_of(l.price, &pane_id))
            .unwrap_or(f64::NAN);
        if freehand_should_sample(x, y, lx, ly) {
            // At the path cap a long stroke keeps drawing: the sampled points halve (the
            // endpoints stay), so the older trail gets coarser instead of the capture dying
            // mid-gesture.
            thin_freehand_anchors(&mut draft.base_mut().anchors, *need);
            let point = proj.px_to_point(x, y, &pane_id);
            draft.base_mut().anchors.push(point);
        }
        *cursor = Some(proj.px_to_point(x, y, &pane_id));
        deps.changed();
    }

    /// Finish a variable click-placement (polyline). `drop_last` removes the duplicate point
    /// a double-click adds. Returns whether a drawing was committed.
    pub fn finish_placing(&mut self, deps: &mut dyn InteractionDeps, drop_last: bool) -> bool {
        let State::Placing { draft, .. } = &mut self.state else {
            return false;
        };
        let sch = draft.anchor_schema();
        if draft.placement_mode() != PlacementMode::Click || sch.max <= sch.min {
            return false; // only variable click tools
        }
        if drop_last && draft.base().anchors.len() > sch.min {
            draft.base_mut().anchors.pop();
        }
        if draft.base().anchors.len() < sch.min {
            return false;
        }
        self.finalize(deps);
        true
    }

    /// Abandon an in-progress placement (too few points) and disarm.
    fn discard_placing(&mut self, deps: &mut dyn InteractionDeps) {
        let State::Placing { draft, .. } = &self.state else {
            return;
        };
        let type_key = draft.type_key().to_string();
        self.state = State::Idle;
        deps.emit(DrawingIntent::ToolFinished { type_key });
        deps.changed();
    }

    fn finalize(&mut self, deps: &mut dyn InteractionDeps) {
        self.snap_at = None;
        let State::Placing { mut draft, .. } = std::mem::replace(&mut self.state, State::Idle)
        else {
            return;
        };
        // Let the type finalize its anchors (a position derives its box).
        draft.on_placed(deps.projector());
        let type_key = draft.type_key().to_string();
        deps.emit(DrawingIntent::Create {
            doc: draft.serialize(),
        });
        deps.emit(DrawingIntent::ToolFinished { type_key });
        deps.changed();
    }

    /// The id of a showing handle's drawing (any selected or hovered drawing), even off the
    /// body (an ellipse's bounding-box corners), which wins; otherwise the topmost drawing
    /// whose body is under `(x, y)`.
    fn hit_id_at(&self, deps: &dyn InteractionDeps, x: f64, y: f64) -> Option<String> {
        let proj = deps.projector();
        let shown = deps.drawings();
        let mut ids: Vec<String> = deps.selected_ids();
        if let Some(h) = deps.hovered_id()
            && !ids.contains(&h)
        {
            ids.push(h);
        }
        for id in &ids {
            if let Some(d) = shown.iter().find(|d| d.base().id == *id)
                && d.hit_handle(x, y, proj, HIT_TOLERANCE).is_some()
            {
                return Some(id.clone());
            }
        }
        top_drawing_at(&shown, x, y, proj, HIT_TOLERANCE).map(|d| d.base().id.clone())
    }
}

#[cfg(test)]
mod tests;
