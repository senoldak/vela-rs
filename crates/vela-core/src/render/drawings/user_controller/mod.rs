//! The renderer-side user-drawings controller, ported from the original
//! `UserDrawingController.ts` (with `DrawingInteraction.ts` in [`super::interaction`]).
//!
//! It is the native renderer's implementation of the core's [`DrawingsRendererPort`] AND of
//! the shell's [`UserDrawingsHook`]: it owns the drawings canvas (L1.5, over the series) and
//! the INTERLEAVE layers (prepainted plot-sized surfaces handed to the backend for drawings
//! whose z puts them under a series), the interaction state machine, hover, selection,
//! clipboard and keyboard actions, the measure ruler and eraser, snapping, and the inline
//! text editing and settings popup STATE. The core `DrawingController` stays the source of
//! truth: this projects its `sync_drawings` snapshots and reports gestures back as intents.
//! It never holds authoritative state beyond the current projection.
//!
//! # Ownership
//!
//! One `RefCell<Ctl>` ([`state::Ctl`]); every entry point borrows it, changes state, collects
//! the [`Effect`]s the change owes (intents for the core, repaints, host UI events), and
//! delivers them in order AFTER the borrow is released, so the core may call straight back
//! into the controller. Entry points the renderer calls while its own state is borrowed
//! (the [`UserDrawingsHook`] methods other than `deselect`) never touch the renderer or the
//! core synchronously: whatever they owe is deferred to the next host frame.
//!
//! A drag mutates the controller's own projected copy of a drawing in place; the release
//! hands the final shape to the core as ONE `edit` / `edit-many` / `clone` intent, so the
//! core store sees a gesture as one history entry. The core then syncs the snapshot back.
//!
//! # Input: who decides whether a press belongs to drawings
//!
//! The original `InputController` (DOM, kept by the browser crate) asks the renderer through
//! its deps; here those deps are the public methods below. On a primary-button press it calls
//! [`claim`](UserDrawingController::claim) first: `true` means the drawings layer owns the
//! whole gesture (`pointer_down`, every following `pointer_move`, `pointer_up`) and the chart
//! does not pan; `false` lets the chart pan. See the method docs for the exact protocol
//! (Shift = measure start, Ctrl/Cmd = marquee, middle = delete, right = cancel, double click).
//!
//! # Left for the browser crate
//!
//! The toolbar, settings popup and panel, the inline `<textarea>`, the magnifier menu, the
//! cursor and pointer capture: see [`ui`] (`DrawingsUiEvent`). Series-gateway updates call
//! [`UserDrawingController::series_updated`].
//!
//! # Differences from the original
//!
//! - The painter resolves the magnifier's bar-transform styles through the BUILT-IN chart
//!   types (the shell's registry is not shareable with it).
//! - The core's series gateway has no `on_update` subscription: the host calls
//!   [`series_updated`](UserDrawingController::series_updated).
//! - `setTimeout(0)` after a `create` (open the new drawing's settings and editor) is a
//!   host frame task.

mod paint;
mod state;
#[cfg(test)]
mod tests;
pub mod ui;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::rc::{Rc, Weak};

use serde_json::Value;

use crate::drawings::style::DrawingStyle;
use crate::drawings::{
    Drawing, DrawingPoint, DrawingSeriesGateway, DrawingToolDefaults, DrawingToolTemplate, Props,
    SerializedDrawing, SnapMode,
};
use crate::i18n::source_text;
use crate::ports::chart_renderer::{
    DrawingIntent, DrawingMode, DrawingTypeKey, DrawingsRendererPort, LayoutMode, Listener,
    ReorderTo, StackRange, VelaTheme,
};
use crate::render::core::scene_graph::DrawingSlice;
use crate::render::drawings::keys::DrawingKeyEvent;
use crate::render::native::{FrameView, NativeHost, NativeRenderer, UserDrawingsHook};
use crate::render::shared::drawing_geometry::contrast_color;
use crate::runtime::Unsubscribe;

use state::{Ctl, Effect, Env, NullProjector, is_inline_editable};
pub use state::{TOOLBAR_COLLAPSED_WIDTH, TOOLBAR_WIDTH};
pub use ui::{
    DrawingsUiEvent, MagnifierMenuRequest, PopupEvent, PopupRequest, TextEditorEvent,
    TextEditorKind, TextEditorSpec, ToolbarUiState,
};

type IntentSink = Rc<RefCell<Listener<DrawingIntent>>>;

/// The renderer-side user-drawings controller. See the module documentation.
pub struct UserDrawingController {
    me: Weak<UserDrawingController>,
    host: Rc<dyn NativeHost>,
    renderer: Weak<NativeRenderer>,
    st: RefCell<Ctl>,
    /// Core-pushed series gateway (finer-timeframe bars for data-driven drawings). Its own
    /// cell: the renderer reads it (`series_gateway`) while the controller is borrowed.
    gateway: RefCell<Option<Rc<dyn DrawingSeriesGateway>>>,
    intent_cb: RefCell<Option<IntentSink>>,
    /// Intents waiting for the core callback (one delivery runs at a time; an intent
    /// raised from inside the callback is delivered when it returns).
    intent_queue: RefCell<VecDeque<DrawingIntent>>,
    delivering: Cell<bool>,
    /// Work deferred to a host frame (hook calls made under the renderer's borrow).
    deferred: RefCell<Vec<Deferred>>,
    frame_armed: Cell<bool>,
}

/// Work waiting for the next host frame.
enum Deferred {
    Effect(Box<Effect>),
    /// A `create` was delivered: open the new drawing's settings (and its inline editor).
    OpenFresh(HashSet<String>),
}

