//! Object tree: the docked panel listing the chart's panes, indicators and drawings
//! (see [`object_tree_model`](super::object_tree_model) for the layout rules).
//!
//! The original `ObjectTree` is a 1,579-line DOM component. This module is everything in it
//! that is a rule rather than an element:
//!
//! - [`ObjectTreeModel`]: the panel's own state and its reducers: the pick, the groups, the
//!   rename, the pruning of what the chart no longer has;
//! - drag-and-drop: [`DragSession`] (the click-vs-drag slop), [`resolve_drop`] (what a drop
//!   at a point would do, resolved from plain-data geometry) and [`plan_slot_drop`] /
//!   [`plan_restack_group`] (the stacking writes a drop implies);
//! - the context menus: [`price_menu`], [`indicator_menu`], [`drawing_menu`],
//!   [`group_menu`], each a list of descriptors plus the [`TreeCommand`] every id fires;
//! - the labels ([`TreeText`]) and the events that refresh the panel ([`REFRESH_EVENTS`]).
//!
//! Left for the browser crate: the element tree (pane blocks, rows, buttons, the rename
//! field, the selection bar, the ghost that follows the pointer) and its CSS, measuring the
//! rendered rows into the geometry types below, painting the drop hint, window pointer
//! listeners, the kit context menu, `chart.drawings` / `chart.panes` / `chart.renderer`
//! calls (a [`TreeCommand`] or a plan is applied there), and re-rendering. The panel
//! suspends refresh for the duration of a drag so the measured geometry stays true.

use std::collections::{HashMap, HashSet};

use super::object_tree_model::{
    DrawGroup, DrawUnit, PRICE_PANE_ID, StackToken, StackWrites, TreeItem, TreePane, TreeRow,
    ZEntry, assign_to_group, can_group, group_of, group_state, group_token_index, next_group_name,
    pane_rows, pane_tokens, place_tokens, prune_groups, remove_from_groups, stack_writes,
    token_index_of_slot, tokens_equal, z_stack_bounds,
};
use crate::drawings::drawing::SerializedDrawing;
use crate::events::types::ChartEventName;
use crate::i18n::{MessageArg, MessageLike, Translator, msg, text_of};
use crate::model::PaneKind;
use crate::ui::MenuItemDescriptor;

/// Events that refresh the panel.
pub const REFRESH_EVENTS: [ChartEventName; 8] = [
    ChartEventName::IndicatorAdded,
    ChartEventName::IndicatorRemoved,
    ChartEventName::IndicatorMoved,
    ChartEventName::IndicatorVisibility,
    ChartEventName::PaneChanged,
    ChartEventName::DrawingCreated,
    ChartEventName::DrawingEdited,
    ChartEventName::DrawingRemoved,
];

/// How far a press has to travel before it counts as a drag rather than a click.
pub const DRAG_SLOP: f64 = 4.0;

/// The panel's labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeText {
    ObjectTree,
    Price,
    EmptyHint,
    AlreadyInAGroup,
    DuplicateSelected,
    Duplicate,
    MovePaneUp,
    MovePaneDown,
    ExpandPane,
    CollapsePane,
    RestorePanes,
    MaximizePane,
    Hide,
    Show,
    RestackHint,
    Remove,
    MoveIndicatorHint,
    UnlockAll,
    LockAll,
    ShowAll,
    HideAll,
    RemoveAll,
    Unlock,
    Lock,
    MoveDrawingHint,
    OwnScaleHint,
    BringToFront,
    SendToBack,
    IndicatorSettings,
    MoveTo,
    NewPaneAbove,
    NewPaneBelow,
    NewGroup,
    AddToGroup,
    MoveToGroup,
    Rename,
    SelectAll,
    BringAllToFront,
    SendAllToBack,
    Ungroup,
    GroupDropHint,
    ScaleTag,
}

impl TreeText {
    pub fn message(self) -> MessageLike {
        use TreeText::*;
        match self {
            ObjectTree => msg("widget.objectTree.objectTree", "Object tree"),
            Price => msg("widget.objectTree.price", "Price"),
            EmptyHint => msg(
                "widget.objectTree.emptyHint",
                "Add an indicator or a drawing to populate the tree",
            ),
            AlreadyInAGroup => msg("widget.objectTree.alreadyInAGroup", "Already in a group"),
            DuplicateSelected => msg(
                "widget.objectTree.duplicateSelected",
                "Duplicate the selected drawings",
            ),
            Duplicate => msg("widget.objectTree.duplicate", "Duplicate"),
            MovePaneUp => msg("widget.objectTree.movePaneUp", "Move panel up"),
            MovePaneDown => msg("widget.objectTree.movePaneDown", "Move panel down"),
            ExpandPane => msg("widget.objectTree.expandPane", "Expand panel"),
            CollapsePane => msg("widget.objectTree.collapsePane", "Collapse panel"),
            RestorePanes => msg("widget.objectTree.restorePanes", "Restore panels"),
            MaximizePane => msg("widget.objectTree.maximizePane", "Maximize panel"),
            Hide => msg("widget.objectTree.hide", "Hide"),
            Show => msg("widget.objectTree.show", "Show"),
            RestackHint => msg(
                "widget.objectTree.restackHint",
                "Drag to change what draws in front",
            ),
            Remove => msg("widget.objectTree.remove", "Remove"),
            MoveIndicatorHint => msg(
                "widget.objectTree.moveIndicatorHint",
                "Drag to another panel, or between panels for a new one",
            ),
            UnlockAll => msg("widget.objectTree.unlockAll", "Unlock all"),
            LockAll => msg("widget.objectTree.lockAll", "Lock all"),
            ShowAll => msg("widget.objectTree.showAll", "Show all"),
            HideAll => msg("widget.objectTree.hideAll", "Hide all"),
            RemoveAll => msg("widget.objectTree.removeAll", "Remove all"),
            Unlock => msg("widget.objectTree.unlock", "Unlock"),
            Lock => msg("widget.objectTree.lock", "Lock"),
            MoveDrawingHint => msg(
                "widget.objectTree.moveDrawingHint",
                "Drag to restack, or onto another panel to move it there",
            ),
            OwnScaleHint => msg(
                "widget.objectTree.ownScaleHint",
                "Draws against its own price scale",
            ),
            BringToFront => msg("widget.objectTree.bringToFront", "Bring to front"),
            SendToBack => msg("widget.objectTree.sendToBack", "Send to back"),
            IndicatorSettings => msg("widget.objectTree.indicatorSettings", "Indicator settings"),
            MoveTo => msg("widget.objectTree.moveTo", "Move to"),
            NewPaneAbove => msg("widget.objectTree.newPaneAbove", "New panel above"),
            NewPaneBelow => msg("widget.objectTree.newPaneBelow", "New panel below"),
            NewGroup => msg("widget.objectTree.newGroup", "New group"),
            AddToGroup => msg("widget.objectTree.addToGroup", "Add to group"),
            MoveToGroup => msg("widget.objectTree.moveToGroup", "Move to group"),
            Rename => msg("widget.objectTree.rename", "Rename\u{2026}"),
            SelectAll => msg("widget.objectTree.selectAll", "Select all"),
            BringAllToFront => msg("widget.objectTree.bringAllToFront", "Bring all to front"),
            SendAllToBack => msg("widget.objectTree.sendAllToBack", "Send all to back"),
            Ungroup => msg("widget.objectTree.ungroup", "Ungroup"),
            GroupDropHint => msg("widget.objectTree.groupDropHint", "Drag drawings here"),
            ScaleTag => msg("widget.objectTree.scaleTag", "scale"),
        }
    }

