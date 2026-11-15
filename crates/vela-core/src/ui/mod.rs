//! The public UI vocabulary of the original `src/ui`: the keymap and the kit's data types.
//!
//! The original's `ui/index.ts` is mostly a re-export of the `@luxalgo/vela-ui` component kit
//! (dialogs, menus, pickers: DOM and Zag state machines). None of that is here. What is:
//!
//! - [`keymap`]: the `KeymapManager`, with a key event taken as plain data
//!   ([`keymap::KeyEvent`]). Attaching it to a DOM target is the browser crate's job.
//! - [`renamed`]: the pure parts of `renamed.ts` (the deprecated names the kit keeps for one
//!   release): the tab availability rule, the `id` to `value` keying and the badge / button
//!   controllers' root props. The classes wrapping DOM elements stay with the browser.
//! - [`MenuItemDescriptor`]: the kit's menu item, the descriptor currency the widget's menus
//!   are built from.
//!
//! `chromeStyles.ts` (CSS injection into a document or shadow root) and `sanitize-html.ts`
//! (already ported in `vela-web`) are not part of this crate.

pub mod keymap;
pub mod renamed;

pub use keymap::{
    KeyBindingDescriptor, KeyEvent, KeyEventTarget, KeymapManager, KeymapOptions, KeymapPlatform,
    ResolvedBinding, chord_key_of, is_editable_target,
};

/// One menu entry. `id` is the selection value reported to the menu's `onSelect`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MenuItemDescriptor {
    pub id: String,
    pub label: String,
    pub disabled: bool,
    /// The row's tooltip, e.g. why a disabled row is unavailable.
    pub title: Option<String>,
    /// Draw a separator line above this item.
    pub separator_before: bool,
    /// Right-aligned hint (e.g. a shortcut display from the keymap).
    pub hint: Option<String>,
    /// Selected state (`None`: a plain item).
    pub checked: Option<bool>,
    /// Render as a switch row and keep the menu open on selection.
    pub toggle: bool,
    /// Icon id of the icon registry.
    pub icon: Option<String>,
    /// Favorite star at the row's right edge (`Some(false)` outline, `Some(true)` filled).
    pub favorite: Option<bool>,
    /// Nested entries: the row becomes a submenu trigger.
    pub submenu: Option<Vec<MenuItemDescriptor>>,
}

impl MenuItemDescriptor {
    /// A plain item.
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            ..Self::default()
        }
    }
}