impl UserDrawingController {
    /// A controller on `host`, pulling the plot's geometry from `renderer`. The shell builds
    /// one by default (see `NativeRenderer::set_user_drawings` to replace it).
    pub fn new(host: Rc<dyn NativeHost>, renderer: Weak<NativeRenderer>) -> Rc<Self> {
        Rc::new_cyclic(|me| Self {
            me: me.clone(),
            host,
            renderer,
            st: RefCell::new(Ctl::new()),
            gateway: RefCell::new(None),
            intent_cb: RefCell::new(None),
            intent_queue: RefCell::new(VecDeque::new()),
            delivering: Cell::new(false),
            deferred: RefCell::new(Vec::new()),
            frame_armed: Cell::new(false),
        })
    }

    // ── plumbing: environments, effects ──

    /// Run `f` with the projector and the plot's facts, then deliver what it queued.
    fn run<R>(&self, f: impl FnOnce(&mut Ctl, &Env) -> R) -> R {
        let r = self.with_env(f);
        self.flush();
        r
    }

    fn with_env<R>(&self, f: impl FnOnce(&mut Ctl, &Env) -> R) -> R {
        let Some(renderer) = self.renderer.upgrade() else {
            let theme = Value::Null;
            let snap = |p: DrawingPoint, _: &str, _: SnapMode, _: Option<(f64, f64)>| p;
            let env = Env {
                proj: &NullProjector,
                bar_ms: 0.0,
                theme: &theme,
                snap: &snap,
            };
            return f(&mut self.st.borrow_mut(), &env);
        };
        let theme = renderer.plot_theme();
        let bar_ms = renderer.chart_bar_ms();
        let snap = |p: DrawingPoint, pane: &str, mode: SnapMode, cursor: Option<(f64, f64)>| {
            let (time, price) = renderer.snap_to_candle((p.time, p.price), pane, mode, cursor);
            DrawingPoint { time, price }
        };
        renderer.with_drawing_projector(|proj| {
            let env = Env {
                proj,
                bar_ms,
                theme: &theme,
                snap: &snap,
            };
            f(&mut self.st.borrow_mut(), &env)
        })
    }

    /// Run a state change that needs no projector, then deliver what it queued.
    fn act<R>(&self, f: impl FnOnce(&mut Ctl) -> R) -> R {
        let r = f(&mut self.st.borrow_mut());
        self.flush();
        r
    }

    /// Deliver the effects the state queued, in order, with no state borrowed.
    fn flush(&self) {
        let fx = std::mem::take(&mut self.st.borrow_mut().fx);
        for e in fx {
            self.exec(e);
        }
    }

    fn exec(&self, e: Effect) {
        match e {
            Effect::Emit(i) => self.emit_intent(i),
            Effect::Changed => {
                self.act(Ctl::sync_lift);
                self.repaint();
            }
            Effect::Repaint => self.repaint(),
            Effect::DataPaint => {
                if let Some(r) = self.renderer.upgrade() {
                    r.invalidate_data();
                }
            }
            Effect::Gutter(px) => {
                if let Some(r) = self.renderer.upgrade() {
                    r.set_toolbar_gutter(px);
                }
            }
            Effect::SetSnap(mode) => {
                if let Some(r) = self.renderer.upgrade() {
                    r.set_snap_mode(mode);
                }
            }
            Effect::Ui(ev) => self.host.drawings_ui(&ev),
            Effect::OpenSettings(id) => self.run(|ctl, env| ctl.open_settings_by_id(env, &id)),
        }
    }

    fn repaint(&self) {
        if let Some(r) = self.renderer.upgrade() {
            r.repaint_user_drawings();
        }
    }

    /// For the entry points the renderer calls under its own borrow: host UI events go
    /// straight out (the host seam is never re-entered), everything else waits for a frame.
    fn flush_deferred(&self) {
        let fx = std::mem::take(&mut self.st.borrow_mut().fx);
        let mut waiting = false;
        for e in fx {
            match e {
                Effect::Ui(ev) => self.host.drawings_ui(&ev),
                other => {
                    self.deferred
                        .borrow_mut()
                        .push(Deferred::Effect(Box::new(other)));
                    waiting = true;
                }
            }
        }
        if waiting {
            self.arm_frame();
        }
    }

    fn arm_frame(&self) {
        if self.frame_armed.replace(true) {
            return;
        }
        let me = self.me.clone();
        self.host.request_frame(Box::new(move || {
            if let Some(ctl) = me.upgrade() {
                ctl.run_deferred();
            }
        }));
    }

    fn run_deferred(&self) {
        self.frame_armed.set(false);
        let work = std::mem::take(&mut *self.deferred.borrow_mut());
        for w in work {
            match w {
                Deferred::Effect(e) => self.exec(*e),
                Deferred::OpenFresh(before) => self.open_fresh(&before),
            }
        }
    }

    /// Open the editing UI of a freshly placed drawing: every type opens its settings menu,
    /// and a text label or callout additionally opens its inline editor so the caret is
    /// already waiting.
    fn open_fresh(&self, before: &HashSet<String>) {
        self.run(|ctl, env| {
            let Some(fresh) = ctl
                .drawings
                .iter()
                .find(|d| !before.contains(&d.base().id))
                .map(|d| (d.base().id.clone(), is_inline_editable(d.as_ref())))
            else {
                return;
            };
            ctl.open_settings_by_id(env, &fresh.0);
            if fresh.1 {
                ctl.edit_text_inline(env.proj, env.theme, &fresh.0); // focus lands in the editor
            }
        });
    }

    // ── intents up ──

