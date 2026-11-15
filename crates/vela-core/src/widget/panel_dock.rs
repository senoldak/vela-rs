//! The PANEL DOCK: the one owner of the shell's side-panel column. It holds the shell's own
//! panels (object tree, data window) and every contributed one as equal entries, enforces
//! the single-open rule, and carries the dock's persistable state (which panel is open, the
//! widths the user dragged, the floating panels the user pinned).
//!
//! Both shells drive the same dock, so the exclusivity rule and the button wiring exist
//! ONCE. The chrome is reached through [`PanelChrome`] (the topbar and the sidebar implement
//! it): the dock never imports them.
//!
//! This is the pure half of the original `PanelDock`: [`PanelDockModel`] owns one
//! [`SidePanelModel`] per entry and turns every toggle into an ordered list of
//! [`DockEffect`]s for the browser crate to apply. Left for the browser crate: constructing
//! the panel elements (a contributed descriptor's `mount` runs there, with its failures
//! kept off the shell: log the error, leave the panel docked and empty), the chrome calls
//! the effects ask for, and the listener notifications (one listener's failure spares the
//! rest).

use std::collections::{BTreeMap, BTreeSet};

use super::contributions::{DEFAULT_PANEL_ORDER, SidePanelButton};
use super::side_panel::{ResizeDrag, SidePanelModel, SidePanelOptions, ToggleOutcome};
use crate::i18n::{MessageLike, Translator, text_of};
use crate::state::PanelsState;

/// What the dock needs from whatever chrome shows its toggles.
pub trait PanelChrome {
    /// Replace the panel toggle group (dock order). The click handler receives the panel id.
    fn set_panel_buttons(&self, buttons: &[SidePanelButton]);
    /// Reflect one panel's open state on its button.
    fn set_panel_active(&self, id: &str, open: bool);
}

/// One docked panel.
pub struct DockEntry {
    pub id: String,
    pub title: MessageLike,
    pub icon: String,
    pub order: f64,
    pub panel: SidePanelModel,
    /// Contributed entries are created (and destroyed) by the dock; built-ins by the shell.
    pub contributed: bool,
}

impl DockEntry {
    /// A panel the SHELL owns and destroys: the dock only docks it.
    pub fn built_in(
        id: &str,
        title: impl Into<MessageLike>,
        icon: &str,
        order: f64,
        opts: SidePanelOptions,
    ) -> Self {
        Self {
            id: id.to_string(),
            title: title.into(),
            icon: icon.to_string(),
            order,
            panel: SidePanelModel::new(opts),
            contributed: false,
        }
    }

    /// A contributed panel: order defaults after the built-ins.
    pub fn contributed(
        id: &str,
        title: impl Into<MessageLike>,
        icon: &str,
        order: Option<f64>,
        opts: SidePanelOptions,
    ) -> Self {
        Self {
            order: order.unwrap_or(DEFAULT_PANEL_ORDER),
            contributed: true,
            ..Self::built_in(id, title, icon, 0.0, opts)
        }
    }
}

/// What the browser crate owes after a dock operation, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum DockEffect {
    /// Apply this outcome to the panel `id`'s element (show, hide, animate, announce).
    Panel { id: String, outcome: ToggleOutcome },
    /// Reflect the open state on the chrome's button.
    ChromeActive { id: String, open: bool },
    /// A contributed panel's handle `on_open`.
    HandleOpen { id: String },
    /// The dock's persistable state changed (open panel, or a width): the shell saves.
    Changed,
    /// Tell the plugins reading the dock that it changed.
    Notify,
}

/// The dock.
#[derive(Default)]
pub struct PanelDockModel {
    entries: Vec<DockEntry>,
    /// Widths the USER settled, by panel id: the only ones worth persisting.
    widths: BTreeMap<String, f64>,
    /// Floating panels the USER pinned as columns, by id: remembered for late registrations.
    pinned: BTreeSet<String>,
    /// A restored `open` naming a panel that has not registered yet: honored when it docks.
    pending_open: Option<String>,
}

