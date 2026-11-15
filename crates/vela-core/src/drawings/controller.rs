//! The core-side owner of the user-drawing model and the tool, selection and
//! preference state around it (`DrawingController` of the original).
//!
//! It holds the [`DrawingStore`] (source of truth) and the undo
//! [`DrawingHistory`], pushes snapshots to the renderer through
//! [`DrawingsRendererPort`], and turns renderer intents into store mutations
//! plus `drawing:*` events. With a renderer that lacks `user_drawings` it is
//! inert for the interactive operations, but persistence, undo and the
//! remembered tool settings keep working.
//!
//! # Differences from the original, by construction
//!
//! - The controller is a shared handle (`Rc` of interior state, `&self`
//!   methods). No interior borrow is held while the renderer port or an event
//!   handler runs, so both may call straight back into the controller.
//! - The store reports change by bumping its revision instead of calling
//!   listeners. Every store mutation here goes through [`DrawingController::mutate`],
//!   which re-syncs the renderer right after it, exactly where the original's
//!   `store.onChange` listener ran (one sync per mutation, before any code
//!   that follows the mutation). After [`destroy`](DrawingController::destroy)
//!   the original has unsubscribed that listener, so nothing syncs.
//! - The intent callback registered on the renderer holds a `Weak` of the
//!   controller state, so the renderer never keeps a destroyed controller alive.
//! - Where the original hands out a live `Drawing`, the methods return a
//!   [`SerializedDrawing`] snapshot taken once the call is complete.
//! - Maps keyed by tool type keep their insertion order internally (it decides
//!   the order of the events a bulk restore emits); the maps handed out are
//!   `BTreeMap`s, as the renderer port takes.
//! - `setActiveTool(.., lastStyle, ..)`: the port takes a full [`DrawingStyle`];
//!   a remembered style is a partial one, so it is merged onto the neutral
//!   default style first (a remembered style captured from a drawing is always
//!   complete, so this only matters for a hand-written restored set).

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::{Value, json};

use crate::events::types::{ChartEvent, ChartEventBus};
use crate::i18n::Translator;
use crate::ports::chart_renderer::{
    ChartRenderer, DrawingIntent, DrawingMode, DrawingsRendererPort, ReorderTo,
};
use crate::runtime::Unsubscribe;

use super::defaults::{
    DrawingToolDefaults, DrawingToolTemplate, apply_tool_defaults, capture_tool_defaults,
    same_tool_defaults, sanitize_tool_defaults, sanitize_tool_templates,
};
use super::document::DrawingsDocument;
use super::drawing::{DrawingInit, Props, SerializedDrawing};
use super::geometry::{DrawingPoint, SnapMode};
use super::history::DrawingHistory;
use super::registry::DrawingRegistry;
use super::series::DrawingSeriesGateway;
use super::store::{DrawingPatch, DrawingStore};
use super::style::{DrawingStyle, DrawingText, PartialDrawingStyle};
use super::toolbar::{DrawingsOption, build_toolbar};

/// The chart's translator, read each time a toolbar is built (it can change
/// language between builds). `None` reads English.
pub type TranslatorSource = Rc<dyn Fn() -> Option<Rc<dyn Translator>>>;

/// A [`TranslatorSource`] with no translator: toolbar labels read English.
pub fn no_translator() -> TranslatorSource {
    Rc::new(|| None)
}

/// Optional seed for a programmatic [`DrawingController::add`].
#[derive(Clone, Debug, Default)]
pub struct AddInit {
    /// Defaults to `price`.
    pub pane_id: Option<String>,
    pub anchors: Option<Vec<DrawingPoint>>,
    pub style: Option<PartialDrawingStyle>,
    pub text: Option<DrawingText>,
    /// Per-type extras (e.g. a glyph stamp's `glyph`, a fib tool's `levels`).
    pub props: Option<Props>,
    /// Draw-order key. Defaults to just under the pane's price on a renderer
    /// with `drawing_depth`: the candles read on top of a fresh drawing; pass an
    /// explicit key for any other slot in the stack.
    pub z_index: Option<f64>,
}

/// A partial record applied to a drawing (the original's
/// `Partial<SerializedDrawing>`): `None` leaves a field alone; the doubly
/// optional fields tell "leave alone" from "clear". A partial `style` merges
/// onto the live style in [`DrawingController::update`] (so a caller does not
/// wipe sibling keys); renderer edit intents replace the style wholesale.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrawingUpdate {
    pub anchors: Option<Vec<DrawingPoint>>,
    pub style: Option<PartialDrawingStyle>,
    /// `Some(None)` removes the text block.
    pub text: Option<Option<DrawingText>>,
    pub pane_id: Option<String>,
    pub locked: Option<bool>,
    pub visible: Option<bool>,
    pub z_index: Option<f64>,
    pub props: Option<Props>,
    /// `Some(None)` lifts a timeframe limit.
    pub show_on: Option<Option<Vec<String>>>,
}

impl DrawingUpdate {
    /// The patch carrying every field of a whole record (what an edit intent
    /// sends). An absent `text` or `props` leaves the drawing's alone, as an
    /// `undefined` does in the original; `showOn` is always present, so an
    /// absent one lifts the limit.
    pub fn from_record(doc: &SerializedDrawing) -> Self {
        Self {
            anchors: Some(doc.anchors.clone()),
            style: Some(doc.style.to_partial()),
            text: doc.text.clone().map(Some),
            pane_id: Some(doc.pane_id.clone()),
            locked: Some(doc.locked),
            visible: Some(doc.visible),
            z_index: Some(doc.z_index),
            props: doc.props.clone(),
            show_on: Some(doc.show_on.clone()),
        }
    }

    fn into_patch(self, style: impl FnOnce(PartialDrawingStyle) -> DrawingStyle) -> DrawingPatch {
        DrawingPatch {
            anchors: self.anchors,
            style: self.style.map(style),
            text: self.text,
            pane_id: self.pane_id,
            locked: self.locked,
            visible: self.visible,
            z_index: self.z_index,
            props: self.props,
            show_on: self.show_on,
        }
    }
}

/// A whole record as a store patch: the style replaces the drawing's.
fn patch_from_record(doc: &SerializedDrawing) -> DrawingPatch {
    DrawingPatch {
        anchors: Some(doc.anchors.clone()),
        style: Some(doc.style.clone()),
        text: doc.text.clone().map(Some),
        pane_id: Some(doc.pane_id.clone()),
        locked: Some(doc.locked),
        visible: Some(doc.visible),
        z_index: Some(doc.z_index),
        props: doc.props.clone(),
        show_on: Some(doc.show_on.clone()),
    }
}

fn ordered_get<'a, V>(entries: &'a [(String, V)], key: &str) -> Option<&'a V> {
    entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn ordered_set<V>(entries: &mut Vec<(String, V)>, key: &str, value: V) {
    match entries.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = value,
        None => entries.push((key.to_string(), value)),
    }
}

fn ordered_remove<V>(entries: &mut Vec<(String, V)>, key: &str) -> bool {
    let before = entries.len();
    entries.retain(|(k, _)| k != key);
    entries.len() != before
}

fn as_map<V: Clone>(entries: &[(String, V)]) -> BTreeMap<String, V> {
    entries.iter().cloned().collect()
}

struct State {
    store: DrawingStore,
    history: DrawingHistory,
    /// The toolbar as last configured: re-read in the chart's language on
    /// [`DrawingController::relocalize`].
    toolbar_option: Option<DrawingsOption>,
    active_tool: Option<String>,
    /// Mirror of the renderer's sticky magnet mode (the renderer default is `off`).
    snap_mode: SnapMode,
    /// When on, finishing a drawing leaves the tool armed (instead of one-shot disarm).
    stay_in_drawing_mode: bool,
    /// Mirror of the renderer-local mode (measure / eraser / none).
    mode: Option<DrawingMode>,
    /// FAVORITE tool types (insertion-ordered): user prefs, not document data.
    favs: Vec<String>,
    /// Ordered; `[0]` is the primary (settings-popup) selection.
    selected_ids: Vec<String>,
    /// In-memory copy buffer (per chart).
    clipboard: Vec<SerializedDrawing>,
    /// Per-tool remembered settings: the last drawing of each type the user
    /// styled seeds the next.
    defaults: Vec<(String, DrawingToolDefaults)>,
    /// Per-tool saved looks ("styles"), in the order they were saved.
    templates: Vec<(String, Vec<DrawingToolTemplate>)>,
    /// The renderer-intent subscription, until `destroy`.
    intent_subscription: Option<Unsubscribe>,
}

struct Inner {
    port: Option<Rc<dyn DrawingsRendererPort>>,
    enabled: bool,
    events: ChartEventBus,
    registry: Rc<DrawingRegistry>,
    i18n: TranslatorSource,
    destroyed: Cell<bool>,
    state: RefCell<State>,
}

/// Renderer-agnostic owner of the user-drawing model and the tool / selection
/// state. A cheap-to-clone shared handle.
#[derive(Clone)]
pub struct DrawingController {
    inner: Rc<Inner>,
}

impl DrawingController {
    /// Build the controller. When the renderer has the `user_drawings`
    /// capability AND a drawing port, the toolbar, its visibility and the
    /// series gateway are pushed to it and its intents are subscribed;
    /// otherwise the controller stays headless.
    pub fn new(
        renderer: Rc<dyn ChartRenderer>,
        events: ChartEventBus,
        registry: Rc<DrawingRegistry>,
        option: Option<DrawingsOption>,
        series_gateway: Option<Rc<dyn DrawingSeriesGateway>>,
        i18n: TranslatorSource,
    ) -> Self {
        let port = if renderer.capabilities().user_drawings {
            renderer.user_drawings_port()
        } else {
            None
        };
        let inner = Rc::new(Inner {
            enabled: port.is_some(),
            port,
            events,
            registry,
            i18n,
            destroyed: Cell::new(false),
            state: RefCell::new(State {
                store: DrawingStore::new(),
                history: DrawingHistory::default(),
                toolbar_option: option.clone(),
                active_tool: None,
                snap_mode: SnapMode::Off,
                stay_in_drawing_mode: false,
                mode: None,
                favs: Vec::new(),
                selected_ids: Vec::new(),
                clipboard: Vec::new(),
                defaults: Vec::new(),
                templates: Vec::new(),
                intent_subscription: None,
            }),
        });
        let controller = Self { inner };
        if let Some(port) = controller.inner.port.clone() {
            let (definition, visible) = controller.toolbar_value(option.as_ref());
            port.set_toolbar(&definition);
            port.show_toolbar(visible);
            if let Some(gateway) = series_gateway {
                port.set_series_gateway(gateway);
            }
            let weak = Rc::downgrade(&controller.inner);
            let unsubscribe = port.on_drawing_intent(Box::new(move |intent| {
                if let Some(inner) = weak.upgrade() {
                    DrawingController { inner }.on_intent(intent);
                }
            }));
            controller.inner.state.borrow_mut().intent_subscription = Some(unsubscribe);
        }
        controller
    }

    /// Whether the active renderer supports interactive drawings.
    pub fn supported(&self) -> bool {
        self.inner.enabled
    }

    // ── tool / toolbar control ──

    /// Arm a tool (`None` = back to the pointer). Inert without a port.
    pub fn set_tool(&self, type_key: Option<&str>) {
        let Some(port) = self.port() else { return };
        let (changed, defaults) = {
            let mut st = self.inner.state.borrow_mut();
            let next = type_key.map(str::to_string);
            let changed = next != st.active_tool;
            st.active_tool = next;
            let defaults = type_key.and_then(|t| ordered_get(&st.defaults, t).cloned());
            (changed, defaults)
        };
        // Seed the renderer's placement preview with the tool's remembered settings so the
        // ghost matches what will be committed (the `create` intent re-applies them too).
        let style = defaults
            .as_ref()
            .and_then(|d| d.style.as_ref())
            .map(|s| DrawingStyle::default().merged(s));
        port.set_active_tool(type_key, style.as_ref(), defaults.as_ref());
        // Announce every ACTUAL change (arm, one-shot tool-finished, or programmatic) so
        // external toolbars (a workspace's shared bar) reflect the armed tool.
        if changed {
            self.emit(ChartEvent::DrawingTool {
                type_key: type_key.map(str::to_string),
            });
        }
    }

    pub fn get_tool(&self) -> Option<String> {
        self.inner.state.borrow().active_tool.clone()
    }

    // ── magnet + renderer-local modes (measure / eraser) ──
    // The renderer owns the states and their mutual exclusion; the core holds a MIRROR
    // driven from both ends: facade setters push port commands, in-chart toolbar clicks
    // arrive as intents. Equal values no-op on either path, which keeps the
    // command/intent echo loop convergent.

    pub fn get_snap_mode(&self) -> SnapMode {
        self.inner.state.borrow().snap_mode
    }

    pub fn set_snap_mode(&self, mode: SnapMode) {
        let Some(port) = self.port() else { return };
        {
            let mut st = self.inner.state.borrow_mut();
            if mode == st.snap_mode {
                return;
            }
            st.snap_mode = mode;
        }
        port.set_snap_mode(mode);
        self.emit(ChartEvent::DrawingSnap { mode });
    }

    pub fn get_stay_mode(&self) -> bool {
        self.inner.state.borrow().stay_in_drawing_mode
    }

    pub fn set_stay_mode(&self, on: bool) {
        let Some(port) = self.port() else { return };
        {
            let mut st = self.inner.state.borrow_mut();
            if on == st.stay_in_drawing_mode {
                return;
            }
            st.stay_in_drawing_mode = on;
        }
        port.set_stay_mode(on);
        self.emit(ChartEvent::DrawingStay { on });
    }

    pub fn get_mode(&self) -> Option<DrawingMode> {
        self.inner.state.borrow().mode
    }

    pub fn set_mode(&self, mode: Option<DrawingMode>) {
        let Some(port) = self.port() else { return };
        {
            let mut st = self.inner.state.borrow_mut();
            if mode == st.mode {
                return;
            }
            st.mode = mode;
        }
        port.set_mode(mode);
        self.emit(ChartEvent::DrawingMode { mode });
    }

    pub fn show_toolbar(&self, visible: bool) {
        if let Some(port) = self.port() {
            port.show_toolbar(visible);
        }
    }

    /// The favorite tool types, in the order they were starred.
    pub fn favorites(&self) -> Vec<String> {
        self.inner.state.borrow().favs.clone()
    }

    pub fn is_favorite(&self, type_key: &str) -> bool {
        self.inner.state.borrow().favs.iter().any(|t| t == type_key)
    }

    /// Star/unstar one tool. Unknown types are ignored; no-ops don't emit.
    pub fn set_favorite(&self, type_key: &str, on: bool) {
        if self.inner.registry.get(type_key).is_none() {
            return;
        }
        {
            let mut st = self.inner.state.borrow_mut();
            let has = st.favs.iter().any(|t| t == type_key);
            if on == has {
                return;
            }
            if on {
                st.favs.push(type_key.to_string());
            } else {
                st.favs.retain(|t| t != type_key);
            }
        }
        self.push_favorites();
    }

    /// Replace the whole favorite set (bulk restore): unknown types are dropped.
    pub fn set_favorites(&self, types: &[String]) {
        let next: Vec<String> = types
            .iter()
            .filter(|t| self.inner.registry.get(t).is_some())
            .cloned()
            .collect();
        {
            let mut st = self.inner.state.borrow_mut();
            if next.len() == st.favs.len() && next.iter().all(|t| st.favs.contains(t)) {
                return;
            }
            let mut unique: Vec<String> = Vec::new();
            for t in next {
                if !unique.contains(&t) {
                    unique.push(t);
                }
            }
            st.favs = unique;
        }
        self.push_favorites();
    }

    fn push_favorites(&self) {
        let list = self.favorites();
        if let Some(port) = self.port() {
            port.set_favorites(&list);
        }
        self.emit(ChartEvent::DrawingFavorites { favorites: list });
    }

    /// Every tool's remembered settings, by type (plain JSON: persist it as-is).
    pub fn tool_defaults(&self) -> BTreeMap<String, DrawingToolDefaults> {
        as_map(&self.inner.state.borrow().defaults)
    }