    /// The one door intents leave through. A freshly-placed drawing surfaces its editing UI
    /// by default once it's set; a drag-to-duplicate ends like a click on the copy.
    fn emit_intent(&self, mut i: DrawingIntent) {
        match &mut i {
            DrawingIntent::Create { doc } => {
                // A fresh annotation FIXES its text ink at creation: max contrast against the
                // live theme's plot background, stored on the drawing itself, so a later
                // theme or background change never recolors what is already placed. A color
                // the user chose (or a deserialized document carries) passes through.
                let background = self
                    .renderer
                    .upgrade()
                    .map(|r| {
                        r.plot_theme()
                            .get("background")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    })
                    .unwrap_or_default();
                if let Some(text) = doc.text.as_mut()
                    && text.color.is_none()
                {
                    text.color = Some(contrast_color(Some(&background)).to_string());
                }
                // A fresh annotation's starter text ("Note") is written in the chart's
                // language; text the user already typed, or a deserialized document's,
                // passes through.
                let starter = {
                    let st = self.st.borrow();
                    doc.text.as_ref().and_then(|text| {
                        let placeholder = st.registry.deserialize(doc)?.placeholder_text()?;
                        (text.value == source_text(&placeholder)).then(|| st.t(&placeholder))
                    })
                };
                if let (Some(value), Some(text)) = (starter, doc.text.as_mut()) {
                    text.value = value;
                }
                let before: HashSet<String> = self
                    .st
                    .borrow()
                    .drawings
                    .iter()
                    .map(|d| d.base().id.clone())
                    .collect();
                self.deliver(i);
                // Defer past the sync and the placing click (which would otherwise steal
                // focus or dismiss the popup) before opening the editing UI.
                self.deferred.borrow_mut().push(Deferred::OpenFresh(before));
                self.arm_frame();
            }
            DrawingIntent::Clone { .. } => {
                // A drag-to-duplicate ends like a click on the copy: its settings bar opens
                // so it can be restyled right away. Several copies need nothing here: the
                // selection they become brings up the multi-selection bar on its own.
                let before: HashSet<String> = self
                    .st
                    .borrow()
                    .drawings
                    .iter()
                    .map(|d| d.base().id.clone())
                    .collect();
                self.deliver(i);
                let fresh: Vec<String> = self
                    .st
                    .borrow()
                    .drawings
                    .iter()
                    .map(|d| d.base().id.clone())
                    .filter(|id| !before.contains(id))
                    .collect();
                if let [only] = fresh.as_slice() {
                    self.run(|ctl, env| ctl.open_settings_by_id(env, only));
                }
            }
            _ => self.deliver(i),
        }
    }

    /// Hand an intent to the core. One delivery runs at a time: an intent raised from inside
    /// the callback (the core syncing back into this controller, which reacts) follows when
    /// the callback returns.
    fn deliver(&self, i: DrawingIntent) {
        self.intent_queue.borrow_mut().push_back(i);
        if self.delivering.replace(true) {
            return;
        }
        loop {
            let Some(next) = self.intent_queue.borrow_mut().pop_front() else {
                break;
            };
            let cb = self.intent_cb.borrow().clone();
            if let Some(cb) = cb
                && let Ok(mut f) = cb.try_borrow_mut()
            {
                (**f)(&next);
            }
        }
        self.delivering.set(false);
    }

    // ── gestures (what InputController calls) ──

    /// Should the drawing layer win this press (vs pan)? The input controller asks on every
    /// primary-button press INSIDE the plot (and calls [`clear_transient`](Self::clear_transient)
    /// before it for every press). `true` when the measure ruler or eraser is armed, the
    /// press is on a magnifier's timeframe chip, a tool is armed or a gesture is in flight,
    /// or the press lands on a drawing or one of its showing handles; then the input
    /// controller routes the gesture to [`pointer_down`](Self::pointer_down),
    /// [`pointer_move`](Self::pointer_move) and [`pointer_up`](Self::pointer_up) and does not
    /// start a pan. Only while the `drawingsInteractive` feature is on
    /// ([`NativeRenderer::drawings_interactive`]); with it off the browser never asks.
    pub fn claim(&self, x: f64, y: f64) -> bool {
        self.run(|ctl, env| {
            if ctl.measure_mode || ctl.eraser_mode {
                return true; // these modes capture their clicks (no pan)
            }
            if ctl.magnifier_chip_at(env.bar_ms, x, y).is_some() {
                return true; // the chip is a dropdown trigger, never a pan start
            }
            ctl.with_interaction(env, |it, deps| it.claim(deps, x, y))
        })
    }

    /// Shift+press on the empty plot (inside the data area): arm the measure ruler AND start
    /// it at `(x, y)` in one gesture, the equivalent of clicking the toolbar's Measure
    /// button, then pressing. `snap` is the effective magnet (the sticky mode, or Ctrl/Cmd
    /// forced `Strong`). The input controller calls it only when the press was NOT claimed;
    /// `true` means the drawings layer owns the rest of the gesture. `false` when a mode or
    /// tool is already active (the normal press path owns it).
    pub fn begin_measure_at(&self, x: f64, y: f64, snap: SnapMode) -> bool {
        self.run(|ctl, env| {
            if ctl.measure_mode || ctl.eraser_mode || ctl.active_tool.is_some() {
                return false;
            }
            ctl.with_mode_intent(Ctl::toggle_measure);
            let g = ctl.with_interaction(env, |it, deps| it.snap_cursor(deps, x, y, snap));
            ctl.measure.down(x, y, Some(g));
            ctl.repaint();
            true
        })
    }

    /// A press landed on the chart: a finished transient measurement clears (the ruler
    /// vanishes on the next press, pan or zoom), and an open inline edit ends. The input
    /// controller calls it for EVERY press and on zoom / pan, including the ones the
    /// drawings layer doesn't claim: a press on empty chart is a pan, and the editor may not
    /// hold focus, so neither `pointer_down` nor blur would end the edit.
    pub fn clear_transient(&self) {
        self.act(|ctl| {
            if ctl.text_editor.is_some() {
                ctl.finish_text_editor(true); // clicking off the text keeps what was typed
            }
            if ctl.measure.is_finished() {
                ctl.measure.clear();
                ctl.repaint();
            }
        });
    }

