//! Shortcuts help: a dialog listing every registered keymap binding, grouped by category,
//! with platform-formatted chords. Driven entirely by [`KeymapManager::bindings`]
//! (plugin-registered bindings appear automatically).
//!
//! The pure half of the original `ShortcutsHelp`: [`shortcuts_help_sections`] lays the
//! panel out. Left for the browser crate: the dialog, the category/row/key elements and
//! CSS. The panel reads the shell's translator each time it opens.

use crate::i18n::{MessageLike, Translator, msg, text_of};
use crate::ui::KeymapManager;

/// The dialog's title.
pub fn title_message() -> MessageLike {
    msg("widget.shortcuts.title", "Keyboard shortcuts")
}

/// One row: a label and its chords.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelpRow {
    pub label: String,
    pub keys: Vec<String>,
}

/// One category heading and its rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelpCategory {
    pub title: String,
    pub rows: Vec<HelpRow>,
}

/// The panel's content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutsHelpView {
    pub categories: Vec<HelpCategory>,
    /// The closing paragraph about the two type-to-act routes.
    pub typing_note: String,
}

/// Lay the panel out: the keymap's bindings grouped by category (first-seen order), then the
/// pointer gestures, which live outside the keymap and are listed like bindings so the panel
/// stays the one complete reference, then the note on the two type-to-act routes (also
/// outside the keymap: any-printable routing).
pub fn shortcuts_help_sections(
    keymap: &KeymapManager,
    i18n: Option<&dyn Translator>,
) -> ShortcutsHelpView {
    let t = |m: MessageLike| text_of(i18n, &m, &[]);
    let mut categories: Vec<HelpCategory> = Vec::new();
    for b in keymap.bindings(i18n) {
        let row = HelpRow {
            label: b.label,
            keys: b.display,
        };
        match categories.iter_mut().find(|c| c.title == b.category) {
            Some(c) => c.rows.push(row),
            None => categories.push(HelpCategory {
                title: b.category,
                rows: vec![row],
            }),
        }
    }
    let gesture = |label: MessageLike, keys: MessageLike| HelpRow {
        label: t(label),
        keys: vec![t(keys)],
    };
    categories.push(HelpCategory {
        title: t(msg("widget.shortcuts.mouse", "Mouse")),
        rows: vec![
            gesture(
                msg("widget.shortcuts.scrollHistory", "Scroll through history"),
                msg("widget.shortcuts.scrollHistoryKeys", "Shift+Scroll"),
            ),
            gesture(
                msg("widget.shortcuts.measure", "Measure from the press point"),
                msg("widget.shortcuts.measureKeys", "Shift+Click"),
            ),
            gesture(
                msg(
                    "widget.shortcuts.deleteDrawing",
                    "Delete the drawing under the cursor",
                ),
                msg("widget.shortcuts.deleteDrawingKeys", "Middle-click"),
            ),
        ],
    });
    ShortcutsHelpView {
        categories,
        typing_note: t(msg(
            "widget.shortcuts.typing",
            "Typing a letter or 0 opens the symbol search; typing a digit from 1 to 9 opens the timeframe entry.",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{KeyBindingDescriptor, KeymapOptions, KeymapPlatform};

    fn keymap() -> KeymapManager {
        let km = KeymapManager::new(KeymapOptions {
            platform: Some(KeymapPlatform::Other),
            base_scope: None,
        });
        let mut undo = KeyBindingDescriptor::new("undo", "mod+z", "Undo", |_| {});
        undo.category = Some("Chart".into());
        let mut redo = KeyBindingDescriptor::new("redo", "mod+shift+z", "Redo", |_| {});
        redo.category = Some("Chart".into());
        let mut tool = KeyBindingDescriptor::new("tool", "alt+t", "Trend line", |_| {});
        tool.category = Some("Drawings".into());
        let plain = KeyBindingDescriptor::new("plain", "x", "Plain", |_| {});
        for d in [undo, tool, redo, plain] {
            let _ = km.register(d);
        }
        km
    }

    #[test]
    fn bindings_group_by_category_in_first_seen_order() {
        let view = shortcuts_help_sections(&keymap(), None);
        let titles: Vec<&str> = view.categories.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["Chart", "Drawings", "General", "Mouse"]);
        let chart = &view.categories[0];
        assert_eq!(
            chart.rows,
            [
                HelpRow {
                    label: "Undo".into(),
                    keys: vec!["Ctrl+Z".into()]
                },
                HelpRow {
                    label: "Redo".into(),
                    keys: vec!["Ctrl+Shift+Z".into()]
                },
            ]
        );
    }

    #[test]
    fn the_pointer_gestures_close_the_list_and_the_typing_note_follows() {
        let view = shortcuts_help_sections(&keymap(), None);
        let mouse = view.categories.last().expect("a mouse category");
        assert_eq!(mouse.title, "Mouse");
        assert_eq!(
            mouse
                .rows
                .iter()
                .map(|r| (r.label.as_str(), r.keys[0].as_str()))
                .collect::<Vec<_>>(),
            [
                ("Scroll through history", "Shift+Scroll"),
                ("Measure from the press point", "Shift+Click"),
                ("Delete the drawing under the cursor", "Middle-click"),
            ]
        );
        assert!(view.typing_note.starts_with("Typing a letter or 0"));
    }

    #[test]
    fn an_empty_keymap_still_lists_the_gestures() {
        let km = KeymapManager::default();
        let view = shortcuts_help_sections(&km, None);
        assert_eq!(view.categories.len(), 1);
        assert_eq!(text_of(None, &title_message(), &[]), "Keyboard shortcuts");
    }

    #[test]
    fn a_binding_with_several_chords_lists_them_all() {
        let km = KeymapManager::new(KeymapOptions {
            platform: Some(KeymapPlatform::Other),
            base_scope: None,
        });
        let mut d = KeyBindingDescriptor::new("zoom", "ctrl+=", "Zoom in", |_| {});
        d.keys.push("ctrl+plus".into());
        let _ = km.register(d);
        let view = shortcuts_help_sections(&km, None);
        assert_eq!(view.categories[0].rows[0].keys, ["Ctrl+=", "Ctrl++"]);
    }
}
