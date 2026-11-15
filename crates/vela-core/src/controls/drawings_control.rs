//! The chart's drawing control surface (`chart.drawings`), a sibling of
//! `chart.data` / `chart.renderer`. Activate tools, create and mutate drawings
//! programmatically, and persist them. Always present; on a renderer without
//! the `user_drawings` capability the interactive methods warn and no-op, while
//! `to_json` / `from_json` / `all` still round-trip because the model is
//! core-owned.
//!
//! Chainable methods return `&Self` (the original's `this`). Where the original
//! hands back a live `Drawing`, the methods return a [`SerializedDrawing`]
//! snapshot (see [`DrawingController`]). `clone` is [`DrawingsControl::clone_drawing`],
//! and `select(ids | id | null)` is [`select`](DrawingsControl::select) /
//! [`select_id`](DrawingsControl::select_id) (an empty slice clears).

use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::Value;

use crate::drawings::controller::{AddInit, DrawingController, DrawingUpdate};
use crate::drawings::toolbar::DrawingsOption;
use crate::drawings::{
    DrawingToolDefaults, DrawingToolTemplate, DrawingsDocument, SerializedDrawing, SnapMode,
};
use crate::ports::chart_renderer::DrawingMode;
use crate::runtime::Platform;

/// The `chart.drawings` facade over a [`DrawingController`].
pub struct DrawingsControl {
    ctrl: DrawingController,
    platform: Rc<dyn Platform>,
}

impl DrawingsControl {
    /// `platform` carries the warning of an ignored interactive call.
    pub fn new(ctrl: DrawingController, platform: Rc<dyn Platform>) -> Self {
        Self { ctrl, platform }
    }

    /// Whether the active renderer supports interactive drawings.
    pub fn supported(&self) -> bool {
        self.ctrl.supported()
    }

    /// Arm a tool: the next click(s) place it. `None` returns to select/idle.
    pub fn set_tool(&self, type_key: Option<&str>) -> &Self {
        if self.ok("setTool") {
            self.ctrl.set_tool(type_key);
        }
        self
    }

    /// The currently armed tool (`None` = select/idle). Mirrors in-chart toolbar
    /// clicks and one-shot tools disarming: listen on `drawing:tool` to follow
    /// changes.
    pub fn get_tool(&self) -> Option<String> {
        self.ctrl.get_tool()
    }

    /// Set the sticky magnet snap mode.
    pub fn set_snap_mode(&self, mode: SnapMode) -> &Self {
        if self.ok("setSnapMode") {
            self.ctrl.set_snap_mode(mode);
        }
        self
    }

    /// The current magnet snap mode: follow changes on `drawing:snap`.
    pub fn get_snap_mode(&self) -> SnapMode {
        self.ctrl.get_snap_mode()
    }

    /// Keep the armed tool after each placement (`true`) or one-shot disarm (`false`).
    pub fn set_stay_mode(&self, on: bool) -> &Self {
        if self.ok("setStayMode") {
            self.ctrl.set_stay_mode(on);
        }
        self
    }

    /// Whether tools stay armed after placement: follow changes on `drawing:stay`.
    pub fn get_stay_mode(&self) -> bool {
        self.ctrl.get_stay_mode()
    }

    /// Enter/exit a renderer-local mode: measure (transient ruler), eraser, or
    /// `None`. Mutually exclusive with each other and with any armed tool: the
    /// renderer enforces the exclusion and the outcome lands on `drawing:mode`.
    pub fn set_mode(&self, mode: Option<DrawingMode>) -> &Self {
        if self.ok("setMode") {
            self.ctrl.set_mode(mode);
        }
        self
    }

    /// The current renderer-local mode: follow changes on `drawing:mode`.
    pub fn get_mode(&self) -> Option<DrawingMode> {
        self.ctrl.get_mode()
    }

    /// Show or hide the on-chart drawing toolbar.
    pub fn show_toolbar(&self, visible: bool) -> &Self {
        if self.ok("showToolbar") {
            self.ctrl.show_toolbar(visible);
        }
        self
    }

    /// Reconfigure the toolbar groups/tools live.
    pub fn set_toolbar(&self, option: DrawingsOption) -> &Self {
        if self.ok("setToolbar") {
            self.ctrl.set_toolbar(option);
        }
        self
    }

    /// Show per-tool shortcut hints in the toolbar flyouts. Values are
    /// PRE-FORMATTED display strings (`Alt+T`): the host owns the keymap and the
    /// platform formatting, so hints always match the host's actual bindings.
    pub fn set_tool_shortcuts(&self, map: &BTreeMap<String, String>) -> &Self {
        if self.ok("setToolShortcuts") {
            self.ctrl.set_tool_shortcuts(map);
        }
        self
    }