    /// Ctrl/Cmd+press on the empty plot (inside the data area): sweep a selection box (adds
    /// the drawings it touches to the selection). The input controller calls it when the
    /// press was NOT claimed; `true` means the drawings layer owns the rest of the gesture.
    /// `false` when a mode or tool is active (the normal press path owns it).
    pub fn begin_marquee_at(&self, x: f64, y: f64) -> bool {
        self.run(|ctl, env| {
            if ctl.measure_mode || ctl.eraser_mode {
                return false;
            }
            if !ctl.with_interaction(env, |it, deps| it.begin_marquee(deps, x, y)) {
                return false;
            }
            ctl.repaint();
            true
        })
    }

    /// A claimed press (primary button). `snap` is the effective magnet (the sticky mode,
    /// `Strong` while Ctrl/Cmd is held), `shift` and `modifier` (Ctrl/Cmd) the key state.
    pub fn pointer_down(&self, x: f64, y: f64, snap: SnapMode, shift: bool, modifier: bool) {
        self.run(|ctl, env| {
            if ctl.eraser_mode {
                ctl.erasing = true; // hold to drag-erase across multiple drawings
                ctl.delete_at(env, x, y, false);
                return;
            }
            if ctl.measure_mode {
                let g = ctl.with_interaction(env, |it, deps| it.snap_cursor(deps, x, y, snap));
                ctl.measure.down(x, y, Some(g));
                // 2nd click finishes: disarm (reported as a mode change), keep the ruler shown.
                if ctl.measure.is_finished() {
                    ctl.with_mode_intent(|c| c.exit_measure(false));
                }
                ctl.repaint();
                return;
            }
            // The magnifier's timeframe chip swallows its press: it opens the pick menu
            // instead of selecting or dragging the drawing under it (placement wins when a
            // tool is armed).
            if ctl.active_tool.is_none()
                && let Some(chip) = ctl.magnifier_chip_at(env.bar_ms, x, y)
            {
                ctl.open_magnifier_chip_menu(&chip);
                return;
            }
            // The popup self-dismisses on any outside press (the browser reports it).
            ctl.with_interaction(env, |it, deps| it.down(deps, x, y, snap, shift, modifier));
        });
    }

    /// A pointer move: while a claimed gesture is in flight, and on every hover inside the
    /// plot (so a placing ghost advances and handles show on hover). Same `snap`, `shift`
    /// and `modifier` as [`pointer_down`](Self::pointer_down).
    pub fn pointer_move(&self, x: f64, y: f64, snap: SnapMode, shift: bool, modifier: bool) {
        self.run(|ctl, env| {
            if ctl.eraser_mode {
                if ctl.erasing {
                    ctl.delete_at(env, x, y, false); // erase only while the button is held
                }
                return;
            }
            if ctl.measure_mode {
                let g = ctl.with_interaction(env, |it, deps| it.snap_cursor(deps, x, y, snap));
                ctl.measure.move_to(g.0, g.1); // click-move-click: no button needed
                ctl.repaint();
                return;
            }
            ctl.with_interaction(env, |it, deps| it.move_to(deps, x, y, snap, shift));
            ctl.update_hover(env, x, y, modifier); // show handles for the drawing under the cursor
        });
    }

    /// Drop the hover (its handles): the pointer no longer reaches the drawings.
    pub fn clear_hover_now(&self) {
        self.act(|ctl| {
            if ctl.hovered.take().is_some() {
                ctl.repaint();
            }
        });
    }

    /// The release of a claimed gesture.
    pub fn pointer_up(&self, x: f64, y: f64, snap: SnapMode) {
        enum After {
            Done,
            Release(Option<String>),
        }
        let after = self.run(|ctl, env| {
            if ctl.eraser_mode {
                ctl.erasing = false;
                return After::Done;
            }
            if ctl.measure_mode {
                let g = ctl.with_interaction(env, |it, deps| it.snap_cursor(deps, x, y, snap));
                ctl.measure.up(x, y, Some(g)); // press-drag-release finishes the ruler in one gesture
                if ctl.measure.is_finished() {
                    ctl.with_mode_intent(|c| c.exit_measure(false));
                }
                ctl.repaint();
                return After::Done;
            }
            // A drag of a SELECTED drawing dismissed its bar on the press (but kept the
            // selection); once the drag lands, the bar comes back over the moved drawings.
            let dragged = if ctl.interaction.is_dragging() {
                ctl.interaction.pressed_id().map(str::to_string)
            } else {
                None
            };
            ctl.with_interaction(env, |it, deps| it.up(deps, x, y)); // commit a drag, or open settings on a no-move click
            After::Release(dragged)
        });
        let After::Release(Some(dragged)) = after else {
            return;
        };
        self.run(|ctl, env| {
            if ctl.selected.contains(&dragged) && ctl.popup.is_none() {
                if ctl.selected.len() >= 2 {
                    let ids = ctl.selected.clone();
                    ctl.open_settings_for_selection(env, &ids);
                } else {
                    ctl.open_settings_by_id(env, &dragged);
                }
            }
        });
    }