    pub fn text(self, i18n: Option<&dyn Translator>) -> String {
        text_of(i18n, &self.message(), &[])
    }
}

/// The panel's title (also its toggle's tooltip).
pub fn object_tree_title() -> MessageLike {
    TreeText::ObjectTree.message()
}

/// `Group this drawing` / `Group these drawings`.
pub fn group_drawings_label(count: usize, i18n: Option<&dyn Translator>) -> String {
    text_of(
        i18n,
        &msg(
            "widget.objectTree.groupDrawings",
            "{count, plural, one {Group this drawing} other {Group these drawings}}",
        ),
        &[("count", MessageArg::from(count))],
    )
}

/// The selection bar's count.
pub fn selected_label(count: usize, i18n: Option<&dyn Translator>) -> String {
    text_of(
        i18n,
        &msg(
            "widget.objectTree.selected",
            "{count, plural, one {# selected} other {# selected}}",
        ),
        &[("count", MessageArg::from(count))],
    )
}

/// A group row's hint: how many drawings it holds.
pub fn group_members_label(count: usize, i18n: Option<&dyn Translator>) -> String {
    text_of(
        i18n,
        &msg(
            "widget.objectTree.groupMembers",
            "{count, plural, one {# drawing \u{2014} drag to move it} other {# drawings \u{2014} drag to move them together}}",
        ),
        &[("count", MessageArg::from(count))],
    )
}

fn group_pick_label(count: usize, i18n: Option<&dyn Translator>) -> String {
    text_of(
        i18n,
        &msg("widget.objectTree.groupPick", "Group selection ({count})"),
        &[("count", MessageArg::from(count))],
    )
}

fn remove_from_group_label(group: &str, i18n: Option<&dyn Translator>) -> String {
    text_of(
        i18n,
        &msg("widget.objectTree.removeFromGroup", "Remove from {group}"),
        &[("group", MessageArg::from(group.to_string()))],
    )
}

// ── the panel's own state ──────────────────────────────────

/// The panel's state: a group is the panel's own idea, not the chart's: it bundles drawings
/// so they hide, lock, delete and move as one, and lives as long as the panel does.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ObjectTreeModel {
    groups: Vec<DrawGroup>,
    /// The panel's pick (drawing ids, in the order picked).
    picked: Vec<String>,
    /// Groups folded shut, by id.
    collapsed: HashSet<String>,
    /// The group whose name is being edited.
    renaming: Option<String>,
    group_seq: u32,
}

