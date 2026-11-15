//! Timeline marks, the public model behind `chart.marks`: host-supplied events
//! pinned to a moment in time (a dividend, a split, an earnings call, a
//! deployment) and shown as small glyphs on a lane at the bottom of the plot,
//! each opening a detail popup on click. Renderer-agnostic: nothing here knows
//! how a glyph is painted.
//!
//! Left out:
//! - `MarksController`: it pushes snapshots to the renderer port, re-emits the
//!   renderer's glyph clicks on the event bus and reads group choices back from
//!   the renderer's `marks` feature. Its renderer-free state is ported as
//!   [`MarksModel`].
//! - The lazy content source (`MarkContentSource`'s thunk, which may return a
//!   `Promise`): a mark carries only its immediate [`MarkContent`].

pub mod controller;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::Millis;

/// The built-in glyph outlines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkShape {
    #[default]
    Circle,
    Square,
    Diamond,
    Pin,
}

impl MarkShape {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Circle => "circle",
            Self::Square => "square",
            Self::Diamond => "diamond",
            Self::Pin => "pin",
        }
    }
}

/// How a mark looks on the lane: a colored token carrying either a short letter
/// or a registered icon (an id from the icon registry, never inline markup, so
/// a mark payload can come from data without ever carrying SVG). `icon` wins
/// over `letter` when both are given; the symbol's ink is auto-contrasted
/// against `color`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkGlyph {
    /// Token outline (default [`MarkShape::Circle`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<MarkShape>,
    /// Token fill: any CSS color.
    pub color: String,
    /// One or two characters drawn inside the token (`D`, `E`, `!`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub letter: Option<String>,
    /// Icon id from the icon registry, drawn inside the token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// The action behind a panel button. Compared by identity.
#[derive(Clone)]
pub struct MarkAction(Rc<dyn Fn()>);

impl MarkAction {
    pub fn new(run: impl Fn() + 'static) -> Self {
        Self(Rc::new(run))
    }

    pub fn run(&self) {
        (self.0)();
    }
}

impl fmt::Debug for MarkAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MarkAction")
    }
}