    /// Replace every tool's remembered settings (restoring persisted prefs).
    /// `map` is an untrusted JSON object by type key: unknown types and malformed
    /// entries are dropped; an unchanged set is a no-op and emits nothing.
    pub fn set_tool_defaults(&self, map: &Value) {
        let mut next: Vec<(String, DrawingToolDefaults)> = Vec::new();
        if let Some(entries) = map.as_object() {
            for (type_key, raw) in entries {
                if self.inner.registry.get(type_key).is_none() {
                    continue;
                }
                if let Some(d) = sanitize_tool_defaults(raw) {
                    ordered_set(&mut next, type_key, d);
                }
            }
        }
        let (changed, active) = {
            let mut st = self.inner.state.borrow_mut();
            let mut keys: Vec<String> = st.defaults.iter().map(|(k, _)| k.clone()).collect();
            for (k, _) in &next {
                if !keys.contains(k) {
                    keys.push(k.clone());
                }
            }
            keys.retain(|k| {
                !same_tool_defaults(ordered_get(&st.defaults, k), ordered_get(&next, k))
            });
            if keys.is_empty() {
                return;
            }
            st.defaults = next;
            (keys, st.active_tool.clone())
        };
        // Re-seed the placement preview.
        if let Some(active) = active.filter(|a| changed.contains(a)) {
            self.set_tool(Some(&active));
        }
        for type_key in changed {
            self.emit(ChartEvent::DrawingDefaults { type_key });
        }
    }

    /// Forget a tool's remembered settings (every tool's when `type_key` is
    /// `None`): new drawings start from the factory defaults again.
    pub fn reset_tool_defaults(&self, type_key: Option<&str>) {
        let types: Vec<String> = {
            let st = self.inner.state.borrow();
            match type_key {
                Some(t) if ordered_get(&st.defaults, t).is_some() => vec![t.to_string()],
                Some(_) => Vec::new(),
                None => st.defaults.iter().map(|(k, _)| k.clone()).collect(),
            }
        };
        for t in types {
            ordered_remove(&mut self.inner.state.borrow_mut().defaults, &t);
            self.emit(ChartEvent::DrawingDefaults { type_key: t });
        }
    }

    /// Every tool's saved looks, by type (persist it as-is).
    pub fn tool_templates(&self) -> BTreeMap<String, Vec<DrawingToolTemplate>> {
        as_map(&self.inner.state.borrow().templates)
    }

    /// Replace every tool's saved looks (restoring persisted prefs). `map` is an
    /// untrusted JSON object by type key: unknown types and malformed entries are
    /// dropped; an unchanged set is a no-op and emits nothing.
    pub fn set_tool_templates(&self, map: &Value) {
        let mut next: Vec<(String, Vec<DrawingToolTemplate>)> = Vec::new();
        if let Some(entries) = map.as_object() {
            for (type_key, raw) in entries {
                if self.inner.registry.get(type_key).is_none() {
                    continue;
                }
                let list = sanitize_tool_templates(raw);
                if !list.is_empty() {
                    ordered_set(&mut next, type_key, list);
                }
            }
        }
        let changed = {
            let mut st = self.inner.state.borrow_mut();
            let mut keys: Vec<String> = st.templates.iter().map(|(k, _)| k.clone()).collect();
            for (k, _) in &next {
                if !keys.contains(k) {
                    keys.push(k.clone());
                }
            }
            let empty: Vec<DrawingToolTemplate> = Vec::new();
            keys.retain(|k| {
                ordered_get(&st.templates, k).unwrap_or(&empty)
                    != ordered_get(&next, k).unwrap_or(&empty)
            });
            if keys.is_empty() {
                return;
            }
            st.templates = next;
            keys
        };
        self.push_templates();
        for type_key in changed {
            self.emit(ChartEvent::DrawingTemplates { type_key });
        }
    }

    /// Save the given settings as a named look for a tool (a same-named one is replaced).
    pub fn save_tool_template(&self, type_key: &str, name: &str, settings: &DrawingToolDefaults) {
        if self.inner.registry.get(type_key).is_none() {
            return;
        }
        let raw = json!([{ "name": name, "settings": serde_json::to_value(settings).unwrap_or(Value::Null) }]);
        let Some(entry) = sanitize_tool_templates(&raw).into_iter().next() else {
            return;
        };
        {
            let mut st = self.inner.state.borrow_mut();
            let mut list: Vec<DrawingToolTemplate> = ordered_get(&st.templates, type_key)
                .cloned()
                .unwrap_or_default();
            list.retain(|t| t.name != entry.name);
            list.push(entry);
            ordered_set(&mut st.templates, type_key, list);
        }
        self.push_templates();
        self.emit(ChartEvent::DrawingTemplates {
            type_key: type_key.to_string(),
        });
    }

    /// Delete a tool's saved look by name.
    pub fn remove_tool_template(&self, type_key: &str, name: &str) {
        {
            let mut st = self.inner.state.borrow_mut();
            let Some(list) = ordered_get(&st.templates, type_key) else {
                return;
            };
            if !list.iter().any(|t| t.name == name) {
                return;
            }
            let rest: Vec<DrawingToolTemplate> =
                list.iter().filter(|t| t.name != name).cloned().collect();
            if rest.is_empty() {
                ordered_remove(&mut st.templates, type_key);
            } else {
                ordered_set(&mut st.templates, type_key, rest);
            }
        }
        self.push_templates();
        self.emit(ChartEvent::DrawingTemplates {
            type_key: type_key.to_string(),
        });
    }

    fn push_templates(&self) {
        if let Some(port) = self.port() {
            port.set_tool_templates(&self.tool_templates());
        }
    }

    /// Reconfigure the toolbar groups / tools live.
    pub fn set_toolbar(&self, option: DrawingsOption) {
        let Some(port) = self.port() else {
            self.inner.state.borrow_mut().toolbar_option = Some(option);
            return;
        };
        let (definition, _) = self.toolbar_value(Some(&option));
        self.inner.state.borrow_mut().toolbar_option = Some(option);
        port.set_toolbar(&definition);
    }

    /// The chart's language changed: hand the renderer the toolbar relabelled in it.
    pub fn relocalize(&self) {
        let Some(port) = self.port() else { return };
        let option = self.inner.state.borrow().toolbar_option.clone();
        let (definition, _) = self.toolbar_value(option.as_ref());
        port.set_toolbar(&definition);
    }

    /// Display (or clear) another chart's in-progress placement as a ghost: silently
    /// inert on a renderer without the optional seam.
    pub fn set_external_ghost(&self, doc: Option<&SerializedDrawing>) {
        if let Some(port) = self.port() {
            port.set_external_ghost(doc);
        }
    }

    /// Push per-tool shortcut hints (pre-formatted display strings) to the toolbar flyouts.
    pub fn set_tool_shortcuts(&self, map: &BTreeMap<String, String>) {
        if let Some(port) = self.port() {
            port.set_tool_shortcuts(map);
        }
    }

    // ── programmatic CRUD (facade-facing): each is one undo step ──

    /// Create a drawing programmatically. `None` without renderer support or for
    /// an unknown type. Returns a snapshot of the stored drawing.
    pub fn add(&self, type_key: &str, init: AddInit) -> Option<SerializedDrawing> {
        if !self.inner.enabled {
            return None;
        }
        let defaults = ordered_get(&self.inner.state.borrow().defaults, type_key).cloned();
        let pane_id = init.pane_id.clone().unwrap_or_else(|| "price".to_string());
        let id = self.inner.state.borrow_mut().store.next_id();
        let z_index = init.z_index.or_else(|| self.start_z(type_key, &pane_id));
        let mut d = self.inner.registry.create(
            type_key,
            DrawingInit {
                id: Some(id),
                pane_id,
                anchors: init.anchors.clone().unwrap_or_default(),
                text: init.text.clone(),
                z_index,
                ..Default::default()
            },
        )?;
        // The tool's remembered settings first, then whatever the caller passed explicitly.
        apply_tool_defaults(d.as_mut(), defaults.as_ref());
        if let Some(style) = &init.style {
            let base = d.base_mut();
            base.style = base.style.merged(style);
        }
        if let Some(text) = &init.text {
            let base = d.base_mut();
            base.text = Some(match base.text.take() {
                Some(mut current) => {
                    current.value = text.value.clone();
                    current.size = text.size;
                    current.h_align = text.h_align;
                    current.v_align = text.v_align;
                    current.color = text.color.clone().or(current.color);
                    current.bold = text.bold.or(current.bold);
                    current.italic = text.italic.or(current.italic);
                    current.place = text.place.or(current.place);
                    current.side = text.side.or(current.side);
                    current
                }
                None => text.clone(),
            });
        }
        if let Some(props) = &init.props {
            d.apply_props(props);
        }
        let id = d.base().id.clone();
        self.mutate(|st| {
            let before = st.store.serialize();
            st.history.record(&before);
            st.store.add(d);
        });
        self.capture_defaults(&id);
        self.emit(ChartEvent::DrawingCreated { id: id.clone() });
        self.snapshot_of(&id)
    }

    pub fn remove(&self, id: &str) {
        self.delete_many(&[id.to_string()]);
    }

    /// Apply a partial record to a drawing (headless write-back from a custom UI).
    pub fn update(&self, id: &str, patch: DrawingUpdate) {
        let before = self.document();
        // Public callers may send a partial style; merge onto the live style so they
        // don't wipe sibling keys. Edit intents write a full style and go straight to
        // the store (which replaces wholesale: needed for settings reset).
        let live_style = {
            let st = self.inner.state.borrow();
            st.store.get(id).map(|d| d.base().style.clone())
        };
        let patch = patch.into_patch(|partial| match &live_style {
            Some(live) => live.merged(&partial),
            None => DrawingStyle::default().merged(&partial),
        });
        if self.update_drawing(id, patch) {
            self.mutate(|st| st.history.record(&before));
            self.emit(ChartEvent::DrawingEdited { id: id.to_string() });
        }
    }

    /// Apply several partial records as ONE undo step (e.g. hiding / locking /
    /// reordering a group). A partial `style` replaces the drawing's style, as
    /// every non-`update` path does (missing line fields take the neutral
    /// default).
    pub fn update_many(&self, patches: Vec<(String, DrawingUpdate)>) {
        self.begin_transaction();
        for (id, patch) in patches {
            let patch = patch.into_patch(|partial| DrawingStyle::default().merged(&partial));
            if self.update_drawing(&id, patch) {
                self.inner.state.borrow_mut().history.mark_dirty();
                self.emit(ChartEvent::DrawingEdited { id });
            }
        }
        self.inner.state.borrow_mut().history.commit();
    }

    /// Delete several drawings as ONE undo step.
    pub fn remove_many(&self, ids: &[String]) {
        self.delete_many(ids);
    }

    pub fn set_locked(&self, id: &str, locked: bool) {
        self.mutate(|st| {
            let before = st.store.serialize();
            st.history.record(&before);
            st.store.set_locked(id, locked);
        });
        self.emit(ChartEvent::DrawingEdited { id: id.to_string() });
    }

    pub fn set_visible(&self, id: &str, visible: bool) {
        self.mutate(|st| {
            let before = st.store.serialize();
            st.history.record(&before);
            st.store.set_visible(id, visible);
        });
        self.emit(ChartEvent::DrawingEdited { id: id.to_string() });
    }

    pub fn bring_to_front(&self, id: &str) {
        self.record_snapshot();
        let pane = self.pane_of(id);
        let floor = self.series_top_z(pane.as_deref());
        self.mutate(|st| st.store.bring_to_front(id, floor));
        self.emit(ChartEvent::DrawingEdited { id: id.to_string() });
    }

    pub fn send_to_back(&self, id: &str) {
        self.record_snapshot();
        let pane = self.pane_of(id);
        let ceil = self.series_bottom_z(pane.as_deref());
        self.mutate(|st| st.store.send_to_back(id, ceil));
        self.emit(ChartEvent::DrawingEdited { id: id.to_string() });
    }

    /// The top of the pane's series stack: what a front drawing has to clear. 0 when
    /// the renderer keeps drawings on their own layer (no shared z space).
    fn series_top_z(&self, pane_id: Option<&str>) -> f64 {
        pane_id
            .and_then(|pane| self.port()?.stack_range(pane))
            .map_or(0.0, |range| range.front)
    }

    /// The bottom of the pane's series stack: what a backmost drawing has to undercut.
    fn series_bottom_z(&self, pane_id: Option<&str>) -> f64 {
        pane_id
            .and_then(|pane| self.port()?.stack_range(pane))
            .map_or(1.0, |range| range.back)
    }

    /// Where a NEW drawing starts: just under the pane's price so the candles read on
    /// top of it (falling back to just under the pane's top series where there is no
    /// price, a study pane). Half a key down never ties a series; drawings tying each
    /// other paint in insertion order, so consecutive new drawings still stack
    /// newest-in-front. `None` without a shared z space: the store then places it over
    /// the other drawings, its own layer's top. A type that COVERS the series (an
    /// opaque inset) instead starts just above the whole stack, because under the
    /// candles its content would be buried.
    fn start_z(&self, type_key: &str, pane_id: &str) -> Option<f64> {
        let range = self.port()?.stack_range(pane_id)?;
        if self
            .inner
            .registry
            .get(type_key)
            .is_some_and(|meta| meta.covers_series)
        {
            return Some(range.front + 0.5);
        }
        Some(range.price.unwrap_or(range.front) - 0.5)
    }

    /// Programmatically select drawings (host UI to chart): shows the on-chart
    /// handles + toolbar. `additive` toggles membership (matching shift-click)
    /// instead of replacing.
    pub fn select(&self, ids: &[String], additive: bool) {
        self.set_selection(ids, additive);
    }

    /// Open a drawing's on-chart settings popup (and select it): a click on it,
    /// driven from a host UI.
    pub fn open_settings(&self, id: &str) {
        if let Some(port) = self.port() {
            port.open_settings(id);
        }
    }

    // ── undo / redo (core-owned: works even without renderer support) ──

    pub fn undo(&self) {
        let next = {
            let mut st = self.inner.state.borrow_mut();
            let current = st.store.serialize();
            st.history.undo(&current)
        };
        if let Some(doc) = next {
            self.restore_snapshot(&doc);
        }
    }

    pub fn redo(&self) {
        let next = {
            let mut st = self.inner.state.borrow_mut();
            let current = st.store.serialize();
            st.history.redo(&current)
        };
        if let Some(doc) = next {
            self.restore_snapshot(&doc);
        }
    }

    pub fn can_undo(&self) -> bool {
        self.inner.state.borrow().history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.inner.state.borrow().history.can_redo()
    }

    // ── clone / clipboard ──

    /// Duplicate drawings in place (clones land on the source, auto-selected: "duplicate
    /// then drag").
    pub fn duplicate(&self, ids: &[String]) -> Vec<SerializedDrawing> {
        if !self.inner.enabled {
            return Vec::new();
        }
        let sources = self.resolve(ids);
        if sources.is_empty() {
            return Vec::new();
        }
        self.insert_clones(&sources)
    }

    pub fn clone_drawing(&self, id: &str) -> Option<SerializedDrawing> {
        self.duplicate(&[id.to_string()]).into_iter().next()
    }

    /// Copy drawings into the in-memory clipboard (no model change, no event).
    pub fn copy(&self, ids: &[String]) {
        let docs = self.resolve(ids);
        if !docs.is_empty() {
            self.inner.state.borrow_mut().clipboard = docs;
        }
    }

    pub fn paste(&self) -> Vec<SerializedDrawing> {
        if !self.inner.enabled {
            return Vec::new();
        }
        let clipboard = self.inner.state.borrow().clipboard.clone();
        if clipboard.is_empty() {
            return Vec::new();
        }
        self.insert_clones(&clipboard)
    }

    /// Every drawing as plain JSON, in paint order.
    pub fn all(&self) -> Vec<SerializedDrawing> {
        self.document().drawings
    }

    // ── persistence (works regardless of renderer support) ──

    pub fn to_json(&self) -> DrawingsDocument {
        self.document()
    }

    /// Restore from an untrusted document (`migrate` drops what it cannot read).
    pub fn from_json(&self, doc: &Value) {
        // A new document is a context switch: don't undo into the old one. The
        // selection is cleared silently (the original announces nothing here).
        {
            let mut st = self.inner.state.borrow_mut();
            st.history.clear();
            st.selected_ids.clear();
        }
        let registry = Rc::clone(&self.inner.registry);
        self.mutate(|st| st.store.load(&registry, doc));
    }

    /// Detach from the renderer: the intent subscription ends and the store stops
    /// syncing.
    pub fn destroy(&self) {
        self.inner.destroyed.set(true);
        let subscription = self.inner.state.borrow_mut().intent_subscription.take();
        if let Some(unsubscribe) = subscription {
            unsubscribe();
        }
    }

    // ── internals ──

    fn port(&self) -> Option<Rc<dyn DrawingsRendererPort>> {
        self.inner.port.clone()
    }

    fn emit(&self, event: ChartEvent) {
        self.inner.events.emit(&event.name(), &event);
    }

    fn document(&self) -> DrawingsDocument {
        self.inner.state.borrow().store.serialize()
    }

    fn snapshot_of(&self, id: &str) -> Option<SerializedDrawing> {
        self.inner
            .state
            .borrow()
            .store
            .get(id)
            .map(|d| d.serialize())
    }

