//! The controller's single state struct and every transition on it.
//!
//! [`Ctl`] holds what the original `UserDrawingController` holds as fields (the projection
//! of the core's drawings, selection, hover, modes, the interaction, the painter, the
//! measure ruler, the inline editor and settings popup bookkeeping). A transition is a
//! `&mut Ctl` method that changes state and QUEUES the work it owes the outside world as
//! [`Effect`]s (intents for the core, repaints, host events); the wrapper in `mod.rs`
//! delivers them after the state borrow is released, in order. That is the shell's
//! ownership model: one `RefCell`, events after the borrow, the host seam never re-entered.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use serde_json::Value;

use crate::drawings::types::annotations::CalloutTool;
use crate::drawings::types::measure::Magnifier;
use crate::drawings::{
    Bounds, Drawing, DrawingPoint, DrawingRegistry, DrawingToolDefaults, DrawingToolTemplate,
    PaneRect, Projector, Props, SerializedDrawing, SnapMode, apply_tool_defaults,
    capture_tool_defaults, shown_on_timeframe,
};
use crate::i18n::{Translator, msg, text_of};
use crate::indicators::classics::math::js_round;
use crate::ports::chart_renderer::{DrawingIntent, DrawingMode, ReorderTo};
use crate::render::canvas::Canvas2d;
use crate::render::core::color::with_alpha;
use crate::render::drawings::hit_tester::{
    HIT_TOLERANCE, deletable_selection, delete_targets, top_drawing_at,
};
use crate::render::drawings::interaction::{DrawingInteraction, InteractionDeps};
use crate::render::drawings::interaction_logic::slice_key_for;
use crate::render::drawings::keys::{
    DrawingKeyAction, DrawingKeyContext, DrawingKeyEvent, key_to_drawing_action,
};
use crate::render::drawings::measure_overlay::MeasureOverlay;
use crate::render::drawings::painter::DrawingPainter;
use crate::render::shared::drawing_geometry::{
    TEXT_FRAME_INSET, TEXT_FRAME_RISE, label_line_height, named_font_size,
};

use super::ui::{
    DrawingsUiEvent, MagnifierMenuRequest, PopupEvent, PopupRequest, TextEditorEvent,
    TextEditorKind, TextEditorSpec, ToolbarUiState,
};

/// Docked toolbar widths (`TOOLBAR_WIDTH`, `TOOLBAR_COLLAPSED_WIDTH` of the original).
pub const TOOLBAR_WIDTH: f64 = 44.0;
pub const TOOLBAR_COLLAPSED_WIDTH: f64 = 16.0;
/// Extra width past the measured glyphs so the caret has somewhere to sit.
const CARET_ROOM: f64 = 8.0;
/// The editor's border weight; its padding is the shared frame inset/rise minus this, so the
/// border lands exactly on the frame the painter draws.
const EDITOR_BORDER: f64 = 1.0;

/// The work a transition owes the outside world, in call order.
#[derive(Debug)]
pub(super) enum Effect {
    /// A renderer-to-core intent (the original's `emit`, with its create / clone specials).
    Emit(DrawingIntent),
    /// `syncLift()` then a drawings-layer repaint (the interaction's `changed`).
    Changed,
    /// A drawings-layer repaint.
    Repaint,
    /// A data-tier repaint (`requestScaleUpdate` / `requestDataPaint`).
    DataPaint,
    /// Reserve the toolbar's left gutter.
    Gutter(f64),
    /// The sticky magnet mode the renderer applies.
    SetSnap(SnapMode),
    /// A host UI event.
    Ui(DrawingsUiEvent),
    /// A click landed on a drawing: float its settings popup.
    OpenSettings(String),
}

/// The magnet: snap a data point on a pane, given the mode and the cursor pixel.
pub(super) type SnapFn<'a> =
    dyn Fn(DrawingPoint, &str, SnapMode, Option<(f64, f64)>) -> DrawingPoint + 'a;

/// What gestures resolve through: the projector, the chart's bar interval, the plot theme
/// and the magnet.
pub(super) struct Env<'a> {
    pub proj: &'a dyn Projector,
    pub bar_ms: f64,
    pub theme: &'a Value,
    pub snap: &'a SnapFn<'a>,
}

/// A projector with no panes (the renderer is gone).
pub(super) struct NullProjector;

impl Projector for NullProjector {
    fn x_of(&self, _time: f64) -> f64 {
        0.0
    }
    fn y_of(&self, _price: f64, _pane_id: &str) -> Option<f64> {
        None
    }
    fn px_to_point(&self, _x: f64, _y: f64, _pane_id: &str) -> DrawingPoint {
        DrawingPoint {
            time: 0.0,
            price: 0.0,
        }
    }
    fn pane_id_at_y(&self, _y: f64) -> Option<String> {
        None
    }
    fn pane_rect(&self, _pane_id: &str) -> Option<PaneRect> {
        None
    }
    fn width(&self) -> f64 {
        0.0
    }
    fn height(&self) -> f64 {
        0.0
    }
}

/// The open inline editor.
pub(super) struct TextEditor {
    pub id: String,
    /// The text it opened with (so Escape can put it back).
    pub initial: String,
    /// The text the core currently holds (so an edit is reported exactly when it changes).
    pub stored: String,
    /// The live value of the DOM element.
    pub value: String,
}

/// The open settings popup.
pub(super) struct PopupState {
    pub ids: Vec<String>,
    pub multi: bool,
    /// The look under a style being previewed.
    pub preview_base: Option<SerializedDrawing>,
}