    /// Cursor hint while hovering: `Some("pointer")` over a drawing or handle, while erasing
    /// and over a magnifier chip, else `None` (the input controller then falls back to the
    /// mark-glyph hint and the default cursor). Used only while `drawingsInteractive` is on.
    pub fn cursor_at(&self, x: f64, y: f64) -> Option<&'static str> {
        self.run(|ctl, env| {
            if ctl.eraser_mode {
                return Some("pointer"); // signal "click to delete" while erasing
            }
            if ctl.active_tool.is_none() && ctl.magnifier_chip_at(env.bar_ms, x, y).is_some() {
                return Some("pointer"); // the chip is clickable
            }
            ctl.with_interaction(env, |it, deps| it.cursor_at(deps, x, y))
        })
    }

    /// Delete the (unlocked) drawing under the cursor. The middle-click shortcut calls it
    /// with `with_selection` true (a hit on a member of a multi-selection removes the
    /// selection's unlocked members); the eraser goes through `pointer_*`. `true` when one
    /// was removed.
    pub fn delete_at(&self, x: f64, y: f64, with_selection: bool) -> bool {
        self.run(|ctl, env| ctl.delete_at(env, x, y, with_selection))
    }

    /// Right-click: an explicit escape back to the pointer. Cancels an in-progress placement
    /// or measurement, and also plain-disarms an armed-but-idle drawing tool or the eraser,
    /// so a right-click ALWAYS reverts to the pointer, even in stay-in-drawing-mode (where
    /// Escape would leave a drawing tool armed). Persistent toggles (magnet, stay mode,
    /// favorites) are untouched. Returns whether the press was consumed; `false` lets the
    /// host's context menu open normally.
    pub fn cancel_placement(&self) -> bool {
        enum Step {
            Consumed,
            Placing,
            Armed,
            None,
        }
        let step = self.run(|ctl, env| {
            if ctl.measure_mode || ctl.eraser_mode {
                ctl.with_mode_intent(|c| {
                    if c.measure_mode {
                        c.exit_measure(true);
                    } else {
                        c.exit_eraser();
                    }
                });
                return Step::Consumed;
            }
            if ctl.interaction.is_placing() {
                // Emits tool-finished: the core disarms (stay mode and the brush excepted).
                ctl.with_interaction(env, |it, deps| it.cancel(deps));
                return Step::Placing;
            }
            if ctl.active_tool.is_some() {
                return Step::Armed;
            }
            Step::None
        });
        match step {
            Step::Consumed => true,
            Step::Placing => {
                self.act(|ctl| {
                    if ctl.active_tool.is_some() {
                        ctl.emit(DrawingIntent::Arm { type_key: None });
                    }
                });
                true
            }
            Step::Armed => {
                self.act(|ctl| ctl.emit(DrawingIntent::Arm { type_key: None })); // armed, no anchor yet
                true
            }
            Step::None => false,
        }
    }

    /// Double-click over a drawing opens its full settings panel (a callout or text label
    /// edits its words in place instead) and the input controller then suppresses the
    /// chart's view reset and the pane maximize. A double-click that finishes a polyline
    /// (drops the duplicate point) also returns `true`. `true` only when the drawings layer
    /// took the double-click.
    ///
    /// The input controller also treats a double-click as drawings' when EITHER of the two
    /// presses it was made of was claimed (`claim` returned `true`), and then never forwards
    /// it to the pane-maximize toggle.
    pub fn dbl_click(&self, x: f64, y: f64) -> bool {
        self.run(|ctl, env| {
            if ctl.with_interaction(env, |it, deps| it.finish_placing(deps, true)) {
                return true; // double-click finishes a polyline
            }
            let shown = ctl.on_screen(env.bar_ms);
            let hit = crate::render::drawings::hit_tester::top_drawing_at(
                &shown,
                x,
                y,
                env.proj,
                crate::render::drawings::hit_tester::HIT_TOLERANCE,
            )
            .map(|d| (d.base().id.clone(), is_inline_editable(d)));
            let Some((id, inline)) = hit else {
                return false;
            };
            if inline {
                ctl.edit_text_inline(env.proj, env.theme, &id); // edit its text in place
                return true;
            }
            if ctl.popup_multi || !ctl.selected.contains(&id) || ctl.popup.is_none() {
                ctl.open_settings_by_id(env, &id);
            }
            ctl.fx
                .push(Effect::Ui(DrawingsUiEvent::Popup(PopupEvent::OpenPanel)));
            true
        })
    }

    /// Keyboard pre-empt (the keyboard controller's `preempt`): Escape (editor, popup,
    /// placing, ruler, selection), undo / redo (unless the `historyChords` feature is off),
    /// copy / paste / duplicate, delete and arrow nudge. `editing_text` is whether the
    /// event's target is an input, textarea or content-editable field (the DOM adapter calls
    /// `keys::is_editing_text` on it); the layer then stands down. Returns whether the key
    /// was handled (and so must not reach the chart's own key handling).
    pub fn handle_key(&self, e: &DrawingKeyEvent, editing_text: bool) -> bool {
        self.run(|ctl, env| ctl.handle_key(env, e, editing_text))
    }

    /// Plain click on the empty plot: drop the selection. The popup's dismiss-on-outside
    /// press does the same, but a selection can exist with no bar open (a lone Ctrl-click
    /// pick, a selection whose bar a modifier press dismissed); this covers those. The shell
    /// calls it from `NativeRenderer::click`.
    pub fn deselect_now(&self) {
        self.act(Ctl::clear_selection);
    }

    // ── toolbar (user actions from the host's DOM bar) ──

    /// A toolbar tool button: arm it (`None` = back to the pointer). The core decides.
    pub fn toolbar_arm(&self, type_key: Option<&str>) {
        self.act(|ctl| {
            ctl.emit(DrawingIntent::Arm {
                type_key: type_key.map(str::to_string),
            });
        });
    }

    /// The magnet button: applies to the renderer and keeps the core mirror in sync.
    pub fn toolbar_set_snap_mode(&self, mode: SnapMode) {
        self.act(|ctl| {
            ctl.magnet = mode;
            ctl.push(Effect::SetSnap(mode));
            ctl.emit(DrawingIntent::SnapMode { mode });
            ctl.push_toolbar();
        });
    }

    /// The Measure button.
    pub fn toolbar_toggle_measure(&self) {
        self.act(|ctl| ctl.with_mode_intent(Ctl::toggle_measure));
    }

    /// The Eraser button.
    pub fn toolbar_toggle_eraser(&self) {
        self.act(|ctl| ctl.with_mode_intent(Ctl::toggle_eraser));
    }

    /// A flyout star.
    pub fn toolbar_favorite(&self, type_key: &str, on: bool) {
        self.act(|ctl| {
            ctl.emit(DrawingIntent::Favorite {
                type_key: type_key.to_string(),
                on,
            });
        });
    }

    /// The stay-in-drawing-mode toggle.
    pub fn toolbar_stay_mode(&self, on: bool) {
        self.act(|ctl| ctl.emit(DrawingIntent::StayMode { on }));
    }

    /// The bar collapsed to (or expanded from) its slim strip: the gutter follows.
    pub fn toolbar_collapsed(&self, collapsed: bool) {
        self.act(|ctl| {
            ctl.toolbar_collapsed = collapsed;
            ctl.sync_gutter();
            ctl.push_toolbar();
        });
    }

    // ── settings popup (user actions from the host's popup) ──

    /// A control edited settings (`{ "dot.path": value }`): applied to every drawing of the
    /// popup and reported as one edit (one undo step).
    pub fn popup_patch(&self, patch: &Props) {
        self.act(|ctl| ctl.popup_patch(patch));
    }

    pub fn popup_set_locked(&self, locked: bool) {
        self.act(|ctl| ctl.popup_set_locked(locked));
    }

    pub fn popup_reorder(&self, to: ReorderTo) {
        self.act(|ctl| ctl.popup_reorder(to));
    }

    /// Clone in place (the copies become the selection); closes the popup.
    pub fn popup_duplicate(&self) {
        self.act(Ctl::popup_duplicate);
    }

    /// Restore the factory look; the popup reopens so its controls show it.
    pub fn popup_reset_settings(&self) {
        self.run(|ctl, env| ctl.popup_reset_settings(env));
    }

    /// Cancel in the full panel: put a serialized snapshot back (one drawing).
    pub fn popup_restore(&self, doc: &SerializedDrawing) {
        self.act(|ctl| ctl.popup_restore(doc));
    }

    /// The saved styles of the popup drawing's tool.
    pub fn popup_tool_templates(&self) -> Vec<DrawingToolTemplate> {
        self.st.borrow().popup_tool_templates()
    }

    pub fn popup_save_template(&self, name: &str) {
        self.act(|ctl| ctl.popup_save_template(name));
    }

    pub fn popup_remove_template(&self, name: &str) {
        self.act(|ctl| ctl.popup_remove_template(name));
    }

    /// Apply a whole style as one edit.
    pub fn popup_apply_tool_settings(&self, settings: &DrawingToolDefaults) {
        self.act(|ctl| ctl.popup_apply_tool_settings(settings));
    }

    /// Show a style on the drawing without committing (`None` puts the drawing back).
    pub fn popup_preview(&self, settings: Option<&DrawingToolDefaults>) {
        self.act(|ctl| ctl.popup_preview(settings));
    }

    /// Delete the popup's drawing(s) (locked members of a multi-selection stay).
    pub fn popup_remove(&self) {
        self.act(Ctl::popup_remove);
    }

    /// The popup closed ITSELF (outside press, or Escape in one of its text fields).
    /// `modifier`: the press carried Shift, Ctrl or Cmd.
    pub fn popup_dismissed(&self, modifier: bool) {
        self.act(|ctl| ctl.popup_dismissed(modifier));
    }

    /// The live state of a drawing as the popup shows it.
    pub fn drawing_doc(&self, id: &str) -> Option<SerializedDrawing> {
        self.st
            .borrow()
            .drawings
            .iter()
            .find(|d| d.base().id == id)
            .map(|d| d.serialize())
    }

    /// Run `f` on the live drawing (the popup reads its schema and values through it).
    pub fn with_drawing<R>(&self, id: &str, f: impl FnOnce(&dyn Drawing) -> R) -> Option<R> {
        let st = self.st.borrow();
        st.drawings
            .iter()
            .find(|d| d.base().id == id)
            .map(|d| f(d.as_ref()))
    }

    // ── inline text editor (user actions from the host's textarea) ──

    /// The textarea's value changed (typing, or the line break the DOM inserted).
    pub fn text_editor_input(&self, value: &str) {
        self.act(|ctl| ctl.text_editor_input(value));
    }

    /// Ctrl/Cmd+Enter: keep the text and close.
    pub fn text_editor_commit(&self) {
        self.act(|ctl| ctl.finish_text_editor(true));
    }

    /// Escape: put back what was there and close.
    pub fn text_editor_cancel(&self) {
        self.act(|ctl| ctl.finish_text_editor(false));
    }

    /// The textarea lost focus. Reaching for a control in the settings popup keeps the edit
    /// alive (`into_popup`), but hands the text to the core first so the popup's own
    /// patches build on it; clicking away keeps the text and closes.
    pub fn text_editor_blur(&self, into_popup: bool) {
        self.act(|ctl| {
            if into_popup {
                let value = ctl.text_editor.as_ref().map(|e| e.value.clone());
                if let Some(value) = value {
                    ctl.store_text(&value);
                }
            } else {
                ctl.finish_text_editor(true);
            }
        });
    }

    /// The magnifier chip's menu picked a timeframe.
    pub fn magnifier_timeframe_picked(&self, id: &str, value: &str) {
        self.act(|ctl| ctl.magnifier_timeframe_picked(id, value));
    }

    /// A background series fetch landed: repaint this layer and the interleave slices.
    pub fn series_updated(&self) {
        self.act(|ctl| {
            ctl.invalidate_slices();
            ctl.repaint();
            ctl.push(Effect::DataPaint);
        });
    }

    // ── queries ──

    /// The selected drawing ids, in selection order.
    pub fn selected_ids(&self) -> Vec<String> {
        self.st.borrow().selected.clone()
    }

    /// The projected drawings (every one, in the core's paint order).
    pub fn drawing_docs(&self) -> Vec<SerializedDrawing> {
        self.st
            .borrow()
            .drawings
            .iter()
            .map(|d| d.serialize())
            .collect()
    }

    pub fn active_tool(&self) -> Option<String> {
        self.st.borrow().active_tool.clone()
    }

    /// Whether the settings popup is open (as the controller last asked).
    pub fn is_popup_open(&self) -> bool {
        self.st.borrow().popup.is_some()
    }

    /// Whether the inline text editor is open, and for which drawing.
    pub fn text_editing(&self) -> Option<String> {
        self.st.borrow().text_editor.as_ref().map(|e| e.id.clone())
    }

    pub fn is_measure_mode(&self) -> bool {
        self.st.borrow().measure_mode
    }

    pub fn is_eraser_mode(&self) -> bool {
        self.st.borrow().eraser_mode
    }

    /// The drawing under the pointer (its handles show).
    pub fn hovered_id(&self) -> Option<String> {
        self.st.borrow().hovered.clone()
    }

    /// The union of the visible drawings' price ranges on `pane_id` whose time extent
    /// intersects `[from_time, to_time]`. A seam for a future per-drawing autoscale opt-in;
    /// the renderer does not fold it in today.
    pub fn price_range_for_pane(
        &self,
        pane_id: &str,
        from_time: f64,
        to_time: f64,
    ) -> Option<(f64, f64)> {
        self.st
            .borrow()
            .price_range_for_pane(pane_id, from_time, to_time)
    }
}

