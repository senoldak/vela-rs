//! Display state of the `marks` renderer feature: the timeline-mark lane's master toggle
//! plus per-group visibility. It is ported with the marks model
//! ([`crate::marks::MarksDisplayState`]); this module re-exports it under the original's
//! path, with the original's free functions as thin delegates.

use serde_json::Value;

use crate::marks::MarkGroup;
pub use crate::marks::{MarkGroupRow, MarksDisplayState, mark_group_own_visible, mark_group_rows};

pub fn default_marks_state() -> MarksDisplayState {
    MarksDisplayState::default()
}

/// Validated merge of an untrusted partial onto a state (see
/// [`MarksDisplayState::merged`]).
pub fn merge_marks_state(base: &MarksDisplayState, patch: &Value) -> MarksDisplayState {
    base.merged(patch)
}

/// Whether a group's marks paint under this display state: its own switch and every
/// ancestor's.
pub fn mark_group_visible(
    state: &MarksDisplayState,
    group_id: Option<&str>,
    groups: &[MarkGroup],
) -> bool {
    state.group_visible(group_id, groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_re_export_is_the_marks_model_state() {
        let state = default_marks_state();
        assert!(state.visible && state.groups.is_empty());
        let merged = merge_marks_state(&state, &json!({ "groups": { "news": false } }));
        assert!(!mark_group_visible(&merged, Some("news"), &[]));
        assert!(mark_group_visible(&merged, None, &[]));
        assert!(!merge_marks_state(&merged, &json!(false)).visible);
    }
}
