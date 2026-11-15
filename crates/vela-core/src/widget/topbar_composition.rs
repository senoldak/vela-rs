//! Topbar COMPOSITION: the host's declarative control of which topbar entries render,
//! and in what order. The option DESCRIBES what is there (an allowlist per side), never
//! what isn't: a side left undeclared keeps the shell's default composition, so the
//! plug-and-play behavior survives untouched for hosts that never reach for this.
//!
//! Entries are IDS in one shared vocabulary: the built-in controls below, the `"actions"`
//! flow slot (where unlisted contributed actions land, per their declared `align`), and
//! any CONTRIBUTED action id (a registered widget action's `id`). Naming one PINS that
//! action at the list position, overriding its declared `align`/`order`. An explicit
//! list is the side's complete contract: ids not listed do not render there, and hiding
//! a built-in removes its other entry points too (mobile counterpart, keyboard chord:
//! `mod+alt+S` for `"screenshot"`). Ctrl+Z / Ctrl+Y are NOT tied to `"undo-redo"`:
//! hiding the buttons keeps the editing chords.
//!
//! The trade-off is deliberate and documented: an explicit list FREEZES that side. A
//! built-in a future release adds will not appear for a host that curates. Hosts that
//! declare nothing keep getting new chrome by default.

use std::collections::HashSet;

/// The host-facing option: visible entries per side, in render order. An undeclared
/// side falls back to its default list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TopbarComposition {
    pub left: Option<Vec<String>>,
    pub right: Option<Vec<String>>,
}

/// The built-in entry vocabulary (everything else in a list is a contributed-action id).
/// `"layout"` renders only on multi-chart shells and `"indicators"` only while an
/// indicator surface exists (the built-in picker, or a slot OVERRIDE replacing it), so
/// listing them is necessary but not sufficient.
pub const TOPBAR_BUILTIN_IDS: [&str; 10] = [
    "symbol",
    "timeframes",
    "style",
    "layout",
    "indicators",
    "actions",
    "undo-redo",
    "alerts",
    "panels",
    "screenshot",
];

/// The default left side: the shell composition, verbatim.
pub const TOPBAR_DEFAULT_LEFT: [&str; 7] = [
    "symbol",
    "timeframes",
    "style",
    "layout",
    "indicators",
    "actions",
    "undo-redo",
];

/// The default right side (the `margin-left: auto` cluster).
pub const TOPBAR_DEFAULT_RIGHT: [&str; 4] = ["actions", "alerts", "panels", "screenshot"];

/// A composition with both sides resolved (defaults applied, duplicates dropped).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolvedTopbarComposition {
    pub left: Vec<String>,
    pub right: Vec<String>,
}

/// Resolve the host option: absent side means its default list; a duplicated id keeps its
/// FIRST occurrence (left before right) so an entry never renders twice. `"actions"`
/// is the exception: it is a flow SLOT each side legitimately owns (the defaults
/// carry one on both), so it dedupes per side only.
pub fn resolve_topbar_composition(opt: Option<&TopbarComposition>) -> ResolvedTopbarComposition {
    let mut seen: HashSet<String> = HashSet::new();
    let mut take = |list: &[String]| -> Vec<String> {
        let mut out = Vec::new();
        let mut local: HashSet<&str> = HashSet::new();
        for id in list {
            if id.is_empty()
                || local.contains(id.as_str())
                || (id != "actions" && seen.contains(id))
            {
                continue;
            }
            local.insert(id);
            seen.insert(id.clone());
            out.push(id.clone());
        }
        out
    };
    let defaults = |d: &[&str]| d.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let left = opt
        .and_then(|o| o.left.clone())
        .unwrap_or_else(|| defaults(&TOPBAR_DEFAULT_LEFT));
    let right = opt
        .and_then(|o| o.right.clone())
        .unwrap_or_else(|| defaults(&TOPBAR_DEFAULT_RIGHT));
    ResolvedTopbarComposition {
        left: take(&left),
        right: take(&right),
    }
}