impl DrawingsRendererPort for UserDrawingController {
    fn set_toolbar(&self, def: &Value) {
        self.act(|ctl| {
            ctl.toolbar_def = def.clone();
            ctl.push_toolbar();
        });
    }

    fn show_toolbar(&self, visible: bool) {
        self.act(|ctl| {
            ctl.toolbar_visible = visible;
            ctl.push_toolbar();
            ctl.sync_gutter(); // reserve or release the left gutter so the bar never overlaps the plot
        });
    }

    fn sync_drawings(&self, docs: &[SerializedDrawing]) {
        self.act(|ctl| {
            ctl.drawings = docs
                .iter()
                .filter_map(|d| ctl.registry.deserialize(d))
                .collect();
            // The edited drawing can vanish from under the editor (deleted, erased, undone):
            // take the editor with it instead of leaving a live textarea over nothing.
            if let Some(ed) = &ctl.text_editor
                && ctl.edited_drawing(&ed.id).is_none()
            {
                ctl.close_text_editor();
            }
            ctl.invalidate_slices();
            ctl.repaint();
            ctl.push(Effect::DataPaint); // requestScaleUpdate
            if ctl.popup.is_some() {
                ctl.push(Effect::Ui(DrawingsUiEvent::Popup(PopupEvent::SyncPanel)));
            }
        });
    }