impl PanelDockModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entries(&self) -> &[DockEntry] {
        &self.entries
    }

    pub fn entry(&self, id: &str) -> Option<&DockEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Dock a panel: entries stay sorted by `order` (registration order breaks ties), the
    /// user's stored width and pin apply, and a restored `open` naming it opens it now.
    pub fn add(&mut self, mut entry: DockEntry) -> Vec<DockEffect> {
        let id = entry.id.clone();
        if let Some(px) = self.widths.get(&id) {
            entry.panel.set_width(*px);
        }
        if self.pinned.contains(&id) {
            entry.panel.set_overlay(false);
        }
        let at = self
            .entries
            .iter()
            .position(|e| e.order > entry.order)
            .unwrap_or(self.entries.len());
        self.entries.insert(at, entry);
        let mut effects = Vec::new();
        if self.pending_open.as_deref() == Some(id.as_str()) {
            effects.extend(self.toggle_panel(&id, Some(true), false));
        }
        effects.push(DockEffect::Notify);
        effects
    }

    /// Remove a panel (a contributed one gone from the registry, or replaced).
    pub fn drop_entry(&mut self, id: &str) -> Vec<DockEffect> {
        let before = self.entries.len();
        self.entries.retain(|e| e.id != id);
        if self.entries.len() == before {
            return Vec::new();
        }
        vec![DockEffect::Notify]
    }

    /// The open panel's id, or `None` when the column is closed.
    pub fn open_id(&self) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.panel.is_open())
            .map(|e| e.id.as_str())
    }

    /// Open/close one panel by id: `None` flips it. Unknown ids are ignored. Opening one
    /// closes whichever was showing (instantly: the dock stays exclusive).
    pub fn toggle(&mut self, id: &str, open: Option<bool>) -> Vec<DockEffect> {
        self.toggle_panel(id, open, false)
    }

    fn toggle_panel(&mut self, id: &str, open: Option<bool>, instant: bool) -> Vec<DockEffect> {
        let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) else {
            return Vec::new();
        };
        let outcome = entry.panel.toggle(open, instant);
        let mut effects = vec![DockEffect::Panel {
            id: id.to_string(),
            outcome,
        }];
        let Some(now_open) = outcome.notify_open else {
            return effects;
        };
        // The panel's own open-change hook.
        if now_open {
            let others: Vec<String> = self
                .entries
                .iter()
                .filter(|e| e.id != id)
                .map(|e| e.id.clone())
                .collect();
            for other in others {
                effects.extend(self.toggle_panel(&other, Some(false), true));
            }
            // The user chose a column: a stale restore must not steal it.
            self.pending_open = None;
        }
        effects.push(DockEffect::ChromeActive {
            id: id.to_string(),
            open: now_open,
        });
        if now_open {
            effects.push(DockEffect::HandleOpen { id: id.to_string() });
        }
        effects.push(DockEffect::Changed);
        effects.push(DockEffect::Notify);
        effects
    }

    /// The USER settled a new width (drag release, or a double-click reset).
    pub fn on_width_change(&mut self, id: &str, px: f64) -> Vec<DockEffect> {
        self.widths.insert(id.to_string(), px);
        vec![DockEffect::Changed]
    }

    /// The USER pinned (`overlay` false) or unpinned a floating panel.
    pub fn on_placement_change(&mut self, id: &str, overlay: bool) -> Vec<DockEffect> {
        if overlay {
            self.pinned.remove(id);
        } else {
            self.pinned.insert(id.to_string());
        }
        vec![DockEffect::Changed]
    }

    /// A panel maximized or restored.
    pub fn on_maximized_change(&self) -> Vec<DockEffect> {
        vec![DockEffect::Notify]
    }

    // ── the entries' own state (the dock's copy is the one source of truth) ──

    /// One entry's panel state (open, width, placement, maximized).
    pub fn panel(&self, id: &str) -> Option<&SidePanelModel> {
        self.entry(id).map(|e| &e.panel)
    }

    /// Whether entry `id` covers the shell's charts right now.
    pub fn is_maximized(&self, id: &str) -> bool {
        self.panel(id).is_some_and(SidePanelModel::maximized)
    }

    /// Maximize or restore entry `id` (a no-op on a panel not declared `maximizable`, and
    /// on an unknown id). A real change owes [`DockEffect::Notify`].
    pub fn set_maximized(&mut self, id: &str, maximized: bool) -> Vec<DockEffect> {
        let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) else {
            return Vec::new();
        };
        if entry.panel.set_maximized(maximized) {
            self.on_maximized_change()
        } else {
            Vec::new()
        }
    }

    /// The exit animation of entry `id`'s close ended (the view hides it): a closed panel
    /// reopens at its own size, so a maximize is undone here. A real change owes
    /// [`DockEffect::Notify`].
    pub fn exit_finished(&mut self, id: &str) -> Vec<DockEffect> {
        let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) else {
            return Vec::new();
        };
        if entry.panel.exit_finished() {
            self.on_maximized_change()
        } else {
            Vec::new()
        }
    }

    /// Resize entry `id` (clamped), silently: nothing is persisted. Whether the width
    /// changed.
    pub fn set_width(&mut self, id: &str, px: f64) -> bool {
        self.entries
            .iter_mut()
            .find(|e| e.id == id)
            .is_some_and(|e| e.panel.set_width(px))
    }

    /// Start a resizer drag of entry `id` at pointer x.
    pub fn begin_resize(&self, id: &str, client_x: f64) -> Option<ResizeDrag> {
        self.panel(id).map(|p| p.begin_resize(client_x))
    }

    /// A drag move: whether the width changed (the view repaints).
    pub fn resize_to(&mut self, id: &str, drag: &ResizeDrag, client_x: f64) -> bool {
        self.entries
            .iter_mut()
            .find(|e| e.id == id)
            .is_some_and(|e| e.panel.resize_to(drag, client_x))
    }

    /// The drag ended: the width the user settled is remembered (and persisted) when it
    /// changed.
    pub fn end_resize(&mut self, id: &str, drag: &ResizeDrag) -> Vec<DockEffect> {
        let settled = self.panel(id).and_then(|p| p.end_resize(drag));
        match settled {
            Some(px) => self.on_width_change(id, px),
            None => Vec::new(),
        }
    }

    /// A double-click on the handle: back to the declared width, remembered when it moved.
    pub fn reset_width(&mut self, id: &str) -> Vec<DockEffect> {
        let settled = self
            .entries
            .iter_mut()
            .find(|e| e.id == id)
            .and_then(|e| e.panel.reset_width());
        match settled {
            Some(px) => self.on_width_change(id, px),
            None => Vec::new(),
        }
    }

    /// The USER pinned (`overlay` false) or floated entry `id` with its header pin: the
    /// placement changes and is remembered. A panel not declared `overlay` ignores it.
    pub fn set_overlay(&mut self, id: &str, overlay: bool) -> Vec<DockEffect> {
        let changed = self
            .entries
            .iter_mut()
            .find(|e| e.id == id)
            .is_some_and(|e| e.panel.set_overlay(overlay));
        if changed {
            self.on_placement_change(id, overlay)
        } else {
            Vec::new()
        }
    }

    /// The docked panels in dock order: what a non-topbar chrome (the mobile three-dots
    /// drawer) lists so every panel stays reachable there too.
    pub fn list(&self, i18n: Option<&dyn Translator>) -> Vec<SidePanelButton> {
        self.entries
            .iter()
            .map(|e| SidePanelButton {
                id: e.id.clone(),
                title: text_of(i18n, &e.title, &[]),
                icon: e.icon.clone(),
            })
            .collect()
    }

    /// The dock's persistable state, or `None` when there is nothing worth saving.
    pub fn get_state(&self) -> Option<PanelsState> {
        let open = self.open_id().map(str::to_string);
        let widths = (!self.widths.is_empty()).then(|| self.widths.clone());
        let pinned =
            (!self.pinned.is_empty()).then(|| self.pinned.iter().cloned().collect::<Vec<_>>());
        if open.is_none() && widths.is_none() && pinned.is_none() {
            return None;
        }
        Some(PanelsState {
            open,
            widths,
            pinned,
        })
    }

    /// Restore a persisted dock state. Widths apply to any panel present, and are
    /// remembered for panels that register later; `open` opens that panel, and its absence
    /// closes the column (a document that predates the dock has no `panels` field at all,
    /// so the shell never calls this and the default, everything closed, stands). An `open`
    /// naming a panel that has not registered yet is held until it docks (a plugin loaded
    /// after the restore), unless the user opens something in the meantime. `pinned` is the
    /// whole list of floating panels the user docked: its absence means none is, so every
    /// floatable panel floats again.
    pub fn apply_state(&mut self, state: Option<&PanelsState>) -> Vec<DockEffect> {
        let Some(state) = state else {
            return Vec::new();
        };
        let mut effects = Vec::new();
        if let Some(widths) = &state.widths {
            for (id, px) in widths {
                self.widths.insert(id.clone(), *px);
                if let Some(entry) = self.entries.iter_mut().find(|e| e.id == *id) {
                    entry.panel.set_width(*px);
                }
            }
        }
        self.pinned = state.pinned.iter().flatten().cloned().collect();
        for entry in &mut self.entries {
            entry.panel.set_overlay(!self.pinned.contains(&entry.id));
        }
        let ids: Vec<String> = self.entries.iter().map(|e| e.id.clone()).collect();
        for id in &ids {
            let open = state.open.as_deref() == Some(id.as_str());
            effects.extend(self.toggle_panel(id, Some(open), true));
        }
        self.pending_open = match &state.open {
            Some(open) if !ids.contains(open) => Some(open.clone()),
            _ => None,
        };
        effects
    }

    /// The toggle group the chrome shows, with each panel's pressed state.
    pub fn publish(
        &self,
        i18n: Option<&dyn Translator>,
    ) -> (Vec<SidePanelButton>, Vec<(String, bool)>) {
        (
            self.list(i18n),
            self.entries
                .iter()
                .map(|e| (e.id.clone(), e.panel.is_open()))
                .collect(),
        )
    }

    /// Drop the contributed panels (the shell destroys its own).
    pub fn destroy(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::text_of;

    fn panel(id: &str, order: f64, opts: SidePanelOptions) -> DockEntry {
        DockEntry::built_in(id, "Title", "icon", order, opts)
    }

    fn dock() -> PanelDockModel {
        let mut d = PanelDockModel::new();
        d.add(panel("objects", 0.0, SidePanelOptions::default()));
        d.add(panel("dataWindow", 1.0, SidePanelOptions::default()));
        d
    }

    fn ids(d: &PanelDockModel) -> Vec<&str> {
        d.entries().iter().map(|e| e.id.as_str()).collect()
    }

    #[test]
    fn panels_dock_sorted_by_order_with_ties_in_registration_order() {
        let mut d = dock();
        d.add(DockEntry::contributed(
            "late",
            "L",
            "i",
            None,
            SidePanelOptions::default(),
        ));
        d.add(DockEntry::contributed(
            "first",
            "F",
            "i",
            Some(-5.0),
            SidePanelOptions::default(),
        ));
        d.add(DockEntry::contributed(
            "tie",
            "T",
            "i",
            None,
            SidePanelOptions::default(),
        ));
        assert_eq!(ids(&d), ["first", "objects", "dataWindow", "late", "tie"]);
        let titles: Vec<String> = d.list(None).into_iter().map(|b| b.id).collect();
        assert_eq!(titles, ids(&d));
    }

    #[test]
    fn opening_a_panel_closes_whichever_was_showing() {
        let mut d = dock();
        let effects = d.toggle("objects", Some(true));
        assert_eq!(d.open_id(), Some("objects"));
        assert!(effects.contains(&DockEffect::ChromeActive {
            id: "objects".into(),
            open: true
        }));
        assert!(effects.contains(&DockEffect::HandleOpen {
            id: "objects".into()
        }));
        let effects = d.toggle("dataWindow", Some(true));
        assert_eq!(d.open_id(), Some("dataWindow"));
        // objects was closed instantly, and its button reflected the close.
        assert!(effects.contains(&DockEffect::ChromeActive {
            id: "objects".into(),
            open: false
        }));
        assert!(!d.entry("objects").expect("entry").panel.is_open());
        assert_eq!(
            effects
                .iter()
                .filter(|e| matches!(e, DockEffect::Changed))
                .count(),
            2
        );
    }

    #[test]
    fn a_bare_toggle_flips_and_an_unknown_id_is_ignored() {
        let mut d = dock();
        d.toggle("objects", None);
        assert_eq!(d.open_id(), Some("objects"));
        d.toggle("objects", None);
        assert_eq!(d.open_id(), None);
        assert!(d.toggle("ghost", Some(true)).is_empty());
        assert_eq!(d.open_id(), None);
    }

    #[test]
    fn the_state_holds_the_open_panel_the_user_widths_and_the_pins_or_nothing() {
        let mut d = dock();
        assert_eq!(d.get_state(), None);
        d.toggle("objects", Some(true));
        d.on_width_change("objects", 320.0);
        d.on_placement_change("objects", false);
        let state = d.get_state().expect("a state");
        assert_eq!(state.open.as_deref(), Some("objects"));
        assert_eq!(
            state.widths.as_ref().and_then(|w| w.get("objects")),
            Some(&320.0)
        );
        assert_eq!(state.pinned, Some(vec!["objects".to_string()]));
        d.on_placement_change("objects", true);
        assert_eq!(d.get_state().and_then(|s| s.pinned), None);
    }

    #[test]
    fn applying_a_state_opens_that_panel_instantly_and_closes_the_rest() {
        let mut d = dock();
        d.toggle("objects", Some(true));
        let state = PanelsState {
            open: Some("dataWindow".into()),
            ..PanelsState::default()
        };
        d.apply_state(Some(&state));
        assert_eq!(d.open_id(), Some("dataWindow"));
        // No `open` closes the column; no state at all leaves it as it stands.
        d.apply_state(Some(&PanelsState::default()));
        assert_eq!(d.open_id(), None);
        d.toggle("objects", Some(true));
        assert!(d.apply_state(None).is_empty());
        assert_eq!(d.open_id(), Some("objects"));
    }

    #[test]
    fn restored_widths_apply_now_and_wait_for_late_panels() {
        let mut d = dock();
        let widths: BTreeMap<String, f64> =
            [("objects".to_string(), 360.0), ("late".to_string(), 410.0)]
                .into_iter()
                .collect();
        let opts = SidePanelOptions {
            resizable: true,
            ..SidePanelOptions::default()
        };
        let mut d2 = PanelDockModel::new();
        d2.add(panel("objects", 0.0, opts));
        d2.apply_state(Some(&PanelsState {
            widths: Some(widths.clone()),
            ..PanelsState::default()
        }));
        assert_eq!(d2.entry("objects").expect("entry").panel.width(), 360.0);
        d2.add(panel("late", 5.0, opts));
        assert_eq!(d2.entry("late").expect("entry").panel.width(), 410.0);
        // Non-resizable panels clamp to their own fixed bounds; the unknown id is only remembered.
        d.apply_state(Some(&PanelsState {
            widths: Some(widths),
            ..PanelsState::default()
        }));
        assert_eq!(
            d.get_state().and_then(|s| s.widths).map(|w| w.len()),
            Some(2)
        );
    }

    #[test]
    fn a_restored_open_naming_a_late_panel_is_held_until_it_docks() {
        let mut d = dock();
        d.apply_state(Some(&PanelsState {
            open: Some("late".into()),
            ..PanelsState::default()
        }));
        assert_eq!(d.open_id(), None);
        d.add(panel("late", 5.0, SidePanelOptions::default()));
        assert_eq!(d.open_id(), Some("late"));
    }

    #[test]
    fn the_user_opening_something_else_cancels_a_pending_restore() {
        let mut d = dock();
        d.apply_state(Some(&PanelsState {
            open: Some("late".into()),
            ..PanelsState::default()
        }));
        d.toggle("objects", Some(true));
        d.add(panel("late", 5.0, SidePanelOptions::default()));
        assert_eq!(
            d.open_id(),
            Some("objects"),
            "a stale restore must not steal the column"
        );
    }

    #[test]
    fn pins_restore_per_panel_and_are_remembered_for_late_registrations() {
        let floating = SidePanelOptions {
            overlay: true,
            ..SidePanelOptions::default()
        };
        let mut d = PanelDockModel::new();
        d.add(panel("notes", 0.0, floating));
        assert!(d.entry("notes").expect("entry").panel.overlay());
        d.apply_state(Some(&PanelsState {
            pinned: Some(vec!["notes".into(), "late".into()]),
            ..PanelsState::default()
        }));
        assert!(!d.entry("notes").expect("entry").panel.overlay());
        d.add(panel("late", 3.0, floating));
        assert!(!d.entry("late").expect("entry").panel.overlay());
        // The whole pinned list is restored: its absence floats every floatable panel again.
        d.apply_state(Some(&PanelsState::default()));
        assert!(d.entry("notes").expect("entry").panel.overlay());
    }

    #[test]
    fn dropping_a_panel_notifies_and_publishing_lists_the_toggle_group() {
        let mut d = dock();
        d.toggle("objects", Some(true));
        let (buttons, pressed) = d.publish(None);
        assert_eq!(buttons.len(), 2);
        assert_eq!(
            pressed,
            [
                ("objects".to_string(), true),
                ("dataWindow".to_string(), false)
            ]
        );
        assert_eq!(text_of(None, &d.entries()[0].title, &[]), "Title");
        assert_eq!(d.drop_entry("dataWindow"), vec![DockEffect::Notify]);
        assert!(d.drop_entry("dataWindow").is_empty());
        d.destroy();
        assert!(d.entries().is_empty());
    }

    fn resizable() -> SidePanelOptions {
        SidePanelOptions {
            resizable: true,
            maximizable: true,
            overlay: true,
            ..SidePanelOptions::default()
        }
    }

    #[test]
    fn entries_resize_with_a_drag_and_the_settled_width_is_remembered() {
        let mut d = PanelDockModel::new();
        d.add(panel("notes", 0.0, resizable()));
        let drag = d.begin_resize("notes", 500.0).expect("a drag");
        assert!(d.resize_to("notes", &drag, 440.0));
        assert_eq!(d.panel("notes").expect("panel").width(), 340.0);
        // Nothing is persisted before the drag ends.
        assert_eq!(d.get_state(), None);
        assert_eq!(d.end_resize("notes", &drag), vec![DockEffect::Changed]);
        assert_eq!(
            d.get_state()
                .and_then(|s| s.widths)
                .and_then(|w| w.get("notes").copied()),
            Some(340.0)
        );
        // A drag that moved nothing owes nothing; a double-click returns to the declared width.
        let again = d.begin_resize("notes", 10.0).expect("a drag");
        assert!(d.end_resize("notes", &again).is_empty());
        assert_eq!(d.reset_width("notes"), vec![DockEffect::Changed]);
        assert_eq!(d.panel("notes").expect("panel").width(), 280.0);
        assert!(d.reset_width("notes").is_empty());
        assert!(d.begin_resize("ghost", 0.0).is_none());
        assert!(!d.set_width("ghost", 300.0));
        assert!(d.set_width("notes", 300.0));
    }

    #[test]
    fn entries_maximize_and_a_close_restores_them() {
        let mut d = PanelDockModel::new();
        d.add(panel("notes", 0.0, resizable()));
        d.add(panel("plain", 1.0, SidePanelOptions::default()));
        assert!(d.set_maximized("plain", true).is_empty(), "not maximizable");
        assert!(d.set_maximized("ghost", true).is_empty());
        d.toggle("notes", Some(true));
        assert_eq!(d.set_maximized("notes", true), vec![DockEffect::Notify]);
        assert!(d.is_maximized("notes"));
        assert!(d.set_maximized("notes", true).is_empty(), "already there");
        d.toggle("notes", Some(false));
        assert!(
            d.is_maximized("notes"),
            "held until the exit animation ends"
        );
        assert_eq!(d.exit_finished("notes"), vec![DockEffect::Notify]);
        assert!(!d.is_maximized("notes"), "a close restores");
        assert!(d.exit_finished("notes").is_empty());
        assert!(d.exit_finished("ghost").is_empty());
    }

    #[test]
    fn the_header_pin_floats_or_docks_an_entry_and_is_remembered() {
        let mut d = PanelDockModel::new();
        d.add(panel("notes", 0.0, resizable()));
        d.add(panel("plain", 1.0, SidePanelOptions::default()));
        assert!(d.panel("notes").expect("panel").overlay());
        assert_eq!(d.set_overlay("notes", false), vec![DockEffect::Changed]);
        assert!(!d.panel("notes").expect("panel").overlay());
        assert_eq!(
            d.get_state().and_then(|s| s.pinned),
            Some(vec!["notes".to_string()])
        );
        assert!(d.set_overlay("notes", false).is_empty());
        assert!(d.set_overlay("plain", false).is_empty(), "cannot float");
        assert_eq!(d.set_overlay("notes", true), vec![DockEffect::Changed]);
        assert_eq!(d.get_state().and_then(|s| s.pinned), None);
    }
}
