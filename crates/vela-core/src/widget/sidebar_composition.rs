//! Sidebar COMPOSITION: the host's declarative layout of the right-edge toolbar, the way the
//! topbar composition lays out the topbar. Two lists of ids, top to bottom: the upper
//! section (from the top edge down) and the lower section (anchored to the bottom edge).
//!
//! Entries are IDS in one shared vocabulary: a side panel's id (the shell's own `objects`
//! and `dataWindow`, or a registered side panel's id), a sidebar action's id (a widget
//! action with target `sidebar`), the flow slots `"panels"` and `"actions"` (everything of
//! that kind not placed explicitly, in order), and `"|"`, which starts a new group (a
//! hairline between groups). Unknown ids are skipped, so a list can name a panel that
//! registers later.

use std::collections::{HashMap, HashSet};

/// The host-facing option: entries per section, top to bottom. An undeclared section
/// keeps its default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SidebarComposition {
    pub top: Option<Vec<String>>,
    pub bottom: Option<Vec<String>>,
}

pub const SIDEBAR_PANELS_SLOT: &str = "panels";
pub const SIDEBAR_ACTIONS_SLOT: &str = "actions";
pub const SIDEBAR_SEPARATOR: &str = "|";

/// Default upper section: every side panel the host did not place, in dock order.
pub const SIDEBAR_DEFAULT_TOP: [&str; 1] = [SIDEBAR_PANELS_SLOT];
/// Default lower section: the shell's inspection panels, then the sidebar actions.
pub const SIDEBAR_DEFAULT_BOTTOM: [&str; 4] = [
    "objects",
    "dataWindow",
    SIDEBAR_SEPARATOR,
    SIDEBAR_ACTIONS_SLOT,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarKind {
    Panel,
    Action,
}

/// Something the sidebar can hold. Implement it on the host's own entry type so
/// [`compose_sidebar`] lays out the entries themselves.
pub trait SidebarSource {
    fn id(&self) -> &str;
    fn kind(&self) -> SidebarKind;
    fn order(&self) -> f64;
}

/// A plain source record.
#[derive(Clone, Debug, PartialEq)]
pub struct SidebarEntry {
    pub id: String,
    pub kind: SidebarKind,
    pub order: f64,
}

impl SidebarSource for SidebarEntry {
    fn id(&self) -> &str {
        &self.id
    }
    fn kind(&self) -> SidebarKind {
        self.kind
    }
    fn order(&self) -> f64 {
        self.order
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SidebarLayout<T> {
    /// Groups top to bottom; each group is a run of entries.
    pub top: Vec<Vec<T>>,
    pub bottom: Vec<Vec<T>>,
}

/// Lay `sources` out into the sidebar. An id placed explicitly is used once (its first
/// placement wins, so a slot never repeats it); a flow slot takes, in `order`, whatever of
/// its kind is left; empty groups collapse.
pub fn compose_sidebar<T: SidebarSource + Clone>(
    sources: &[T],
    comp: &SidebarComposition,
) -> SidebarLayout<T> {
    let to_vec = |d: &[&str]| d.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let top = comp
        .top
        .clone()
        .unwrap_or_else(|| to_vec(&SIDEBAR_DEFAULT_TOP));
    let bottom = comp
        .bottom
        .clone()
        .unwrap_or_else(|| to_vec(&SIDEBAR_DEFAULT_BOTTOM));
    // A later source with the same id wins the lookup, as `new Map(...)` does.
    let by_id: HashMap<&str, &T> = sources.iter().map(|s| (s.id(), s)).collect();
    let placed: HashSet<&str> = top
        .iter()
        .chain(bottom.iter())
        .map(String::as_str)
        .filter(|id| by_id.contains_key(id))
        .collect();
    let mut used: HashSet<String> = HashSet::new();
    let mut lay = |ids: &[String]| -> Vec<Vec<T>> {
        let mut groups: Vec<Vec<T>> = vec![Vec::new()];
        for id in ids {
            if id == SIDEBAR_SEPARATOR {
                groups.push(Vec::new());
                continue;
            }
            let run: Vec<T> = if id == SIDEBAR_PANELS_SLOT || id == SIDEBAR_ACTIONS_SLOT {
                let kind = if id == SIDEBAR_PANELS_SLOT {
                    SidebarKind::Panel
                } else {
                    SidebarKind::Action
                };
                let mut flow: Vec<&T> = sources
                    .iter()
                    .filter(|s| {
                        s.kind() == kind && !placed.contains(s.id()) && !used.contains(s.id())
                    })
                    .collect();
                flow.sort_by(|a, b| a.order().total_cmp(&b.order()));
                flow.into_iter().cloned().collect()
            } else {
                match by_id.get(id.as_str()) {
                    Some(s) if !used.contains(id) => vec![(*s).clone()],
                    _ => Vec::new(),
                }
            };
            for s in &run {
                used.insert(s.id().to_string());
            }
            if let Some(group) = groups.last_mut() {
                group.extend(run);
            }
        }
        groups.retain(|g| !g.is_empty());
        groups
    };
    let top = lay(&top);
    let bottom = lay(&bottom);
    SidebarLayout { top, bottom }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, kind: SidebarKind, order: f64) -> SidebarEntry {
        SidebarEntry {
            id: id.into(),
            kind,
            order,
        }
    }

    fn ids(groups: &[Vec<SidebarEntry>]) -> Vec<Vec<&str>> {
        groups
            .iter()
            .map(|g| g.iter().map(|e| e.id.as_str()).collect())
            .collect()
    }

    fn comp(top: Option<&[&str]>, bottom: Option<&[&str]>) -> SidebarComposition {
        let conv = |l: &[&str]| l.iter().map(|s| s.to_string()).collect();
        SidebarComposition {
            top: top.map(conv),
            bottom: bottom.map(conv),
        }
    }

    fn sources() -> Vec<SidebarEntry> {
        vec![
            entry("objects", SidebarKind::Panel, 0.0),
            entry("dataWindow", SidebarKind::Panel, 1.0),
            entry("notes", SidebarKind::Panel, 100.0),
            entry("snap", SidebarKind::Action, 0.0),
            entry("grid", SidebarKind::Action, -5.0),
        ]
    }

    #[test]
    fn the_default_layout_puts_the_inspection_panels_below_and_the_rest_on_top() {
        let layout = compose_sidebar(&sources(), &SidebarComposition::default());
        assert_eq!(ids(&layout.top), [vec!["notes"]]);
        assert_eq!(
            ids(&layout.bottom),
            [vec!["objects", "dataWindow"], vec!["grid", "snap"]]
        );
    }

    #[test]
    fn an_explicit_placement_is_used_once_and_leaves_the_flow_slot() {
        let layout = compose_sidebar(
            &sources(),
            &comp(Some(&["snap", "panels", "snap"]), Some(&["actions"])),
        );
        assert_eq!(
            ids(&layout.top),
            [vec!["snap", "objects", "dataWindow", "notes"]]
        );
        assert_eq!(ids(&layout.bottom), [vec!["grid"]]);
    }

    #[test]
    fn separators_start_groups_and_empty_groups_collapse() {
        let layout = compose_sidebar(
            &sources(),
            &comp(
                Some(&["|", "notes", "|", "|", "unknown", "|", "snap"]),
                Some(&[]),
            ),
        );
        assert_eq!(ids(&layout.top), [vec!["notes"], vec!["snap"]]);
        assert!(layout.bottom.is_empty());
    }

    #[test]
    fn unknown_ids_are_skipped_and_no_sources_lay_out_nothing() {
        let layout = compose_sidebar::<SidebarEntry>(&[], &SidebarComposition::default());
        assert!(layout.top.is_empty() && layout.bottom.is_empty());
    }
}