impl ObjectTreeModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn groups(&self) -> &[DrawGroup] {
        &self.groups
    }

    pub fn picked(&self) -> &[String] {
        &self.picked
    }

    pub fn is_picked(&self, id: &str) -> bool {
        self.picked.iter().any(|p| p == id)
    }

    pub fn renaming(&self) -> Option<&str> {
        self.renaming.as_deref()
    }

    pub fn is_collapsed(&self, group_id: &str) -> bool {
        self.collapsed.contains(group_id)
    }

    /// Fold or unfold a group; returns whether it is now collapsed.
    pub fn toggle_collapsed(&mut self, group_id: &str) -> bool {
        if !self.collapsed.remove(group_id) {
            self.collapsed.insert(group_id.to_string());
            return true;
        }
        false
    }

    /// A click on a drawing row. With ctrl/cmd it toggles the drawing in the pick; a plain
    /// click on the only picked drawing clears the pick; otherwise it becomes the pick.
    /// Returns the new pick (the caller pushes it onto the chart, muting the
    /// `drawing:selected` echo it causes).
    pub fn on_draw_click(&mut self, id: &str, ctrl_or_meta: bool) -> Vec<String> {
        if ctrl_or_meta {
            if self.is_picked(id) {
                self.picked.retain(|p| p != id);
            } else {
                self.picked.push(id.to_string());
            }
        } else if self.is_picked(id) && self.picked.len() == 1 {
            self.picked.clear();
        } else {
            self.picked = vec![id.to_string()];
        }
        self.picked.clone()
    }

    /// Replace the pick (a group's "Select all"; the chart's own selection echo).
    pub fn set_picked(&mut self, ids: &[String]) {
        self.picked = ids.to_vec();
    }

    /// Bundle drawings into a fresh group, which takes over the pick. Returns the new
    /// group's id, or `None` when the selection cannot form one (empty, or already grouped).
    pub fn make_group(&mut self, ids: &[String], i18n: Option<&dyn Translator>) -> Option<String> {
        if !can_group(ids, &self.groups) {
            return None;
        }
        self.group_seq += 1;
        let id = format!("grp-{}", self.group_seq);
        let name = next_group_name(&self.groups, i18n);
        self.groups.push(DrawGroup {
            id: id.clone(),
            name,
            ids: ids.to_vec(),
            pane: None,
        });
        self.picked.clear();
        Some(id)
    }

    /// Start an empty group at the top of the main chart, named and ready to rename, for
    /// drawings to be dragged into.
    pub fn make_empty_group(&mut self, i18n: Option<&dyn Translator>) -> String {
        self.group_seq += 1;
        let id = format!("grp-{}", self.group_seq);
        let name = next_group_name(&self.groups, i18n);
        self.groups.push(DrawGroup {
            id: id.clone(),
            name,
            ids: Vec::new(),
            pane: Some(PRICE_PANE_ID.to_string()),
        });
        self.picked.clear();
        self.renaming = Some(id.clone());
        id
    }

    pub fn begin_rename(&mut self, id: &str) {
        self.renaming = Some(id.to_string());
    }

    /// Enter and losing focus both land here, and one rename can raise both: only the first
    /// counts. An empty name keeps the old one. Returns whether the rename ended.
    pub fn commit_rename(&mut self, id: &str, value: &str) -> bool {
        if self.renaming.as_deref() != Some(id) {
            return false;
        }
        let name = value.trim();
        if !name.is_empty() {
            for g in &mut self.groups {
                if g.id == id {
                    g.name = name.to_string();
                }
            }
        }
        self.renaming = None;
        true
    }

    /// Dissolve a group. With `with_members` the drawings it held go too: they are returned
    /// for the caller to delete (as one `remove_many`).
    pub fn remove_group(&mut self, id: &str, with_members: bool) -> Vec<String> {
        let held = self
            .groups
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.ids.clone())
            .unwrap_or_default();
        self.groups.retain(|g| g.id != id);
        self.collapsed.remove(id);
        if !with_members {
            return Vec::new();
        }
        self.picked.retain(|p| !held.contains(p));
        held
    }

    /// Put drawings into a group (leaving any other).
    pub fn assign(&mut self, group_id: &str, ids: &[String]) {
        self.groups = assign_to_group(&self.groups, group_id, ids);
    }

    /// Take drawings out of whatever group held them.
    pub fn unassign(&mut self, ids: &[String]) {
        self.groups = remove_from_groups(&self.groups, ids);
    }

    /// Duplicate a drawing, keeping the copy in the same group as its original: the chart
    /// has no notion of the groups, so the caller spots the new id and files it here.
    pub fn file_clone(&mut self, original: &str, clone: &str) {
        if let Some(g) = group_of(&self.groups, original).map(|g| g.id.clone()) {
            self.assign(&g, &[clone.to_string()]);
        }
    }

    /// Forget everything that refers to a drawing or group the chart no longer has:
    /// otherwise a deleted drawing would keep a group alive, or stay counted in the
    /// selection bar.
    pub fn prune(&mut self, live: &HashSet<String>) {
        self.picked.retain(|id| live.contains(id));
        if self.groups.is_empty() {
            self.renaming = None;
            return;
        }
        self.groups = prune_groups(&self.groups, live);
        if let Some(r) = &self.renaming
            && !self.groups.iter().any(|g| g.id == *r)
        {
            self.renaming = None;
        }
    }
}

// ── drag and drop ──────────────────────────────────────────

/// What is being dragged. Mirrors a row's `data-row-kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragKind {
    Price,
    Indicator,
    Drawing,
    Group,
}

/// A press on a draggable row, live until the pointer is released.
#[derive(Clone, Debug, PartialEq)]
pub struct DragSession {
    pub kind: DragKind,
    /// The dragged object's id: `None` for the candles, which have none.
    pub id: Option<String>,
    pub from_pane: String,
    pub label: String,
    start_x: f64,
    start_y: f64,
    /// False until the pointer clears [`DRAG_SLOP`]; a press that never does stays a click.
    active: bool,
}

impl DragSession {
    pub fn new(
        kind: DragKind,
        id: Option<String>,
        from_pane: &str,
        label: &str,
        x: f64,
        y: f64,
    ) -> Self {
        Self {
            kind,
            id,
            from_pane: from_pane.to_string(),
            label: label.to_string(),
            start_x: x,
            start_y: y,
            active: false,
        }
    }

    pub fn active(&self) -> bool {
        self.active
    }

    /// A pointer move. Returns `Some(true)` the moment the drag begins (build the ghost),
    /// `Some(false)` while it continues, `None` while the press is still within the slop.
    pub fn on_move(&mut self, x: f64, y: f64) -> Option<bool> {
        if !self.active {
            if (x - self.start_x).hypot(y - self.start_y) < DRAG_SLOP {
                return None;
            }
            self.active = true;
            return Some(true);
        }
        Some(false)
    }
}

/// A rectangle in viewport coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Rect {
    pub fn new(left: f64, top: f64, right: f64, bottom: f64) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    fn height(&self) -> f64 {
        self.bottom - self.top
    }
}

/// Insertion index in a vertical stack: the first element whose midpoint is below `y`.
pub fn insertion_slot(rects: &[Rect], y: f64) -> usize {
    rects
        .iter()
        .position(|r| y < r.top + r.height() / 2.0)
        .unwrap_or(rects.len())
}

/// A new-pane band between two blocks.
#[derive(Clone, Debug, PartialEq)]
pub struct GapGeometry {
    pub before: Option<String>,
    pub after: Option<String>,
    pub rect: Rect,
}

/// One rendered entry of a pane's stack.
#[derive(Clone, Debug, PartialEq)]
pub struct UnitGeometry {
    pub rect: Rect,
    /// Set on a group block.
    pub group_id: Option<String>,
    /// A group block's member rows.
    pub subrows: Vec<Rect>,
}

/// One pane block as rendered.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneGeometry {
    pub pane_id: String,
    pub kind: PaneKind,
    pub rect: Rect,
    /// The stack's children, front-most first.
    pub units: Vec<UnitGeometry>,
}