    /// Display another chart's in-progress placement as a GHOST at reduced
    /// opacity (`None` clears it): how a multi-chart host mirrors
    /// `drawing:draft` onto linked charts. Never a real drawing: no store entry,
    /// no selection, no persistence. Silently inert on a renderer without the
    /// optional seam.
    pub fn set_external_ghost(&self, doc: Option<&SerializedDrawing>) -> &Self {
        self.ctrl.set_external_ghost(doc);
        self
    }

    /// Create a drawing programmatically (no clicking). Returns a snapshot of it,
    /// or `None` if unsupported.
    pub fn add(&self, type_key: &str, init: AddInit) -> Option<SerializedDrawing> {
        if !self.ok("add") {
            return None;
        }
        self.ctrl.add(type_key, init)
    }

    pub fn remove(&self, id: &str) -> &Self {
        self.ctrl.remove(id);
        self
    }

    /// Apply a partial record to a drawing: for a custom (headless) settings UI.
    pub fn update(&self, id: &str, patch: DrawingUpdate) -> &Self {
        self.ctrl.update(id, patch);
        self
    }

    /// Apply several partial records as one undo step (group hide/lock/reorder
    /// from a host UI).
    pub fn update_many(&self, patches: Vec<(String, DrawingUpdate)>) -> &Self {
        self.ctrl.update_many(patches);
        self
    }

    /// Remove several drawings as one undo step.
    pub fn remove_many(&self, ids: &[String]) -> &Self {
        self.ctrl.remove_many(ids);
        self
    }

    pub fn lock(&self, id: &str, locked: bool) -> &Self {
        self.ctrl.set_locked(id, locked);
        self
    }

    pub fn show(&self, id: &str, visible: bool) -> &Self {
        self.ctrl.set_visible(id, visible);
        self
    }

    /// Select drawings on the chart (highlight + open their toolbar): for a
    /// host-side tree/list. An empty slice clears the selection.
    pub fn select(&self, ids: &[String]) -> &Self {
        if self.ok("select") {
            self.ctrl.select(ids, false);
        }
        self
    }

    /// Select one drawing on the chart.
    pub fn select_id(&self, id: &str) -> &Self {
        self.select(&[id.to_string()])
    }

    /// Open a drawing's settings popup on the chart (selecting it): the twin of
    /// clicking it.
    pub fn open_settings(&self, id: &str) -> &Self {
        if self.ok("openSettings") {
            self.ctrl.open_settings(id);
        }
        self
    }

    pub fn bring_to_front(&self, id: &str) -> &Self {
        self.ctrl.bring_to_front(id);
        self
    }

    pub fn send_to_back(&self, id: &str) -> &Self {
        self.ctrl.send_to_back(id);
        self
    }

    // ── undo / redo (core-owned: work regardless of renderer support) ──

    /// Revert the last edit. No-op when there is nothing to undo.
    pub fn undo(&self) -> &Self {
        self.ctrl.undo();
        self
    }

    /// Re-apply the last undone edit. No-op when there is nothing to redo.
    pub fn redo(&self) -> &Self {
        self.ctrl.redo();
        self
    }

    pub fn can_undo(&self) -> bool {
        self.ctrl.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.ctrl.can_redo()
    }

    // ── clone / clipboard ──

    /// Duplicate a drawing in place; the clone is selected.
    pub fn clone_drawing(&self, id: &str) -> &Self {
        if self.ok("clone") {
            self.ctrl.clone_drawing(id);
        }
        self
    }

    /// Duplicate several drawings in place; the clones become the selection.
    pub fn duplicate(&self, ids: &[String]) -> &Self {
        if self.ok("duplicate") {
            self.ctrl.duplicate(ids);
        }
        self
    }

    /// Copy drawings into the in-memory clipboard for a later [`paste`](Self::paste).
    pub fn copy_to_clipboard(&self, ids: &[String]) -> &Self {
        if self.ok("copy") {
            self.ctrl.copy(ids);
        }
        self
    }

    /// Paste the clipboard as new drawings (fresh ids), selecting them.
    pub fn paste(&self) -> &Self {
        if self.ok("paste") {
            self.ctrl.paste();
        }
        self
    }

    /// Every drawing as plain JSON, in paint order.
    pub fn all(&self) -> Vec<SerializedDrawing> {
        self.ctrl.all()
    }

    /// Snapshot all drawings as a versioned document (persistence).
    pub fn to_json(&self) -> DrawingsDocument {
        self.ctrl.to_json()
    }

    /// Restore drawings from a document produced by [`to_json`](Self::to_json)
    /// (untrusted-safe).
    pub fn from_json(&self, doc: &Value) -> &Self {
        self.ctrl.from_json(doc);
        self
    }