    fn set_active_tool(
        &self,
        tool: Option<&str>,
        last_style: Option<&DrawingStyle>,
        defaults: Option<&DrawingToolDefaults>,
    ) {
        self.run(|ctl, env| {
            if tool.is_some() {
                ctl.finish_text_editor(true); // arming a real tool ends an open inline edit (keeping the text)
                // Picking a drawing tool cancels the ruler and the eraser: a mutual
                // exclusion side effect the core (and any external toolbar) learns via the
                // mode intent.
                ctl.with_mode_intent(|c| {
                    if c.measure_mode {
                        c.exit_measure(true);
                    }
                    if c.eraser_mode {
                        c.exit_eraser();
                    }
                });
            }
            ctl.active_tool = tool.map(str::to_string);
            // Seeds the placement ghost so it matches what will be committed; a caller that
            // only knows the style still gets a matching color.
            ctl.active_tool_defaults = defaults.cloned().or_else(|| {
                last_style.map(|style| DrawingToolDefaults {
                    style: Some(style.to_partial()),
                    ..DrawingToolDefaults::default()
                })
            });
            ctl.push_toolbar();
            if tool.is_none() {
                ctl.with_interaction(env, |it, deps| it.on_tool_cleared(deps));
            } else {
                ctl.clear_selection(); // arming a tool dismisses an open settings popup + selection
            }
            ctl.repaint();
        });
    }

    fn set_selection(&self, ids: &[String]) {
        self.run(|ctl, env| ctl.set_selection(env, ids));
    }

    fn set_favorites(&self, tools: &[DrawingTypeKey]) {
        self.act(|ctl| {
            ctl.favorites = tools.to_vec();
            ctl.push_toolbar();
        });
    }

    fn set_tool_templates(&self, map: &BTreeMap<DrawingTypeKey, Vec<DrawingToolTemplate>>) {
        self.act(|ctl| ctl.tool_templates = map.clone());
    }

    fn set_tool_shortcuts(&self, map: &BTreeMap<DrawingTypeKey, String>) {
        self.act(|ctl| {
            ctl.shortcuts = map.clone();
            ctl.push_toolbar();
        });
    }

    fn set_snap_mode(&self, mode: SnapMode) {
        self.act(|ctl| {
            ctl.push(Effect::SetSnap(mode));
            ctl.magnet = mode;
            ctl.push_toolbar();
        });
    }

    fn set_stay_mode(&self, on: bool) {
        self.act(|ctl| {
            ctl.stay_mode = on;
            ctl.push_toolbar();
        });
    }