/// What dropping at the current pointer would do.
#[derive(Clone, Debug, PartialEq)]
pub enum Drop {
    /// Open a fresh pane in the band between two blocks.
    NewPane {
        before: Option<String>,
        after: Option<String>,
    },
    /// Join an existing pane, leaving its internal order alone.
    Merge { pane_id: String },
    /// Take a slot in a pane's stack: the ONE front-first column of drawings, indicators and
    /// (on the main pane) the candles. For a drawing, a different pane also re-panes it.
    Slot { pane_id: String, slot: usize },
    /// Join a group, at a slot among its members.
    IntoGroup {
        pane_id: String,
        group_id: String,
        member_slot: usize,
    },
}

/// What a render pass knows about the renderer.
#[derive(Clone, Copy, Debug)]
pub struct PassFlags {
    /// Whether the main pane's stacking can be reordered on this renderer.
    pub stackable: bool,
    /// Whether indicators can move between panes on this renderer.
    pub repanable: bool,
    /// Whether drawings share the series' draw-order space: a drawing can then take any slot
    /// in its pane's stack, under the candles or between two indicators.
    pub interleave: bool,
}

/// What a drop at this point would do. Measured off the live geometry, which is why a
/// refresh is suspended for the duration of the drag.
pub fn resolve_drop(
    drag: &DragSession,
    x: f64,
    y: f64,
    gaps: &[GapGeometry],
    panes_geo: &[PaneGeometry],
    panes: &[TreePane],
    flags: PassFlags,
) -> Option<Drop> {
    // The new-pane bands win: they are deliberately thin and sit between the blocks, so
    // testing them first keeps them reachable.
    if drag.kind == DragKind::Indicator {
        for gap in gaps {
            let r = gap.rect;
            if y >= r.top - 4.0 && y <= r.bottom + 4.0 && x >= r.left && x <= r.right {
                return Some(Drop::NewPane {
                    before: gap.before.clone(),
                    after: gap.after.clone(),
                });
            }
        }
    }
    for block in panes_geo {
        let r = block.rect;
        if y < r.top || y > r.bottom {
            continue;
        }
        let pane_id = block.pane_id.clone();
        // The candles never leave the main pane; an indicator on a renderer without editable
        // stacking can only join a pane whole.
        if drag.kind == DragKind::Price && block.kind != PaneKind::Price {
            return None;
        }
        if drag.kind == DragKind::Indicator && !flags.stackable {
            return if pane_id == drag.from_pane {
                None
            } else {
                Some(Drop::Merge { pane_id })
            };
        }
        // A lone drawing held over a group's block joins that group: anywhere else in the
        // column it stays (or becomes) loose. A whole group only ever drops at the top level.
        if drag.kind == DragKind::Drawing {
            for unit in &block.units {
                let Some(group_id) = &unit.group_id else {
                    continue;
                };
                if y < unit.rect.top || y > unit.rect.bottom {
                    continue;
                }
                return Some(Drop::IntoGroup {
                    pane_id,
                    group_id: group_id.clone(),
                    member_slot: insertion_slot(&unit.subrows, y),
                });
            }
        }
        let rects: Vec<Rect> = block.units.iter().map(|u| u.rect).collect();
        let mut slot = insertion_slot(&rects, y);
        // Without a shared draw-order space every drawing paints over the series, so its
        // slots stop at the leading drawings: the column can't promise an interleave the
        // renderer won't honor.
        if matches!(drag.kind, DragKind::Drawing | DragKind::Group) && !flags.interleave {
            let lead = panes
                .iter()
                .find(|p| p.id == pane_id)
                .and_then(|p| p.items.iter().position(|it| matches!(it, TreeItem::Row(_))));
            if let Some(lead) = lead {
                slot = slot.min(lead);
            }
        }
        return Some(Drop::Slot { pane_id, slot });
    }
    None
}

/// Where an indicator goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndicatorTarget {
    Price,
    Pane(String),
    NewPane {
        before: Option<String>,
        after: Option<String>,
    },
}

fn pane_target(pane_id: &str) -> IndicatorTarget {
    if pane_id == PRICE_PANE_ID {
        IndicatorTarget::Price
    } else {
        IndicatorTarget::Pane(pane_id.to_string())
    }
}

/// One drawing patch of a restack: the new z key, and the pane it moves to when it crossed.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawingPatch {
    pub id: String,
    pub z_index: f64,
    pub pane_id: Option<String>,
}

/// The stacking writes of a slot drop: the series through the renderer's order settings, the
/// drawings as ONE `update_many` (a single undo step).
#[derive(Clone, Debug, PartialEq)]
pub struct StackPlan {
    /// An indicator arriving from another pane moves there first.
    pub move_indicator: Option<(String, IndicatorTarget)>,
    pub pane_id: String,
    pub writes: StackWrites,
    pub drawing_patches: Vec<DrawingPatch>,
}

/// What applying a drop asks of the chart.
#[derive(Clone, Debug, PartialEq)]
pub enum DropPlan {
    /// Nothing to do.
    None,
    MoveIndicator {
        id: String,
        to: IndicatorTarget,
    },
    Stack(StackPlan),
}

/// A live drawing as the planner needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveDrawing {
    pub id: String,
    pub pane_id: String,
}

/// Renormalize one pane's z keys from a placed token stack. `live` is the pane each drawing
/// is in now (a dragged drawing that arrived from another pane is re-paned).
fn write_stack(
    pane_id: &str,
    placed: &[StackToken],
    dragged_ids: &HashSet<String>,
    live: &HashMap<String, String>,
    stackable: bool,
) -> StackPlan {
    let mut writes = stack_writes(placed);
    if !stackable {
        // A renderer without editable stacking has no series order to write.
        writes.candle_z = None;
        writes.series.clear();
    }
    let drawing_patches = writes
        .drawings
        .iter()
        .map(|ZEntry { id, z }| DrawingPatch {
            id: id.clone(),
            z_index: *z,
            pane_id: (dragged_ids.contains(id)
                && live.get(id).map(String::as_str) != Some(pane_id))
            .then(|| pane_id.to_string()),
        })
        .collect();
    StackPlan {
        move_indicator: None,
        pane_id: pane_id.to_string(),
        writes,
        drawing_patches,
    }
}