    fn pane_of(&self, id: &str) -> Option<String> {
        self.inner
            .state
            .borrow()
            .store
            .get(id)
            .map(|d| d.base().pane_id.clone())
    }

    fn record_snapshot(&self) {
        self.mutate(|st| {
            let before = st.store.serialize();
            st.history.record(&before);
        });
    }

    fn begin_transaction(&self) {
        let mut st = self.inner.state.borrow_mut();
        let before = st.store.serialize();
        st.history.begin(&before);
    }

    /// Run one store mutation and re-sync the renderer if the store changed: the
    /// original's `store.onChange(() => sync())`. `f` must perform at most one
    /// store mutation, so syncs stay one per mutation.
    fn mutate<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let (result, changed) = {
            let mut st = self.inner.state.borrow_mut();
            let before = st.store.revision();
            let result = f(&mut st);
            (result, st.store.revision() != before)
        };
        if changed {
            self.sync();
        }
        result
    }

    fn sync(&self) {
        if self.inner.destroyed.get() {
            return;
        }
        if let Some(port) = self.port() {
            let docs = self.document().drawings;
            port.sync_drawings(&docs);
        }
    }

    /// The toolbar the renderer paints, read in the chart's language, plus its
    /// initial visibility.
    fn toolbar_value(&self, option: Option<&DrawingsOption>) -> (Value, bool) {
        let translator = (self.inner.i18n)();
        let (definition, visible) =
            build_toolbar(&self.inner.registry, option, translator.as_deref());
        (
            serde_json::to_value(&definition).unwrap_or(Value::Null),
            visible,
        )
    }

    /// Set/extend the selection. `additive` toggles membership (shift-click) vs replacing it.
    fn set_selection(&self, ids: &[String], additive: bool) {
        {
            let mut st = self.inner.state.borrow_mut();
            let next: Vec<String> = if additive {
                let mut next = st.selected_ids.clone();
                for id in ids {
                    match next.iter().position(|s| s == id) {
                        Some(at) => {
                            next.remove(at);
                        }
                        None => next.push(id.clone()),
                    }
                }
                next
            } else {
                ids.to_vec()
            };
            st.selected_ids = next.into_iter().filter(|id| st.store.has(id)).collect();
        }
        self.announce_selection();
    }

    /// Push the selection to the renderer and announce it (primary + the full list).
    fn announce_selection(&self) {
        let ids = self.inner.state.borrow().selected_ids.clone();
        if let Some(port) = self.port() {
            port.set_selection(&ids);
        }
        self.emit(ChartEvent::DrawingSelected {
            id: ids.first().cloned(),
            ids,
        });
    }

    /// Restore a history snapshot, reconciling selection against what survived.
    fn restore_snapshot(&self, doc: &DrawingsDocument) {
        let registry = Rc::clone(&self.inner.registry);
        self.mutate(|st| st.store.load_document(&registry, doc));
        {
            let mut st = self.inner.state.borrow_mut();
            let survivors: Vec<String> = st
                .selected_ids
                .iter()
                .filter(|id| st.store.has(id))
                .cloned()
                .collect();
            st.selected_ids = survivors;
        }
        self.announce_selection();
    }

    /// Delete drawings as one undo step; prune them from the selection.
    fn delete_many(&self, ids: &[String]) {
        self.begin_transaction();
        let mut removed: Vec<String> = Vec::new();
        for id in ids {
            if self.mutate(|st| st.store.remove(id)) {
                removed.push(id.clone());
            }
            self.inner.state.borrow_mut().history.mark_dirty();
        }
        self.inner.state.borrow_mut().history.commit();
        if removed.is_empty() {
            return;
        }
        let (survivors, pruned) = {
            let st = self.inner.state.borrow();
            let survivors: Vec<String> = st
                .selected_ids
                .iter()
                .filter(|id| st.store.has(id))
                .cloned()
                .collect();
            let pruned = survivors.len() != st.selected_ids.len();
            (survivors, pruned)
        };
        if pruned {
            self.set_selection(&survivors, false);
        }
        for id in removed {
            self.emit(ChartEvent::DrawingRemoved { id });
        }
    }

    /// Add clones (fresh ids, fresh mount-order z) as one undo step + select them.
    fn insert_clones(&self, docs: &[SerializedDrawing]) -> Vec<SerializedDrawing> {
        self.begin_transaction();
        let mut ids: Vec<String> = Vec::new();
        for doc in docs {
            // A copy keeps its source's DEPTH: the clone lands at the same z and, tying it,
            // paints just in front of it ("duplicate then drag" without a jump up the stack).
            let id = self.inner.state.borrow_mut().store.next_id();
            let record = SerializedDrawing {
                id: id.clone(),
                ..doc.clone()
            };
            let Some(d) = self.inner.registry.deserialize(&record) else {
                continue;
            };
            self.mutate(|st| {
                st.store.add(d);
            });
            self.inner.state.borrow_mut().history.mark_dirty();
            ids.push(id);
        }
        self.inner.state.borrow_mut().history.commit();
        if !ids.is_empty() {
            self.set_selection(&ids, false);
            for id in &ids {
                self.emit(ChartEvent::DrawingCreated { id: id.clone() });
            }
        }
        ids.iter().filter_map(|id| self.snapshot_of(id)).collect()
    }

    fn resolve(&self, ids: &[String]) -> Vec<SerializedDrawing> {
        let st = self.inner.state.borrow();
        ids.iter()
            .filter_map(|id| st.store.get(id))
            .map(|d| d.serialize())
            .collect()
    }

    /// `store.update` that remembers the drawing's settings only when the patch changed
    /// them: moving an older drawing must not hand its look back to the tool.
    fn update_drawing(&self, id: &str, patch: DrawingPatch) -> bool {
        let before = {
            let st = self.inner.state.borrow();
            st.store.get(id).map(capture_tool_defaults)
        };
        if !self.mutate(|st| st.store.update(id, patch).is_some()) {
            return false;
        }
        let after = {
            let st = self.inner.state.borrow();
            st.store.get(id).map(capture_tool_defaults)
        };
        if after.is_some() && !same_tool_defaults(before.as_ref(), after.as_ref()) {
            self.capture_defaults(id);
        }
        true
    }

    /// Remember a drawing's settings as its tool's defaults (they seed the next one),
    /// and announce a real change so a host can save it.
    fn capture_defaults(&self, id: &str) {
        let type_key = {
            let mut st = self.inner.state.borrow_mut();
            let Some(d) = st.store.get(id) else { return };
            let type_key = d.type_key().to_string();
            let captured = capture_tool_defaults(d);
            // A drawing still on its factory look sets nothing: factory settings are never
            // frozen into the remembered set, so a later change to them still reaches the user.
            let factory = self
                .inner
                .registry
                .create(&type_key, DrawingInit::on_pane(d.base().pane_id.clone()))
                .map(|fresh| capture_tool_defaults(fresh.as_ref()));
            let next = if same_tool_defaults(Some(&captured), factory.as_ref()) {
                None
            } else {
                Some(captured)
            };
            if same_tool_defaults(ordered_get(&st.defaults, &type_key), next.as_ref()) {
                return;
            }
            match next {
                Some(next) => ordered_set(&mut st.defaults, &type_key, next),
                None => {
                    ordered_remove(&mut st.defaults, &type_key);
                }
            }
            type_key
        };
        self.emit(ChartEvent::DrawingDefaults { type_key });
    }

    /// Renderer gesture to authoritative store mutation + event.
    fn on_intent(&self, intent: &DrawingIntent) {
        if self.inner.destroyed.get() {
            return;
        }
        match intent {
            DrawingIntent::Arm { type_key } => {
                // Toolbar click: core-authoritative tool state.
                self.set_tool(type_key.as_deref());
            }
            DrawingIntent::Draft { doc } => {
                // Placement progress: transient, no store mutation. Re-emitted so a
                // multi-chart host can mirror the ghost on linked charts live.
                self.emit(ChartEvent::DrawingDraft { doc: doc.clone() });
            }
            DrawingIntent::Create { doc } => self.on_create(doc),
            DrawingIntent::Edit { doc } => {
                let before = self.document();
                if self.update_drawing(&doc.id, patch_from_record(doc)) {
                    self.mutate(|st| st.history.record(&before));
                    self.emit(ChartEvent::DrawingEdited { id: doc.id.clone() });
                }
            }
            DrawingIntent::EditMany { docs } => {
                self.begin_transaction();
                for doc in docs {
                    if self.update_drawing(&doc.id, patch_from_record(doc)) {
                        self.inner.state.borrow_mut().history.mark_dirty();
                        self.emit(ChartEvent::DrawingEdited { id: doc.id.clone() });
                    }
                }
                self.inner.state.borrow_mut().history.commit();
            }
            DrawingIntent::Select { ids, additive } => self.set_selection(ids, *additive),
            DrawingIntent::Delete { ids } => self.delete_many(ids),
            DrawingIntent::Reorder { id, to } => match to {
                ReorderTo::Front => self.bring_to_front(id),
                ReorderTo::Back => self.send_to_back(id),
            },
            DrawingIntent::Settings { id } => {
                self.emit(ChartEvent::DrawingSettings { id: id.clone() });
            }
            DrawingIntent::Favorite { type_key, on } => self.set_favorite(type_key, *on),
            DrawingIntent::TemplateSave {
                type_key,
                name,
                settings,
            } => self.save_tool_template(type_key, name, settings),
            DrawingIntent::TemplateRemove { type_key, name } => {
                self.remove_tool_template(type_key, name);
            }
            DrawingIntent::SnapMode { mode } => {
                // In-chart magnet click (already applied renderer-side): mirror + announce.
                let changed = {
                    let mut st = self.inner.state.borrow_mut();
                    let changed = *mode != st.snap_mode;
                    st.snap_mode = *mode;
                    changed
                };
                if changed {
                    self.emit(ChartEvent::DrawingSnap { mode: *mode });
                }
            }
            DrawingIntent::StayMode { on } => {
                // In-chart stay-mode click (already applied renderer-side): mirror + announce.
                let changed = {
                    let mut st = self.inner.state.borrow_mut();
                    let changed = *on != st.stay_in_drawing_mode;
                    st.stay_in_drawing_mode = *on;
                    changed
                };
                if changed {
                    self.emit(ChartEvent::DrawingStay { on: *on });
                }
            }
            DrawingIntent::Mode { mode } => {
                // Measure/eraser toggled in-chart, or exited as a mutual-exclusion side effect.
                let changed = {
                    let mut st = self.inner.state.borrow_mut();
                    let changed = *mode != st.mode;
                    st.mode = *mode;
                    changed
                };
                if changed {
                    self.emit(ChartEvent::DrawingMode { mode: *mode });
                }
            }
            DrawingIntent::ToolFinished { type_key } => {
                // Most tools are one-shot (revert to the pointer once placed); brush-family
                // tools always stay armed, and stay-in-drawing-mode keeps every tool armed.
                let stay = self.inner.state.borrow().stay_in_drawing_mode;
                if !stay && type_key != "freehand" && type_key != "highlighter" {
                    self.set_tool(None);
                }
            }
            DrawingIntent::Undo => self.undo(),
            DrawingIntent::Redo => self.redo(),
            DrawingIntent::Duplicate { ids } => {
                self.duplicate(ids);
            }
            DrawingIntent::Clone { docs } => {
                if self.inner.enabled && !docs.is_empty() {
                    self.insert_clones(docs);
                }
            }
            DrawingIntent::Copy { ids } => self.copy(ids),
            DrawingIntent::Paste => {
                self.paste();
            }
        }
    }

    fn on_create(&self, doc: &SerializedDrawing) {
        let before = self.document();
        let id = self.inner.state.borrow_mut().store.next_id();
        let record = SerializedDrawing {
            id: id.clone(),
            ..doc.clone()
        };
        let Some(mut d) = self.inner.registry.deserialize(&record) else {
            return;
        };
        // A freshly drawn shape starts from its tool's remembered settings.
        let defaults = ordered_get(&self.inner.state.borrow().defaults, d.type_key()).cloned();
        apply_tool_defaults(d.as_mut(), defaults.as_ref());
        // A freshly placed drawing starts under the price (or over the stack when it
        // covers the series).
        let z = d.base().z_index;
        if z == 0.0 || z.is_nan() {
            let type_key = d.type_key();
            let pane_id = d.base().pane_id.clone();
            d.base_mut().z_index = self.start_z(type_key, &pane_id).unwrap_or(0.0);
        }
        self.mutate(|st| {
            st.history.record(&before);
            st.store.add(d);
        });
        self.capture_defaults(&id);
        // No auto-select: selection (= the drawing being edited) is driven by the
        // settings popup. A freshly drawn shape shows handles via hover instead.
        self.emit(ChartEvent::DrawingCreated { id });
    }
}

/// A test-only recording renderer: a [`ChartRenderer`] with a drawing port and a
/// timeline-marks seam that log what the core pushes down and let a test fire a
/// drawing intent or a mark click.
#[cfg(test)]
pub(crate) mod test_support {
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use serde_json::Value;

    use super::*;
    use crate::drawings::style::DrawingStyle;
    use crate::marks::{MarkClickEvent, MarkGroup, MarksDisplayState, TimelineMark};
    use crate::model::{IndicatorModel, Inputs, Pane, ScenePatch};
    use crate::ports::chart_renderer::{
        BarColorSupport, ClickEvent, CrosshairEvent, IndicatorRenderHandle, InputChangeEvent,
        Listener, RendererCapabilities, SetBarsOptions, StackRange, Support, VelaTheme,
        VisibleRange, VisibleRangeOptions,
    };
    use crate::runtime::noop_unsubscribe;

    type StackFn = Box<dyn Fn(&str) -> Option<StackRange>>;
    type IntentSlot = Rc<RefCell<Option<Rc<RefCell<Listener<DrawingIntent>>>>>>;

    /// Records every call the controller makes on the drawing port.
    #[derive(Default)]
    pub(crate) struct FakePort {
        /// Every command, by name, in call order.
        pub log: RefCell<Vec<String>>,
        pub toolbar: RefCell<Option<Value>>,
        pub toolbar_pushes: Cell<usize>,
        pub visible: Cell<Option<bool>>,
        pub synced: RefCell<Vec<SerializedDrawing>>,
        pub sync_count: Cell<usize>,
        pub active_tool: RefCell<Option<String>>,
        pub active_tool_style: RefCell<Option<DrawingStyle>>,
        pub active_defaults: RefCell<Option<DrawingToolDefaults>>,
        pub selection_ids: RefCell<Vec<String>>,
        pub favorites_pushed: RefCell<Vec<Vec<String>>>,
        pub templates_pushed: RefCell<BTreeMap<String, Vec<DrawingToolTemplate>>>,
        pub shortcuts_pushed: RefCell<Vec<BTreeMap<String, String>>>,
        pub snap_modes: RefCell<Vec<SnapMode>>,
        pub stay_modes: RefCell<Vec<bool>>,
        pub modes: RefCell<Vec<Option<DrawingMode>>>,
        pub settings_opened_for: RefCell<Option<String>>,
        pub ghost: RefCell<Option<SerializedDrawing>>,
        pub gateway_set: Cell<bool>,
        /// Assigned per test to emulate a renderer whose drawings share the series' z space.
        pub stack_range: RefCell<Option<StackFn>>,
        intent: IntentSlot,
    }

    impl FakePort {
        /// Deliver an intent as the renderer would.
        pub fn fire(&self, intent: DrawingIntent) {
            let callback = self.intent.borrow().clone();
            if let Some(callback) = callback {
                (callback.borrow_mut())(&intent);
            }
        }

        pub fn subscribed(&self) -> bool {
            self.intent.borrow().is_some()
        }

        pub fn set_stack_range(&self, f: impl Fn(&str) -> Option<StackRange> + 'static) {
            *self.stack_range.borrow_mut() = Some(Box::new(f));
        }

        pub fn count(&self, name: &str) -> usize {
            self.log.borrow().iter().filter(|c| *c == name).count()
        }

        fn note(&self, name: &str) {
            self.log.borrow_mut().push(name.to_string());
        }
    }