    /// The favorite tool types (starred in the toolbar flyouts), insertion-ordered.
    pub fn favorites(&self) -> Vec<String> {
        self.ctrl.favorites()
    }

    pub fn is_favorite(&self, type_key: &str) -> bool {
        self.ctrl.is_favorite(type_key)
    }

    /// Star/unstar one tool type.
    pub fn set_favorite(&self, type_key: &str, on: bool) -> &Self {
        if self.ok("setFavorite") {
            self.ctrl.set_favorite(type_key, on);
        }
        self
    }

    /// Replace the whole favorite set (e.g. restoring persisted prefs).
    pub fn set_favorites(&self, types: &[String]) -> &Self {
        if self.ok("setFavorites") {
            self.ctrl.set_favorites(types);
        }
        self
    }

    /// Every tool's remembered settings, by type: what the next drawing of each
    /// type starts from (its style, text styling and per-type settings such as
    /// fib levels). Plain JSON: persist it and hand it back to
    /// [`set_tool_defaults`](Self::set_tool_defaults) on the next load.
    pub fn tool_defaults(&self) -> BTreeMap<String, DrawingToolDefaults> {
        self.ctrl.tool_defaults()
    }

    /// Replace every tool's remembered settings (e.g. restoring persisted
    /// prefs). Malformed entries and unknown types are dropped. Follow changes on
    /// `drawing:defaults`.
    pub fn set_tool_defaults(&self, map: &Value) -> &Self {
        self.ctrl.set_tool_defaults(map);
        self
    }

    /// Forget one tool's remembered settings (every tool's with `None`).
    pub fn reset_tool_defaults(&self, type_key: Option<&str>) -> &Self {
        self.ctrl.reset_tool_defaults(type_key);
        self
    }

    /// Every tool's saved looks ("styles" in the settings panel), by type.
    pub fn tool_templates(&self) -> BTreeMap<String, Vec<DrawingToolTemplate>> {
        self.ctrl.tool_templates()
    }

    /// Replace every tool's saved looks (e.g. restoring persisted prefs). Follow
    /// changes on `drawing:templates`.
    pub fn set_tool_templates(&self, map: &Value) -> &Self {
        self.ctrl.set_tool_templates(map);
        self
    }

    /// Alias mirroring `renderer.getConfig()` for symmetry.
    pub fn get_config(&self) -> DrawingsDocument {
        self.ctrl.to_json()
    }

    /// Alias mirroring `renderer.applyConfig()` for symmetry.
    pub fn apply_config(&self, doc: &Value) -> &Self {
        self.ctrl.from_json(doc);
        self
    }

    fn ok(&self, op: &str) -> bool {
        if self.ctrl.supported() {
            return true;
        }
        self.platform.warn(&format!(
            "[vela] chart.drawings.{op}() ignored \u{2014} the active renderer has no userDrawings capability."
        ));
        false
    }
}

