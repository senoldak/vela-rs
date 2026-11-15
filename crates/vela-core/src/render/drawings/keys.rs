//! Pure keyboard-shortcut mapping for the drawings layer: `(event, context)` to an
//! action, with no DOM access. The drawing controller adapts the action into an intent.
//! `Escape` is handled by the controller directly (it is stateful: popup open, placing,
//! selection).
//!
//! The original's `isEditingText(target)` reads a DOM element; here the caller passes
//! what it reads off the element ([`is_editing_text`]).

/// A keyboard event as plain data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DrawingKeyEvent {
    /// `KeyboardEvent.key`.
    pub key: String,
    pub ctrl_key: bool,
    pub meta_key: bool,
    pub shift_key: bool,
}

impl DrawingKeyEvent {
    /// An unmodified key press.
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrawingKeyContext {
    /// A real selection exists (drives copy / duplicate / nudge).
    pub has_selection: bool,
    /// A deletable target exists (selection OR the hovered drawing).
    pub has_target: bool,
    /// A label text field is focused: don't hijack keystrokes.
    pub editing_text: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DrawingKeyAction {
    Undo,
    Redo,
    Copy,
    Paste,
    Duplicate,
    Delete,
    /// Pixel deltas (the controller converts them via the projector).
    Nudge {
        dx: f64,
        dy: f64,
    },
}

/// Px per arrow press.
const NUDGE_STEP: f64 = 1.0;
/// Shift = a coarser jump.
const NUDGE_STEP_COARSE: f64 = 10.0;

/// The action a key press maps to, if any.
pub fn key_to_drawing_action(
    e: &DrawingKeyEvent,
    ctx: &DrawingKeyContext,
) -> Option<DrawingKeyAction> {
    if ctx.editing_text {
        return None; // typing a label: leave the keys alone
    }
    if e.ctrl_key || e.meta_key {
        return match js_lower(&e.key).as_str() {
            "z" => Some(if e.shift_key {
                DrawingKeyAction::Redo
            } else {
                DrawingKeyAction::Undo
            }),
            "y" => Some(DrawingKeyAction::Redo),
            "c" => ctx.has_selection.then_some(DrawingKeyAction::Copy),
            "v" => Some(DrawingKeyAction::Paste),
            "d" => ctx.has_selection.then_some(DrawingKeyAction::Duplicate),
            _ => None,
        };
    }
    if e.key == "Delete" || e.key == "Backspace" {
        return ctx.has_target.then_some(DrawingKeyAction::Delete);
    }
    if ctx.has_selection {
        let s = if e.shift_key {
            NUDGE_STEP_COARSE
        } else {
            NUDGE_STEP
        };
        match e.key.as_str() {
            "ArrowLeft" => return Some(DrawingKeyAction::Nudge { dx: -s, dy: 0.0 }),
            "ArrowRight" => return Some(DrawingKeyAction::Nudge { dx: s, dy: 0.0 }),
            "ArrowUp" => return Some(DrawingKeyAction::Nudge { dx: 0.0, dy: -s }),
            "ArrowDown" => return Some(DrawingKeyAction::Nudge { dx: 0.0, dy: s }),
            _ => {}
        }
    }
    None
}

/// `String.prototype.toLowerCase` (full Unicode lower-casing; Rust's matches for the
/// letters a shortcut can be).
fn js_lower(s: &str) -> String {
    s.to_lowercase()
}

/// Whether the keyboard event targets an editable field (so shortcuts must stand down):
/// an `INPUT`, a `TEXTAREA` or a content-editable element. `tag_name` is `None` for no
/// target or one without a tag.
pub fn is_editing_text(tag_name: Option<&str>, is_content_editable: bool) -> bool {
    match tag_name {
        None | Some("") => false,
        Some(tag) => tag == "INPUT" || tag == "TEXTAREA" || is_content_editable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> DrawingKeyContext {
        DrawingKeyContext {
            has_selection: true,
            has_target: true,
            editing_text: false,
        }
    }

    fn ctrl(key: &str) -> DrawingKeyEvent {
        DrawingKeyEvent {
            ctrl_key: true,
            ..DrawingKeyEvent::new(key)
        }
    }

    fn act(e: &DrawingKeyEvent, c: &DrawingKeyContext) -> Option<DrawingKeyAction> {
        key_to_drawing_action(e, c)
    }

    #[test]
    fn maps_undo_and_redo_for_ctrl_or_cmd() {
        assert_eq!(act(&ctrl("z"), &ctx()), Some(DrawingKeyAction::Undo));
        let shifted = DrawingKeyEvent {
            shift_key: true,
            ..ctrl("z")
        };
        assert_eq!(act(&shifted, &ctx()), Some(DrawingKeyAction::Redo));
        assert_eq!(act(&ctrl("y"), &ctx()), Some(DrawingKeyAction::Redo));
        let meta = DrawingKeyEvent {
            meta_key: true,
            ..DrawingKeyEvent::new("z")
        };
        assert_eq!(act(&meta, &ctx()), Some(DrawingKeyAction::Undo));
    }

    #[test]
    fn a_shifted_letter_key_is_lower_cased() {
        assert_eq!(act(&ctrl("Z"), &ctx()), Some(DrawingKeyAction::Undo));
    }

    #[test]
    fn copy_and_duplicate_need_a_selection_paste_does_not() {
        let none = DrawingKeyContext {
            has_selection: false,
            ..ctx()
        };
        assert_eq!(act(&ctrl("c"), &ctx()), Some(DrawingKeyAction::Copy));
        assert_eq!(act(&ctrl("c"), &none), None);
        assert_eq!(act(&ctrl("d"), &ctx()), Some(DrawingKeyAction::Duplicate));
        assert_eq!(act(&ctrl("d"), &none), None);
        assert_eq!(act(&ctrl("v"), &none), Some(DrawingKeyAction::Paste));
    }

    #[test]
    fn delete_needs_a_target_and_nudge_needs_a_selection_shift_is_coarse() {
        let no_target = DrawingKeyContext {
            has_target: false,
            ..ctx()
        };
        let no_selection = DrawingKeyContext {
            has_selection: false,
            ..ctx()
        };
        assert_eq!(
            act(&DrawingKeyEvent::new("Delete"), &ctx()),
            Some(DrawingKeyAction::Delete)
        );
        assert_eq!(act(&DrawingKeyEvent::new("Backspace"), &no_target), None);
        assert_eq!(
            act(&DrawingKeyEvent::new("ArrowLeft"), &ctx()),
            Some(DrawingKeyAction::Nudge { dx: -1.0, dy: 0.0 })
        );
        let shifted = DrawingKeyEvent {
            shift_key: true,
            ..DrawingKeyEvent::new("ArrowDown")
        };
        assert_eq!(
            act(&shifted, &ctx()),
            Some(DrawingKeyAction::Nudge { dx: 0.0, dy: 10.0 })
        );
        assert_eq!(
            act(&DrawingKeyEvent::new("ArrowRight"), &no_selection),
            None
        );
        assert_eq!(
            act(&DrawingKeyEvent::new("ArrowUp"), &ctx()),
            Some(DrawingKeyAction::Nudge { dx: 0.0, dy: -1.0 })
        );
        assert_eq!(
            act(&DrawingKeyEvent::new("ArrowRight"), &ctx()),
            Some(DrawingKeyAction::Nudge { dx: 1.0, dy: 0.0 })
        );
    }

    #[test]
    fn a_delete_with_a_modifier_is_not_a_delete() {
        assert_eq!(act(&ctrl("Delete"), &ctx()), None);
    }

    #[test]
    fn stands_down_entirely_while_a_text_field_is_focused() {
        let editing = DrawingKeyContext {
            editing_text: true,
            ..ctx()
        };
        assert_eq!(act(&ctrl("z"), &editing), None);
        assert_eq!(act(&DrawingKeyEvent::new("Delete"), &editing), None);
    }

    #[test]
    fn ignores_unrelated_keys() {
        assert_eq!(act(&DrawingKeyEvent::new("a"), &ctx()), None);
        assert_eq!(act(&DrawingKeyEvent::new("Enter"), &ctx()), None);
        assert_eq!(act(&ctrl("x"), &ctx()), None);
    }

    #[test]
    fn is_editing_text_detects_inputs_textareas_and_content_editable() {
        assert!(is_editing_text(Some("TEXTAREA"), false));
        assert!(is_editing_text(Some("INPUT"), false));
        assert!(is_editing_text(Some("DIV"), true));
        assert!(!is_editing_text(Some("CANVAS"), false));
        assert!(!is_editing_text(None, false));
        assert!(!is_editing_text(Some(""), true));
    }
}