    impl DrawingsRendererPort for FakePort {
        fn set_toolbar(&self, def: &Value) {
            self.note("set_toolbar");
            *self.toolbar.borrow_mut() = Some(def.clone());
            self.toolbar_pushes.set(self.toolbar_pushes.get() + 1);
        }
        fn show_toolbar(&self, visible: bool) {
            self.note("show_toolbar");
            self.visible.set(Some(visible));
        }
        fn sync_drawings(&self, docs: &[SerializedDrawing]) {
            self.note("sync_drawings");
            *self.synced.borrow_mut() = docs.to_vec();
            self.sync_count.set(self.sync_count.get() + 1);
        }
        fn set_active_tool(
            &self,
            tool: Option<&str>,
            last_style: Option<&DrawingStyle>,
            defaults: Option<&DrawingToolDefaults>,
        ) {
            self.note("set_active_tool");
            *self.active_tool.borrow_mut() = tool.map(str::to_string);
            *self.active_tool_style.borrow_mut() = last_style.cloned();
            *self.active_defaults.borrow_mut() = defaults.cloned();
        }
        fn set_selection(&self, ids: &[String]) {
            self.note("set_selection");
            *self.selection_ids.borrow_mut() = ids.to_vec();
        }
        fn set_favorites(&self, tools: &[String]) {
            self.note("set_favorites");
            self.favorites_pushed.borrow_mut().push(tools.to_vec());
        }
        fn set_tool_templates(&self, map: &BTreeMap<String, Vec<DrawingToolTemplate>>) {
            self.note("set_tool_templates");
            *self.templates_pushed.borrow_mut() = map.clone();
        }
        fn set_tool_shortcuts(&self, map: &BTreeMap<String, String>) {
            self.note("set_tool_shortcuts");
            self.shortcuts_pushed.borrow_mut().push(map.clone());
        }
        fn set_snap_mode(&self, mode: SnapMode) {
            self.note("set_snap_mode");
            self.snap_modes.borrow_mut().push(mode);
        }
        fn set_stay_mode(&self, on: bool) {
            self.note("set_stay_mode");
            self.stay_modes.borrow_mut().push(on);
        }
        fn set_mode(&self, mode: Option<DrawingMode>) {
            self.note("set_mode");
            self.modes.borrow_mut().push(mode);
        }
        fn set_series_gateway(&self, _gateway: Rc<dyn DrawingSeriesGateway>) {
            self.note("set_series_gateway");
            self.gateway_set.set(true);
        }
        fn open_settings(&self, id: &str) {
            self.note("open_settings");
            *self.settings_opened_for.borrow_mut() = Some(id.to_string());
        }
        fn set_external_ghost(&self, doc: Option<&SerializedDrawing>) {
            self.note("set_external_ghost");
            *self.ghost.borrow_mut() = doc.cloned();
        }
        fn stack_range(&self, pane_id: &str) -> Option<StackRange> {
            self.stack_range.borrow().as_ref().and_then(|f| f(pane_id))
        }
        fn on_drawing_intent(&self, cb: Listener<DrawingIntent>) -> Unsubscribe {
            self.note("on_drawing_intent");
            *self.intent.borrow_mut() = Some(Rc::new(RefCell::new(cb)));
            let slot = Rc::clone(&self.intent);
            Box::new(move || {
                *slot.borrow_mut() = None;
            })
        }
    }

    /// What a [`FakeRenderer`] can do.
    #[derive(Clone, Copy)]
    pub(crate) struct FakeOptions {
        pub user_drawings: bool,
        pub timeline_marks: bool,
    }

    impl Default for FakeOptions {
        fn default() -> Self {
            Self {
                user_drawings: true,
                timeline_marks: true,
            }
        }
    }

    /// One `set_timeline_marks` call.
    #[derive(Clone, Debug, PartialEq)]
    pub(crate) struct MarksPush {
        pub marks: Vec<TimelineMark>,
        pub groups: Vec<MarkGroup>,
    }

    type MarkClickSlot = Rc<RefCell<Option<Rc<RefCell<Listener<MarkClickEvent>>>>>>;

    pub(crate) struct FakeRenderer {
        options: FakeOptions,
        pub port: Rc<FakePort>,
        pub pushes: RefCell<Vec<MarksPush>>,
        /// Every `apply_feature` call.
        pub features_applied: RefCell<Vec<(String, Value)>>,
        marks_state: RefCell<MarksDisplayState>,
        mark_click: MarkClickSlot,
    }

    impl FakeRenderer {
        pub fn new(options: FakeOptions) -> Rc<Self> {
            Rc::new(Self {
                options,
                port: Rc::new(FakePort::default()),
                pushes: RefCell::new(Vec::new()),
                features_applied: RefCell::new(Vec::new()),
                marks_state: RefCell::new(MarksDisplayState::default()),
                mark_click: Rc::default(),
            })
        }

        /// Deliver a glyph click as the renderer would.
        pub fn click(&self, event: MarkClickEvent) {
            let callback = self.mark_click.borrow().clone();
            if let Some(callback) = callback {
                (callback.borrow_mut())(&event);
            }
        }

        pub fn mark_click_subscribed(&self) -> bool {
            self.mark_click.borrow().is_some()
        }
    }

    impl ChartRenderer for FakeRenderer {
        fn capabilities(&self) -> RendererCapabilities {
            RendererCapabilities {
                panes: true,
                pane_management: false,
                fills: Support::Unsupported,
                bgcolor: Support::Native,
                hline: Support::Primitive,
                markers: false,
                barcolor: BarColorSupport::Approximated,
                per_point_color: false,
                drawings: false,
                user_drawings: self.options.user_drawings,
                drawing_depth: false,
                tables: false,
                trades: false,
                timeline_marks: self.options.timeline_marks,
                inputs_ui: false,
            }
        }
        fn name(&self) -> &str {
            "fake"
        }
        fn features(&self) -> Vec<String> {
            if self.options.timeline_marks {
                vec!["marks".to_string()]
            } else {
                Vec::new()
            }
        }
        fn apply_feature(&self, key: &str, value: &Value) {
            self.features_applied
                .borrow_mut()
                .push((key.to_string(), value.clone()));
            if key == "marks" {
                let merged = self.marks_state.borrow().merged(value);
                *self.marks_state.borrow_mut() = merged;
            }
        }
        fn read_feature(&self, key: &str) -> Option<Value> {
            if self.options.timeline_marks && key == "marks" {
                serde_json::to_value(&*self.marks_state.borrow()).ok()
            } else {
                None
            }
        }
        fn mount(&self, _theme: &VelaTheme) {}
        fn set_theme(&self, _theme: &VelaTheme) {}
        fn resize(&self) {}
        fn destroy(&self) {}
        fn set_bars(&self, _bars: &[crate::model::Ohlcv], _opts: SetBarsOptions) {}
        fn update_bar(&self, _bar: &crate::model::Ohlcv) {}
        fn ensure_pane(&self, _pane: &Pane) {}
        fn remove_pane(&self, _id: &str) {}
        fn mount_indicator(&self, model: &IndicatorModel) -> IndicatorRenderHandle {
            IndicatorRenderHandle {
                id: model.id.clone(),
            }
        }
        fn update_indicator(&self, _handle: &IndicatorRenderHandle, _patch: &ScenePatch) {}
        fn remove_indicator(&self, _handle: &IndicatorRenderHandle) {}
        fn set_indicator_inputs(
            &self,
            _handle: &IndicatorRenderHandle,
            _values: &Inputs,
            _props: Option<&Inputs>,
        ) {
        }
        fn on_input_change(&self, _cb: Listener<InputChangeEvent>) -> Unsubscribe {
            noop_unsubscribe()
        }
        fn on_remove_indicator(&self, _cb: Listener<String>) -> Unsubscribe {
            noop_unsubscribe()
        }
        fn on_crosshair_move(&self, _cb: Listener<CrosshairEvent>) -> Unsubscribe {
            noop_unsubscribe()
        }
        fn on_click(&self, _cb: Listener<ClickEvent>) -> Unsubscribe {
            noop_unsubscribe()
        }
        fn get_visible_range(&self) -> Option<VisibleRange> {
            None
        }
        fn set_visible_range(&self, _range: VisibleRange, _opts: VisibleRangeOptions) {}
        fn on_viewport_change(&self, _cb: Listener<VisibleRange>) -> Unsubscribe {
            noop_unsubscribe()
        }

        fn set_timeline_marks(&self, marks: &[TimelineMark], groups: &[MarkGroup]) {
            self.pushes.borrow_mut().push(MarksPush {
                marks: marks.to_vec(),
                groups: groups.to_vec(),
            });
        }
        fn supports_set_timeline_marks(&self) -> bool {
            self.options.timeline_marks
        }
        fn on_mark_click(&self, cb: Listener<MarkClickEvent>) -> Option<Unsubscribe> {
            if !self.options.timeline_marks {
                return None;
            }
            *self.mark_click.borrow_mut() = Some(Rc::new(RefCell::new(cb)));
            let slot = Rc::clone(&self.mark_click);
            Some(Box::new(move || {
                *slot.borrow_mut() = None;
            }))
        }

        fn user_drawings_port(&self) -> Option<Rc<dyn DrawingsRendererPort>> {
            self.options
                .user_drawings
                .then(|| Rc::clone(&self.port) as Rc<dyn DrawingsRendererPort>)
        }
    }
}