pub(super) struct Ctl {
    pub registry: DrawingRegistry,
    pub drawings: Vec<Box<dyn Drawing>>,
    /// Ids painted on an interleave layer this frame: the top canvas paints only their handles.
    pub sliced: HashSet<String>,
    /// Interleaved drawings mid-drag: painted on the top canvas, left out of their slice.
    pub lifted: HashSet<String>,
    /// Cached slice surfaces, keyed `paneId|beforeZ`, reused across frames.
    pub slice_cache:
        HashMap<String, Rc<dyn crate::render::drawings::indicator_slices::SliceSurface>>,
    /// Series boundaries per pane as of the last `prepare_slices`: lets a repaint between
    /// data frames split front / interleaved consistently.
    pub last_bounds: HashMap<String, Vec<f64>>,
    /// Selected drawings (handles shown); the first drives the popup.
    pub selected: Vec<String>,
    pub hovered: Option<String>,
    pub active_tool: Option<String>,
    pub active_tool_defaults: Option<DrawingToolDefaults>,
    pub tool_templates: std::collections::BTreeMap<String, Vec<DrawingToolTemplate>>,
    pub external_ghost: Option<Box<dyn Drawing>>,
    pub last_draft_key: Option<String>,
    pub measure: MeasureOverlay,
    pub toolbar_visible: bool,
    pub toolbar_collapsed: bool,
    pub mobile_layout: bool,
    pub history_chords: bool,
    pub measure_mode: bool,
    pub eraser_mode: bool,
    pub erasing: bool,
    pub text_editor: Option<TextEditor>,
    pub painter: DrawingPainter,
    pub interaction: DrawingInteraction,
    pub popup: Option<PopupState>,
    /// The open (or last open) popup is the multi-selection bar.
    pub popup_multi: bool,
    // The toolbar mirror.
    pub toolbar_def: Value,
    pub favorites: Vec<String>,
    pub shortcuts: std::collections::BTreeMap<String, String>,
    pub magnet: SnapMode,
    pub stay_mode: bool,
    /// The gutter last reserved through the renderer.
    pub gutter_sent: f64,
    // What the last frame handed over, for work between frames.
    pub ctx: Option<Rc<dyn Canvas2d>>,
    pub i18n: Option<Rc<dyn Translator>>,
    pub fx: Vec<Effect>,
}

/// The interaction's host: the controller's own pieces, borrowed for one call.
struct IDeps<'a, 'e> {
    env: &'a Env<'e>,
    drawings: &'a mut Vec<Box<dyn Drawing>>,
    hovered: Option<&'a str>,
    selected: &'a [String],
    tool: Option<&'a str>,
    defaults: Option<&'a DrawingToolDefaults>,
    registry: &'a DrawingRegistry,
    fx: &'a mut Vec<Effect>,
}

impl InteractionDeps for IDeps<'_, '_> {
    fn projector(&self) -> &dyn Projector {
        self.env.proj
    }
    fn active_tool(&self) -> Option<String> {
        self.tool.map(str::to_string)
    }
    fn drawings(&self) -> Vec<&dyn Drawing> {
        let bar_ms = self.env.bar_ms;
        self.drawings
            .iter()
            .filter(|d| shown_on_timeframe(d.base().show_on.as_deref(), bar_ms))
            .map(|d| d.as_ref())
            .collect()
    }
    fn drawing_mut(&mut self, id: &str) -> Option<&mut dyn Drawing> {
        let bar_ms = self.env.bar_ms;
        self.drawings
            .iter_mut()
            .find(|d| d.base().id == id && shown_on_timeframe(d.base().show_on.as_deref(), bar_ms))
            .map(|d| d.as_mut())
    }
    fn hovered_id(&self) -> Option<String> {
        self.hovered.map(str::to_string)
    }
    fn selected_ids(&self) -> Vec<String> {
        self.selected.to_vec()
    }
    fn emit(&mut self, intent: DrawingIntent) {
        self.fx.push(Effect::Emit(intent));
    }
    fn changed(&mut self) {
        self.fx.push(Effect::Changed);
    }
    fn open_settings(&mut self, id: &str, _x: f64, _y: f64) {
        self.fx.push(Effect::OpenSettings(id.to_string()));
    }
    fn snap(
        &self,
        point: DrawingPoint,
        pane_id: &str,
        mode: SnapMode,
        cursor_px: Option<(f64, f64)>,
    ) -> DrawingPoint {
        (self.env.snap)(point, pane_id, mode, cursor_px)
    }
    fn tool_defaults(&self) -> Option<DrawingToolDefaults> {
        self.defaults.cloned()
    }
    fn registry(&self) -> &DrawingRegistry {
        self.registry
    }
}

/// A callout or a plain text label: the drawings whose text is typed straight onto the chart
/// rather than through the settings popup.
pub(super) fn is_inline_editable(d: &dyn Drawing) -> bool {
    matches!(d.type_key(), "callout" | "text")
}

/// Put a drawing's look (style, label, per-type settings) back to a serialized one.
fn restore_look(d: &mut dyn Drawing, doc: &SerializedDrawing) {
    d.base_mut().style = doc.style.clone();
    d.base_mut().text = doc.text.clone();
    if let Some(props) = &doc.props {
        d.apply_props(props);
    }
}

/// `#RRGGBB` and alpha of a CSS color (the kit's `splitColor`).
fn split_color(color: &str) -> (String, f64) {
    let s = color.trim();
    if let Some(hex) = s.strip_prefix('#') {
        let ok = hex.bytes().all(|b| b.is_ascii_hexdigit());
        if ok && (hex.len() == 6 || hex.len() == 8) {
            let alpha = if hex.len() == 8 {
                u8::from_str_radix(&hex[6..8], 16).map_or(1.0, |a| f64::from(a) / 255.0)
            } else {
                1.0
            };
            return (format!("#{}", hex[..6].to_lowercase()), alpha);
        }
        if ok && hex.len() == 3 {
            let x = hex.to_lowercase();
            let c: Vec<char> = x.chars().collect();
            return (format!("#{0}{0}{1}{1}{2}{2}", c[0], c[1], c[2]), 1.0);
        }
    }
    if let Some(rgb) = crate::render::core::color::parse_rgb(s) {
        let alpha = s
            .strip_prefix("rgba(")
            .and_then(|r| r.strip_suffix(')'))
            .and_then(|r| r.split(',').nth(3))
            .and_then(|a| a.trim().parse::<f64>().ok())
            .map_or(1.0, |a| a.clamp(0.0, 1.0));
        let hx = |n: f64| format!("{:02x}", js_round(n.clamp(0.0, 255.0)) as u32);
        return (format!("#{}{}{}", hx(rgb.r), hx(rgb.g), hx(rgb.b)), alpha);
    }
    (crate::palette::ACCENT_BRIGHT.to_string(), 1.0)
}