/// Whether an entry id is visible anywhere in the resolved composition.
pub fn topbar_has(comp: &ResolvedTopbarComposition, id: &str) -> bool {
    comp.left.iter().any(|x| x == id) || comp.right.iter().any(|x| x == id)
}

/// The non-built-in entries: contributed-action ids PINNED to a list position.
pub fn pinned_topbar_action_ids(comp: &ResolvedTopbarComposition) -> Vec<String> {
    comp.left
        .iter()
        .chain(comp.right.iter())
        .filter(|id| !TOPBAR_BUILTIN_IDS.contains(&id.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn comp(left: Option<&[&str]>, right: Option<&[&str]>) -> TopbarComposition {
        TopbarComposition {
            left: left.map(strs),
            right: right.map(strs),
        }
    }

    #[test]
    fn no_option_or_empty_resolves_to_the_default_composition() {
        for opt in [None, Some(TopbarComposition::default())] {
            let c = resolve_topbar_composition(opt.as_ref());
            assert_eq!(c.left, strs(&TOPBAR_DEFAULT_LEFT));
            assert_eq!(c.right, strs(&TOPBAR_DEFAULT_RIGHT));
        }
    }

    #[test]
    fn a_declared_side_is_taken_verbatim_and_the_other_keeps_its_default() {
        let c = resolve_topbar_composition(Some(&comp(
            None,
            Some(&["alerts", "mytool.screenshot.open"]),
        )));
        assert_eq!(c.left, strs(&TOPBAR_DEFAULT_LEFT));
        assert_eq!(c.right, strs(&["alerts", "mytool.screenshot.open"]));
        // An explicit list is the side's complete contract: omitted ids are OUT.
        assert!(!topbar_has(&c, "screenshot"));
        assert!(!topbar_has(&c, "panels"));
    }

    #[test]
    fn an_empty_declared_side_renders_nothing_there() {
        let c = resolve_topbar_composition(Some(&comp(Some(&[]), None)));
        assert!(c.left.is_empty());
        assert_eq!(c.right, strs(&TOPBAR_DEFAULT_RIGHT));
    }

    #[test]
    fn duplicates_keep_their_first_occurrence_left_before_right() {
        let c = resolve_topbar_composition(Some(&comp(
            Some(&["symbol", "alerts", "symbol"]),
            Some(&["alerts", "screenshot"]),
        )));
        assert_eq!(c.left, strs(&["symbol", "alerts"]));
        assert_eq!(c.right, strs(&["screenshot"])); // "alerts" already claimed by left
    }

    #[test]
    fn the_actions_slot_dedupes_per_side_only() {
        let c = resolve_topbar_composition(Some(&comp(
            Some(&["actions", "actions"]),
            Some(&["actions"]),
        )));
        assert_eq!(c.left, strs(&["actions"]));
        assert_eq!(c.right, strs(&["actions"]));
    }

    #[test]
    fn empty_string_entries_are_dropped() {
        let c = resolve_topbar_composition(Some(&comp(Some(&["", "symbol"]), None)));
        assert_eq!(c.left, strs(&["symbol"]));
    }

    #[test]
    fn pinned_ids_are_exactly_the_non_built_in_entries_across_both_sides() {
        let c = resolve_topbar_composition(Some(&comp(
            Some(&["symbol", "vela-pro.indicator-menu.open", "undo-redo"]),
            Some(&["mytool.screenshot.open", "alerts"]),
        )));
        assert_eq!(
            pinned_topbar_action_ids(&c),
            strs(&["vela-pro.indicator-menu.open", "mytool.screenshot.open"])
        );
    }

    #[test]
    fn the_default_composition_pins_nothing() {
        let c = resolve_topbar_composition(None);
        assert!(pinned_topbar_action_ids(&c).is_empty());
        for id in c.left.iter().chain(c.right.iter()) {
            assert!(TOPBAR_BUILTIN_IDS.contains(&id.as_str()));
        }
    }
}
