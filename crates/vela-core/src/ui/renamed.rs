//! The pure parts of the original's `renamed.ts`: the kit's earlier names for components
//! renamed after shadcn's (kept for one release), and the classes the button, badge, tabs
//! and toggle group had before they became `create<Name>` functions.
//!
//! Almost all of that file is aliases of kit components and classes wrapping DOM elements:
//! the browser crate's. What is data and kept here: [`tab_available`], the `id` to `value`
//! keying of the deprecated tab / toggle items ([`Keyed`]), and the badge controller's root
//! props ([`BadgeController`]). The button controller needs the kit's variant resolution
//! (`buttonVariants`) and stays with the kit.

/// A tab a user can reach: neither disabled nor hidden.
pub fn tab_available(disabled: bool, hidden: bool) -> bool {
    !disabled && !hidden
}

/// An item as the deprecated `Tabs` and `ToggleGroup` take it: keyed by `id` (the kit's
/// items carry `value`), or already in the kit's shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Keyed {
    Id(String),
    Value(String),
}

impl Keyed {
    /// The kit's `value` of the item (`withValue`).
    pub fn into_value(self) -> String {
        match self {
            Keyed::Id(v) | Keyed::Value(v) => v,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BadgeVariant {
    #[default]
    Default,
    Up,
    Down,
    Other(&'static str),
}

impl BadgeVariant {
    pub fn as_str(&self) -> &'static str {
        match self {
            BadgeVariant::Default => "default",
            BadgeVariant::Up => "up",
            BadgeVariant::Down => "down",
            BadgeVariant::Other(s) => s,
        }
    }
}

/// The root attributes `badgeController().rootProps()` reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BadgeRootProps {
    pub data_slot: &'static str,
    pub data_variant: &'static str,
    pub data_count: bool,
}

/// `badgeController`: a badge's variant and its root props.
#[derive(Clone, Debug)]
pub struct BadgeController {
    variant: BadgeVariant,
    count: bool,
}

impl BadgeController {
    /// `count`: a round count pill (`3`, `9+`) rather than a label.
    pub fn new(variant: Option<BadgeVariant>, count: bool) -> Self {
        Self {
            variant: variant.unwrap_or_default(),
            count,
        }
    }

    pub fn variant(&self) -> BadgeVariant {
        self.variant
    }

    pub fn set_variant(&mut self, variant: BadgeVariant) {
        self.variant = variant;
    }

    pub fn root_props(&self) -> BadgeRootProps {
        BadgeRootProps {
            data_slot: "badge",
            data_variant: self.variant.as_str(),
            data_count: self.count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tab_is_available_unless_disabled_or_hidden() {
        assert!(tab_available(false, false));
        assert!(!tab_available(false, true));
        assert!(!tab_available(true, false));
    }

    #[test]
    fn keyed_items_resolve_to_a_value() {
        assert_eq!(Keyed::Id("x".into()).into_value(), "x");
        assert_eq!(Keyed::Value("y".into()).into_value(), "y");
    }

    #[test]
    fn the_badge_controller_keeps_its_root_props() {
        let mut badge = BadgeController::new(None, true);
        badge.set_variant(BadgeVariant::Down);
        assert_eq!(
            badge.root_props(),
            BadgeRootProps {
                data_slot: "badge",
                data_variant: "down",
                data_count: true
            }
        );
    }
}