/// Composite `fg` over `bg` at `alpha`, an opaque `#rrggbb` (the kit's `blendOver`).
fn blend_over(fg: &str, bg: &str, alpha: f64) -> String {
    let rgb = |c: &str| {
        let (hex, _) = split_color(c);
        let n = u32::from_str_radix(&hex[1..], 16).unwrap_or(0);
        [
            f64::from((n >> 16) & 255),
            f64::from((n >> 8) & 255),
            f64::from(n & 255),
        ]
    };
    let (f, b) = (rgb(fg), rgb(bg));
    let a = alpha.clamp(0.0, 1.0);
    let hx = |i: usize| format!("{:02x}", js_round(f[i] * a + b[i] * (1.0 - a)) as u32);
    format!("#{}{}{}", hx(0), hx(1), hx(2))
}

fn theme_str(theme: &Value, key: &str) -> String {
    theme
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

impl Ctl {
    pub fn new() -> Self {
        Self {
            registry: DrawingRegistry::builtin(),
            drawings: Vec::new(),
            sliced: HashSet::new(),
            lifted: HashSet::new(),
            slice_cache: HashMap::new(),
            last_bounds: HashMap::new(),
            selected: Vec::new(),
            hovered: None,
            active_tool: None,
            active_tool_defaults: None,
            tool_templates: Default::default(),
            external_ghost: None,
            last_draft_key: None,
            measure: MeasureOverlay::new(),
            toolbar_visible: false,
            toolbar_collapsed: false,
            mobile_layout: false,
            history_chords: true,
            measure_mode: false,
            eraser_mode: false,
            erasing: false,
            text_editor: None,
            painter: {
                let mut painter = DrawingPainter::new();
                painter.chart_types = Some(std::sync::Arc::new(
                    crate::chart_types::ChartTypeRegistry::builtin(),
                ));
                painter
            },
            interaction: DrawingInteraction::new(),
            popup: None,
            popup_multi: false,
            toolbar_def: Value::Null,
            favorites: Vec::new(),
            shortcuts: Default::default(),
            magnet: SnapMode::Off,
            stay_mode: false,
            gutter_sent: 0.0,
            ctx: None,
            i18n: None,
            fx: Vec::new(),
        }
    }

    pub fn push(&mut self, e: Effect) {
        self.fx.push(e);
    }

    pub fn emit(&mut self, i: DrawingIntent) {
        self.fx.push(Effect::Emit(i));
    }

    pub fn repaint(&mut self) {
        self.fx.push(Effect::Repaint);
    }

    /// The chart's translator text (English until the renderer hands one over).
    pub fn t(&self, message: &crate::i18n::MessageLike) -> String {
        text_of(self.i18n.as_deref(), message, &[])
    }

    // ── interaction plumbing ──

    /// Run `f` on the interaction with its host built from the controller's pieces.
    pub fn with_interaction<R>(
        &mut self,
        env: &Env,
        f: impl FnOnce(&mut DrawingInteraction, &mut dyn InteractionDeps) -> R,
    ) -> R {
        let Ctl {
            interaction,
            drawings,
            hovered,
            selected,
            active_tool,
            active_tool_defaults,
            registry,
            fx,
            ..
        } = self;
        let mut deps = IDeps {
            env,
            drawings,
            hovered: hovered.as_deref(),
            selected,
            tool: active_tool.as_deref(),
            defaults: active_tool_defaults.as_ref(),
            registry,
            fx,
        };
        f(interaction, &mut deps)
    }

    /// The drawings shown on the chart's current timeframe. One limited to other timeframes
    /// ("show on") is neither painted nor clickable here, but stays in the model.
    pub fn on_screen(&self, bar_ms: f64) -> Vec<&dyn Drawing> {
        self.drawings
            .iter()
            .filter(|d| shown_on_timeframe(d.base().show_on.as_deref(), bar_ms))
            .map(|d| d.as_ref())
            .collect()
    }

    fn find(&self, id: &str) -> Option<&dyn Drawing> {
        self.drawings
            .iter()
            .find(|d| d.base().id == id)
            .map(|d| d.as_ref())
    }

    fn find_mut(&mut self, id: &str) -> Option<&mut dyn Drawing> {
        self.drawings
            .iter_mut()
            .find(|d| d.base().id == id)
            .map(|d| d.as_mut())
    }

    fn top_hit(&self, env: &Env, x: f64, y: f64) -> Option<String> {
        let shown = self.on_screen(env.bar_ms);
        top_drawing_at(&shown, x, y, env.proj, HIT_TOLERANCE).map(|d| d.base().id.clone())
    }

    // ── slices ──

    /// Whether the drawing's z puts it INSIDE the series stack (per the last-known
    /// boundaries) rather than over it: its body belongs to an interleave layer.
    pub fn is_interleaved(&self, d: &dyn Drawing) -> bool {
        let bounds = self
            .last_bounds
            .get(&d.base().pane_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        slice_key_for(d.base().z_index, bounds).is_some()
    }

    /// A drawing that paints inside the series stack changed (content, not hover): its
    /// pixels live in the backend composite, so this layer alone can't show the change. Ask
    /// for a data frame, which re-runs `prepare_slices` before the backend composites.
    pub fn invalidate_slices(&mut self) {
        if !self.sliced.is_empty()
            || self
                .drawings
                .iter()
                .any(|d| self.is_interleaved(d.as_ref()))
        {
            self.fx.push(Effect::DataPaint);
        }
    }

    /// A drawing being dragged leaves its interleave slice for the top canvas, so each
    /// pointer move repaints one 2D layer instead of re-rasterizing a plot-sized slice. The
    /// slices rebuild once as the drag starts and once as it ends.
    pub fn sync_lift(&mut self) {
        let moving = self.interaction.moving_ids();
        if moving == self.lifted {
            return;
        }
        self.lifted = moving;
        self.invalidate_slices();
    }

    // ── toolbar mirror ──

    pub fn gutter_px(&self) -> f64 {
        if self.toolbar_visible && !self.mobile_layout {
            if self.toolbar_collapsed {
                TOOLBAR_COLLAPSED_WIDTH
            } else {
                TOOLBAR_WIDTH
            }
        } else {
            0.0
        }
    }

    pub fn toolbar_state(&self) -> ToolbarUiState {
        ToolbarUiState {
            definition: self.toolbar_def.clone(),
            visible: self.toolbar_visible && !self.mobile_layout,
            active_tool: self.active_tool.clone(),
            favorites: self.favorites.clone(),
            shortcuts: self.shortcuts.clone(),
            magnet: self.magnet,
            stay_mode: self.stay_mode,
            measure_active: self.measure_mode,
            eraser_active: self.eraser_mode,
            gutter: self.gutter_px(),
        }
    }

    pub fn push_toolbar(&mut self) {
        let state = self.toolbar_state();
        self.fx.push(Effect::Ui(DrawingsUiEvent::Toolbar(state)));
    }

    /// The gutter follows the bar's current footprint: hidden 0, collapsed a slim strip,
    /// else full width.
    pub fn sync_gutter(&mut self) {
        let px = self.gutter_px();
        if px != self.gutter_sent {
            self.gutter_sent = px;
            self.fx.push(Effect::Gutter(px));
        }
    }

    // ── modes: measure and eraser ──

    fn mode_of(&self) -> Option<DrawingMode> {
        if self.measure_mode {
            Some(DrawingMode::Measure)
        } else if self.eraser_mode {
            Some(DrawingMode::Eraser)
        } else {
            None
        }
    }

    /// Run a state transition and report the mode ONCE if it actually changed: the single
    /// choke point that keeps toggles, mutual exclusions and core pushes from
    /// double-emitting.
    pub fn with_mode_intent(&mut self, f: impl FnOnce(&mut Self)) {
        let before = self.mode_of();
        f(self);
        let after = self.mode_of();
        if after != before {
            self.emit(DrawingIntent::Mode { mode: after });
        }
    }

    /// Toggle the transient ruler. Arming it clears any drawing tool and the selection.
    pub fn toggle_measure(&mut self) {
        if self.measure_mode {
            self.exit_measure(true);
            return;
        }
        if self.eraser_mode {
            self.exit_eraser(); // only one renderer-local mode at a time
        }
        self.measure.clear();
        self.measure_mode = true;
        self.emit(DrawingIntent::Arm { type_key: None }); // clear any armed tool (core-authoritative)
        self.clear_selection();
        self.push_toolbar();
        self.repaint();
    }

    /// Toggle the eraser: click or drag over a drawing to delete it. Mutually exclusive with
    /// the ruler and any armed drawing tool.
    pub fn toggle_eraser(&mut self) {
        if self.eraser_mode {
            self.exit_eraser();
            return;
        }
        if self.measure_mode {
            self.exit_measure(true);
        }
        self.eraser_mode = true;
        self.erasing = false;
        self.emit(DrawingIntent::Arm { type_key: None }); // drop any armed drawing tool
        self.clear_selection();
        self.push_toolbar();
        self.repaint();
    }

    pub fn exit_eraser(&mut self) {
        self.eraser_mode = false;
        self.erasing = false;
        self.push_toolbar();
        self.repaint();
    }

    /// Leave ruler mode. `clear_graphic` false keeps a just-finished measurement on screen.
    pub fn exit_measure(&mut self, clear_graphic: bool) {
        self.measure_mode = false;
        self.interaction.clear_snap_marker();
        if clear_graphic {
            self.measure.clear();
        }
        self.push_toolbar();
        self.repaint();
    }

    // ── selection and the settings popup ──

    /// Mark nothing as edited: close the popup and clear the selection (handles then follow
    /// hover).
    pub fn clear_selection(&mut self) {
        self.close_popup();
        if !self.selected.is_empty() {
            self.emit(DrawingIntent::Select {
                ids: Vec::new(),
                additive: false,
            });
        }
        self.repaint();
    }

    /// Close the popup the controller opened (no dismissal report is owed for it).
    pub fn close_popup(&mut self) {
        if self.popup.take().is_some() {
            self.fx
                .push(Effect::Ui(DrawingsUiEvent::Popup(PopupEvent::Close)));
        }
    }

    fn open_popup(&mut self, ids: Vec<String>, multi: bool, anchor: Option<Bounds>) {
        self.popup = Some(PopupState {
            ids: ids.clone(),
            multi,
            preview_base: None,
        });
        self.fx
            .push(Effect::Ui(DrawingsUiEvent::Popup(PopupEvent::Open(
                PopupRequest { ids, multi, anchor },
            ))));
    }

    pub fn set_selection(&mut self, env: &Env, ids: &[String]) {
        self.selected = ids.to_vec();
        // A multi-selection gets ONE bar for all of its drawings; when it shrinks back to a
        // single drawing that drawing's own bar takes over, and an emptied selection takes
        // the bar away.
        if ids.len() >= 2 {
            self.open_settings_for_selection(env, ids);
        } else if self.popup_multi {
            self.popup_multi = false;
            self.close_popup();
            if let [only] = ids {
                self.open_settings_by_id(env, only);
            }
        }
        self.repaint();
    }

    /// The one bar for a multi-selection: it shows the controls all of its drawings share
    /// and every action applies to all of them. Floats clear of their combined bounds.
    pub fn open_settings_for_selection(&mut self, env: &Env, ids: &[String]) {
        let live: Vec<&dyn Drawing> = ids.iter().filter_map(|id| self.find(id)).collect();
        if live.len() < 2 {
            return;
        }
        let mut anchor: Option<Bounds> = None;
        for d in &live {
            let Some(b) = d.bounds(env.proj) else {
                continue;
            };
            anchor = Some(match anchor {
                None => b,
                Some(a) => {
                    let right = (a.x + a.w).max(b.x + b.w);
                    let bottom = (a.y + a.h).max(b.y + b.h);
                    let x = a.x.min(b.x);
                    let y = a.y.min(b.y);
                    Bounds {
                        x,
                        y,
                        w: right - x,
                        h: bottom - y,
                    }
                }
            });
        }
        let first = live[0].base().id.clone();
        self.popup_multi = true;
        self.emit(DrawingIntent::Settings { id: first });
        self.open_popup(ids.to_vec(), true, anchor);
    }

    /// A click on a drawing opens its settings toolbar (text labels edit text here too).
    pub fn open_settings_by_id(&mut self, env: &Env, id: &str) {
        let Some(drawing) = self.find(id) else {
            return;
        };
        let anchor = drawing.bounds(env.proj); // float the toolbar clear of the drawing
        self.popup_multi = false;
        // Editing this drawing: it stays highlighted while the popup is open.
        self.emit(DrawingIntent::Select {
            ids: vec![id.to_string()],
            additive: false,
        });
        self.emit(DrawingIntent::Settings { id: id.to_string() });
        self.open_popup(vec![id.to_string()], false, anchor);
    }

    fn live_popup(&self) -> Vec<String> {
        self.popup
            .as_ref()
            .map(|p| {
                p.ids
                    .iter()
                    .filter(|id| self.find(id).is_some())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn popup_is_multi(&self) -> bool {
        self.popup.as_ref().is_some_and(|p| p.multi)
    }

    /// Report the live drawings' current state as one edit (one undo step, however many).
    fn emit_edits(&mut self, ids: &[String]) {
        let mut docs: Vec<SerializedDrawing> = ids
            .iter()
            .filter_map(|id| self.find(id).map(|d| d.serialize()))
            .collect();
        match docs.len() {
            0 => {}
            1 => self.emit(DrawingIntent::Edit {
                doc: docs.remove(0),
            }),
            _ => self.emit(DrawingIntent::EditMany { docs }),
        }
    }

    pub fn popup_patch(&mut self, patch: &Props) {
        let ids = self.live_popup();
        for id in &ids {
            if let Some(d) = self.find_mut(id) {
                d.apply_settings(patch);
            }
        }
        self.repaint();
        self.emit_edits(&ids);
    }

    pub fn popup_set_locked(&mut self, locked: bool) {
        let ids = self.live_popup();
        for id in &ids {
            if let Some(d) = self.find_mut(id) {
                d.base_mut().locked = locked;
            }
        }
        self.emit_edits(&ids);
    }

    pub fn popup_reorder(&mut self, to: ReorderTo) {
        for id in self.live_popup() {
            self.emit(DrawingIntent::Reorder { id, to });
        }
    }

    pub fn popup_duplicate(&mut self) {
        let ids = self.live_popup();
        if !self.popup_is_multi() {
            // The copy lands on its source and becomes the selection (same as Ctrl/Cmd+D);
            // typed text is part of what gets copied.
            self.finish_text_editor(true);
        }
        self.close_popup();
        self.emit(DrawingIntent::Duplicate { ids });
    }

    pub fn popup_reset_settings(&mut self, env: &Env) {
        let ids = self.live_popup();
        let multi = self.popup_is_multi();
        let registry = self.registry.clone();
        for id in &ids {
            if let Some(d) = self.find_mut(id) {
                registry.reset_settings(d);
            }
        }
        self.repaint();
        self.emit_edits(&ids);
        // Rebuild the bar so the controls reflect the restored defaults.
        if multi {
            let all = self
                .popup
                .as_ref()
                .map(|p| p.ids.clone())
                .unwrap_or_default();
            self.open_settings_for_selection(env, &all);
        } else if let Some(id) = ids.first() {
            self.open_settings_by_id(env, id);
        }
    }

    pub fn popup_restore(&mut self, doc: &SerializedDrawing) {
        let Some(id) = self.live_popup().into_iter().next() else {
            return;
        };
        let Some(d) = self.find_mut(&id) else { return };
        // A label added since the snapshot is emptied rather than dropped: an edit without
        // text leaves the core's text as it is.
        let text = doc.text.clone().or_else(|| {
            d.base().text.clone().map(|mut t| {
                t.value.clear();
                t
            })
        });
        let mut restored = doc.clone();
        restored.text = text;
        restore_look(d, &restored);
        d.base_mut().anchors = doc.anchors.clone();
        d.base_mut().show_on = doc.show_on.clone();
        self.repaint();
        self.emit(DrawingIntent::Edit { doc: restored });
    }

    pub fn popup_tool_templates(&self) -> Vec<DrawingToolTemplate> {
        self.live_popup()
            .first()
            .and_then(|id| self.find(id))
            .and_then(|d| self.tool_templates.get(d.type_key()))
            .cloned()
            .unwrap_or_default()
    }

    pub fn popup_save_template(&mut self, name: &str) {
        let Some(id) = self.live_popup().into_iter().next() else {
            return;
        };
        if let Some(d) = self.find(&id) {
            let intent = DrawingIntent::TemplateSave {
                type_key: d.type_key().to_string(),
                name: name.to_string(),
                settings: capture_tool_defaults(d),
            };
            self.emit(intent);
        }
    }

    pub fn popup_remove_template(&mut self, name: &str) {
        let Some(id) = self.live_popup().into_iter().next() else {
            return;
        };
        if let Some(d) = self.find(&id) {
            let intent = DrawingIntent::TemplateRemove {
                type_key: d.type_key().to_string(),
                name: name.to_string(),
            };
            self.emit(intent);
        }
    }

    pub fn popup_apply_tool_settings(&mut self, settings: &DrawingToolDefaults) {
        let Some(id) = self.live_popup().into_iter().next() else {
            return;
        };
        let Some(d) = self.find_mut(&id) else { return };
        apply_tool_defaults(d, Some(settings));
        let doc = d.serialize();
        self.repaint();
        self.emit(DrawingIntent::Edit { doc });
    }

    /// A hovered style paints on the drawing without an edit; leaving puts the look back.
    pub fn popup_preview(&mut self, settings: Option<&DrawingToolDefaults>) {
        let Some(id) = self.live_popup().into_iter().next() else {
            return;
        };
        let mut base = self.popup.as_mut().and_then(|p| p.preview_base.take());
        let Some(d) = self.find_mut(&id) else { return };
        match settings {
            Some(settings) => {
                match &base {
                    Some(b) => restore_look(d, b),
                    None => base = Some(d.serialize()),
                }
                apply_tool_defaults(d, Some(settings));
            }
            None => match base.take() {
                Some(b) => restore_look(d, &b),
                None => return,
            },
        }
        if let Some(p) = self.popup.as_mut() {
            p.preview_base = base;
        }
        self.repaint();
    }

    pub fn popup_remove(&mut self) {
        let ids = self.live_popup();
        if self.popup_is_multi() {
            // Locked members are protected: they stay behind (still selected) while the
            // rest go.
            let removable: Vec<String> = ids
                .into_iter()
                .filter(|id| self.find(id).is_some_and(|d| !d.base().locked))
                .collect();
            if removable.is_empty() {
                return;
            }
            self.close_popup();
            self.emit(DrawingIntent::Delete { ids: removable });
        } else if let Some(id) = ids.into_iter().next() {
            // Else the editor floats over the deleted label until it loses focus.
            self.close_text_editor();
            self.close_popup();
            self.emit(DrawingIntent::Delete { ids: vec![id] });
        }
    }

    /// The popup closed itself on a dismissal. Dismiss-on-outside-press also clears the
    /// selection, except when the press is ABOUT the selection: it carries a multi-select
    /// modifier (it will add to the selection or sweep a marquee onto it), or it landed on a
    /// drawing that is already selected (the canvas handler ran first and is holding it for
    /// a drag or a click), so the drawings whose bar just closed must stay selected.
    pub fn popup_dismissed(&mut self, modifier: bool) {
        self.popup = None;
        if modifier {
            return;
        }
        if let Some(pressed) = self.interaction.pressed_id()
            && self.selected.iter().any(|s| s == pressed)
        {
            return;
        }
        self.clear_selection();
    }

    // ── hover, eraser, delete ──

    /// Track which drawing is hovered so its handles appear on hover (and only then). While
    /// Ctrl/Cmd is held the cursor is picking a selection: handles then mark only what IS
    /// selected, so a hovered candidate doesn't already read as selected.
    pub fn update_hover(&mut self, env: &Env, x: f64, y: f64, modifier: bool) {
        let id = if !modifier
            && self.active_tool.is_none()
            && !self.interaction.is_placing()
            && !self.interaction.is_dragging()
        {
            self.top_hit(env, x, y)
        } else {
            None
        };
        if id != self.hovered {
            self.hovered = id;
            self.repaint();
        }
    }

    /// Delete the (unlocked) drawing under the cursor. True when one was removed. Shared by
    /// the eraser (click and drag) and the middle-click shortcut; the latter passes
    /// `with_selection` so a hit on a member of a multi-selection removes the selection's
    /// unlocked members.
    pub fn delete_at(&mut self, env: &Env, x: f64, y: f64, with_selection: bool) -> bool {
        let Some(hit) = self.top_hit(env, x, y) else {
            return false;
        };
        let ids = {
            let all: Vec<&dyn Drawing> = self.drawings.iter().map(|d| d.as_ref()).collect();
            let Some(hit) = all.iter().find(|d| d.base().id == hit) else {
                return false;
            };
            delete_targets(*hit, &self.selected, &all, with_selection)
        };
        if ids.is_empty() {
            return false;
        }
        self.close_popup();
        self.emit(DrawingIntent::Delete { ids });
        true
    }

    /// The topmost visible (unlocked) magnifier whose timeframe chip contains `(x, y)`.
    pub fn magnifier_chip_at(&self, bar_ms: f64, x: f64, y: f64) -> Option<String> {
        let shown = self.on_screen(bar_ms);
        for d in shown.iter().rev() {
            if d.type_key() != "magnifier" || !d.base().visible || d.base().locked {
                continue;
            }
            if let Some(r) = self.painter.chip_rect(&d.base().id)
                && x >= r.x
                && x <= r.x + r.w
                && y >= r.y
                && y <= r.y + r.h
            {
                return Some(d.base().id.clone());
            }
        }
        None
    }

    /// Open the on-chart chip's timeframe menu; the pick patches the drawing like a
    /// settings-popup edit (same intent, same undo step).
    pub fn open_magnifier_chip_menu(&mut self, id: &str) {
        let Some(rect) = self.painter.chip_rect(id) else {
            return;
        };
        let Some(current) = self
            .find(id)
            .and_then(|d| d.as_any().downcast_ref::<Magnifier>())
            .map(|m| m.magnifier.timeframe.clone())
        else {
            return;
        };
        self.fx.push(Effect::Ui(DrawingsUiEvent::MagnifierMenu(
            MagnifierMenuRequest {
                id: id.to_string(),
                rect,
                current,
            },
        )));
    }

    pub fn magnifier_timeframe_picked(&mut self, id: &str, value: &str) {
        let Some(d) = self.find_mut(id) else { return };
        if d.type_key() != "magnifier" {
            return;
        }
        let mut patch = Props::new();
        patch.insert(
            "magnifier.timeframe".into(),
            Value::String(value.to_string()),
        );
        d.apply_settings(&patch);
        let doc = d.serialize();
        self.repaint();
        self.emit(DrawingIntent::Edit { doc });
    }

    // ── the inline text editor ──

    /// The drawing an inline editor may edit, resolved fresh from the current projection (a
    /// core sync replaces the instances, so the editor holds an id rather than the object).
    pub fn edited_drawing(&self, id: &str) -> Option<&dyn Drawing> {
        self.find(id).filter(|d| is_inline_editable(*d))
    }

    /// Open an inline textarea over the drawing's own text so it is typed directly on the
    /// chart (see [`TextEditorSpec`]): a callout fills its bubble, a plain text label sits
    /// exactly where the glyphs paint (the canvas label is muted meanwhile) inside a thin
    /// frame that marks the text as being edited.
    pub fn edit_text_inline(&mut self, proj: &dyn Projector, theme: &Value, id: &str) {
        let Some(d) = self.edited_drawing(id) else {
            return;
        };
        let initial = d
            .base()
            .text
            .as_ref()
            .map(|t| t.value.clone())
            .unwrap_or_default();
        self.close_text_editor();
        let Some(spec) = self
            .edited_drawing(id)
            .and_then(|d| self.inline_editor_spec(d, &initial, proj, theme))
        else {
            return;
        };
        self.text_editor = Some(TextEditor {
            id: id.to_string(),
            initial: initial.clone(),
            stored: initial.clone(),
            value: initial,
        });
        self.fx.push(Effect::Ui(DrawingsUiEvent::TextEditor(
            TextEditorEvent::Open(spec),
        )));
    }

    /// The DOM element's text changed: the canvas bubble and hit box follow the typed text.
    pub fn text_editor_input(&mut self, value: &str) {
        let Some(ed) = self.text_editor.as_mut() else {
            return;
        };
        ed.value = value.to_string();
        let id = ed.id.clone();
        if let Some(d) = self.find_mut(&id) {
            let mut patch = Props::new();
            patch.insert("text.value".into(), Value::String(value.to_string()));
            d.apply_settings(&patch);
        }
        self.repaint();
    }

    /// Hand the edited text to the core (once per actual change) without ending the edit.
    pub fn store_text(&mut self, value: &str) {
        let Some(ed) = self.text_editor.as_ref() else {
            return;
        };
        let id = ed.id.clone();
        let stored = ed.stored.clone();
        let Some(d) = self.find_mut(&id) else { return };
        if !is_inline_editable(d) {
            return;
        }
        let mut patch = Props::new();
        patch.insert("text.value".into(), Value::String(value.to_string()));
        d.apply_settings(&patch);
        if value != stored {
            let doc = d.serialize();
            if let Some(ed) = self.text_editor.as_mut() {
                ed.stored = value.to_string();
            }
            self.emit(DrawingIntent::Edit { doc });
        }
    }

    /// Close the inline editor, keeping (`commit`) or reverting the typed text. A text label
    /// left with nothing in it would paint nothing at all, so it's dropped rather than left
    /// invisible.
    pub fn finish_text_editor(&mut self, commit: bool) {
        let Some(ed) = self.text_editor.as_ref() else {
            return;
        };
        let value = if commit {
            ed.value.clone()
        } else {
            ed.initial.clone()
        };
        let (id, stored) = (ed.id.clone(), ed.stored.clone());
        self.close_text_editor();
        let Some(d) = self.edited_drawing(&id) else {
            return; // deleted from under the editor
        };
        let is_label = d.type_key() == "text";
        if let Some(d) = self.find_mut(&id) {
            let mut patch = Props::new();
            patch.insert("text.value".into(), Value::String(value.clone()));
            d.apply_settings(&patch);
        }
        self.repaint();
        if is_label && value.trim().is_empty() {
            self.clear_selection(); // its settings popup would outlive the drawing
            self.emit(DrawingIntent::Delete { ids: vec![id] });
        } else if value != stored
            && let Some(d) = self.find(&id)
        {
            let doc = d.serialize();
            self.emit(DrawingIntent::Edit { doc });
        }
    }

    pub fn close_text_editor(&mut self) {
        if self.text_editor.take().is_some() {
            // None first: removing a focused textarea fires blur, a re-entrant close.
            self.fx.push(Effect::Ui(DrawingsUiEvent::TextEditor(
                TextEditorEvent::Close,
            )));
            self.repaint();
        }
    }

    /// Absolute position and text metrics for the inline editor, so the typed glyphs land
    /// where the painter would draw them. `None` when the drawing has no on-screen box
    /// (off-pane anchor).
    pub fn inline_editor_spec(
        &self,
        d: &dyn Drawing,
        value: &str,
        proj: &dyn Projector,
        theme: &Value,
    ) -> Option<TextEditorSpec> {
        let base = d.base();
        let text = base.text.as_ref();
        let fs = named_font_size(text.map_or(crate::model::BoxTextSize::Normal, |t| t.size));
        let font = format!(
            "{}{}{}px {}",
            if text.is_some_and(|t| t.italic == Some(true)) {
                "italic "
            } else {
                ""
            },
            if text.is_some_and(|t| t.bold == Some(true)) {
                "bold "
            } else {
                ""
            },
            crate::util::js_number(fs),
            theme_str(theme, "fontFamily"),
        );
        let placeholder = self.t(&msg("drawing.inlineEditor.placeholder", "Enter Text"));
        let ink = text
            .and_then(|t| t.color.clone())
            .unwrap_or_else(|| theme_str(theme, "textColor"));
        let id = base.id.clone();
        if d.type_key() == "callout" {
            let callout = d.as_any().downcast_ref::<CalloutTool>()?;
            let b = callout.pixel_box(proj)?;
            let fill = base
                .style
                .fill_color
                .clone()
                .unwrap_or_else(|| theme_str(theme, "background"));
            let background =
                blend_over(&fill, &theme_str(theme, "background"), split_color(&fill).1);
            return Some(TextEditorSpec {
                id,
                kind: TextEditorKind::Callout,
                x: js_round(b.x),
                y: js_round(b.y),
                width: js_round(b.w),
                height: js_round(b.h),
                text: value.to_string(),
                placeholder,
                font,
                color: ink,
                background,
                border_color: base.style.line_color.clone(),
                border_width: 1.0,
                border_radius: 5.0,
                padding: (5.0, 8.0),
                line_height: None,
                preserve_whitespace: false,
            });
        }
        let anchor = *d.handle_points(proj).first()?;
        let lh = label_line_height(fs);
        let shown = if value.is_empty() {
            &placeholder
        } else {
            value
        };
        let lines: Vec<&str> = shown.split('\n').collect();
        let measured = self.measure_text_width(&lines, &font);
        let w = measured.ceil() + CARET_ROOM;
        Some(TextEditorSpec {
            id,
            kind: TextEditorKind::Label,
            // The painter draws the label at (anchor + 2) with a `top` baseline; a textarea
            // centers each glyph in its line box, so lift it by half the leading to keep
            // both in the same place. The frame then grows outwards around that origin.
            x: js_round(anchor.0 + 2.0) - TEXT_FRAME_INSET,
            y: js_round(anchor.1 + 2.0 - (lh - fs) / 2.0) - TEXT_FRAME_RISE,
            width: w + TEXT_FRAME_INSET * 2.0,
            height: lines.len() as f64 * lh + TEXT_FRAME_RISE * 2.0,
            text: value.to_string(),
            placeholder,
            font,
            color: ink,
            background: "transparent".to_string(),
            border_color: with_alpha(&theme_str(theme, "textColor"), 0.3),
            border_width: EDITOR_BORDER,
            border_radius: 4.0,
            padding: (
                TEXT_FRAME_RISE - EDITOR_BORDER,
                TEXT_FRAME_INSET - EDITOR_BORDER,
            ),
            line_height: Some(lh),
            preserve_whitespace: true,
        })
    }

    /// Widest of `lines` at `font`, measured on the drawings canvas (transform-independent).
    fn measure_text_width(&self, lines: &[&str], font: &str) -> f64 {
        let Some(ctx) = &self.ctx else {
            return 120.0;
        };
        ctx.save();
        ctx.set_font(font);
        let w = lines
            .iter()
            .map(|l| ctx.measure_text(l).width)
            .fold(8.0_f64, f64::max);
        ctx.restore();
        w
    }

    /// Keep an open editor glued to its drawing (typing, panning, zooming all move the anchor).
    pub fn layout_text_editor(&mut self, proj: &dyn Projector, theme: &Value) {
        let Some(ed) = self.text_editor.as_ref() else {
            return;
        };
        let spec = self
            .edited_drawing(&ed.id)
            .and_then(|d| self.inline_editor_spec(d, &ed.value, proj, theme));
        if let Some(spec) = spec {
            self.fx.push(Effect::Ui(DrawingsUiEvent::TextEditor(
                TextEditorEvent::Layout(spec),
            )));
        }
    }

    // ── keyboard ──

    /// Keyboard pre-empt: Escape (popup, placing, selection), undo/redo, copy / paste /
    /// duplicate, delete (multi) and arrow nudge. Stands down while a label text field is
    /// focused (`editing_text`).
    pub fn handle_key(&mut self, env: &Env, e: &DrawingKeyEvent, editing_text: bool) -> bool {
        if e.key == "Escape" {
            if self.text_editor.is_some() {
                self.finish_text_editor(false); // cancel the edit before the popup / selection
                return true;
            }
            if self.popup.is_some() {
                self.clear_selection();
                return true;
            }
            if self.with_interaction(env, |it, deps| it.cancel(deps)) {
                return true;
            }
            if self.measure_mode {
                self.with_mode_intent(|c| c.exit_measure(true));
                return true;
            }
            if !self.selected.is_empty() {
                self.clear_selection();
                return true;
            }
            return false;
        }
        if e.key == "Enter" && self.with_interaction(env, |it, deps| it.finish_placing(deps, false))
        {
            return true; // Enter finishes a polyline
        }
        let ctx = DrawingKeyContext {
            has_selection: !self.selected.is_empty(),
            has_target: !self.selected.is_empty() || self.hovered.is_some(),
            editing_text,
        };
        let Some(action) = key_to_drawing_action(e, &ctx) else {
            return false;
        };
        match action {
            DrawingKeyAction::Undo => {
                if !self.history_chords {
                    return false; // the host's keymap owns Ctrl+Z/Y
                }
                self.emit(DrawingIntent::Undo);
            }
            DrawingKeyAction::Redo => {
                if !self.history_chords {
                    return false;
                }
                self.emit(DrawingIntent::Redo);
            }
            DrawingKeyAction::Copy => {
                let ids = self.selected.clone();
                self.emit(DrawingIntent::Copy { ids });
            }
            DrawingKeyAction::Paste => self.emit(DrawingIntent::Paste),
            DrawingKeyAction::Duplicate => {
                let ids = self.selected.clone();
                self.emit(DrawingIntent::Duplicate { ids });
            }
            DrawingKeyAction::Delete => {
                let ids = if !self.selected.is_empty() {
                    let all: Vec<&dyn Drawing> = self.drawings.iter().map(|d| d.as_ref()).collect();
                    deletable_selection(&self.selected, &all)
                } else {
                    self.hovered.iter().cloned().collect()
                };
                if !ids.is_empty() {
                    self.close_popup();
                    self.emit(DrawingIntent::Delete { ids });
                }
            }
            DrawingKeyAction::Nudge { dx, dy } => self.nudge_selection(env, dx, dy),
        }
        true
    }

    /// Move every selected (unlocked) drawing by a pixel delta: one edit or edit-many, so
    /// one undo step.
    fn nudge_selection(&mut self, env: &Env, dx: f64, dy: f64) {
        let mut docs: Vec<SerializedDrawing> = Vec::new();
        for d in &self.drawings {
            if !self.selected.contains(&d.base().id) || d.base().locked {
                continue;
            }
            let base = d.base();
            let anchors: Vec<DrawingPoint> = base
                .anchors
                .iter()
                .map(|a| match env.proj.y_of(a.price, &base.pane_id) {
                    None => *a,
                    Some(y) => {
                        env.proj
                            .px_to_point(env.proj.x_of(a.time) + dx, y + dy, &base.pane_id)
                    }
                })
                .collect();
            let mut doc = d.serialize();
            doc.anchors = anchors;
            docs.push(doc);
        }
        match docs.len() {
            0 => {}
            1 => self.emit(DrawingIntent::Edit {
                doc: docs.remove(0),
            }),
            _ => self.emit(DrawingIntent::EditMany { docs }),
        }
    }

    // ── placement summary ──

    /// The union of the visible drawings' price ranges on `pane_id` whose time extent
    /// intersects `[from_time, to_time]` (a seam for a future per-drawing autoscale opt-in;
    /// the renderer does not fold it in today).
    pub fn price_range_for_pane(
        &self,
        pane_id: &str,
        from_time: f64,
        to_time: f64,
    ) -> Option<(f64, f64)> {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for d in &self.drawings {
            if !d.base().visible || d.base().pane_id != pane_id {
                continue;
            }
            if let Some(ext) = d.time_extent()
                && (ext.max < from_time || ext.min > to_time)
            {
                continue; // off-screen in time
            }
            let Some(pr) = d.price_range() else { continue };
            if pr.min < lo {
                lo = pr.min;
            }
            if pr.max > hi {
                hi = pr.max;
            }
        }
        (lo <= hi).then_some((lo, hi))
    }
}