/// The drawings a session moves: a group carries every member, front-most first, so the
/// bundle keeps its own stacking wherever it lands. `live_back_to_front` is the chart's
/// drawings in paint order (back to front); everything here works front-first.
pub fn dragged_drawings(
    drag: &DragSession,
    groups: &[DrawGroup],
    live_back_to_front: &[LiveDrawing],
) -> Vec<String> {
    let Some(id) = &drag.id else {
        return Vec::new();
    };
    if drag.kind != DragKind::Group {
        return vec![id.clone()];
    }
    let Some(g) = groups.iter().find(|x| x.id == *id) else {
        return Vec::new();
    };
    live_back_to_front
        .iter()
        .filter(|d| g.ids.contains(&d.id))
        .map(|d| d.id.clone())
        .rev()
        .collect()
}

/// What applying a drop asks of the chart. Group bookkeeping (a lone drawing dropped on a
/// group joins it, at the top level it leaves its group) is applied to `model` here.
pub fn plan_drop(
    model: &mut ObjectTreeModel,
    drag: &DragSession,
    drop: &Drop,
    panes: &[TreePane],
    drawings_supported: bool,
    live_back_to_front: &[LiveDrawing],
    stackable: bool,
) -> DropPlan {
    match drop {
        Drop::Merge { pane_id } => match &drag.id {
            Some(id) if *pane_id != drag.from_pane => DropPlan::MoveIndicator {
                id: id.clone(),
                to: pane_target(pane_id),
            },
            _ => DropPlan::None,
        },
        Drop::NewPane { before, after } => {
            // An indicator alone in its own pane, dropped on a band touching that pane, has
            // nowhere new to go: the "new" pane would be the one it is already in, rebuilt.
            let alone = panes
                .iter()
                .find(|p| p.id == drag.from_pane)
                .is_some_and(|from| from.kind != PaneKind::Price && pane_rows(from).len() <= 1);
            let touches = before.as_deref() == Some(drag.from_pane.as_str())
                || after.as_deref() == Some(drag.from_pane.as_str());
            if alone && touches {
                return DropPlan::None;
            }
            match &drag.id {
                Some(id) => DropPlan::MoveIndicator {
                    id: id.clone(),
                    to: IndicatorTarget::NewPane {
                        before: before.clone(),
                        after: after.clone(),
                    },
                },
                None => DropPlan::None,
            }
        }
        Drop::Slot { pane_id, .. } | Drop::IntoGroup { pane_id, .. } => {
            match plan_slot_drop(
                model,
                drag,
                drop,
                pane_id,
                panes,
                drawings_supported,
                live_back_to_front,
                stackable,
            ) {
                Some(plan) => DropPlan::Stack(plan),
                None => DropPlan::None,
            }
        }
    }
}

/// Land a drop in a pane's stack: place the dragged tokens (the candles, an indicator, a
/// drawing, or a group's whole run of drawings) at the measured position, then renormalize
/// the pane's z keys in one sweep. `None` when nothing changes (a drag that changes nothing
/// should not cost an undo step).
#[allow(clippy::too_many_arguments)]
pub fn plan_slot_drop(
    model: &mut ObjectTreeModel,
    drag: &DragSession,
    drop: &Drop,
    pane_id: &str,
    panes: &[TreePane],
    drawings_supported: bool,
    live_back_to_front: &[LiveDrawing],
    stackable: bool,
) -> Option<StackPlan> {
    let target = panes.iter().find(|p| p.id == pane_id)?;
    let is_drawing = matches!(drag.kind, DragKind::Drawing | DragKind::Group);
    if is_drawing && !drawings_supported {
        return None;
    }
    let dragged: Vec<StackToken> = match drag.kind {
        DragKind::Price => vec![StackToken::Price],
        DragKind::Indicator => vec![StackToken::Indicator(drag.id.clone()?)],
        DragKind::Drawing | DragKind::Group => {
            dragged_drawings(drag, model.groups(), live_back_to_front)
                .into_iter()
                .map(StackToken::Drawing)
                .collect()
        }
    };
    if dragged.is_empty() || (drag.kind != DragKind::Price && drag.id.is_none()) {
        return None;
    }
    // An indicator arriving from another pane moves there first; the restack below then
    // assigns its slot. (Drawings re-pane through their own patch instead.)
    let move_indicator = match (&drag.kind, &drag.id) {
        (DragKind::Indicator, Some(id)) if drag.from_pane != pane_id => {
            Some((id.clone(), pane_target(pane_id)))
        }
        _ => None,
    };
    let tokens = pane_tokens(target);
    let at = match drop {
        Drop::IntoGroup {
            group_id,
            member_slot,
            ..
        } => group_token_index(target, group_id, *member_slot),
        Drop::Slot { slot, .. } => token_index_of_slot(target, *slot),
        _ => return None,
    };
    let placed = place_tokens(&tokens, &dragged, at as i64);
    // Group bookkeeping for a lone drawing: dropped on a group it joins it; dropped at the
    // top level it leaves whatever group held it.
    if drag.kind == DragKind::Drawing
        && let Some(id) = &drag.id
    {
        match drop {
            Drop::IntoGroup { group_id, .. } => model.assign(group_id, std::slice::from_ref(id)),
            _ => model.unassign(std::slice::from_ref(id)),
        }
    }
    let live: HashMap<String, String> = live_back_to_front
        .iter()
        .map(|d| (d.id.clone(), d.pane_id.clone()))
        .collect();
    let dragged_ids: HashSet<String> = dragged
        .iter()
        .filter_map(|t| match t {
            StackToken::Drawing(id) => Some(id.clone()),
            _ => None,
        })
        .collect();
    let repaned = dragged_ids
        .iter()
        .any(|id| live.get(id).map(String::as_str) != Some(pane_id));
    if tokens_equal(&tokens, &placed) && !repaned && move_indicator.is_none() {
        return None;
    }
    let mut plan = write_stack(pane_id, &placed, &dragged_ids, &live, stackable);
    plan.move_indicator = move_indicator;
    Some(plan)
}