#[cfg(test)]
#[allow(clippy::cloned_ref_to_slice_refs, clippy::type_complexity)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::drawings::DrawingPoint;
    use crate::drawings::DrawingRegistry;
    use crate::drawings::controller::no_translator;
    use crate::drawings::controller::test_support::{FakeOptions, FakeRenderer};
    use crate::drawings::style::DrawingStyle;
    use crate::events::types::ChartEventBus;
    use crate::model::LineStyle;
    use crate::runtime::testing::ManualPlatform;

    struct Setup {
        renderer: Rc<FakeRenderer>,
        platform: ManualPlatform,
        control: DrawingsControl,
    }

    fn setup(user_drawings: bool) -> Setup {
        let renderer = FakeRenderer::new(FakeOptions {
            user_drawings,
            timeline_marks: false,
        });
        let platform = ManualPlatform::new(0.0);
        let ctrl = DrawingController::new(
            renderer.clone(),
            ChartEventBus::new(),
            Rc::new(DrawingRegistry::builtin()),
            None,
            None,
            no_translator(),
        );
        let control = DrawingsControl::new(ctrl, platform.handle());
        Setup {
            renderer,
            platform,
            control,
        }
    }

    fn hline_doc(id: &str) -> Value {
        serde_json::to_value(crate::drawings::SerializedDrawing {
            id: id.into(),
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
        })
        .expect("json")
    }

    #[test]
    fn warns_on_interactive_methods_when_unsupported_and_still_round_trips_json() {
        let s = setup(false);
        let c = &s.control;
        assert!(!c.supported());
        assert!(c.add("hline", AddInit::default()).is_none());
        c.set_tool(Some("trendline")).show_toolbar(true);
        assert_eq!(
            s.platform.warnings(),
            [
                "[vela] chart.drawings.add() ignored \u{2014} the active renderer has no userDrawings capability.",
                "[vela] chart.drawings.setTool() ignored \u{2014} the active renderer has no userDrawings capability.",
                "[vela] chart.drawings.showToolbar() ignored \u{2014} the active renderer has no userDrawings capability.",
            ]
        );
        c.from_json(&json!({ "version": 1, "drawings": [hline_doc("dw-3")] }));
        assert_eq!(c.to_json().drawings[0].id, "dw-3");
        assert_eq!(c.get_config().drawings.len(), 1);
    }

    #[test]
    fn model_methods_work_without_support_and_without_a_warning() {
        let s = setup(false);
        let c = &s.control;
        c.from_json(&json!({ "version": 1, "drawings": [hline_doc("dw-1")] }));
        c.lock("dw-1", true).show("dw-1", false).undo().redo();
        c.update(
            "dw-1",
            DrawingUpdate {
                z_index: Some(5.0),
                ..Default::default()
            },
        );
        c.remove_many(&["dw-1".to_string()]);
        assert!(c.all().is_empty());
        assert!(c.can_undo());
        assert!(!c.can_redo());
        assert!(s.platform.warnings().is_empty());
    }

    #[test]
    fn every_interactive_method_warns_by_its_original_name() {
        let s = setup(false);
        let c = &s.control;
        let ids = ["x".to_string()];
        c.set_snap_mode(SnapMode::Weak)
            .set_stay_mode(true)
            .set_mode(Some(DrawingMode::Measure))
            .set_toolbar(DrawingsOption::Bool(true))
            .set_tool_shortcuts(&BTreeMap::new())
            .select(&[])
            .select_id("x")
            .open_settings("x")
            .clone_drawing("x")
            .duplicate(&ids)
            .copy_to_clipboard(&ids)
            .paste()
            .set_favorite("hline", true)
            .set_favorites(&[]);
        let ops: Vec<String> = s
            .platform
            .warnings()
            .iter()
            .map(|w| {
                w.trim_start_matches("[vela] chart.drawings.")
                    .split("()")
                    .next()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            ops,
            [
                "setSnapMode",
                "setStayMode",
                "setMode",
                "setToolbar",
                "setToolShortcuts",
                "select",
                "select",
                "openSettings",
                "clone",
                "duplicate",
                "copy",
                "paste",
                "setFavorite",
                "setFavorites",
            ]
        );
    }

    #[test]
    fn a_supported_control_chains_and_mirrors_the_controller() {
        let s = setup(true);
        let c = &s.control;
        assert!(c.supported());
        let out = c
            .set_tool(Some("trendline"))
            .set_snap_mode(SnapMode::Strong)
            .set_stay_mode(true);
        assert!(std::ptr::eq(out, c));
        assert_eq!(c.get_tool().as_deref(), Some("trendline"));
        assert_eq!(c.get_snap_mode(), SnapMode::Strong);
        assert!(c.get_stay_mode());
        assert_eq!(c.get_mode(), None);
        let d = c
            .add(
                "hline",
                AddInit {
                    anchors: Some(vec![DrawingPoint {
                        time: 1.0,
                        price: 2.0,
                    }]),
                    ..Default::default()
                },
            )
            .expect("added");
        c.lock(&d.id, true).select_id(&d.id).open_settings(&d.id);
        assert!(c.all()[0].locked);
        assert_eq!(*s.renderer.port.selection_ids.borrow(), [d.id.clone()]);
        assert_eq!(
            s.renderer.port.settings_opened_for.borrow().as_deref(),
            Some(d.id.as_str())
        );
        c.select(&[]);
        assert!(s.renderer.port.selection_ids.borrow().is_empty());
        c.clone_drawing(&d.id)
            .copy_to_clipboard(&[d.id.clone()])
            .paste();
        assert_eq!(c.all().len(), 3);
        c.undo();
        assert_eq!(c.all().len(), 2);
        c.set_favorite("box", true)
            .set_favorites(&["hline".to_string()]);
        assert_eq!(c.favorites(), ["hline"]);
        assert!(c.is_favorite("hline"));
        assert!(s.platform.warnings().is_empty());
    }

    #[test]
    fn tool_defaults_and_templates_pass_through() {
        let s = setup(false);
        let c = &s.control;
        c.set_tool_defaults(&json!({ "trendline": { "style": { "lineColor": "#ff0000" } } }));
        assert_eq!(c.tool_defaults().len(), 1);
        c.reset_tool_defaults(None);
        assert!(c.tool_defaults().is_empty());
        c.set_tool_templates(&json!({ "box": [{ "name": "A", "settings": { "style": { "lineColor": "#00ff00" } } }] }));
        assert_eq!(c.tool_templates()["box"][0].name, "A");
        c.apply_config(&json!({ "version": 1, "drawings": [] }));
        assert!(c.all().is_empty());
    }
}
