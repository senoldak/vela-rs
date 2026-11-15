//! Drawing-tool shortcut hints: maps the widget/workspace keymap bindings that arm or
//! place a specific drawing tool onto that tool's type, as platform-formatted display
//! strings (from [`KeymapManager::bindings`], so user rebinds are respected). The result
//! is pushed to the drawing toolbar, which shows each hint beside its tool in the flyout.

use std::collections::HashMap;

use crate::i18n::Translator;
use crate::ui::KeymapManager;

/// Binding id to the drawing tool its chord arms/places.
const TOOL_BINDINGS: [(&str, &str); 3] = [
    ("trendline", "drawings.trendline"),
    ("hline", "drawings.hline-cursor"),
    ("vline", "drawings.vline-cursor"),
];

/// Per-tool shortcut hints (display strings) for the toolbar flyouts, from the live
/// keymap. Keyed by drawing type key; a tool without a binding has no entry.
pub fn tool_shortcut_hints(
    keymap: &KeymapManager,
    i18n: Option<&dyn Translator>,
) -> HashMap<String, String> {
    let by_id: HashMap<String, Option<String>> = keymap
        .bindings(i18n)
        .into_iter()
        .map(|b| (b.id, b.display.into_iter().next()))
        .collect();
    let mut out = HashMap::new();
    for (tool, id) in TOOL_BINDINGS {
        if let Some(Some(display)) = by_id.get(id)
            && !display.is_empty()
        {
            out.insert(tool.to_string(), display.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{KeyBindingDescriptor, KeymapOptions, KeymapPlatform};

    fn keymap_with_defaults(platform: KeymapPlatform) -> KeymapManager {
        let km = KeymapManager::new(KeymapOptions {
            platform: Some(platform),
            base_scope: None,
        });
        for (id, keys, label) in [
            ("drawings.trendline", "alt+t", "Arm the trend line tool"),
            (
                "drawings.hline-cursor",
                "alt+h",
                "Horizontal line at the cursor price",
            ),
            (
                "drawings.vline-cursor",
                "alt+v",
                "Vertical line at the cursor time",
            ),
            ("chart.reset-view", "alt+r", "Reset view"), // unrelated: no hint
        ] {
            let _ = km.register(KeyBindingDescriptor::new(id, keys, label, |_| {}));
        }
        km
    }

    fn expect(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn maps_the_bound_drawing_tools_to_platform_formatted_display_strings() {
        assert_eq!(
            tool_shortcut_hints(&keymap_with_defaults(KeymapPlatform::Other), None),
            expect(&[
                ("trendline", "Alt+T"),
                ("hline", "Alt+H"),
                ("vline", "Alt+V")
            ])
        );
    }

    #[test]
    fn uses_mac_glyphs_on_macos() {
        assert_eq!(
            tool_shortcut_hints(&keymap_with_defaults(KeymapPlatform::Mac), None),
            expect(&[
                ("trendline", "\u{2325}T"),
                ("hline", "\u{2325}H"),
                ("vline", "\u{2325}V")
            ])
        );
    }

    #[test]
    fn respects_a_user_rebind() {
        let km = keymap_with_defaults(KeymapPlatform::Other);
        km.rebind("drawings.trendline", Some(vec!["mod+shift+t".into()]));
        assert_eq!(
            tool_shortcut_hints(&km, None)
                .get("trendline")
                .map(String::as_str),
            Some("Ctrl+Shift+T")
        );
    }

    #[test]
    fn omits_tools_whose_binding_is_missing_or_unbound() {
        let km = KeymapManager::new(KeymapOptions {
            platform: Some(KeymapPlatform::Other),
            base_scope: None,
        });
        let _ = km.register(KeyBindingDescriptor::new(
            "drawings.trendline",
            "alt+t",
            "Trend line",
            |_| {},
        ));
        let hints = tool_shortcut_hints(&km, None);
        assert_eq!(hints, expect(&[("trendline", "Alt+T")]));
        assert!(!hints.contains_key("hline"));
        // A binding rebound to no chord at all shows no hint.
        km.rebind("drawings.trendline", Some(Vec::new()));
        assert!(tool_shortcut_hints(&km, None).is_empty());
    }
}