/// Move a group's whole run to an end of its pane's stack: the menu twin of dragging it to
/// the top or the bottom of the column.
pub fn plan_restack_group(
    g: &DrawGroup,
    panes: &[TreePane],
    live_back_to_front: &[LiveDrawing],
    to_front: bool,
    stackable: bool,
) -> Option<StackPlan> {
    let mine: Vec<&LiveDrawing> = live_back_to_front
        .iter()
        .filter(|d| g.ids.contains(&d.id))
        .collect();
    let pane_id = mine.first().map_or(PRICE_PANE_ID, |d| d.pane_id.as_str());
    // Orphaned drawings fold into the price pane's block, so the fallback matches the render.
    let target = panes
        .iter()
        .find(|p| p.id == pane_id)
        .or_else(|| panes.iter().find(|p| p.kind == PaneKind::Price))?;
    if mine.is_empty() {
        return None;
    }
    let dragged: Vec<StackToken> = mine
        .iter()
        .map(|d| StackToken::Drawing(d.id.clone()))
        .rev() // front-first
        .collect();
    let tokens = pane_tokens(target);
    let placed = place_tokens(
        &tokens,
        &dragged,
        if to_front { 0 } else { tokens.len() as i64 },
    );
    if tokens_equal(&tokens, &placed) {
        return None;
    }
    let live: HashMap<String, String> = live_back_to_front
        .iter()
        .map(|d| (d.id.clone(), d.pane_id.clone()))
        .collect();
    Some(write_stack(
        &target.id,
        &placed,
        &HashSet::new(),
        &live,
        stackable,
    ))
}

// ── context menus ──────────────────────────────────────────

/// What a menu row asks of the chart. The kit menu reports a selected id and the tree's
/// entries are per-object, so the id to command mapping is built fresh on each right-click.
#[derive(Clone, Debug, PartialEq)]
pub enum TreeCommand {
    /// Hide/show the candles (`candleVisible`).
    SetCandlesVisible(bool),
    /// Write the renderer's `candleZOrder`.
    SetCandleZ(f64),
    SetIndicatorVisible {
        id: String,
        visible: bool,
    },
    OpenIndicatorSettings(String),
    MoveIndicator {
        id: String,
        to: IndicatorTarget,
    },
    /// Write one `seriesOrder` entry.
    SetSeriesZ {
        id: String,
        z: f64,
    },
    RemoveIndicator(String),
    ShowDrawing {
        id: String,
        visible: bool,
    },
    LockDrawing {
        id: String,
        locked: bool,
    },
    DuplicateDrawing(String),
    BringDrawingToFront(String),
    SendDrawingToBack(String),
    RemoveDrawing(String),
    /// Bundle these drawings into a new group.
    MakeGroup(Vec<String>),
    AssignToGroup {
        group_id: String,
        drawing: String,
    },
    RemoveFromGroup(String),
    BeginRename(String),
    /// One patch to every member of a group, as a single undo step.
    SetMembersVisible {
        ids: Vec<String>,
        visible: bool,
    },
    SetMembersLocked {
        ids: Vec<String>,
        locked: bool,
    },
    SelectAll(Vec<String>),
    RestackGroup {
        group_id: String,
        to_front: bool,
    },
    /// Dissolve a group; with `with_members` delete its drawings too.
    RemoveGroup {
        group_id: String,
        with_members: bool,
    },
}

/// Assembles menu descriptors together with the commands their ids fire. Ids are unique
/// across nesting levels, since a submenu leaf reports to the root.
#[derive(Default)]
pub struct MenuBuilder {
    pub commands: HashMap<String, TreeCommand>,
    seq: u32,
}

impl MenuBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    fn next(&mut self) -> String {
        self.seq += 1;
        format!("ot{}", self.seq)
    }

    pub fn entry(
        &mut self,
        label: String,
        icon: Option<&str>,
        command: TreeCommand,
        separator_before: bool,
    ) -> MenuItemDescriptor {
        let id = self.next();
        self.commands.insert(id.clone(), command);
        MenuItemDescriptor {
            icon: icon.map(str::to_string),
            separator_before,
            ..MenuItemDescriptor::new(id, label)
        }
    }

    pub fn submenu(
        &mut self,
        label: String,
        icon: Option<&str>,
        submenu: Vec<MenuItemDescriptor>,
        separator_before: bool,
    ) -> MenuItemDescriptor {
        let id = self.next();
        MenuItemDescriptor {
            icon: icon.map(str::to_string),
            separator_before,
            submenu: Some(submenu),
            ..MenuItemDescriptor::new(id, label)
        }
    }
}

/// The pane's stacking extremes as of now: what a front/back command has to beat. The z keys
/// are plain numbers, not commands, so the writer beats the extremes itself; with a shared
/// draw-order space the drawings' keys count too. Returns `(top, bottom)`.
pub fn stack_bounds(series_order: &[ZEntry], candle_z: f64, pane_drawing_z: &[f64]) -> (f64, f64) {
    z_stack_bounds(series_order, candle_z, pane_drawing_z)
}

/// What a menu build needs to know about the chart right now.
#[derive(Clone, Copy)]
pub struct MenuInputs<'a> {
    pub panes: &'a [TreePane],
    pub groups: &'a [DrawGroup],
    pub picked: &'a [String],
    pub stackable: bool,
    /// `chart.panes.supported`.
    pub panes_supported: bool,
    /// `chart.renderer.supportsIndicatorSettings`.
    pub supports_indicator_settings: bool,
    pub series_order: &'a [ZEntry],
    pub candle_z: f64,
    /// Each pane's drawing z keys (only when drawings share the draw-order space).
    pub drawing_z_by_pane: &'a HashMap<String, Vec<f64>>,
    pub i18n: Option<&'a dyn Translator>,
}

impl MenuInputs<'_> {
    fn t(&self, key: TreeText) -> String {
        key.text(self.i18n)
    }

    fn bounds(&self, pane_id: &str) -> (f64, f64) {
        let extra = self
            .drawing_z_by_pane
            .get(pane_id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        stack_bounds(self.series_order, self.candle_z, extra)
    }

    fn visible_label(&self, visible: bool) -> String {
        self.t(if visible {
            TreeText::Hide
        } else {
            TreeText::Show
        })
    }
}