impl PartialEq for MarkAction {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

/// One row of a structured panel: the callout vocabulary plus a label/value
/// field.
#[derive(Clone, Debug, PartialEq)]
pub enum MarkPanelItem {
    Text {
        text: String,
    },
    Field {
        label: String,
        value: String,
    },
    Button {
        label: String,
        /// Emphasized (selection-colored) button: the panel's main action.
        primary: Option<bool>,
        /// Close the popup after `run` (default true).
        close: Option<bool>,
        run: MarkAction,
    },
}

/// What the popup shows for one mark: plain text (escaped), HTML (sanitized
/// through an allowlist: scripts, styles, frames, event handlers and
/// `javascript:` URLs never reach the page), or a structured panel of rows and
/// buttons.
#[derive(Clone, Debug, PartialEq)]
pub enum MarkContent {
    Text(String),
    Html(String),
    Panel(Vec<MarkPanelItem>),
}

/// One timeline mark.
#[derive(Clone, Debug, PartialEq)]
pub struct TimelineMark {
    /// Stable id: re-adding an id replaces the mark.
    pub id: String,
    /// The moment the mark belongs to, epoch-ms. The glyph never sits at this
    /// exact pixel: it centers on the bar whose span contains the time (a time
    /// falling in a gap, a weekend or a closed session, goes to the first bar
    /// that follows; one past the newest bar projects onto the extrapolated
    /// grid in the right whitespace).
    pub time: Millis,
    /// Popup heading: shown above the content whatever its form.
    pub title: Option<String>,
    pub glyph: MarkGlyph,
    /// Hover text on the glyph.
    pub tooltip: Option<String>,
    /// The visibility group the mark belongs to (`dividends`, `splits`). Marks
    /// of one group that land on the same bar, or whose glyphs would overlap at
    /// the current zoom, cluster into one glyph (the tooltip carries the count,
    /// the popup lists them all); groups get a checkbox each in the chart
    /// settings (the Events tab). Define the display label with
    /// [`MarksModel::define_group`]; an undefined group shows its capitalized
    /// id.
    pub group: Option<String>,
    /// Popup content. Without it a click only emits `mark:click`: the host owns
    /// the UI.
    pub content: Option<MarkContent>,
}

impl TimelineMark {
    /// A mark with only the required fields.
    pub fn new(id: impl Into<String>, time: Millis, glyph: MarkGlyph) -> Self {
        Self {
            id: id.into(),
            time,
            title: None,
            glyph,
            tooltip: None,
            group: None,
            content: None,
        }
    }
}

/// A visibility group's presentation (see [`TimelineMark::group`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkGroup {
    pub id: String,
    /// The settings checkbox label (`Dividends`).
    pub label: String,
    /// Default visibility before the user toggles it (default true). A
    /// persisted choice wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    /// The id of the group this one nests under. A child's marks paint only
    /// while BOTH it and its parent are visible; on the Events tab it lists
    /// indented beneath the parent and dims while the parent is off, keeping
    /// its own choice for when the parent comes back. Lets a host offer
    /// `News -> Latest / All` or `Economic -> High / Medium / Low` as one
    /// master switch with sub-choices. A parent may hold marks of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// The `mark:click` payload: every mark under the clicked glyph (a cluster
/// lists them all).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkClickEvent {
    /// The cluster's first mark (earliest time, then insertion order).
    pub id: String,
    /// Every mark in the clicked cluster, in cluster order.
    pub ids: Vec<String>,
    /// The first mark's time, epoch-ms.
    pub time: Millis,
    /// The cluster's group, when its marks belong to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

// --- visibility -----------------------------------------------------------
// Group visibility for timeline marks: pure and renderer-agnostic, so both the
// core model and the renderers' display state read the same rule.

/// The user's per-group choices, keyed by group id; an absent id follows the
/// group's own declared default.
pub type MarkGroupChoices = HashMap<String, bool>;

/// A group's OWN switch: the user's choice when one is stored, else the group's
/// declared default, else visible.
pub fn mark_group_own_visible(
    choices: &MarkGroupChoices,
    group_id: &str,
    groups: &[MarkGroup],
) -> bool {
    match choices.get(group_id) {
        Some(&chosen) => chosen,
        None => groups
            .iter()
            .find(|g| g.id == group_id)
            .is_none_or(|g| g.visible != Some(false)),
    }
}

/// Whether a group's marks paint: its own switch AND every ancestor's (a child
/// under a switched-off parent is hidden whatever its own choice says).
/// Ungrouped marks always paint. A parent that was never defined is ignored; a
/// cycle stops at the first repeat.
pub fn mark_group_visible(
    choices: &MarkGroupChoices,
    group_id: Option<&str>,
    groups: &[MarkGroup],
) -> bool {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut id = group_id;
    while let Some(current) = id {
        if !seen.insert(current) {
            break;
        }
        if !mark_group_own_visible(choices, current, groups) {
            return false;
        }
        let parent = groups
            .iter()
            .find(|g| g.id == current)
            .and_then(|g| g.parent.as_deref());
        id = parent.filter(|p| groups.iter().any(|g| g.id == *p));
    }
    true
}

/// One Events-tab row: the group and how deep it nests (0 = top level).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkGroupRow<'a> {
    pub group: &'a MarkGroup,
    pub depth: usize,
}

