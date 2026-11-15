//! What the popup's and the panel's controls act through: the original's `SettingsActions`.
//!
//! A [`Core`] names the drawings an open popup (or panel) edits, keeps the [`Snap`]s the
//! controls read, and forwards every edit to the controller's `popup_*` methods. It owns no
//! element and no closure, so the controls can hold it freely.
//!
//! The controller answers an edit by syncing the document back, which reaches the DOM as a
//! `SyncPanel` event from inside the very handler that made the edit. Controls are therefore
//! refreshed from a timer ([`DrawingsUi::request_sync`]), never from inside that handler.

use std::cell::{Ref, RefCell};
use std::rc::{Rc, Weak};

use serde_json::Value;
use vela_core::drawings::{DrawingToolDefaults, DrawingToolTemplate, Props, SerializedDrawing};
use vela_core::ports::chart_renderer::ReorderTo;

use super::DrawingsUi;
use super::model::Snap;
use crate::dialogs::Tr;

pub struct Core {
    ui: Weak<DrawingsUi>,
    /// The drawings the popup edits: one id, or a whole multi-selection.
    pub ids: Vec<String>,
    pub multi: bool,
    snaps: RefCell<Vec<Snap>>,
}

impl Core {
    pub fn new(ui: &Rc<DrawingsUi>, ids: Vec<String>, multi: bool) -> Rc<Core> {
        let core = Rc::new(Core {
            ui: Rc::downgrade(ui),
            ids,
            multi,
            snaps: RefCell::new(Vec::new()),
        });
        core.refresh();
        core
    }

    fn ui(&self) -> Option<Rc<DrawingsUi>> {
        self.ui.upgrade()
    }

    /// Re-read every drawing from the controller. False once none of them is left (deleted,
    /// undone away).
    pub fn refresh(&self) -> bool {
        let Some(ui) = self.ui() else { return false };
        let Some(ctl) = ui.ctl() else { return false };
        let background = ui.chrome_background();
        let snaps: Vec<Snap> = self
            .ids
            .iter()
            .filter_map(|id| ctl.with_drawing(id, |d| Snap::of(d, &background)))
            .collect();
        let any = !snaps.is_empty();
        *self.snaps.borrow_mut() = snaps;
        any
    }

    pub fn snaps(&self) -> Ref<'_, Vec<Snap>> {
        self.snaps.borrow()
    }

    /// The first drawing (the one a single-drawing popup edits), if it is still there.
    pub fn first(&self) -> Option<Snap> {
        self.snaps.borrow().first().cloned()
    }

    pub fn tr(&self) -> Tr {
        self.ui().map(|u| u.tr()).unwrap_or_default()
    }

    /// One chart bar in ms (0 when unknown): timeframe pickers drop the choices at or above it.
    pub fn chart_bar_ms(&self) -> f64 {
        self.ui()
            .and_then(|u| u.renderer())
            .map_or(0.0, |r| r.chart_bar_ms())
    }

    pub fn time_zone(&self) -> String {
        self.ui()
            .and_then(|u| u.renderer())
            .map_or_else(|| "UTC".to_string(), |r| r.timezone())
    }

    pub fn price_decimals(&self) -> Option<f64> {
        self.ui()
            .and_then(|u| u.renderer())
            .and_then(|r| r.price_decimals())
    }

    fn run(&self, f: impl FnOnce(&vela_core::render::drawings::UserDrawingController)) {
        DrawingsUi::with_ctl(&self.ui, f);
        self.refresh();
        if let Some(ui) = self.ui() {
            ui.request_sync();
        }
    }

    /// Apply `{ path: value }` settings to every drawing, as one edit.
    pub fn patch(&self, items: &[(&str, Value)]) {
        let mut props = Props::new();
        for (path, value) in items {
            props.insert((*path).to_string(), value.clone());
        }
        self.run(|c| c.popup_patch(&props));
    }

    pub fn set_locked(&self, locked: bool) {
        self.run(|c| c.popup_set_locked(locked));
    }

    pub fn reorder(&self, to: ReorderTo) {
        self.run(|c| c.popup_reorder(to));
    }

    pub fn duplicate(&self) {
        self.run(|c| c.popup_duplicate());
    }

    pub fn reset_settings(&self) {
        self.run(|c| c.popup_reset_settings());
    }

    pub fn remove(&self) {
        self.run(|c| c.popup_remove());
    }

    /// Put a serialized snapshot back (Cancel in the panel).
    pub fn restore(&self, doc: &SerializedDrawing) {
        self.run(|c| c.popup_restore(doc));
    }

    pub fn templates(&self) -> Vec<DrawingToolTemplate> {
        self.ui()
            .and_then(|u| u.ctl())
            .map(|c| c.popup_tool_templates())
            .unwrap_or_default()
    }

    pub fn save_template(&self, name: &str) {
        self.run(|c| c.popup_save_template(name));
    }

    pub fn remove_template(&self, name: &str) {
        self.run(|c| c.popup_remove_template(name));
    }

    pub fn apply_tool_settings(&self, settings: &DrawingToolDefaults) {
        self.run(|c| c.popup_apply_tool_settings(settings));
    }

    /// Show a style on the drawing without committing it; `None` puts the drawing back.
    pub fn preview(&self, settings: Option<&DrawingToolDefaults>) {
        DrawingsUi::with_ctl(&self.ui, |c| c.popup_preview(settings));
        self.refresh();
    }

    /// A position's stop / target price for a value typed in the Price or Points unit.
    pub fn position_level_price(&self, stop: bool, points: bool, value: f64) -> f64 {
        use vela_core::drawings::types::measure::{PositionLevel, PositionLevelMode, PositionTool};
        let Some(id) = self.ids.first() else {
            return value;
        };
        let Some(ctl) = self.ui().and_then(|u| u.ctl()) else {
            return value;
        };
        ctl.with_drawing(id, |d| {
            d.as_any().downcast_ref::<PositionTool>().map(|p| {
                p.level_price_from_display(
                    if stop {
                        PositionLevel::Stop
                    } else {
                        PositionLevel::Target
                    },
                    if points {
                        PositionLevelMode::Points
                    } else {
                        PositionLevelMode::Price
                    },
                    value,
                )
            })
        })
        .flatten()
        .unwrap_or(value)
    }
}

impl Core {
    /// A string of the chart's plot theme (`upColor`, `downColor`, `textColor`, `background`).
    pub fn theme_color(&self, key: &str) -> Option<String> {
        let theme = self.ui()?.renderer()?.plot_theme();
        theme.get(key).and_then(|v| v.as_str()).map(str::to_string)
    }
}

impl Core {
    /// Run `f` on the first drawing's snapshot (no copy), if it is still there.
    pub fn with_first<R>(&self, f: impl FnOnce(&Snap) -> R) -> Option<R> {
        self.snaps.borrow().first().map(f)
    }
}