/// The candles' menu.
pub fn price_menu(
    b: &mut MenuBuilder,
    inp: &MenuInputs<'_>,
    candle_visible: bool,
) -> Vec<MenuItemDescriptor> {
    let mut items = vec![b.entry(
        inp.visible_label(candle_visible),
        Some(if candle_visible { "eye-off" } else { "eye" }),
        TreeCommand::SetCandlesVisible(!candle_visible),
        false,
    )];
    if inp.stackable {
        let (top, bottom) = inp.bounds(PRICE_PANE_ID);
        items.push(b.entry(
            inp.t(TreeText::BringToFront),
            Some("arrow-up"),
            TreeCommand::SetCandleZ(top + 1.0),
            true,
        ));
        items.push(b.entry(
            inp.t(TreeText::SendToBack),
            Some("arrow-down"),
            TreeCommand::SetCandleZ(bottom - 1.0),
            false,
        ));
    }
    items
}

/// "Move to ..." entries for an indicator: only the moves that would change something.
pub fn move_items(
    b: &mut MenuBuilder,
    inp: &MenuInputs<'_>,
    id: &str,
    from_pane: &str,
) -> Vec<MenuItemDescriptor> {
    let mut items = Vec::new();
    for p in inp.panes {
        if p.id == from_pane {
            continue;
        }
        let to = if p.kind == PaneKind::Price {
            IndicatorTarget::Price
        } else {
            IndicatorTarget::Pane(p.id.clone())
        };
        items.push(b.entry(
            p.label.clone(),
            None,
            TreeCommand::MoveIndicator {
                id: id.to_string(),
                to,
            },
            false,
        ));
    }
    // An indicator alone in its own study pane has nowhere new to go: a fresh pane would
    // just be the pane it already has.
    let alone = inp
        .panes
        .iter()
        .find(|p| p.id == from_pane)
        .is_some_and(|c| c.kind != PaneKind::Price && pane_rows(c).len() <= 1);
    if !alone {
        if from_pane != PRICE_PANE_ID {
            items.push(b.entry(
                inp.t(TreeText::NewPaneAbove),
                None,
                TreeCommand::MoveIndicator {
                    id: id.to_string(),
                    to: IndicatorTarget::NewPane {
                        before: Some(from_pane.to_string()),
                        after: None,
                    },
                },
                false,
            ));
        }
        items.push(b.entry(
            inp.t(TreeText::NewPaneBelow),
            None,
            TreeCommand::MoveIndicator {
                id: id.to_string(),
                to: IndicatorTarget::NewPane {
                    before: None,
                    after: Some(from_pane.to_string()),
                },
            },
            false,
        ));
    }
    items
}

/// An indicator row's menu.
pub fn indicator_menu(
    b: &mut MenuBuilder,
    inp: &MenuInputs<'_>,
    id: &str,
    visible: bool,
    pane_id: &str,
) -> Vec<MenuItemDescriptor> {
    let mut items = vec![b.entry(
        inp.visible_label(visible),
        Some(if visible { "eye-off" } else { "eye" }),
        TreeCommand::SetIndicatorVisible {
            id: id.to_string(),
            visible: !visible,
        },
        false,
    )];
    // The legend gear's twin: reachable here even where the legend is folded away (mobile) or
    // replaced by the overview chip (multi-chart grids).
    if inp.supports_indicator_settings {
        items.push(b.entry(
            inp.t(TreeText::IndicatorSettings),
            Some("gear"),
            TreeCommand::OpenIndicatorSettings(id.to_string()),
            false,
        ));
    }
    if inp.panes_supported {
        let moves = move_items(b, inp, id, pane_id);
        if !moves.is_empty() {
            items.push(b.submenu(inp.t(TreeText::MoveTo), Some("move-vertical"), moves, false));
        }
    }
    if inp.stackable {
        let (top, bottom) = inp.bounds(pane_id);
        items.push(b.entry(
            inp.t(TreeText::BringToFront),
            Some("arrow-up"),
            TreeCommand::SetSeriesZ {
                id: id.to_string(),
                z: top + 1.0,
            },
            true,
        ));
        items.push(b.entry(
            inp.t(TreeText::SendToBack),
            Some("arrow-down"),
            TreeCommand::SetSeriesZ {
                id: id.to_string(),
                z: bottom - 1.0,
            },
            false,
        ));
    }
    items.push(b.entry(
        inp.t(TreeText::Remove),
        Some("trash"),
        TreeCommand::RemoveIndicator(id.to_string()),
        true,
    ));
    items
}

/// The grouping half of a drawing's menu: what it can do about the bundle it is (or isn't)
/// part of. Only the moves that mean something for this row are offered.
pub fn grouping_items(
    b: &mut MenuBuilder,
    inp: &MenuInputs<'_>,
    id: &str,
) -> Vec<MenuItemDescriptor> {
    let mut items = Vec::new();
    let mine = group_of(inp.groups, id);
    // A right-click inside a multi-pick acts on the whole pick; on a row outside it, just that
    // row: the same rule the platform's file managers use.
    let pick =
        (inp.picked.iter().any(|p| p == id) && inp.picked.len() > 1).then(|| inp.picked.to_vec());
    match &pick {
        Some(pick) if can_group(pick, inp.groups) => {
            items.push(b.entry(
                group_pick_label(pick.len(), inp.i18n),
                Some("group"),
                TreeCommand::MakeGroup(pick.clone()),
                true,
            ));
        }
        None if mine.is_none() => {
            items.push(b.entry(
                inp.t(TreeText::NewGroup),
                Some("group"),
                TreeCommand::MakeGroup(vec![id.to_string()]),
                true,
            ));
        }
        _ => {}
    }
    let others: Vec<&DrawGroup> = inp
        .groups
        .iter()
        .filter(|g| Some(g.id.as_str()) != mine.map(|m| m.id.as_str()))
        .collect();
    if pick.is_none() && !others.is_empty() {
        let separator = items.is_empty();
        let entries: Vec<MenuItemDescriptor> = others
            .iter()
            .map(|g| {
                b.entry(
                    g.name.clone(),
                    None,
                    TreeCommand::AssignToGroup {
                        group_id: g.id.clone(),
                        drawing: id.to_string(),
                    },
                    false,
                )
            })
            .collect();
        let label = inp.t(if mine.is_none() {
            TreeText::AddToGroup
        } else {
            TreeText::MoveToGroup
        });
        items.push(b.submenu(label, Some("folder-plus"), entries, separator));
    }
    if let Some(mine) = mine {
        let separator = items.is_empty();
        items.push(b.entry(
            remove_from_group_label(&mine.name, inp.i18n),
            Some("folder-minus"),
            TreeCommand::RemoveFromGroup(id.to_string()),
            separator,
        ));
    }
    items
}