/// The Events tab's row order: each top-level group in the given order,
/// followed by its children (in the given order), recursively. A group naming
/// an unknown parent, or one in a cycle, lists at the top level so it can never
/// disappear from the tab.
pub fn mark_group_rows(groups: &[MarkGroup]) -> Vec<MarkGroupRow<'_>> {
    fn place<'a>(
        groups: &'a [MarkGroup],
        group: &'a MarkGroup,
        depth: usize,
        placed: &mut HashSet<&'a str>,
        out: &mut Vec<MarkGroupRow<'a>>,
    ) {
        if !placed.insert(group.id.as_str()) {
            return;
        }
        out.push(MarkGroupRow { group, depth });
        for child in groups {
            if child.parent.as_deref() == Some(group.id.as_str()) && child.id != group.id {
                place(groups, child, depth + 1, placed, out);
            }
        }
    }

    let ids: HashSet<&str> = groups.iter().map(|g| g.id.as_str()).collect();
    let mut out = Vec::with_capacity(groups.len());
    let mut placed = HashSet::new();
    for g in groups {
        let parent_known = g
            .parent
            .as_deref()
            .is_some_and(|p| ids.contains(p) && p != g.id);
        if !parent_known {
            place(groups, g, 0, &mut placed, &mut out);
        }
    }
    // Anything left (cycles) surfaces at the top level.
    for g in groups {
        place(groups, g, 0, &mut placed, &mut out);
    }
    out
}

// --- display state of the `marks` renderer feature ------------------------

/// Display state of the `marks` renderer feature: the timeline-mark lane's
/// master toggle plus per-group visibility. Pure (no DOM, no renderer types):
/// the settings dialog, the config reducer and the lane painter all read it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarksDisplayState {
    /// Master visibility of the lane.
    pub visible: bool,
    /// Per-group visibility keyed by group id. An absent id follows the group's
    /// own declared default.
    pub groups: MarkGroupChoices,
}

impl Default for MarksDisplayState {
    fn default() -> Self {
        Self {
            visible: true,
            groups: HashMap::new(),
        }
    }
}

impl MarksDisplayState {
    /// Validated merge of an untrusted partial onto this state: a bare boolean
    /// toggles the lane; an object patches `visible` and/or `groups`. Groups
    /// merge ADDITIVELY: a patch names only the ids it carries and the rest
    /// keep their values, so a persisted choice for a group the host has not
    /// (yet) registered survives verbatim. Malformed values drop.
    pub fn merged(&self, patch: &Value) -> Self {
        if let Value::Bool(visible) = patch {
            return Self {
                visible: *visible,
                groups: self.groups.clone(),
            };
        }
        let mut groups = self.groups.clone();
        // JavaScript's `Object.entries` also walks an array, keyed by index.
        let entries: Vec<(String, &Value)> = match patch.get("groups") {
            Some(Value::Object(map)) => map.iter().map(|(k, v)| (k.clone(), v)).collect(),
            Some(Value::Array(items)) => items
                .iter()
                .enumerate()
                .map(|(i, v)| (i.to_string(), v))
                .collect(),
            _ => Vec::new(),
        };
        for (id, v) in entries {
            if let Value::Bool(b) = v {
                groups.insert(id, *b);
            }
        }
        let visible = patch
            .get("visible")
            .and_then(Value::as_bool)
            .unwrap_or(self.visible);
        Self { visible, groups }
    }

    /// Whether a group's marks paint under this display state: its own switch
    /// and every ancestor's; see [`mark_group_visible`].
    pub fn group_visible(&self, group_id: Option<&str>, groups: &[MarkGroup]) -> bool {
        mark_group_visible(&self.groups, group_id, groups)
    }
}

// --- the renderer-free state of MarksController ---------------------------

/// A mark or group definition the model refuses. Such a mark cannot be placed
/// or drawn, which is a programming error, not data to tolerate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarksError {
    EmptyMarkId,
    EmptyGroupId,
    EmptyGroupParent { group: String },
}

impl fmt::Display for MarksError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyMarkId => f.write_str("[vela] marks: `id` must be a non-empty string"),
            Self::EmptyGroupId => {
                f.write_str("[vela] marks.defineGroup: `id` must be a non-empty string")
            }
            Self::EmptyGroupParent { group } => write!(
                f,
                "[vela] marks.defineGroup: group \"{group}\" has a `parent` that is not a group id"
            ),
        }
    }
}

impl std::error::Error for MarksError {}