#[cfg(test)]
#[allow(clippy::cloned_ref_to_slice_refs, clippy::type_complexity)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use serde_json::json;

    use super::test_support::{FakeOptions, FakeRenderer};
    use super::*;
    use crate::drawings::defaults::{builtin_tool_templates, capture_tool_defaults};
    use crate::events::types::ChartEventName;
    use crate::i18n::english_translator;
    use crate::model::LineStyle;
    use crate::ports::chart_renderer::StackRange;

    type Seen = Rc<RefCell<Vec<(String, Option<String>)>>>;

    struct Setup {
        renderer: Rc<FakeRenderer>,
        events: ChartEventBus,
        seen: Seen,
        ctrl: DrawingController,
    }

    fn setup_with(user_drawings: bool, option: Option<DrawingsOption>) -> Setup {
        let renderer = FakeRenderer::new(FakeOptions {
            user_drawings,
            timeline_marks: false,
        });
        let events = ChartEventBus::new();
        let seen: Seen = Rc::default();
        for name in [
            ChartEventName::DrawingCreated,
            ChartEventName::DrawingEdited,
            ChartEventName::DrawingSelected,
            ChartEventName::DrawingRemoved,
            ChartEventName::DrawingSettings,
        ] {
            let seen = Rc::clone(&seen);
            events.on(name, move |event| {
                let id = match event {
                    ChartEvent::DrawingCreated { id }
                    | ChartEvent::DrawingEdited { id }
                    | ChartEvent::DrawingRemoved { id }
                    | ChartEvent::DrawingSettings { id } => Some(id.clone()),
                    ChartEvent::DrawingSelected { id, .. } => id.clone(),
                    _ => return,
                };
                seen.borrow_mut()
                    .push((event.name().as_str().to_string(), id));
            });
        }
        let ctrl = DrawingController::new(
            renderer.clone(),
            events.clone(),
            Rc::new(DrawingRegistry::builtin()),
            option,
            None,
            no_translator(),
        );
        Setup {
            renderer,
            events,
            seen,
            ctrl,
        }
    }

    fn setup() -> Setup {
        setup_with(true, None)
    }

    fn contains(seen: &Seen, name: &str, id: Option<&str>) -> bool {
        seen.borrow()
            .iter()
            .any(|(n, i)| n == name && i.as_deref() == id)
    }

    fn hline_doc() -> SerializedDrawing {
        SerializedDrawing {
            id: "renderer-temp".into(),
            type_key: "hline".into(),
            pane_id: "price".into(),
            anchors: vec![DrawingPoint {
                time: 1000.0,
                price: 25000.0,
            }],
            style: DrawingStyle::line("#fff", 1.0, LineStyle::Solid),
            text: None,
            locked: false,
            visible: true,
            z_index: 0.0,
            created_at: 0.0,
            props: None,
            show_on: None,
        }
    }

    fn pt(time: f64, price: f64) -> DrawingPoint {
        DrawingPoint { time, price }
    }

    fn init_at(time: f64, price: f64) -> AddInit {
        AddInit {
            anchors: Some(vec![pt(time, price)]),
            ..Default::default()
        }
    }

    fn ids(ctrl: &DrawingController) -> Vec<String> {
        ctrl.all().into_iter().map(|d| d.id).collect()
    }

    fn prices(ctrl: &DrawingController) -> Vec<f64> {
        ctrl.all().iter().map(|d| d.anchors[0].price).collect()
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn color(style: &str) -> PartialDrawingStyle {
        PartialDrawingStyle {
            line_color: Some(style.into()),
            ..Default::default()
        }
    }

    // ── DrawingController (enabled) ──

    #[test]
    fn pushes_the_toolbar_and_initial_visibility_on_construction() {
        let on = setup_with(true, Some(true.into()));
        assert!(on.renderer.port.toolbar.borrow().is_some());
        assert_eq!(on.renderer.port.visible.get(), Some(true));
        let default = setup_with(true, None);
        assert_eq!(default.renderer.port.visible.get(), Some(true));
        let off = setup_with(true, Some(false.into()));
        assert_eq!(off.renderer.port.visible.get(), Some(false));
    }

    #[test]
    fn construction_pushes_toolbar_then_visibility_then_subscribes() {
        let s = setup();
        assert_eq!(
            *s.renderer.port.log.borrow(),
            ["set_toolbar", "show_toolbar", "on_drawing_intent"]
        );
        let toolbar = s.renderer.port.toolbar.borrow().clone().expect("toolbar");
        assert_eq!(
            toolbar["groups"][0]["id"],
            json!("lines-channels-pitchforks")
        );
        assert_eq!(toolbar["groups"][0]["label"], json!("Lines"));
        assert!(toolbar["groups"][0]["tools"][0]["type"].is_string());
    }

    #[test]
    fn the_series_gateway_is_handed_to_the_port() {
        struct Gateway;
        impl DrawingSeriesGateway for Gateway {
            fn series_in_range(
                &self,
                _timeframe: &str,
                _from: f64,
                _to: f64,
            ) -> crate::drawings::DrawingSeriesState {
                crate::drawings::DrawingSeriesState::Unavailable {
                    reason: crate::drawings::SeriesUnavailable::NoSource,
                }
            }
        }
        let renderer = FakeRenderer::new(FakeOptions::default());
        let _ctrl = DrawingController::new(
            renderer.clone(),
            ChartEventBus::new(),
            Rc::new(DrawingRegistry::builtin()),
            None,
            Some(Rc::new(Gateway)),
            no_translator(),
        );
        assert_eq!(
            *renderer.port.log.borrow(),
            [
                "set_toolbar",
                "show_toolbar",
                "set_series_gateway",
                "on_drawing_intent"
            ]
        );
    }

    #[test]
    fn arms_and_disarms_a_tool_through_the_port() {
        let s = setup();
        s.ctrl.set_tool(Some("trendline"));
        assert_eq!(
            s.renderer.port.active_tool.borrow().as_deref(),
            Some("trendline")
        );
        s.ctrl.set_tool(None);
        assert_eq!(*s.renderer.port.active_tool.borrow(), None);
    }

    #[test]
    fn add_creates_syncs_and_emits() {
        let s = setup();
        let d = s
            .ctrl
            .add(
                "hline",
                AddInit {
                    pane_id: Some("price".into()),
                    anchors: Some(vec![pt(1.0, 2.0)]),
                    ..Default::default()
                },
            )
            .expect("added");
        assert_eq!(s.renderer.port.synced.borrow().len(), 1);
        assert_eq!(s.renderer.port.synced.borrow()[0].type_key, "hline");
        assert!(contains(&s.seen, "drawing:created", Some(&d.id)));
    }

    #[test]
    fn add_syncs_once_before_it_announces() {
        let s = setup();
        let order: Rc<RefCell<Vec<&'static str>>> = Rc::default();
        {
            let order = Rc::clone(&order);
            let port = Rc::clone(&s.renderer.port);
            s.events.on(ChartEventName::DrawingCreated, move |_| {
                assert_eq!(port.sync_count.get(), 1, "synced before the event");
                order.borrow_mut().push("created");
            });
        }
        s.ctrl.add("hline", init_at(1.0, 2.0));
        assert_eq!(*order.borrow(), ["created"]);
        assert_eq!(s.renderer.port.count("sync_drawings"), 1);
    }

    #[test]
    fn add_returns_none_for_an_unknown_type() {
        let s = setup();
        assert!(s.ctrl.add("not-a-tool", AddInit::default()).is_none());
        assert!(s.ctrl.all().is_empty());
    }

    #[test]
    fn select_drives_the_port_selection_and_emits() {
        let s = setup();
        s.renderer
            .port
            .fire(DrawingIntent::Create { doc: hline_doc() });
        let id = ids(&s.ctrl)[0].clone();
        s.ctrl.select(&[id.clone()], false);
        assert_eq!(*s.renderer.port.selection_ids.borrow(), [id.clone()]);
        assert!(contains(&s.seen, "drawing:selected", Some(&id)));
        s.ctrl.select(&[], false);
        assert!(s.renderer.port.selection_ids.borrow().is_empty());
        assert!(contains(&s.seen, "drawing:selected", None));
    }

    #[test]
    fn open_settings_forwards_to_the_port() {
        let s = setup();
        s.renderer
            .port
            .fire(DrawingIntent::Create { doc: hline_doc() });
        let id = ids(&s.ctrl)[0].clone();
        s.ctrl.open_settings(&id);
        assert_eq!(
            s.renderer.port.settings_opened_for.borrow().as_deref(),
            Some(id.as_str())
        );
    }

    #[test]
    fn a_create_intent_becomes_a_store_owned_drawing() {
        let s = setup();
        s.renderer
            .port
            .fire(DrawingIntent::Create { doc: hline_doc() });
        assert_eq!(s.renderer.port.synced.borrow().len(), 1);
        // The store assigns the id, not the renderer's 'renderer-temp'.
        assert_eq!(s.renderer.port.synced.borrow()[0].id, "dw-1");
        assert!(contains(&s.seen, "drawing:created", Some("dw-1")));
        assert_eq!(s.ctrl.all()[0].anchors[0].price, 25000.0);
        // Creation no longer auto-selects.
        assert!(!s.seen.borrow().iter().any(|(n, _)| n == "drawing:selected"));
    }

    #[test]
    fn an_unknown_type_in_a_create_intent_is_dropped() {
        let s = setup();
        let mut doc = hline_doc();
        doc.type_key = "not-a-tool".into();
        s.renderer.port.fire(DrawingIntent::Create { doc });
        assert!(s.ctrl.all().is_empty());
        assert!(!s.ctrl.can_undo());
    }

    #[test]
    fn edit_select_and_delete_intents_mutate_and_emit() {
        let s = setup();
        let port = &s.renderer.port;
        port.fire(DrawingIntent::Create { doc: hline_doc() });
        let id = ids(&s.ctrl)[0].clone();
        let mut moved = hline_doc();
        moved.id = id.clone();
        moved.anchors = vec![pt(1.0, 30000.0)];
        port.fire(DrawingIntent::Edit { doc: moved });
        assert_eq!(s.ctrl.all()[0].anchors[0].price, 30000.0);
        assert!(contains(&s.seen, "drawing:edited", Some(&id)));
        port.fire(DrawingIntent::Select {
            ids: vec![],
            additive: false,
        });
        assert!(port.selection_ids.borrow().is_empty());
        port.fire(DrawingIntent::Delete {
            ids: vec![id.clone()],
        });
        assert!(s.ctrl.all().is_empty());
        assert!(contains(&s.seen, "drawing:removed", Some(&id)));
    }

    #[test]
    fn settings_and_tool_finished_intents() {
        let s = setup();
        s.ctrl.set_tool(Some("trendline"));
        s.renderer.port.fire(DrawingIntent::ToolFinished {
            type_key: "trendline".into(),
        });
        assert_eq!(*s.renderer.port.active_tool.borrow(), None); // one-shot disarm
        s.renderer
            .port
            .fire(DrawingIntent::Settings { id: "x".into() });
        assert!(contains(&s.seen, "drawing:settings", Some("x")));
    }

    #[test]
    fn brush_and_highlighter_stay_armed_after_a_stroke() {
        for tool in ["freehand", "highlighter"] {
            let s = setup();
            s.ctrl.set_tool(Some(tool));
            s.renderer.port.fire(DrawingIntent::ToolFinished {
                type_key: tool.into(),
            });
            assert_eq!(s.ctrl.get_tool().as_deref(), Some(tool));
            assert_eq!(s.renderer.port.active_tool.borrow().as_deref(), Some(tool));
        }
    }

    #[test]
    fn stay_in_drawing_mode_keeps_a_one_shot_tool_armed() {
        let s = setup();
        s.ctrl.set_stay_mode(true);
        s.ctrl.set_tool(Some("trendline"));
        s.renderer.port.fire(DrawingIntent::ToolFinished {
            type_key: "trendline".into(),
        });
        assert_eq!(s.ctrl.get_tool().as_deref(), Some("trendline"));
        assert_eq!(
            s.renderer.port.active_tool.borrow().as_deref(),
            Some("trendline")
        );
        s.ctrl.set_stay_mode(false);
        s.renderer.port.fire(DrawingIntent::ToolFinished {
            type_key: "trendline".into(),
        });
        assert_eq!(s.ctrl.get_tool(), None);
    }

    // ── disabled / LwC ──

    #[test]
    fn a_controller_without_a_port_is_inert_for_interactive_ops_but_persists() {
        let s = setup_with(false, None);
        assert!(!s.ctrl.supported());
        assert!(s.ctrl.add("hline", AddInit::default()).is_none());
        s.ctrl.set_tool(Some("trendline"));
        assert_eq!(s.ctrl.get_tool(), None);
        let mut doc = hline_doc();
        doc.id = "dw-9".into();
        s.ctrl.from_json(
            &json!({ "version": 1, "drawings": [serde_json::to_value(&doc).expect("json")] }),
        );
        assert_eq!(s.ctrl.all().len(), 1);
        assert_eq!(s.ctrl.to_json().drawings[0].id, "dw-9");
        assert_eq!(s.renderer.port.sync_count.get(), 0);
    }

    #[test]
    fn a_renderer_with_a_port_but_no_capability_is_headless() {
        // The capability, not the port's presence, decides.
        let renderer = FakeRenderer::new(FakeOptions {
            user_drawings: false,
            timeline_marks: false,
        });
        let ctrl = DrawingController::new(
            renderer.clone(),
            ChartEventBus::new(),
            Rc::new(DrawingRegistry::builtin()),
            None,
            None,
            no_translator(),
        );
        assert!(!ctrl.supported());
        assert!(renderer.port.log.borrow().is_empty());
    }

    #[test]
    fn undo_works_on_the_headless_controller() {
        let s = setup_with(false, None);
        let mut doc = hline_doc();
        doc.id = "dw-1".into();
        s.ctrl.from_json(
            &json!({ "version": 1, "drawings": [serde_json::to_value(&doc).expect("json")] }),
        );
        s.ctrl.update(
            "dw-1",
            DrawingUpdate {
                style: Some(color("#123456")),
                ..Default::default()
            },
        );
        assert_eq!(s.ctrl.all()[0].style.line_color, "#123456");
        assert!(s.ctrl.can_undo());
        s.ctrl.undo();
        assert_eq!(s.ctrl.all()[0].style.line_color, "#fff");
    }

    // ── editing foundation ──

    #[test]
    fn undo_and_redo_round_trip_create_intents() {
        let s = setup();
        s.renderer
            .port
            .fire(DrawingIntent::Create { doc: hline_doc() });
        s.renderer
            .port
            .fire(DrawingIntent::Create { doc: hline_doc() });
        assert_eq!(s.ctrl.all().len(), 2);
        assert!(s.ctrl.can_undo());
        s.ctrl.undo();
        assert_eq!(s.ctrl.all().len(), 1);
        s.ctrl.undo();
        assert_eq!(s.ctrl.all().len(), 0);
        assert!(!s.ctrl.can_undo());
        s.ctrl.redo();
        s.ctrl.redo();
        assert_eq!(s.ctrl.all().len(), 2);
    }

    #[test]
    fn a_single_edit_is_one_undo_entry() {
        let s = setup();
        s.renderer
            .port
            .fire(DrawingIntent::Create { doc: hline_doc() });
        let id = ids(&s.ctrl)[0].clone();
        let mut edited = hline_doc();
        edited.id = id;
        edited.anchors = vec![pt(1.0, 99999.0)];
        s.renderer.port.fire(DrawingIntent::Edit { doc: edited });
        assert_eq!(s.ctrl.all()[0].anchors[0].price, 99999.0);
        s.ctrl.undo();
        assert_eq!(s.ctrl.all().len(), 1);
        assert_eq!(s.ctrl.all()[0].anchors[0].price, 25000.0);
    }

    #[test]
    fn edit_many_coalesces_into_one_undo_entry() {
        let s = setup();
        s.renderer
            .port
            .fire(DrawingIntent::Create { doc: hline_doc() });
        s.renderer
            .port
            .fire(DrawingIntent::Create { doc: hline_doc() });
        let all = ids(&s.ctrl);
        let doc = |id: &str, price: f64| {
            let mut d = hline_doc();
            d.id = id.to_string();
            d.anchors = vec![pt(1.0, price)];
            d
        };
        s.renderer.port.fire(DrawingIntent::EditMany {
            docs: vec![doc(&all[0], 1.0), doc(&all[1], 2.0)],
        });
        assert_eq!(prices(&s.ctrl), [1.0, 2.0]);
        s.ctrl.undo(); // one undo reverts BOTH
        assert_eq!(prices(&s.ctrl), [25000.0, 25000.0]);
    }

    #[test]
    fn duplicate_clones_with_a_fresh_id_selects_emits_and_is_one_undo_step() {
        let s = setup();
        s.renderer
            .port
            .fire(DrawingIntent::Create { doc: hline_doc() });
        let id = ids(&s.ctrl)[0].clone();
        s.seen.borrow_mut().clear();
        let clones = s.ctrl.duplicate(&[id.clone()]);
        assert_eq!(s.ctrl.all().len(), 2);
        assert_ne!(clones[0].id, id);
        assert_eq!(
            *s.renderer.port.selection_ids.borrow(),
            [clones[0].id.clone()]
        );
        assert!(contains(&s.seen, "drawing:created", Some(&clones[0].id)));
        s.ctrl.undo();
        assert_eq!(ids(&s.ctrl), [id]);
    }

    #[test]
    fn clone_intent_commits_the_given_docs_as_fresh_copies() {
        let s = setup();
        s.renderer
            .port
            .fire(DrawingIntent::Create { doc: hline_doc() });
        let src = s.ctrl.all()[0].clone();
        s.seen.borrow_mut().clear();
        let mut twin = src.clone();
        twin.anchors = vec![pt(2000.0, 26000.0)];
        s.renderer
            .port
            .fire(DrawingIntent::Clone { docs: vec![twin] });
        assert_eq!(s.ctrl.all().len(), 2);
        let clone = s
            .ctrl
            .all()
            .into_iter()
            .find(|d| d.id != src.id)
            .expect("clone");
        assert_eq!(clone.anchors[0], pt(2000.0, 26000.0));
        assert_eq!(clone.z_index, src.z_index); // keeps its source's depth
        let source_now = s
            .ctrl
            .all()
            .into_iter()
            .find(|d| d.id == src.id)
            .expect("source");
        assert_eq!(source_now.anchors[0], pt(1000.0, 25000.0)); // source never moved
        assert_eq!(*s.renderer.port.selection_ids.borrow(), [clone.id.clone()]);
        assert!(contains(&s.seen, "drawing:created", Some(&clone.id)));
        s.ctrl.undo();
        assert_eq!(ids(&s.ctrl), [src.id]);
    }

    #[test]
    fn copy_is_silent_and_paste_creates_fresh_drawings() {
        let s = setup();
        let d = s.ctrl.add("hline", init_at(1.0, 1.0)).expect("added");
        s.seen.borrow_mut().clear();
        s.ctrl.copy(&[d.id.clone()]);
        assert!(s.seen.borrow().is_empty());
        assert_eq!(s.ctrl.all().len(), 1);
        let pasted = s.ctrl.paste();
        assert_eq!(s.ctrl.all().len(), 2);
        assert_ne!(pasted[0].id, d.id);
        assert!(contains(&s.seen, "drawing:created", Some(&pasted[0].id)));
    }

    #[test]
    fn copy_of_nothing_keeps_the_clipboard_and_paste_needs_one() {
        let s = setup();
        assert!(s.ctrl.paste().is_empty());
        let d = s.ctrl.add("hline", init_at(1.0, 1.0)).expect("added");
        s.ctrl.copy(&[d.id.clone()]);
        s.ctrl.copy(&["missing".to_string()]); // resolves to nothing: the buffer stays
        assert_eq!(s.ctrl.paste().len(), 1);
        let headless = setup_with(false, None);
        assert!(headless.ctrl.paste().is_empty());
        assert!(headless.ctrl.duplicate(&["x".to_string()]).is_empty());
    }

    #[test]
    fn additive_select_toggles_membership_and_the_event_carries_the_full_list() {
        let s = setup();
        let payloads: Rc<RefCell<Vec<(Option<String>, Vec<String>)>>> = Rc::default();
        {
            let payloads = Rc::clone(&payloads);
            s.events.on(ChartEventName::DrawingSelected, move |event| {
                if let ChartEvent::DrawingSelected { id, ids } = event {
                    payloads.borrow_mut().push((id.clone(), ids.clone()));
                }
            });
        }
        s.renderer
            .port
            .fire(DrawingIntent::Create { doc: hline_doc() });
        s.renderer
            .port
            .fire(DrawingIntent::Create { doc: hline_doc() });
        let all = ids(&s.ctrl);
        let (a, b) = (all[0].clone(), all[1].clone());
        s.renderer.port.fire(DrawingIntent::Select {
            ids: vec![a.clone()],
            additive: false,
        });
        assert_eq!(*s.renderer.port.selection_ids.borrow(), [a.clone()]);
        s.seen.borrow_mut().clear();
        s.renderer.port.fire(DrawingIntent::Select {
            ids: vec![b.clone()],
            additive: true,
        });
        assert_eq!(
            *s.renderer.port.selection_ids.borrow(),
            [a.clone(), b.clone()]
        );
        assert!(contains(&s.seen, "drawing:selected", Some(&a))); // primary = first selected
        assert_eq!(
            payloads.borrow().last().cloned(),
            Some((Some(a.clone()), vec![a.clone(), b.clone()]))
        );
        s.renderer.port.fire(DrawingIntent::Select {
            ids: vec![a],
            additive: true,
        });
        assert_eq!(*s.renderer.port.selection_ids.borrow(), [b]);
    }

    #[test]
    fn selecting_an_unknown_id_drops_it() {
        let s = setup();
        s.ctrl.select(&["nope".to_string()], false);
        assert!(s.renderer.port.selection_ids.borrow().is_empty());
    }

    #[test]
    fn last_used_style_seeds_the_next_drawing_of_the_same_type() {
        let s = setup();
        s.ctrl.add(
            "hline",
            AddInit {
                style: Some(color("#ff0000")),
                ..init_at(1.0, 1.0)
            },
        );
        let second = s.ctrl.add("hline", init_at(1.0, 2.0)).expect("added");
        assert_eq!(second.style.line_color, "#ff0000"); // inherited
        let third = s
            .ctrl
            .add(
                "hline",
                AddInit {
                    style: Some(color("#00ff00")),
                    ..init_at(1.0, 3.0)
                },
            )
            .expect("added");
        assert_eq!(third.style.line_color, "#00ff00"); // explicit wins
    }

    #[test]
    fn a_freshly_drawn_shape_inherits_the_last_used_style_of_its_type() {
        let s = setup();
        s.renderer
            .port
            .fire(DrawingIntent::Create { doc: hline_doc() });
        let id = ids(&s.ctrl)[0].clone();
        let mut edited = hline_doc();
        edited.id = id;
        edited.style = DrawingStyle::line("#ff0000", 1.0, LineStyle::Solid);
        s.renderer.port.fire(DrawingIntent::Edit { doc: edited });
        s.renderer
            .port
            .fire(DrawingIntent::Create { doc: hline_doc() }); // #fff, but should inherit #ff0000
        assert_eq!(s.ctrl.all()[1].style.line_color, "#ff0000");
    }

    #[test]
    fn re_arming_a_tool_pushes_its_last_used_style_to_the_renderer() {
        let s = setup();
        s.renderer
            .port
            .fire(DrawingIntent::Create { doc: hline_doc() });
        let id = ids(&s.ctrl)[0].clone();
        let mut edited = hline_doc();
        edited.id = id;
        edited.style = DrawingStyle::line("#ff0000", 3.0, LineStyle::Dashed);
        s.renderer.port.fire(DrawingIntent::Edit { doc: edited });
        s.ctrl.set_tool(Some("hline"));
        assert_eq!(
            s.renderer.port.active_tool.borrow().as_deref(),
            Some("hline")
        );
        assert_eq!(
            *s.renderer.port.active_tool_style.borrow(),
            Some(DrawingStyle::line("#ff0000", 3.0, LineStyle::Dashed))
        );
        s.ctrl.set_tool(None); // disarming carries no style
        assert_eq!(*s.renderer.port.active_tool_style.borrow(), None);
    }

    #[test]
    fn set_locked_and_set_visible_emit_edited() {
        let s = setup();
        s.renderer
            .port
            .fire(DrawingIntent::Create { doc: hline_doc() });
        let id = ids(&s.ctrl)[0].clone();
        s.seen.borrow_mut().clear();
        s.ctrl.set_locked(&id, true);
        s.ctrl.set_visible(&id, false);
        let d = &s.ctrl.all()[0];
        assert!(d.locked && !d.visible);
        let edited: Vec<_> = s
            .seen
            .borrow()
            .iter()
            .filter(|(n, _)| n == "drawing:edited")
            .cloned()
            .collect();
        assert_eq!(
            edited,
            [
                ("drawing:edited".to_string(), Some(id.clone())),
                ("drawing:edited".to_string(), Some(id)),
            ]
        );
    }

    #[test]
    fn a_drawing_under_the_series_survives_a_save_restore_and_is_undoable() {
        let s = setup();
        let d = s.ctrl.add("hline", init_at(1.0, 1.0)).expect("added");
        assert!(s.ctrl.all()[0].z_index > 0.0); // no shared z space: over the other drawings
        s.ctrl.update(
            &d.id,
            DrawingUpdate {
                z_index: Some(-3.0),
                ..Default::default()
            },
        );
        assert_eq!(s.ctrl.all()[0].z_index, -3.0);
        let synced = s.renderer.port.synced.borrow();
        assert_eq!(
            synced.iter().find(|x| x.id == d.id).map(|x| x.z_index),
            Some(-3.0)
        );
        drop(synced);
        let doc = serde_json::to_value(s.ctrl.to_json()).expect("json");
        s.ctrl.from_json(&doc);
        assert_eq!(s.ctrl.all()[0].z_index, -3.0);
        s.ctrl.update(
            &d.id,
            DrawingUpdate {
                z_index: Some(9.0),
                ..Default::default()
            },
        );
        s.ctrl.undo();
        assert_eq!(s.ctrl.all()[0].z_index, -3.0);
    }

    #[test]
    fn add_can_place_a_drawing_under_the_series_in_one_step() {
        let s = setup();
        let d = s
            .ctrl
            .add(
                "hline",
                AddInit {
                    z_index: Some(-5.0),
                    ..init_at(1.0, 1.0)
                },
            )
            .expect("added");
        assert_eq!(s.ctrl.all()[0].z_index, -5.0);
        let synced = s.renderer.port.synced.borrow();
        assert_eq!(
            synced.iter().find(|x| x.id == d.id).map(|x| x.z_index),
            Some(-5.0)
        );
        drop(synced);
        s.ctrl.undo();
        assert!(s.ctrl.all().is_empty()); // one undo step, not two
    }

    #[test]
    fn a_new_drawing_starts_just_under_the_price_and_front_back_clear_the_stack() {
        let s = setup();
        s.renderer.port.set_stack_range(|pane| {
            Some(if pane == "price" {
                StackRange {
                    front: 40.0,
                    back: -6.0,
                    price: Some(0.0),
                }
            } else {
                StackRange {
                    front: 0.0,
                    back: 0.0,
                    price: None,
                }
            })
        });
        let d = s.ctrl.add("hline", init_at(1.0, 1.0)).expect("added");
        assert!(d.z_index < 0.0); // under the candles
        assert!(d.z_index > -6.0); // but not behind the rest of the stack
        s.ctrl.send_to_back(&d.id);
        assert!(s.ctrl.all()[0].z_index < -6.0); // undercuts the whole stack
        s.ctrl.bring_to_front(&d.id);
        assert!(s.ctrl.all()[0].z_index > 40.0); // clears the raised indicator
    }

    #[test]
    fn a_series_covering_tool_starts_above_the_whole_series_stack() {
        let s = setup();
        s.renderer.port.set_stack_range(|_| {
            Some(StackRange {
                front: 40.0,
                back: -6.0,
                price: Some(0.0),
            })
        });
        let d = s
            .ctrl
            .add(
                "magnifier",
                AddInit {
                    anchors: Some(vec![pt(0.0, 1.0), pt(1.0, 2.0)]),
                    ..Default::default()
                },
            )
            .expect("added");
        assert!(d.z_index > 40.0);
    }

    #[test]
    fn without_a_price_key_a_new_drawing_starts_under_the_panes_top_series() {
        let s = setup();
        s.renderer.port.set_stack_range(|_| {
            Some(StackRange {
                front: 3.0,
                back: 1.0,
                price: None,
            })
        });
        let d = s
            .ctrl
            .add(
                "hline",
                AddInit {
                    pane_id: Some("pane-1".into()),
                    ..init_at(1.0, 1.0)
                },
            )
            .expect("added");
        assert!(d.z_index < 3.0);
        assert!(d.z_index > 2.0);
    }

    #[test]
    fn a_duplicate_keeps_its_sources_depth() {
        let s = setup();
        let d = s
            .ctrl
            .add(
                "hline",
                AddInit {
                    z_index: Some(-4.0),
                    ..init_at(1.0, 1.0)
                },
            )
            .expect("added");
        let clone = s.ctrl.duplicate(&[d.id.clone()]).remove(0);
        assert_eq!(clone.z_index, -4.0);
        // Tying its source, the clone paints just in front of it.
        assert_eq!(ids(&s.ctrl), [d.id, clone.id]);
    }

    #[test]
    fn programmatic_crud_are_atomic_undo_steps() {
        let s = setup();
        let d = s.ctrl.add("hline", init_at(1.0, 1.0)).expect("added");
        s.ctrl.update(
            &d.id,
            DrawingUpdate {
                locked: Some(true),
                ..Default::default()
            },
        );
        assert!(s.ctrl.all()[0].locked);
        s.ctrl.undo();
        assert!(!s.ctrl.all()[0].locked); // update reverted
        s.ctrl.undo();
        assert!(s.ctrl.all().is_empty()); // add reverted
    }

    #[test]
    fn a_partial_style_update_merges_onto_the_live_style() {
        let s = setup();
        let d = s
            .ctrl
            .add(
                "hline",
                AddInit {
                    style: Some(PartialDrawingStyle {
                        line_color: Some("#111111".into()),
                        line_width: Some(4.0),
                        ..Default::default()
                    }),
                    ..init_at(1.0, 1.0)
                },
            )
            .expect("added");
        s.ctrl.update(
            &d.id,
            DrawingUpdate {
                style: Some(color("#222222")),
                ..Default::default()
            },
        );
        let style = &s.ctrl.all()[0].style;
        assert_eq!(style.line_color, "#222222");
        assert_eq!(style.line_width, 4.0);
    }

    #[test]
    fn updating_an_unknown_drawing_changes_nothing_and_records_nothing() {
        let s = setup();
        s.seen.borrow_mut().clear();
        s.ctrl.update(
            "ghost",
            DrawingUpdate {
                locked: Some(true),
                ..Default::default()
            },
        );
        assert!(s.seen.borrow().is_empty());
        assert!(!s.ctrl.can_undo());
    }

    #[test]
    fn update_many_is_one_undo_step_and_emits_per_drawing() {
        let s = setup();
        let a = s.ctrl.add("hline", init_at(1.0, 1.0)).expect("a");
        let b = s.ctrl.add("hline", init_at(1.0, 2.0)).expect("b");
        s.seen.borrow_mut().clear();
        let lock = DrawingUpdate {
            locked: Some(true),
            ..Default::default()
        };
        s.ctrl.update_many(vec![
            (a.id.clone(), lock.clone()),
            ("ghost".to_string(), lock.clone()),
            (b.id.clone(), lock),
        ]);
        assert!(s.ctrl.all().iter().all(|d| d.locked));
        let edited: Vec<_> = s
            .seen
            .borrow()
            .iter()
            .filter(|(n, _)| n == "drawing:edited")
            .map(|(_, id)| id.clone())
            .collect();
        assert_eq!(edited, [Some(a.id), Some(b.id)]);
        s.ctrl.undo();
        assert!(s.ctrl.all().iter().all(|d| !d.locked));
        assert_eq!(s.ctrl.all().len(), 2);
    }

    #[test]
    fn remove_many_is_one_undo_step_and_prunes_the_selection() {
        let s = setup();
        let a = s.ctrl.add("hline", init_at(1.0, 1.0)).expect("a");
        let b = s.ctrl.add("hline", init_at(1.0, 2.0)).expect("b");
        let c = s.ctrl.add("hline", init_at(1.0, 3.0)).expect("c");
        s.ctrl.select(&[a.id.clone(), c.id.clone()], false);
        s.seen.borrow_mut().clear();
        s.ctrl
            .remove_many(&[a.id.clone(), b.id.clone(), "ghost".to_string()]);
        assert_eq!(ids(&s.ctrl), [c.id.clone()]);
        assert_eq!(*s.renderer.port.selection_ids.borrow(), [c.id.clone()]);
        let order: Vec<_> = s
            .seen
            .borrow()
            .iter()
            .map(|(n, i)| (n.clone(), i.clone()))
            .collect();
        assert_eq!(
            order,
            [
                ("drawing:selected".to_string(), Some(c.id.clone())),
                ("drawing:removed".to_string(), Some(a.id.clone())),
                ("drawing:removed".to_string(), Some(b.id.clone())),
            ]
        );
        s.ctrl.undo();
        assert_eq!(s.ctrl.all().len(), 3);
    }

    #[test]
    fn deleting_nothing_still_leaves_an_undo_entry_as_the_original_does() {
        let s = setup();
        s.ctrl.remove("ghost");
        assert!(s.ctrl.can_undo());
    }

    #[test]
    fn undo_reconciles_the_selection_against_what_survived() {
        let s = setup();
        let a = s.ctrl.add("hline", init_at(1.0, 1.0)).expect("a");
        s.ctrl.select(&[a.id.clone()], false);
        s.ctrl.undo(); // the add is reverted
        assert!(s.ctrl.all().is_empty());
        assert!(s.renderer.port.selection_ids.borrow().is_empty());
        assert!(contains(&s.seen, "drawing:selected", None));
    }

    #[test]
    fn from_json_clears_history_and_selection_silently() {
        let s = setup();
        let a = s.ctrl.add("hline", init_at(1.0, 1.0)).expect("a");
        s.ctrl.select(&[a.id.clone()], false);
        s.seen.borrow_mut().clear();
        s.ctrl.from_json(&json!({ "version": 1, "drawings": [] }));
        assert!(s.ctrl.all().is_empty());
        assert!(!s.ctrl.can_undo());
        assert!(s.seen.borrow().is_empty());
        assert!(s.renderer.port.synced.borrow().is_empty());
    }

    #[test]
    fn a_malformed_document_loads_as_empty() {
        let s = setup();
        s.ctrl.add("hline", init_at(1.0, 1.0));
        s.ctrl.from_json(&json!("junk"));
        assert!(s.ctrl.all().is_empty());
    }

    #[test]
    fn add_forwards_per_type_props() {
        let s = setup();
        let mut props = Props::new();
        props.insert("glyph".into(), json!("▲"));
        let d = s
            .ctrl
            .add(
                "iconstamp",
                AddInit {
                    pane_id: Some("price".into()),
                    anchors: Some(vec![pt(0.0, 100.0)]),
                    props: Some(props),
                    ..Default::default()
                },
            )
            .expect("added");
        assert_eq!(
            d.props.as_ref().and_then(|p| p.get("glyph")),
            Some(&json!("▲"))
        );
    }

    #[test]
    fn add_overlays_an_explicit_text_block_on_the_remembered_styling() {
        let s = setup();
        let mut text = DrawingText::new("hello");
        text.bold = Some(true);
        let d = s
            .ctrl
            .add(
                "text",
                AddInit {
                    text: Some(text),
                    ..init_at(1.0, 1.0)
                },
            )
            .expect("added");
        let text = d.text.expect("text");
        assert_eq!(text.value, "hello");
        assert_eq!(text.bold, Some(true));
    }

    // ── favorites ──

    #[test]
    fn set_favorite_round_trips_pushes_the_port_and_emits() {
        let s = setup();
        let seen: Rc<RefCell<Vec<Vec<String>>>> = Rc::default();
        {
            let seen = Rc::clone(&seen);
            s.events.on(ChartEventName::DrawingFavorites, move |event| {
                if let ChartEvent::DrawingFavorites { favorites } = event {
                    seen.borrow_mut().push(favorites.clone());
                }
            });
        }
        s.ctrl.set_favorite("trendline", true);
        s.ctrl.set_favorite("box", true);
        assert_eq!(s.ctrl.favorites(), strings(&["trendline", "box"]));
        assert!(s.ctrl.is_favorite("trendline"));
        assert_eq!(
            s.renderer.port.favorites_pushed.borrow().last().cloned(),
            Some(strings(&["trendline", "box"]))
        );
        assert_eq!(
            seen.borrow().last().cloned(),
            Some(strings(&["trendline", "box"]))
        );
        let n = seen.borrow().len();
        s.ctrl.set_favorite("trendline", true); // no-op toggles don't emit
        assert_eq!(seen.borrow().len(), n);
        s.ctrl.set_favorite("trendline", false);
        assert_eq!(s.ctrl.favorites(), strings(&["box"]));
        s.ctrl.set_favorite("nope-tool", true);
        assert_eq!(s.ctrl.favorites(), strings(&["box"]));
    }

    #[test]
    fn set_tool_shortcuts_pushes_the_hint_map_untouched() {
        let s = setup();
        let map: BTreeMap<String, String> = [
            ("trendline", "Alt+T"),
            ("hline", "Alt+H"),
            ("vline", "Alt+V"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        s.ctrl.set_tool_shortcuts(&map);
        assert_eq!(*s.renderer.port.shortcuts_pushed.borrow(), [map]);
    }

    #[test]
    fn starring_never_arms_a_tool() {
        let s = setup();
        s.ctrl.set_tool(Some("hline"));
        let pushes = s.renderer.port.count("set_active_tool");
        s.renderer.port.fire(DrawingIntent::Favorite {
            type_key: "trendline".into(),
            on: true,
        });
        assert!(s.ctrl.is_favorite("trendline"));
        assert_eq!(s.ctrl.get_tool().as_deref(), Some("hline"));
        assert_eq!(s.renderer.port.count("set_active_tool"), pushes);
    }

    #[test]
    fn set_favorites_bulk_replaces_and_drops_unknown_types() {
        let s = setup();
        s.ctrl
            .set_favorites(&strings(&["hline", "nope-tool", "ray"]));
        assert_eq!(s.ctrl.favorites(), strings(&["hline", "ray"]));
        let pushes = s.renderer.port.favorites_pushed.borrow().len();
        s.ctrl.set_favorites(&strings(&["ray", "hline"])); // same set: no-op
        assert_eq!(s.renderer.port.favorites_pushed.borrow().len(), pushes);
        s.renderer.port.fire(DrawingIntent::Favorite {
            type_key: "box".into(),
            on: true,
        });
        assert!(s.ctrl.is_favorite("box"));
        s.renderer.port.fire(DrawingIntent::Favorite {
            type_key: "box".into(),
            on: false,
        });
        assert!(!s.ctrl.is_favorite("box"));
    }

    // ── tool / mode seams ──

    fn collect<T: 'static>(
        events: &ChartEventBus,
        name: ChartEventName,
        pick: impl Fn(&ChartEvent) -> Option<T> + 'static,
    ) -> Rc<RefCell<Vec<T>>> {
        let out: Rc<RefCell<Vec<T>>> = Rc::default();
        let sink = Rc::clone(&out);
        events.on(name, move |event| {
            if let Some(v) = pick(event) {
                sink.borrow_mut().push(v);
            }
        });
        out
    }

    #[test]
    fn set_tool_emits_tool_on_change_only() {
        let s = setup();
        let tools = collect(&s.events, ChartEventName::DrawingTool, |e| match e {
            ChartEvent::DrawingTool { type_key } => Some(type_key.clone()),
            _ => None,
        });
        s.ctrl.set_tool(Some("trendline"));
        s.ctrl.set_tool(Some("trendline")); // port push, but no duplicate event
        s.renderer.port.fire(DrawingIntent::ToolFinished {
            type_key: "trendline".into(),
        });
        assert_eq!(*tools.borrow(), [Some("trendline".to_string()), None]);
        assert_eq!(s.ctrl.get_tool(), None);
        assert_eq!(s.renderer.port.count("set_active_tool"), 3);
    }

    #[test]
    fn an_arm_intent_arms_the_tool() {
        let s = setup();
        s.renderer.port.fire(DrawingIntent::Arm {
            type_key: Some("box".into()),
        });
        assert_eq!(s.ctrl.get_tool().as_deref(), Some("box"));
        assert_eq!(s.renderer.port.active_tool.borrow().as_deref(), Some("box"));
    }

    #[test]
    fn set_snap_mode_pushes_mirrors_emits_and_equal_values_no_op() {
        let s = setup();
        let snaps = collect(&s.events, ChartEventName::DrawingSnap, |e| match e {
            ChartEvent::DrawingSnap { mode } => Some(*mode),
            _ => None,
        });
        s.ctrl.set_snap_mode(SnapMode::Strong);
        s.ctrl.set_snap_mode(SnapMode::Strong);
        assert_eq!(*s.renderer.port.snap_modes.borrow(), [SnapMode::Strong]);
        assert_eq!(s.ctrl.get_snap_mode(), SnapMode::Strong);
        assert_eq!(*snaps.borrow(), [SnapMode::Strong]);
    }

    #[test]
    fn an_in_chart_magnet_click_mirrors_and_never_bounces_back() {
        let s = setup();
        let snaps = collect(&s.events, ChartEventName::DrawingSnap, |e| match e {
            ChartEvent::DrawingSnap { mode } => Some(*mode),
            _ => None,
        });
        s.renderer.port.fire(DrawingIntent::SnapMode {
            mode: SnapMode::Weak,
        });
        s.renderer.port.fire(DrawingIntent::SnapMode {
            mode: SnapMode::Weak,
        });
        assert_eq!(s.ctrl.get_snap_mode(), SnapMode::Weak);
        assert_eq!(*snaps.borrow(), [SnapMode::Weak]);
        assert!(s.renderer.port.snap_modes.borrow().is_empty());
    }

    #[test]
    fn set_stay_mode_pushes_mirrors_emits_and_equal_values_no_op() {
        let s = setup();
        let stays = collect(&s.events, ChartEventName::DrawingStay, |e| match e {
            ChartEvent::DrawingStay { on } => Some(*on),
            _ => None,
        });
        s.ctrl.set_stay_mode(true);
        s.ctrl.set_stay_mode(true);
        assert_eq!(*s.renderer.port.stay_modes.borrow(), [true]);
        assert!(s.ctrl.get_stay_mode());
        assert_eq!(*stays.borrow(), [true]);
    }

    #[test]
    fn an_in_chart_stay_mode_click_mirrors_and_never_bounces_back() {
        let s = setup();
        let stays = collect(&s.events, ChartEventName::DrawingStay, |e| match e {
            ChartEvent::DrawingStay { on } => Some(*on),
            _ => None,
        });
        s.renderer.port.fire(DrawingIntent::StayMode { on: true });
        s.renderer.port.fire(DrawingIntent::StayMode { on: true });
        assert!(s.ctrl.get_stay_mode());
        assert_eq!(*stays.borrow(), [true]);
        assert!(s.renderer.port.stay_modes.borrow().is_empty());
    }

    #[test]
    fn set_mode_pushes_and_the_renderer_intent_reports_the_outcome() {
        let s = setup();
        let modes = collect(&s.events, ChartEventName::DrawingMode, |e| match e {
            ChartEvent::DrawingMode { mode } => Some(*mode),
            _ => None,
        });
        s.ctrl.set_mode(Some(DrawingMode::Measure));
        assert_eq!(
            *s.renderer.port.modes.borrow(),
            [Some(DrawingMode::Measure)]
        );
        assert_eq!(s.ctrl.get_mode(), Some(DrawingMode::Measure));
        // The renderer applies + echoes the same value: dropped (no duplicate event).
        s.renderer.port.fire(DrawingIntent::Mode {
            mode: Some(DrawingMode::Measure),
        });
        // A mutual-exclusion side effect (a tool armed in-chart) exits the mode.
        s.renderer.port.fire(DrawingIntent::Mode { mode: None });
        assert_eq!(s.ctrl.get_mode(), None);
        assert_eq!(*modes.borrow(), [Some(DrawingMode::Measure), None]);
    }

    #[test]
    fn mode_snap_and_stay_setters_are_inert_without_a_port() {
        let s = setup_with(false, None);
        s.ctrl.set_snap_mode(SnapMode::Strong);
        s.ctrl.set_stay_mode(true);
        s.ctrl.set_mode(Some(DrawingMode::Eraser));
        assert_eq!(s.ctrl.get_snap_mode(), SnapMode::Off);
        assert!(!s.ctrl.get_stay_mode());
        assert_eq!(s.ctrl.get_mode(), None);
    }

    #[test]
    fn a_draft_intent_is_re_emitted_without_touching_the_store() {
        let s = setup();
        let drafts = collect(&s.events, ChartEventName::DrawingDraft, |e| match e {
            ChartEvent::DrawingDraft { doc } => Some(doc.as_ref().map(|d| d.id.clone())),
            _ => None,
        });
        s.renderer.port.fire(DrawingIntent::Draft {
            doc: Some(hline_doc()),
        });
        s.renderer.port.fire(DrawingIntent::Draft { doc: None });
        assert_eq!(*drafts.borrow(), [Some("renderer-temp".to_string()), None]);
        assert!(s.ctrl.all().is_empty());
        assert!(!s.ctrl.can_undo());
    }

    #[test]
    fn undo_redo_copy_paste_duplicate_intents_route() {
        let s = setup();
        let port = &s.renderer.port;
        port.fire(DrawingIntent::Create { doc: hline_doc() });
        let id = ids(&s.ctrl)[0].clone();
        port.fire(DrawingIntent::Duplicate {
            ids: vec![id.clone()],
        });
        assert_eq!(s.ctrl.all().len(), 2);
        port.fire(DrawingIntent::Undo);
        assert_eq!(s.ctrl.all().len(), 1);
        port.fire(DrawingIntent::Redo);
        assert_eq!(s.ctrl.all().len(), 2);
        port.fire(DrawingIntent::Copy { ids: vec![id] });
        port.fire(DrawingIntent::Paste);
        assert_eq!(s.ctrl.all().len(), 3);
    }

    #[test]
    fn a_reorder_intent_brings_to_front_or_sends_to_back() {
        let s = setup();
        let a = s.ctrl.add("hline", init_at(1.0, 1.0)).expect("a");
        s.ctrl.add("hline", init_at(1.0, 2.0));
        s.renderer.port.fire(DrawingIntent::Reorder {
            id: a.id.clone(),
            to: ReorderTo::Front,
        });
        assert_eq!(ids(&s.ctrl).last(), Some(&a.id));
        s.renderer.port.fire(DrawingIntent::Reorder {
            id: a.id.clone(),
            to: ReorderTo::Back,
        });
        assert_eq!(ids(&s.ctrl).first(), Some(&a.id));
    }

    // ── destroy ──

    #[test]
    fn destroy_unsubscribes_and_stops_syncing() {
        let s = setup();
        assert!(s.renderer.port.subscribed());
        s.ctrl.add("hline", init_at(1.0, 1.0));
        let syncs = s.renderer.port.sync_count.get();
        s.ctrl.destroy();
        assert!(!s.renderer.port.subscribed());
        s.ctrl.add("hline", init_at(1.0, 2.0));
        assert_eq!(s.ctrl.all().len(), 2); // the model still works
        assert_eq!(s.renderer.port.sync_count.get(), syncs); // the renderer is no longer told
    }

    #[test]
    fn the_renderer_callback_does_not_keep_a_dropped_controller_alive() {
        let s = setup();
        let weak = Rc::downgrade(&s.ctrl.inner);
        let Setup {
            renderer,
            ctrl,
            events,
            ..
        } = s;
        drop(ctrl);
        assert!(weak.upgrade().is_none());
        // The intent arrives after the controller is gone: nothing happens.
        renderer
            .port
            .fire(DrawingIntent::Create { doc: hline_doc() });
        drop(events);
    }

    #[test]
    fn intents_after_destroy_are_ignored_even_if_the_renderer_still_delivers() {
        let s = setup();
        s.ctrl.destroy();
        // A renderer that ignores the unsubscribe: simulate by calling the controller directly.
        s.ctrl
            .on_intent(&DrawingIntent::Create { doc: hline_doc() });
        assert!(s.ctrl.all().is_empty());
    }

    // ── re-entrancy ──

    #[test]
    fn an_event_handler_may_call_back_into_the_controller() {
        let s = setup();
        let ctrl = s.ctrl.clone();
        let locked_in_handler: Rc<RefCell<Vec<usize>>> = Rc::default();
        {
            let sink = Rc::clone(&locked_in_handler);
            s.events.on(ChartEventName::DrawingCreated, move |event| {
                if let ChartEvent::DrawingCreated { id } = event {
                    ctrl.set_locked(id, true);
                    sink.borrow_mut().push(ctrl.all().len());
                }
            });
        }
        s.ctrl.add("hline", init_at(1.0, 1.0));
        assert_eq!(*locked_in_handler.borrow(), [1]);
        assert!(s.ctrl.all()[0].locked);
    }

    // ── remembered tool settings ──

    const ANCHORS: [(f64, f64); 2] = [(1000.0, 10.0), (2000.0, 20.0)];

    fn two_point_init() -> AddInit {
        AddInit {
            anchors: Some(ANCHORS.iter().map(|&(t, p)| pt(t, p)).collect()),
            ..Default::default()
        }
    }

    fn placed(type_key: &str) -> SerializedDrawing {
        SerializedDrawing {
            id: "renderer-temp".into(),
            type_key: type_key.into(),
            pane_id: "price".into(),
            anchors: ANCHORS.iter().map(|&(t, p)| pt(t, p)).collect(),
            style: DrawingStyle::line("#2962ff", 2.0, LineStyle::Solid),
            text: None,
            locked: false,
            visible: true,
            z_index: 0.0,
            created_at: 0.0,
            props: None,
            show_on: None,
        }
    }

    fn announced(events: &ChartEventBus, name: ChartEventName) -> Rc<RefCell<Vec<String>>> {
        collect(events, name, |e| match e {
            ChartEvent::DrawingDefaults { type_key }
            | ChartEvent::DrawingTemplates { type_key } => Some(type_key.clone()),
            _ => None,
        })
    }

    #[test]
    fn a_trend_line_after_a_styled_one_keeps_its_look_but_not_its_words() {
        let s = setup();
        s.renderer.port.fire(DrawingIntent::Create {
            doc: placed("trendline"),
        });
        let mut edited = s.ctrl.all()[0].clone();
        edited.style = DrawingStyle::line("#ff0000", 3.0, LineStyle::Dashed);
        let mut text = DrawingText::new("Weekly support");
        text.color = Some("#00ff00".into());
        text.size = crate::model::BoxTextSize::Large;
        text.bold = Some(true);
        edited.text = Some(text);
        s.renderer.port.fire(DrawingIntent::Edit { doc: edited });
        s.renderer.port.fire(DrawingIntent::Create {
            doc: placed("trendline"),
        });
        let next = s.ctrl.all()[1].clone();
        assert_eq!(next.style.line_color, "#ff0000");
        assert_eq!(next.style.line_width, 3.0);
        assert_eq!(next.style.line_style, LineStyle::Dashed);
        let text = next.text.expect("text styling");
        assert_eq!(text.color.as_deref(), Some("#00ff00"));
        assert_eq!(text.size, crate::model::BoxTextSize::Large);
        assert_eq!(text.bold, Some(true));
        assert_eq!(text.value, "");
    }

    #[test]
    fn a_fib_after_a_customized_one_keeps_its_levels_and_direction() {
        let s = setup();
        s.renderer.port.fire(DrawingIntent::Create {
            doc: placed("fibretracement"),
        });
        let mut first = s.ctrl.all()[0].clone();
        let levels = json!([
            { "ratio": 0.0, "color": "#111111", "enabled": true },
            { "ratio": 0.705, "color": "#222222", "enabled": true },
            { "ratio": 1.0, "color": "#333333", "enabled": false },
        ]);
        let mut props = first.props.clone().unwrap_or_default();
        props.insert("levels".into(), levels.clone());
        props.insert("reverse".into(), json!(true));
        first.props = Some(props);
        s.renderer.port.fire(DrawingIntent::Edit { doc: first });
        s.renderer.port.fire(DrawingIntent::Create {
            doc: placed("fibretracement"),
        });
        let next = s.ctrl.all()[1].clone();
        let props = next.props.expect("props");
        assert_eq!(props.get("levels"), Some(&levels));
        assert_eq!(props.get("reverse"), Some(&json!(true)));
    }

    #[test]
    fn styling_several_drawings_at_once_sets_the_default_for_each_tool() {
        let s = setup();
        s.renderer.port.fire(DrawingIntent::Create {
            doc: placed("trendline"),
        });
        s.renderer
            .port
            .fire(DrawingIntent::Create { doc: placed("box") });
        let all = s.ctrl.all();
        let restyle = |mut d: SerializedDrawing| {
            d.style.line_color = "#abcdef".into();
            d
        };
        s.renderer.port.fire(DrawingIntent::EditMany {
            docs: all.into_iter().map(restyle).collect(),
        });
        assert_eq!(
            s.ctrl
                .add("trendline", two_point_init())
                .expect("line")
                .style
                .line_color,
            "#abcdef"
        );
        assert_eq!(
            s.ctrl
                .add("box", two_point_init())
                .expect("box")
                .style
                .line_color,
            "#abcdef"
        );
    }

    #[test]
    fn an_explicit_style_to_add_wins_over_the_remembered_one() {
        let s = setup();
        s.ctrl.add(
            "trendline",
            AddInit {
                style: Some(PartialDrawingStyle {
                    line_color: Some("#ff0000".into()),
                    line_width: Some(4.0),
                    ..Default::default()
                }),
                ..two_point_init()
            },
        );
        let next = s
            .ctrl
            .add(
                "trendline",
                AddInit {
                    style: Some(color("#00ff00")),
                    ..two_point_init()
                },
            )
            .expect("added");
        assert_eq!(next.style.line_color, "#00ff00");
        assert_eq!(next.style.line_width, 4.0);
    }

    #[test]
    fn arming_a_tool_hands_the_renderer_its_whole_remembered_settings() {
        let s = setup();
        let d = s
            .ctrl
            .add("fibretracement", two_point_init())
            .expect("added");
        let levels = json!([{ "ratio": 0.5, "color": "#123456", "enabled": true }]);
        let mut props = Props::new();
        props.insert("levels".into(), levels.clone());
        s.ctrl.update(
            &d.id,
            DrawingUpdate {
                props: Some(props),
                ..Default::default()
            },
        );
        s.ctrl.set_tool(Some("fibretracement"));
        let pushed = s
            .renderer
            .port
            .active_defaults
            .borrow()
            .clone()
            .expect("defaults");
        assert_eq!(
            pushed.props.and_then(|p| p.get("levels").cloned()),
            Some(levels)
        );
    }

    #[test]
    fn a_settings_change_is_announced_and_moving_the_drawing_is_not() {
        let s = setup();
        let seen = announced(&s.events, ChartEventName::DrawingDefaults);
        s.renderer.port.fire(DrawingIntent::Create {
            doc: placed("trendline"),
        });
        assert_eq!(*seen.borrow(), ["trendline"]); // placed in a non-factory color: remembered
        seen.borrow_mut().clear();
        let d = s.ctrl.all()[0].clone();
        let mut moved = d.clone();
        moved.anchors = vec![pt(1000.0, 10.0), pt(3000.0, 30.0)];
        s.renderer.port.fire(DrawingIntent::Edit { doc: moved });
        assert!(seen.borrow().is_empty());
        let mut thicker = d;
        thicker.style.line_width = 4.0;
        s.renderer.port.fire(DrawingIntent::Edit { doc: thicker });
        assert_eq!(*seen.borrow(), ["trendline"]);
    }

    #[test]
    fn moving_an_older_drawing_keeps_the_settings_the_user_picked_last() {
        let s = setup();
        let seen = announced(&s.events, ChartEventName::DrawingDefaults);
        let styled = |c: &str| AddInit {
            style: Some(color(c)),
            ..two_point_init()
        };
        let red = s.ctrl.add("trendline", styled("#ff0000")).expect("red");
        let plain = s.ctrl.add("trendline", styled("#38c0fd")).expect("plain");
        s.ctrl.add("trendline", styled("#0000ff"));
        seen.borrow_mut().clear();
        let moved = vec![pt(1000.0, 10.0), pt(3000.0, 30.0)];
        let mut red_moved = red.clone();
        red_moved.anchors = moved.clone();
        s.renderer.port.fire(DrawingIntent::Edit { doc: red_moved });
        let mut red_back = red.clone();
        red_back.anchors = ANCHORS.iter().map(|&(t, p)| pt(t, p)).collect();
        let mut plain_moved = plain.clone();
        plain_moved.anchors = moved;
        s.renderer.port.fire(DrawingIntent::EditMany {
            docs: vec![red_back, plain_moved],
        });
        assert!(seen.borrow().is_empty());
        assert_eq!(
            s.ctrl
                .add("trendline", two_point_init())
                .expect("next")
                .style
                .line_color,
            "#0000ff"
        );
    }

    #[test]
    fn a_drawing_left_on_its_factory_look_remembers_nothing() {
        let s = setup();
        let seen = announced(&s.events, ChartEventName::DrawingDefaults);
        let d = s
            .ctrl
            .add("fibretracement", two_point_init())
            .expect("added");
        assert!(s.ctrl.tool_defaults().is_empty());
        assert!(seen.borrow().is_empty());
        let factory = d.style.clone();
        let mut thicker = factory.clone();
        thicker.line_width = 3.0;
        s.ctrl.update(
            &d.id,
            DrawingUpdate {
                style: Some(thicker.to_partial()),
                ..Default::default()
            },
        );
        assert_eq!(
            s.ctrl.tool_defaults().keys().cloned().collect::<Vec<_>>(),
            ["fibretracement"]
        );
        s.ctrl.update(
            &d.id,
            DrawingUpdate {
                style: Some(factory.to_partial()),
                ..Default::default()
            },
        ); // styled back to the factory look
        assert!(s.ctrl.tool_defaults().is_empty());
    }

    #[test]
    fn tool_defaults_round_trip_to_another_chart() {
        let a = setup();
        a.ctrl.add(
            "trendline",
            AddInit {
                style: Some(color("#ff0000")),
                ..two_point_init()
            },
        );
        let saved = serde_json::to_value(a.ctrl.tool_defaults()).expect("json");

        let b = setup();
        let seen = announced(&b.events, ChartEventName::DrawingDefaults);
        b.ctrl.set_tool_defaults(&saved);
        assert_eq!(*seen.borrow(), ["trendline"]);
        assert_eq!(
            b.ctrl
                .add("trendline", two_point_init())
                .expect("added")
                .style
                .line_color,
            "#ff0000"
        );
    }

    #[test]
    fn restoring_the_same_set_again_announces_nothing() {
        let s = setup();
        let seen = announced(&s.events, ChartEventName::DrawingDefaults);
        let saved = json!({ "trendline": { "style": { "lineColor": "#ff0000" } } });
        s.ctrl.set_tool_defaults(&saved);
        seen.borrow_mut().clear();
        s.ctrl.set_tool_defaults(&saved.clone());
        assert!(seen.borrow().is_empty());
    }

    #[test]
    fn restoring_defaults_drops_unknown_tools_and_malformed_entries() {
        let s = setup();
        s.ctrl.set_tool_defaults(&json!({
            "trendline": { "style": { "lineColor": "#ff0000" } },
            "notatool": { "style": {} },
            "box": "junk",
            "hline": [1, 2],
        }));
        assert_eq!(
            s.ctrl.tool_defaults().keys().cloned().collect::<Vec<_>>(),
            ["trendline"]
        );
        s.ctrl.set_tool_defaults(&Value::Null); // an absent map empties the set
        assert!(s.ctrl.tool_defaults().is_empty());
    }

    #[test]
    fn restoring_defaults_for_the_armed_tool_re_seeds_the_preview() {
        let s = setup();
        s.ctrl.set_tool(Some("trendline"));
        let pushes = s.renderer.port.count("set_active_tool");
        s.ctrl
            .set_tool_defaults(&json!({ "trendline": { "style": { "lineColor": "#ff0000" } } }));
        assert_eq!(s.renderer.port.count("set_active_tool"), pushes + 1);
        let style = s
            .renderer
            .port
            .active_tool_style
            .borrow()
            .clone()
            .expect("style");
        assert_eq!(style.line_color, "#ff0000");
    }

    #[test]
    fn resetting_a_tool_sends_its_next_drawing_back_to_the_factory_defaults() {
        let s = setup();
        let seen = announced(&s.events, ChartEventName::DrawingDefaults);
        let factory = s
            .ctrl
            .add("trendline", two_point_init())
            .expect("a")
            .style
            .line_color;
        s.ctrl.add(
            "trendline",
            AddInit {
                style: Some(color("#ff0000")),
                ..two_point_init()
            },
        );
        seen.borrow_mut().clear();
        s.ctrl.reset_tool_defaults(Some("trendline"));
        assert_eq!(*seen.borrow(), ["trendline"]);
        assert_eq!(
            s.ctrl
                .add("trendline", two_point_init())
                .expect("c")
                .style
                .line_color,
            factory
        );
        s.ctrl.reset_tool_defaults(Some("trendline")); // nothing left to forget
        assert_eq!(seen.borrow().len(), 1);
    }

    #[test]
    fn resetting_without_a_type_forgets_every_tool() {
        let s = setup();
        s.ctrl.set_tool_defaults(&json!({
            "trendline": { "style": { "lineColor": "#ff0000" } },
            "box": { "style": { "lineColor": "#00ff00" } },
        }));
        let seen = announced(&s.events, ChartEventName::DrawingDefaults);
        s.ctrl.reset_tool_defaults(None);
        assert_eq!(seen.borrow().len(), 2);
        assert!(s.ctrl.tool_defaults().is_empty());
    }

    #[test]
    fn a_chart_without_interactive_drawings_still_saves_and_restores_defaults() {
        let s = setup_with(false, None);
        s.ctrl
            .set_tool_defaults(&json!({ "trendline": { "style": { "lineColor": "#ff0000" } } }));
        assert_eq!(
            serde_json::to_value(s.ctrl.tool_defaults()).expect("json"),
            json!({ "trendline": { "style": { "lineColor": "#ff0000" } } })
        );
    }

    // ── saved styles ──

    fn red() -> DrawingToolDefaults {
        DrawingToolDefaults {
            style: Some(PartialDrawingStyle {
                line_color: Some("#ff0000".into()),
                line_width: Some(3.0),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn blue() -> DrawingToolDefaults {
        DrawingToolDefaults {
            style: Some(color("#0000ff")),
            ..Default::default()
        }
    }

    fn template_names(ctrl: &DrawingController, type_key: &str) -> Vec<String> {
        ctrl.tool_templates()
            .get(type_key)
            .map(|list| list.iter().map(|t| t.name.clone()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn a_style_saved_from_the_settings_panel_is_listed_pushed_and_announced() {
        let s = setup();
        let seen = announced(&s.events, ChartEventName::DrawingTemplates);
        s.renderer.port.fire(DrawingIntent::TemplateSave {
            type_key: "trendline".into(),
            name: "Weekly".into(),
            settings: red(),
        });
        let templates = s.ctrl.tool_templates();
        assert_eq!(templates.len(), 1);
        assert_eq!(templates["trendline"][0].name, "Weekly");
        assert_eq!(templates["trendline"][0].settings, red());
        assert_eq!(
            s.renderer.port.templates_pushed.borrow()["trendline"][0].name,
            "Weekly"
        );
        assert_eq!(*seen.borrow(), ["trendline"]);
    }

    #[test]
    fn saving_under_a_name_already_in_use_replaces_that_style() {
        let s = setup();
        s.ctrl.save_tool_template("trendline", "Weekly", &red());
        s.ctrl.save_tool_template("trendline", "Daily", &blue());
        s.ctrl.save_tool_template("trendline", "Weekly", &blue());
        assert_eq!(template_names(&s.ctrl, "trendline"), ["Daily", "Weekly"]);
        assert!(s.ctrl.tool_templates()["trendline"].iter().all(|t| {
            t.settings
                .style
                .as_ref()
                .and_then(|st| st.line_color.as_deref())
                == Some("#0000ff")
        }));
    }

    #[test]
    fn deleting_a_style_removes_only_that_one() {
        let s = setup();
        s.ctrl.save_tool_template("box", "Demand", &red());
        s.ctrl.save_tool_template("box", "Supply", &blue());
        s.renderer.port.fire(DrawingIntent::TemplateRemove {
            type_key: "box".into(),
            name: "Demand".into(),
        });
        assert_eq!(template_names(&s.ctrl, "box"), ["Supply"]);
        s.ctrl.remove_tool_template("box", "Supply");
        assert!(s.ctrl.tool_templates().is_empty());
        let seen = announced(&s.events, ChartEventName::DrawingTemplates);
        s.ctrl.remove_tool_template("box", "Supply"); // already gone: silent
        assert!(seen.borrow().is_empty());
    }

    #[test]
    fn a_style_without_a_name_or_for_an_unknown_tool_is_not_saved() {
        let s = setup();
        s.ctrl.save_tool_template("trendline", "   ", &red());
        s.ctrl.save_tool_template("notatool", "x", &red());
        assert!(s.ctrl.tool_templates().is_empty());
    }

    #[test]
    fn saved_styles_round_trip_dropping_unknown_tools_and_malformed_entries() {
        let a = setup();
        a.ctrl.save_tool_template("trendline", "Weekly", &red());
        let mut saved = a.ctrl.tool_templates();
        let mut json_map = serde_json::Map::new();
        for (type_key, list) in std::mem::take(&mut saved) {
            let entries: Vec<Value> = list
                .iter()
                .map(|t| json!({ "name": t.name, "settings": serde_json::to_value(&t.settings).expect("json") }))
                .collect();
            json_map.insert(type_key, Value::Array(entries));
        }
        json_map.insert(
            "notatool".into(),
            json!([{ "name": "x", "settings": serde_json::to_value(red()).expect("json") }]),
        );
        json_map.insert(
            "box".into(),
            json!([
                { "name": "" },
                "junk",
                { "name": "Ok", "settings": serde_json::to_value(blue()).expect("json") }
            ]),
        );

        let b = setup();
        let seen = announced(&b.events, ChartEventName::DrawingTemplates);
        b.ctrl.set_tool_templates(&Value::Object(json_map.clone()));
        assert_eq!(template_names(&b.ctrl, "trendline"), ["Weekly"]);
        assert_eq!(template_names(&b.ctrl, "box"), ["Ok"]);
        assert_eq!(b.ctrl.tool_templates().len(), 2);
        let mut sorted = seen.borrow().clone();
        sorted.sort();
        assert_eq!(sorted, ["box", "trendline"]);
        seen.borrow_mut().clear();
        b.ctrl.set_tool_templates(&Value::Object(json_map)); // unchanged: silent
        assert!(seen.borrow().is_empty());
    }

    #[test]
    fn a_saved_fib_style_keeps_its_levels() {
        let s = setup();
        let mut props = Props::new();
        let levels = json!([{ "ratio": 0.705, "color": "#123456", "enabled": true }]);
        props.insert("levels".into(), levels.clone());
        let registry = DrawingRegistry::builtin();
        let mut fib = registry
            .create(
                "fibretracement",
                DrawingInit::on_pane("price").anchors(vec![pt(0.0, 1.0), pt(1.0, 2.0)]),
            )
            .expect("fib");
        fib.apply_props(&props);
        s.ctrl.save_tool_template(
            "fibretracement",
            "OTE only",
            &capture_tool_defaults(fib.as_ref()),
        );
        let saved = &s.ctrl.tool_templates()["fibretracement"][0];
        assert_eq!(
            saved.settings.props.as_ref().and_then(|p| p.get("levels")),
            Some(&levels)
        );
    }

    #[test]
    fn ready_made_styles_are_offered_per_tool_family() {
        for type_key in ["trendline", "hline", "ray"] {
            let names: Vec<String> = builtin_tool_templates(type_key)
                .into_iter()
                .map(|t| t.name)
                .collect();
            assert_eq!(names, ["Support", "Resistance", "Projection"]);
        }
        for type_key in ["box", "parallelchannel"] {
            let names: Vec<String> = builtin_tool_templates(type_key)
                .into_iter()
                .map(|t| t.name)
                .collect();
            assert_eq!(names, ["Demand", "Supply"]);
        }
    }

    // ── timeframe visibility (show_on rides the document) ──

    #[test]
    fn a_timeframe_limit_survives_a_save_and_a_restore_and_can_be_lifted() {
        let a = setup_with(true, None);
        let d = a.ctrl.add("hline", init_at(1000.0, 10.0)).expect("added");
        a.ctrl.update(
            &d.id,
            DrawingUpdate {
                show_on: Some(Some(strings(&["D", "W"]))),
                ..Default::default()
            },
        );
        let saved = serde_json::to_value(a.ctrl.to_json()).expect("json");

        let b = setup_with(true, None);
        b.ctrl.from_json(&saved);
        let restored = b.ctrl.all()[0].clone();
        assert_eq!(restored.show_on, Some(strings(&["D", "W"])));

        b.ctrl.update(
            &restored.id,
            DrawingUpdate {
                show_on: Some(None),
                ..Default::default()
            },
        );
        assert_eq!(b.ctrl.all()[0].show_on, None);
        let json = serde_json::to_value(b.ctrl.to_json()).expect("json");
        assert!(json["drawings"][0].get("showOn").is_none());
    }

    #[test]
    fn an_edit_intent_without_a_limit_lifts_one() {
        let s = setup();
        let d = s.ctrl.add("hline", init_at(1000.0, 10.0)).expect("added");
        s.ctrl.update(
            &d.id,
            DrawingUpdate {
                show_on: Some(Some(strings(&["D"]))),
                ..Default::default()
            },
        );
        let mut doc = s.ctrl.all()[0].clone();
        doc.show_on = None;
        s.renderer.port.fire(DrawingIntent::Edit { doc });
        assert_eq!(s.ctrl.all()[0].show_on, None);
    }

    // ── toolbar ──

    #[test]
    fn set_toolbar_and_relocalize_push_a_fresh_definition() {
        let s = setup();
        assert_eq!(s.renderer.port.toolbar_pushes.get(), 1);
        s.ctrl.set_toolbar(DrawingsOption::Config(
            crate::drawings::toolbar::DrawingsConfig {
                tools: Some(strings(&["hline"])),
                ..Default::default()
            },
        ));
        assert_eq!(s.renderer.port.toolbar_pushes.get(), 2);
        let after_set = s.renderer.port.toolbar.borrow().clone().expect("toolbar");
        assert_eq!(after_set["groups"].as_array().map(Vec::len), Some(1));
        s.ctrl.relocalize(); // re-reads the stored option
        assert_eq!(s.renderer.port.toolbar_pushes.get(), 3);
        assert_eq!(*s.renderer.port.toolbar.borrow(), Some(after_set));
        // set_toolbar pushes the definition only: the visibility is not re-sent.
        assert_eq!(s.renderer.port.count("show_toolbar"), 1);
    }

    #[test]
    fn the_toolbar_is_read_in_the_charts_language_each_time() {
        let renderer = FakeRenderer::new(FakeOptions::default());
        let ctrl = DrawingController::new(
            renderer.clone(),
            ChartEventBus::new(),
            Rc::new(DrawingRegistry::builtin()),
            None,
            None,
            Rc::new(|| Some(Rc::new(EnglishProxy) as Rc<dyn Translator>)),
        );
        ctrl.relocalize();
        let toolbar = renderer.port.toolbar.borrow().clone().expect("toolbar");
        assert_eq!(toolbar["groups"][0]["label"], json!("[Lines]"));
    }

    /// A translator that brackets every label, to prove the provider is consulted.
    struct EnglishProxy;

    impl Translator for EnglishProxy {
        fn locale(&self) -> &str {
            english_translator().locale()
        }
        fn direction(&self) -> crate::i18n::TextDirection {
            english_translator().direction()
        }
        fn numbering_system(&self) -> &str {
            english_translator().numbering_system()
        }
        fn hour_cycle(&self) -> crate::format::types::HourCycleSetting {
            english_translator().hour_cycle()
        }
        fn week_start(&self) -> u8 {
            english_translator().week_start()
        }
        fn t_with(
            &self,
            message: &crate::i18n::MessageLike,
            args: &[(&str, crate::i18n::MessageArg)],
        ) -> String {
            format!("[{}]", english_translator().t_with(message, args))
        }
        fn format(&self) -> &crate::format::formatters::ResolvedFormatters {
            english_translator().format()
        }
    }

    #[test]
    fn show_toolbar_and_the_ghost_forward_to_the_port() {
        let s = setup();
        s.ctrl.show_toolbar(false);
        assert_eq!(s.renderer.port.visible.get(), Some(false));
        s.ctrl.set_external_ghost(Some(&hline_doc()));
        assert_eq!(
            s.renderer
                .port
                .ghost
                .borrow()
                .as_ref()
                .map(|d| d.id.clone()),
            Some("renderer-temp".into())
        );
        s.ctrl.set_external_ghost(None);
        assert!(s.renderer.port.ghost.borrow().is_none());
        let headless = setup_with(false, None);
        headless.ctrl.show_toolbar(true);
        headless.ctrl.set_external_ghost(None);
        assert!(headless.renderer.port.log.borrow().is_empty());
    }

    #[test]
    fn clone_drawing_returns_the_clone() {
        let s = setup();
        let d = s.ctrl.add("hline", init_at(1.0, 1.0)).expect("added");
        let clone = s.ctrl.clone_drawing(&d.id).expect("clone");
        assert_ne!(clone.id, d.id);
        assert!(s.ctrl.clone_drawing("ghost").is_none());
    }
}