/// A drawing row's menu.
pub fn drawing_menu(
    b: &mut MenuBuilder,
    inp: &MenuInputs<'_>,
    d: &SerializedDrawing,
) -> Vec<MenuItemDescriptor> {
    let id = d.id.clone();
    let mut items = vec![
        b.entry(
            inp.visible_label(d.visible),
            Some(if d.visible { "eye-off" } else { "eye" }),
            TreeCommand::ShowDrawing {
                id: id.clone(),
                visible: !d.visible,
            },
            false,
        ),
        b.entry(
            inp.t(if d.locked {
                TreeText::Unlock
            } else {
                TreeText::Lock
            }),
            Some(if d.locked { "unlock" } else { "lock" }),
            TreeCommand::LockDrawing {
                id: id.clone(),
                locked: !d.locked,
            },
            false,
        ),
        b.entry(
            inp.t(TreeText::Duplicate),
            Some("clone"),
            TreeCommand::DuplicateDrawing(id.clone()),
            false,
        ),
        // Front/back clear the WHOLE stack on a shared-z renderer: over or under the candles
        // and every indicator, not just the other drawings.
        b.entry(
            inp.t(TreeText::BringToFront),
            Some("arrow-up"),
            TreeCommand::BringDrawingToFront(id.clone()),
            true,
        ),
        b.entry(
            inp.t(TreeText::SendToBack),
            Some("arrow-down"),
            TreeCommand::SendDrawingToBack(id.clone()),
            false,
        ),
    ];
    items.extend(grouping_items(b, inp, &id));
    items.push(b.entry(
        inp.t(TreeText::Remove),
        Some("trash"),
        TreeCommand::RemoveDrawing(id),
        true,
    ));
    items
}

/// A group row's menu. `members` are the live drawings of the group.
pub fn group_menu(
    b: &mut MenuBuilder,
    inp: &MenuInputs<'_>,
    g: &DrawGroup,
    members: &[SerializedDrawing],
) -> Vec<MenuItemDescriptor> {
    let rename = b.entry(
        inp.t(TreeText::Rename),
        Some("pen"),
        TreeCommand::BeginRename(g.id.clone()),
        false,
    );
    // Until a drawing joins there is nothing to show, lock, select or restack.
    if g.ids.is_empty() {
        let remove = b.entry(
            inp.t(TreeText::Remove),
            Some("trash"),
            TreeCommand::RemoveGroup {
                group_id: g.id.clone(),
                with_members: false,
            },
            true,
        );
        return vec![rename, remove];
    }
    let ids = g.ids.clone();
    let (all_hidden, all_locked) = group_state(members);
    let mut items = vec![
        rename,
        b.entry(
            inp.t(if all_hidden {
                TreeText::ShowAll
            } else {
                TreeText::HideAll
            }),
            Some(if all_hidden { "eye" } else { "eye-off" }),
            TreeCommand::SetMembersVisible {
                ids: ids.clone(),
                visible: all_hidden,
            },
            true,
        ),
        b.entry(
            inp.t(if all_locked {
                TreeText::UnlockAll
            } else {
                TreeText::LockAll
            }),
            Some(if all_locked { "unlock" } else { "lock" }),
            TreeCommand::SetMembersLocked {
                ids: ids.clone(),
                locked: !all_locked,
            },
            false,
        ),
        b.entry(
            inp.t(TreeText::SelectAll),
            Some("group"),
            TreeCommand::SelectAll(ids),
            false,
        ),
    ];
    // The whole bundle moves to an end of its pane's stack: over or under the candles and
    // every indicator, as one contiguous run, keeping its internal order.
    if !members.is_empty() {
        items.push(b.entry(
            inp.t(TreeText::BringAllToFront),
            Some("arrow-up"),
            TreeCommand::RestackGroup {
                group_id: g.id.clone(),
                to_front: true,
            },
            false,
        ));
        items.push(b.entry(
            inp.t(TreeText::SendAllToBack),
            Some("arrow-down"),
            TreeCommand::RestackGroup {
                group_id: g.id.clone(),
                to_front: false,
            },
            false,
        ));
    }
    // Ungrouping keeps the drawings; the group is only the panel's bookkeeping.
    items.push(b.entry(
        inp.t(TreeText::Ungroup),
        Some("ungroup"),
        TreeCommand::RemoveGroup {
            group_id: g.id.clone(),
            with_members: false,
        },
        true,
    ));
    items.push(b.entry(
        inp.t(TreeText::RemoveAll),
        Some("trash"),
        TreeCommand::RemoveGroup {
            group_id: g.id.clone(),
            with_members: true,
        },
        false,
    ));
    items
}

/// The indicator row with this id, wherever it sits.
pub fn find_indicator_row<'a>(
    panes: &'a [TreePane],
    id: &str,
) -> Option<(&'a TreePane, &'a super::object_tree_model::IndicatorRow)> {
    panes.iter().find_map(|p| {
        pane_rows(p).into_iter().find_map(|r| match r {
            TreeRow::Indicator(i) if i.id == id => Some((p, i)),
            _ => None,
        })
    })
}

/// The unit of a pane's stack that is a group with this id.
pub fn group_unit<'a>(pane: &'a TreePane, group_id: &str) -> Option<&'a DrawUnit> {
    pane.items.iter().find_map(|it| match it {
        TreeItem::Unit(u @ DrawUnit::Group { group, .. }) if group.id == group_id => Some(u),
        _ => None,
    })
}

#[cfg(test)]
mod tests;
