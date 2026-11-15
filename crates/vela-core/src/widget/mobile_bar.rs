//! Mobile bar: the touch-first bottom bar that replaces the topbar and the desktop bottom
//! bar on the mobile layout.
//!
//! The pure half of the original `MobileBar`: which stops the bar has ([`mobile_bar_stops`]),
//! the contributed actions placed on it ([`bar_actions`]), the maximize stop's state
//! ([`MaximizeStop`]) and the labels. Left for the browser crate: the element tree and CSS,
//! the click handlers, and re-projecting the contributed actions after registrations change
//! (re-run [`bar_actions`]).

use std::rc::Rc;

use super::contributions::{
    Contributions, MobilePlacement, WidgetActionDescriptor, WidgetActionTarget, WidgetContext,
    mobile_placement,
};
use super::timeframe::timeframe_label;
use super::topbar::symbol_button_text;
use super::topbar_composition::TOPBAR_BUILTIN_IDS;
use crate::i18n::{MessageLike, Translator, msg, text_of};

pub fn symbol_search_label() -> MessageLike {
    msg("widget.mobileBar.symbolSearch", "Symbol search")
}
pub fn timeframe_label_message() -> MessageLike {
    msg("widget.mobileBar.timeframe", "Timeframe")
}
pub fn indicators_label() -> MessageLike {
    msg("widget.mobileBar.indicators", "Indicators")
}
pub fn drawings_label() -> MessageLike {
    msg("widget.mobileBar.drawings", "Drawings")
}
pub fn maximize_chart_label() -> MessageLike {
    msg("widget.mobileBar.maximizeChart", "Maximize chart")
}
pub fn restore_layout_label() -> MessageLike {
    msg("widget.mobileBar.restoreLayout", "Restore layout")
}
pub fn more_label() -> MessageLike {
    msg("widget.mobileBar.more", "More")
}
pub fn chart_settings_label() -> MessageLike {
    msg("widget.mobileBar.chartSettings", "Chart settings")
}

/// A fixed stop of the bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MobileStop {
    Symbol,
    Timeframe,
    Indicators,
    Drawings,
    Maximize,
    More,
    Settings,
}

impl MobileStop {
    /// The stop's class suffix (`vela-mb-<name>`).
    pub fn name(self) -> &'static str {
        match self {
            MobileStop::Symbol => "symbol",
            MobileStop::Timeframe => "tf",
            MobileStop::Indicators => "indicators",
            MobileStop::Drawings => "drawings",
            MobileStop::Maximize => "maximize",
            MobileStop::More => "more",
            MobileStop::Settings => "settings",
        }
    }

    pub fn icon(self) -> Option<&'static str> {
        match self {
            MobileStop::Symbol | MobileStop::Timeframe => None,
            MobileStop::Indicators => Some("indicators"),
            MobileStop::Drawings => Some("pen"),
            MobileStop::Maximize => Some("maximize"),
            MobileStop::More => Some("kebab"),
            MobileStop::Settings => Some("gear"),
        }
    }

    pub fn label(self) -> MessageLike {
        match self {
            MobileStop::Symbol => symbol_search_label(),
            MobileStop::Timeframe => timeframe_label_message(),
            MobileStop::Indicators => indicators_label(),
            MobileStop::Drawings => drawings_label(),
            MobileStop::Maximize => maximize_chart_label(),
            MobileStop::More => more_label(),
            MobileStop::Settings => chart_settings_label(),
        }
    }
}

/// Which optional stops the shell wires (an omitted callback means no stop).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MobileBarWiring {
    /// Omitted: no indicators stop (the composition hides the `indicators` slot). A slot
    /// OVERRIDE keeps the stop: the shell routes its callback to the override.
    pub indicators: bool,
    /// Omitted: no drawings stop (the shell disabled user drawings).
    pub drawings: bool,
    /// Omitted: no maximize stop (single-chart shells: nothing to isolate).
    pub maximize: bool,
}

/// The bar's stops in order. The contributed bar actions sit between the indicators slot
/// and the drawings stop ([`ACTIONS_SLOT_AFTER`]).
pub fn mobile_bar_stops(w: MobileBarWiring) -> Vec<MobileStop> {
    let mut out = vec![MobileStop::Symbol, MobileStop::Timeframe];
    if w.indicators {
        out.push(MobileStop::Indicators);
    }
    if w.drawings {
        out.push(MobileStop::Drawings);
    }
    if w.maximize {
        out.push(MobileStop::Maximize);
    }
    out.push(MobileStop::More);
    out.push(MobileStop::Settings);
    out
}

/// The contributed actions' host sits right after the indicators slot (after the
/// timeframe stop when there is no indicators stop).
pub const ACTIONS_SLOT_AFTER: MobileStop = MobileStop::Indicators;

/// The symbol stop's text: the bare ticker.
pub fn symbol_stop_text(symbol: &str) -> String {
    symbol_button_text(symbol)
}

/// The timeframe stop's text.
pub fn timeframe_stop_text(tf: &str, i18n: Option<&dyn Translator>) -> String {
    timeframe_label(tf, i18n)
}