/// The marks and group definitions behind `chart.marks`: the source of truth
/// the controller pushes to a renderer. Marks are insertion-ordered (the order a
/// cluster falls back to for equal times); re-adding an id replaces the mark in
/// place.
#[derive(Clone, Debug, Default)]
pub struct MarksModel {
    marks: Vec<TimelineMark>,
    index: HashMap<String, usize>,
    groups: Vec<MarkGroup>,
}

impl MarksModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add (or replace, by id) one mark.
    pub fn add(&mut self, mark: TimelineMark) -> Result<(), MarksError> {
        if mark.id.is_empty() {
            return Err(MarksError::EmptyMarkId);
        }
        match self.index.get(&mark.id) {
            Some(&at) => self.marks[at] = mark,
            None => {
                self.index.insert(mark.id.clone(), self.marks.len());
                self.marks.push(mark);
            }
        }
        Ok(())
    }

    /// Replace the whole set (a market switch). Like the original, a malformed
    /// mark stops the call after the marks before it were added.
    pub fn set(&mut self, marks: impl IntoIterator<Item = TimelineMark>) -> Result<(), MarksError> {
        self.marks.clear();
        self.index.clear();
        for mark in marks {
            self.add(mark)?;
        }
        Ok(())
    }

    /// Remove one mark; whether it was there.
    pub fn remove(&mut self, id: &str) -> bool {
        let Some(at) = self.index.remove(id) else {
            return false;
        };
        self.marks.remove(at);
        for slot in self.index.values_mut() {
            if *slot > at {
                *slot -= 1;
            }
        }
        true
    }

    /// Drop every mark; whether there were any (the controller pushes to the
    /// renderer only then).
    pub fn clear(&mut self) -> bool {
        let had = !self.marks.is_empty();
        self.marks.clear();
        self.index.clear();
        had
    }

    /// Every mark, in insertion order. Marks are plain values, so the slice
    /// cannot be mutated through.
    pub fn all(&self) -> &[TimelineMark] {
        &self.marks
    }

    pub fn get(&self, id: &str) -> Option<&TimelineMark> {
        self.index.get(id).map(|&at| &self.marks[at])
    }

    /// Define (or replace, by id) a group's presentation: its settings label
    /// and default visibility. A redefinition keeps the group's position.
    pub fn define_group(&mut self, group: MarkGroup) -> Result<(), MarksError> {
        if group.id.is_empty() {
            return Err(MarksError::EmptyGroupId);
        }
        if group.parent.as_deref().is_some_and(str::is_empty) {
            return Err(MarksError::EmptyGroupParent { group: group.id });
        }
        match self.groups.iter_mut().find(|g| g.id == group.id) {
            Some(existing) => *existing = group,
            None => self.groups.push(group),
        }
        Ok(())
    }

    /// The defined groups, in definition order.
    pub fn group_definitions(&self) -> &[MarkGroup] {
        &self.groups
    }

    /// A group's effective visibility under the stored `choices`; see
    /// [`mark_group_visible`].
    pub fn is_group_visible(&self, choices: &MarkGroupChoices, id: &str) -> bool {
        mark_group_visible(choices, Some(id), &self.groups)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const T: Millis = 1_718_064_000_000; // 2024-06-11

    fn group(id: &str, label: &str, visible: Option<bool>, parent: Option<&str>) -> MarkGroup {
        MarkGroup {
            id: id.into(),
            label: label.into(),
            visible,
            parent: parent.map(str::to_owned),
        }
    }

    fn glyph() -> MarkGlyph {
        MarkGlyph {
            color: "#2962ff".into(),
            letter: Some("D".into()),
            ..MarkGlyph::default()
        }
    }

    fn mark(id: &str) -> TimelineMark {
        TimelineMark::new(id, T, glyph())
    }

    fn choices(pairs: &[(&str, bool)]) -> MarkGroupChoices {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
    }

    // ---- display state merge (marks-state.test.ts) ----

    #[test]
    fn a_bare_boolean_toggles_the_lane_and_keeps_the_groups() {
        let base = MarksDisplayState {
            visible: true,
            groups: choices(&[("a", false)]),
        };
        assert_eq!(
            base.merged(&json!(false)),
            MarksDisplayState {
                visible: false,
                groups: choices(&[("a", false)])
            }
        );
        assert_eq!(base.merged(&json!(true)).groups, choices(&[("a", false)]));
    }

    #[test]
    fn an_object_patches_visible_and_merges_groups_additively() {
        let base = MarksDisplayState {
            visible: true,
            groups: choices(&[("a", false), ("b", true)]),
        };
        let out = base.merged(&json!({ "groups": { "b": false, "c": false } }));
        assert_eq!(
            out,
            MarksDisplayState {
                visible: true,
                groups: choices(&[("a", false), ("b", false), ("c", false)])
            }
        );
        assert_eq!(
            base.merged(&json!({ "visible": false })).groups,
            base.groups
        );
    }

    #[test]
    fn malformed_patch_values_drop_and_the_base_is_never_mutated() {
        let base = MarksDisplayState::default();
        let out =
            base.merged(&json!({ "visible": "no", "groups": { "a": "off", "b": 0, "c": true } }));
        assert_eq!(
            out,
            MarksDisplayState {
                visible: true,
                groups: choices(&[("c", true)])
            }
        );
        assert_eq!(
            base,
            MarksDisplayState {
                visible: true,
                groups: HashMap::new()
            }
        );
        assert_eq!(base.merged(&Value::Null), base);
        assert_eq!(base.merged(&json!("x")), base);
        // An array of groups reads like an object keyed by index, as `Object.entries` does.
        assert_eq!(
            base.merged(&json!({ "groups": [true, "x"] })).groups,
            choices(&[("0", true)])
        );
        assert_eq!(base.merged(&json!({ "groups": "nope" })), base);
    }

    // ---- visibility ----

    #[test]
    fn the_stored_choice_wins_else_the_declared_default_else_visible() {
        let groups = [
            group("shown", "Shown", None, None),
            group("quiet", "Quiet", Some(false), None),
        ];
        let state = MarksDisplayState {
            visible: true,
            groups: choices(&[("shown", false), ("quiet", true)]),
        };
        assert!(!state.group_visible(Some("shown"), &groups));
        assert!(state.group_visible(Some("quiet"), &groups));
        let fresh = MarksDisplayState::default();
        assert!(!fresh.group_visible(Some("quiet"), &groups));
        assert!(fresh.group_visible(Some("shown"), &groups));
        assert!(fresh.group_visible(Some("unknown"), &groups));
        let other = MarksDisplayState {
            visible: true,
            groups: choices(&[("x", false)]),
        };
        assert!(other.group_visible(None, &groups)); // ungrouped marks always show
    }

    fn nested() -> Vec<MarkGroup> {
        vec![
            group("news", "News", Some(false), None),
            group("news-latest", "Latest", None, Some("news")),
            group("news-all", "All", Some(false), Some("news")),
            group("economic", "Economic", None, None),
            group("economic-high", "High", None, Some("economic")),
            group("economic-low", "Low", Some(false), Some("economic")),
        ]
    }

    #[test]
    fn a_child_paints_only_while_its_parent_does() {
        let groups = nested();
        let none = MarkGroupChoices::new();
        assert!(!mark_group_visible(&none, Some("news-latest"), &groups)); // own default on, parent off
        assert!(mark_group_visible(&none, Some("economic-high"), &groups));
        assert!(!mark_group_visible(&none, Some("economic-low"), &groups)); // own default off
        let news_on = choices(&[("news", true)]);
        assert!(mark_group_visible(&news_on, Some("news-latest"), &groups));
        assert!(!mark_group_visible(&news_on, Some("news-all"), &groups)); // still its own choice
    }

    #[test]
    fn a_child_keeps_its_own_choice_while_the_parent_is_off() {
        let groups = nested();
        let state = choices(&[("economic", false), ("economic-low", true)]);
        assert!(mark_group_own_visible(&state, "economic-low", &groups));
        assert!(!mark_group_visible(&state, Some("economic-low"), &groups));
        let back = choices(&[("economic", true), ("economic-low", true)]);
        assert!(mark_group_visible(&back, Some("economic-low"), &groups));
    }

    fn row_labels(groups: &[MarkGroup]) -> Vec<String> {
        mark_group_rows(groups)
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.group.id))
            .collect()
    }

    #[test]
    fn the_events_tab_lists_each_parent_followed_by_its_children() {
        assert_eq!(
            row_labels(&nested()),
            [
                "news",
                "  news-latest",
                "  news-all",
                "economic",
                "  economic-high",
                "  economic-low"
            ]
        );
    }

    #[test]
    fn unknown_self_or_cyclic_parents_never_make_a_group_disappear() {
        let odd = vec![
            group("a", "A", None, Some("ghost")),
            group("b", "B", None, Some("b")),
            group("c", "C", None, Some("d")),
            group("d", "D", None, Some("c")),
        ];
        let mut ids: Vec<String> = mark_group_rows(&odd)
            .iter()
            .map(|r| r.group.id.clone())
            .collect();
        ids.sort();
        assert_eq!(ids, ["a", "b", "c", "d"]);
        let none = MarkGroupChoices::new();
        assert!(mark_group_visible(&none, Some("a"), &odd));
        assert!(mark_group_visible(&none, Some("c"), &odd)); // cycle stops, no hang
        let off = choices(&[("d", false)]);
        assert!(!mark_group_visible(&off, Some("c"), &odd)); // reached through the cycle
    }

    #[test]
    fn duplicate_group_ids_are_placed_once() {
        let dup = vec![group("a", "A1", None, None), group("a", "A2", None, None)];
        assert_eq!(mark_group_rows(&dup).len(), 1);
    }

    // ---- the model (marks-controller.test.ts, renderer-free parts) ----

    #[test]
    fn marks_keep_insertion_order_and_a_re_added_id_is_replaced_in_place() {
        let mut m = MarksModel::new();
        m.add(mark("b")).unwrap();
        m.add(mark("a")).unwrap();
        let ids = |m: &MarksModel| m.all().iter().map(|x| x.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&m), ["b", "a"]);
        let mut b2 = mark("b");
        b2.title = Some("B2".into());
        m.add(b2).unwrap();
        assert_eq!(ids(&m), ["b", "a"]);
        assert_eq!(m.get("b").and_then(|x| x.title.as_deref()), Some("B2"));
        assert!(m.remove("b"));
        assert!(!m.remove("b"));
        assert_eq!(ids(&m), ["a"]);
        assert_eq!(m.get("a").map(|x| x.id.as_str()), Some("a"));
        m.set([mark("x"), mark("y")]).unwrap();
        assert_eq!(ids(&m), ["x", "y"]);
        assert!(m.remove("x"));
        m.add(mark("z")).unwrap();
        assert_eq!(ids(&m), ["y", "z"]);
        assert_eq!(m.get("z").map(|x| x.id.as_str()), Some("z"));
        assert!(m.clear());
        assert!(m.all().is_empty());
        assert!(!m.clear()); // already empty
    }

    #[test]
    fn marks_without_an_id_are_rejected() {
        let mut m = MarksModel::new();
        let err = m.add(mark("")).unwrap_err();
        assert_eq!(err, MarksError::EmptyMarkId);
        assert_eq!(
            err.to_string(),
            "[vela] marks: `id` must be a non-empty string"
        );
        assert!(m.all().is_empty());
    }

    #[test]
    fn a_failing_set_keeps_the_marks_before_the_bad_one() {
        let mut m = MarksModel::new();
        m.add(mark("old")).unwrap();
        assert_eq!(
            m.set([mark("a"), mark(""), mark("b")]),
            Err(MarksError::EmptyMarkId)
        );
        assert_eq!(
            m.all().iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
            ["a"]
        );
    }

    #[test]
    fn groups_are_defined_in_order_and_redefinition_replaces_in_place() {
        let mut m = MarksModel::new();
        m.define_group(group("dividends", "Dividends", None, None))
            .unwrap();
        m.define_group(group("splits", "Splits", None, None))
            .unwrap();
        m.define_group(group("dividends", "Div", Some(false), None))
            .unwrap();
        assert_eq!(
            m.group_definitions(),
            [
                group("dividends", "Div", Some(false), None),
                group("splits", "Splits", None, None)
            ]
        );
    }

    #[test]
    fn malformed_groups_are_rejected() {
        let mut m = MarksModel::new();
        assert_eq!(
            m.define_group(group("", "x", None, None)),
            Err(MarksError::EmptyGroupId)
        );
        let err = m
            .define_group(group("bad", "Bad", None, Some("")))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "[vela] marks.defineGroup: group \"bad\" has a `parent` that is not a group id"
        );
        assert!(m.group_definitions().is_empty());
    }

    #[test]
    fn group_visibility_uses_the_stored_choice_with_the_declared_default_as_fallback() {
        let mut m = MarksModel::new();
        m.define_group(group("quiet", "Quiet", Some(false), None))
            .unwrap();
        let none = MarkGroupChoices::new();
        assert!(!m.is_group_visible(&none, "quiet"));
        assert!(m.is_group_visible(&none, "other"));
        assert!(m.is_group_visible(&choices(&[("quiet", true)]), "quiet"));
    }

    #[test]
    fn a_nested_group_reads_hidden_while_its_parent_is_off() {
        let mut m = MarksModel::new();
        m.define_group(group("news", "News", Some(false), None))
            .unwrap();
        m.define_group(group("news-latest", "Latest news", None, Some("news")))
            .unwrap();
        assert!(!m.is_group_visible(&MarkGroupChoices::new(), "news-latest"));
        assert!(m.is_group_visible(&choices(&[("news", true)]), "news-latest"));
    }

    // ---- data types ----

    #[test]
    fn glyph_group_and_click_event_json_uses_the_original_field_names() {
        let g = MarkGlyph {
            shape: Some(MarkShape::Diamond),
            ..glyph()
        };
        assert_eq!(
            serde_json::to_value(&g).unwrap(),
            json!({ "shape": "diamond", "color": "#2962ff", "letter": "D" })
        );
        assert_eq!(MarkShape::default().as_str(), "circle");
        assert_eq!(
            serde_json::to_value(group("g", "G", None, Some("p"))).unwrap(),
            json!({ "id": "g", "label": "G", "parent": "p" })
        );
        let click = MarkClickEvent {
            id: "a".into(),
            ids: vec!["a".into(), "b".into()],
            time: T,
            group: Some("dividends".into()),
        };
        let v = serde_json::to_value(&click).unwrap();
        assert_eq!(
            v,
            json!({ "id": "a", "ids": ["a", "b"], "time": T, "group": "dividends" })
        );
        assert_eq!(serde_json::from_value::<MarkClickEvent>(v).unwrap(), click);
    }

    #[test]
    fn panel_buttons_run_their_action_and_compare_by_identity() {
        use std::cell::Cell;
        let hits = Rc::new(Cell::new(0));
        let sink = Rc::clone(&hits);
        let action = MarkAction::new(move || sink.set(sink.get() + 1));
        let item = MarkPanelItem::Button {
            label: "Open".into(),
            primary: Some(true),
            close: None,
            run: action.clone(),
        };
        if let MarkPanelItem::Button { run, .. } = &item {
            run.run();
        }
        assert_eq!(hits.get(), 1);
        assert_eq!(item.clone(), item);
        assert_ne!(action, MarkAction::new(|| {}));
        let mut with_content = mark("p");
        with_content.content = Some(MarkContent::Panel(vec![
            MarkPanelItem::Text { text: "t".into() },
            MarkPanelItem::Field {
                label: "l".into(),
                value: "v".into(),
            },
            item,
        ]));
        assert_eq!(with_content.clone(), with_content);
    }
}