    /// Enter or exit measure or eraser (`None` = none). Reuses the toggles so the mutual
    /// exclusion stays in one place; any ACTUAL change is reported back through the mode
    /// intent.
    fn set_mode(&self, mode: Option<DrawingMode>) {
        self.act(|ctl| {
            ctl.with_mode_intent(|c| match mode {
                Some(DrawingMode::Measure) => {
                    if !c.measure_mode {
                        c.toggle_measure();
                    }
                }
                Some(DrawingMode::Eraser) => {
                    if !c.eraser_mode {
                        c.toggle_eraser();
                    }
                }
                None => {
                    if c.measure_mode {
                        c.exit_measure(true);
                    }
                    if c.eraser_mode {
                        c.exit_eraser();
                    }
                }
            });
        });
    }

    fn set_series_gateway(&self, gateway: Rc<dyn DrawingSeriesGateway>) {
        *self.gateway.borrow_mut() = Some(gateway);
    }

    fn open_settings(&self, id: &str) {
        self.run(|ctl, env| ctl.open_settings_by_id(env, id));
    }

    fn set_external_ghost(&self, doc: Option<&SerializedDrawing>) {
        self.act(|ctl| {
            ctl.external_ghost = doc.and_then(|d| ctl.registry.deserialize(d));
            ctl.repaint();
        });
    }

    /// The pane's series stack in z terms: how the core places a new drawing (just under
    /// `price`) and computes "front of everything" / "behind everything" for the reorders.
    fn stack_range(&self, pane_id: &str) -> Option<StackRange> {
        let renderer = self.renderer.upgrade()?;
        let bounds = renderer.series_boundaries(pane_id);
        Some(StackRange {
            front: bounds.iter().copied().fold(0.0, f64::max),
            back: bounds.iter().copied().fold(0.0, f64::min),
            price: renderer.price_z(pane_id),
        })
    }

    fn on_drawing_intent(&self, cb: Listener<DrawingIntent>) -> Unsubscribe {
        let sink: IntentSink = Rc::new(RefCell::new(cb));
        *self.intent_cb.borrow_mut() = Some(sink.clone());
        let me = self.me.clone();
        Box::new(move || {
            if let Some(ctl) = me.upgrade() {
                let mut slot = ctl.intent_cb.borrow_mut();
                if slot.as_ref().is_some_and(|s| Rc::ptr_eq(s, &sink)) {
                    *slot = None;
                }
            }
        })
    }
}

impl UserDrawingsHook for UserDrawingController {
    fn prepare_slices(
        &self,
        pane_ids: &[String],
        view: &FrameView,
    ) -> std::collections::HashMap<String, Vec<DrawingSlice>> {
        let gateway = self.gateway.borrow().clone();
        let out = view.with_projector(gateway.as_deref(), |proj| {
            self.st
                .borrow_mut()
                .prepare_slices(self.host.as_ref(), view, proj, pane_ids)
        });
        self.flush_deferred();
        out
    }

    fn render(&self, view: &FrameView) {
        let gateway = self.gateway.borrow().clone();
        view.with_projector(gateway.as_deref(), |proj| {
            self.st.borrow_mut().render(view, proj);
        });
        self.flush_deferred();
    }

    /// The plot resized: an open edit ends (keeping the text) and the popup closes.
    fn on_resize(&self) {
        {
            // No repaint of its own: the renderer repaints everything after a resize.
            let mut ctl = self.st.borrow_mut();
            ctl.finish_text_editor(true);
            ctl.close_popup();
        }
        self.flush_deferred();
    }

    fn clear_hover(&self) {
        {
            let mut ctl = self.st.borrow_mut();
            if ctl.hovered.take().is_some() {
                ctl.repaint();
            }
        }
        self.flush_deferred();
    }

    fn deselect(&self) {
        self.deselect_now();
    }

    fn series_gateway(&self) -> Option<Rc<dyn DrawingSeriesGateway>> {
        self.gateway.borrow().clone()
    }

    fn set_theme(&self, _chrome_theme: &VelaTheme) {
        // The toolbar and popup re-theme on the host's side (`apply_chrome_theme`), and the
        // renderer repaints every layer after a theme change.
    }

    /// Mobile suppresses the docked toolbar (hover flyouts don't work with touch; the
    /// shell's own drawer picks tools instead) and releases its gutter; flipping back to
    /// desktop restores whatever `show_toolbar` last asked for.
    fn set_layout_mode(&self, mode: LayoutMode) {
        {
            let mut ctl = self.st.borrow_mut();
            ctl.mobile_layout = mode == LayoutMode::Mobile;
            ctl.push_toolbar();
            ctl.sync_gutter();
        }
        self.flush_deferred();
    }

    fn set_history_chords(&self, on: bool) {
        self.st.borrow_mut().history_chords = on;
    }

    /// The chart's language changed: the toolbar relabels (the core rebuilds and re-pushes
    /// its definition), an open settings bar closes (it reopens in the new language), and
    /// labels painted on drawings repaint.
    fn relocalize(&self) {
        {
            // The renderer repaints everything after a language change.
            let mut ctl = self.st.borrow_mut();
            ctl.push_toolbar();
            ctl.close_popup();
        }
        self.flush_deferred();
    }

    fn destroy(&self) {
        {
            let mut ctl = self.st.borrow_mut();
            ctl.close_text_editor();
            ctl.close_popup();
            ctl.drawings.clear();
        }
        // Host events only: the renderer is mid-destroy.
        let fx = std::mem::take(&mut self.st.borrow_mut().fx);
        for e in fx {
            if let Effect::Ui(ev) = e {
                self.host.drawings_ui(&ev);
            }
        }
        *self.gateway.borrow_mut() = None;
        *self.intent_cb.borrow_mut() = None;
        self.deferred.borrow_mut().clear();
    }
}