/// The maximize stop: lit while something is isolated (a chart over the grid, or a
/// maximized pane inside the active chart), hidden on a one-cell layout, where there is
/// nothing to isolate and the stop would be a dead press. The two flags are independent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MaximizeStop {
    pub active: bool,
    pub hidden: bool,
}

impl MaximizeStop {
    pub fn set_active(&mut self, on: bool) {
        self.active = on;
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.hidden = !visible;
    }

    /// Inverse chip + restore glyph while active.
    pub fn icon(&self) -> &'static str {
        if self.active { "restore" } else { "maximize" }
    }

    pub fn label(&self) -> MessageLike {
        if self.active {
            restore_layout_label()
        } else {
            maximize_chart_label()
        }
    }
}

/// A contributed action as the bar shows it: an icon-only stop (its label is the accessible
/// name; without an icon the label is the text).
#[derive(Clone)]
pub struct BarAction {
    pub id: String,
    pub label: String,
    pub icon: Option<String>,
    action: Rc<WidgetActionDescriptor>,
}

impl BarAction {
    /// Run the action with a fresh context.
    pub fn run(&self, ctx: &dyn WidgetContext) {
        (self.action.run)(ctx);
    }
}

/// The contributed topbar actions placed on the bar: left-aligned by default (see
/// [`mobile_placement`]), `when`-gated by `ctx`. Built-in-id actions are slot OVERRIDES: they
/// reach mobile through the slot's own stop (the shell routes it), never as an extra flow
/// stop here.
pub fn bar_actions(
    contributions: &Contributions,
    ctx: &dyn WidgetContext,
    fallback_i18n: Option<&dyn Translator>,
) -> Vec<BarAction> {
    let i18n = ctx.i18n();
    let translator: &dyn Translator = fallback_i18n.unwrap_or(&*i18n);
    contributions
        .widget_actions(WidgetActionTarget::Topbar, Some(ctx))
        .into_iter()
        .filter(|a| {
            mobile_placement(a.align, a.mobile) == MobilePlacement::Bar
                && !TOPBAR_BUILTIN_IDS.contains(&a.id.as_str())
        })
        .map(|a| BarAction {
            id: a.id.clone(),
            label: text_of(Some(translator), &a.label, &[]),
            icon: a.icon.clone(),
            action: a,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stops(w: MobileBarWiring) -> Vec<&'static str> {
        mobile_bar_stops(w).iter().map(|s| s.name()).collect()
    }

    #[test]
    fn the_bar_always_has_symbol_timeframe_more_and_settings() {
        assert_eq!(
            stops(MobileBarWiring::default()),
            ["symbol", "tf", "more", "settings"]
        );
    }

    #[test]
    fn optional_stops_appear_in_their_slots_only_when_wired() {
        let all = MobileBarWiring {
            indicators: true,
            drawings: true,
            maximize: true,
        };
        assert_eq!(
            stops(all),
            [
                "symbol",
                "tf",
                "indicators",
                "drawings",
                "maximize",
                "more",
                "settings"
            ]
        );
        // The maximize stop is absent when the shell has nothing to isolate (single-chart mode).
        let single = MobileBarWiring {
            maximize: false,
            ..all
        };
        assert!(!stops(single).contains(&"maximize"));
    }

    #[test]
    fn stop_icons_and_labels() {
        assert_eq!(MobileStop::Drawings.icon(), Some("pen"));
        assert_eq!(MobileStop::More.icon(), Some("kebab"));
        assert_eq!(MobileStop::Symbol.icon(), None);
        assert_eq!(
            text_of(None, &MobileStop::Settings.label(), &[]),
            "Chart settings"
        );
        assert_eq!(
            text_of(None, &MobileStop::Symbol.label(), &[]),
            "Symbol search"
        );
        assert_eq!(ACTIONS_SLOT_AFTER, MobileStop::Indicators);
    }

    #[test]
    fn the_stops_read_the_bare_ticker_and_the_compact_timeframe() {
        assert_eq!(symbol_stop_text("BINANCE:BTCUSDT"), "BTCUSDT");
        assert_eq!(timeframe_stop_text("60", None), "1h");
    }

    #[test]
    fn the_maximize_stop_keeps_its_lit_state_independent_of_visibility() {
        let mut stop = MaximizeStop::default();
        assert!(!stop.hidden && !stop.active);
        stop.set_visible(false); // the grid dropped to one cell
        assert!(stop.hidden);
        stop.set_visible(true); // back to a multi-chart layout
        assert!(!stop.hidden);
        stop.set_active(true);
        stop.set_visible(false);
        assert!(stop.active && stop.hidden);
        assert_eq!(stop.icon(), "restore");
        assert_eq!(text_of(None, &stop.label(), &[]), "Restore layout");
        stop.set_active(false);
        assert_eq!(stop.icon(), "maximize");
        assert_eq!(text_of(None, &stop.label(), &[]), "Maximize chart");
    }
}
