//! Topbar: symbol label, timeframe dropdown, and the price-style dropdown (built-ins +
//! every chart type registered through the plugin SDK, labels from the registry).
//!
//! The original `Topbar` is a DOM component; this module is its pure half: what each
//! control SAYS and LISTS.
//!
//! - [`price_style_label`] / [`price_style_icon`]: a style's name and icon id;
//! - [`timeframe_menu_items`] and [`style_menu_items`]: the two dropdowns' descriptors;
//! - [`timeframe_chips`]: the duration-sorted favorite chips beside the caret, with the
//!   current value kept in place;
//! - the symbol button's text and the buttons' accessible labels.
//!
//! The composition (which entries render, and where) is [`topbar_composition`]
//! (super::topbar_composition); contributed actions come from
//! [`Contributions`](super::contributions::Contributions).
//!
//! Left for the browser crate: the element tree and CSS, the dropdown menus, the layout
//! popover trigger, the hairline separators (synced to the pixel grid on resize), the
//! contributed-action buttons and icon-only tooltips, the panel toggles, the alerts badge,
//! runtime hiding of items, and wiring the callbacks (`onSymbolClick`, `onTimeframe`,
//! `onTimeframeFavorite`, `onPriceStyle`, ...).

use std::collections::HashSet;

use super::timeframe::{favorite_timeframe_chips, timeframe_label};
use crate::chart_types::{ChartTypeRegistry, price_style_message, price_style_unavailable};
use crate::data::provider_registry::parse_symbol;
use crate::i18n::{MessageArg, MessageLike, Translator, msg, text_of};
use crate::render::core::chart_config::{BUILTIN_PRICE_STYLES, price_style_ids};
use crate::ui::MenuItemDescriptor;

/// A price style's name, in `i18n`'s language (English without one).
pub fn price_style_label(
    registry: &ChartTypeRegistry,
    id: &str,
    i18n: Option<&dyn Translator>,
) -> String {
    text_of(i18n, &price_style_message(registry, id), &[])
}

/// Icon id for a price style, and the markup to register under it on first use (a plugin
/// chart type's `icon`). `registered` tells whether the icon registry already knows an id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PriceStyleIcon {
    pub id: String,
    /// Register this markup under `id` before using it; `None` when it is already known.
    pub register: Option<String>,
}

pub fn price_style_icon(
    registry: &ChartTypeRegistry,
    id: &str,
    registered: &dyn Fn(&str) -> bool,
) -> Option<PriceStyleIcon> {
    let icon_id = format!("style-{id}");
    if registered(&icon_id) {
        return Some(PriceStyleIcon {
            id: icon_id,
            register: None,
        });
    }
    let svg = registry.get(id).and_then(|d| d.icon.clone())?;
    if svg.is_empty() {
        return None;
    }
    Some(PriceStyleIcon {
        id: icon_id,
        register: Some(svg),
    })
}

/// The symbol button DISPLAYS the bare ticker; the venue-prefixed identity stays in the
/// shell's state (the statusline meta and the picker badges name the venue).
pub fn symbol_button_text(symbol: &str) -> String {
    parse_symbol(symbol).ticker
}

/// The timeframe dropdown's rows. Stars only when the host handles the toggle
/// (`favorites` is `Some`): a starless dropdown otherwise.
pub fn timeframe_menu_items<S: AsRef<str>>(
    timeframes: &[S],
    current: &str,
    favorites: Option<&[String]>,
    i18n: Option<&dyn Translator>,
) -> Vec<MenuItemDescriptor> {
    timeframes
        .iter()
        .map(|tf| {
            let tf = tf.as_ref();
            MenuItemDescriptor {
                checked: Some(tf == current),
                favorite: favorites.map(|f| f.iter().any(|x| x == tf)),
                ..MenuItemDescriptor::new(tf, timeframe_label(tf, i18n))
            }
        })
        .collect()
}

/// The style dropdown's rows. A live list: built-ins plus plugin-registered chart types (a
/// registered type shows up here automatically: the SDK's style-picker contribution).
/// Registered types sit BELOW a separator: the built-in price styles and the plugin chart
/// types read as two distinct families. Styles the current symbol's data cannot draw are
/// listed greyed, with a tooltip saying why. `icon_for` resolves the icon id of a style.
pub fn style_menu_items(
    registry: &ChartTypeRegistry,
    current: &str,
    unavailable: &HashSet<String>,
    icon_for: &dyn Fn(&str) -> Option<String>,
    i18n: Option<&dyn Translator>,
) -> Vec<MenuItemDescriptor> {
    price_style_ids(registry)
        .into_iter()
        .enumerate()
        .map(|(i, id)| {
            let blocked = unavailable.contains(&id);
            MenuItemDescriptor {
                icon: icon_for(&id),
                checked: Some(id == current),
                separator_before: i == BUILTIN_PRICE_STYLES.len(),
                disabled: blocked,
                title: blocked.then(|| text_of(i18n, &price_style_unavailable(), &[])),
                ..MenuItemDescriptor::new(id.clone(), price_style_label(registry, &id, i18n))
            }
        })
        .collect()
}

/// One quick-switch chip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeframeChip {
    pub timeframe: String,
    pub label: String,
    /// The chip of the current timeframe: highlighted, not clickable.
    pub current: bool,
    /// Accessible name of a clickable chip (`Switch timeframe to 1h`); the current chip
    /// carries none (it is `aria-current`).
    pub aria_label: Option<String>,
}

/// The chips beside the caret and what the caret itself reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeframeChips {
    pub chips: Vec<TimeframeChip>,
    /// No favorites: the caret doubles as the current label (`solo`).
    pub caret_solo: bool,
    /// The caret's text: the current timeframe's label when solo, else none.
    pub caret_text: Option<String>,
    pub caret_aria_label: String,
}

/// The quick-switch chips: duration-sorted favorites with the highlight in place. An
/// unstarred current timeframe sits next to the caret so the favorite row never jumps.
/// Without a star handler (`stars` false) no chips render and the host owns the favorites.
pub fn timeframe_chips(
    favorites: &[String],
    current: &str,
    stars: bool,
    i18n: Option<&dyn Translator>,
) -> TimeframeChips {
    let favs = if stars {
        favorite_timeframe_chips(favorites)
    } else {
        Vec::new()
    };
    let current_is_fav = favs.iter().any(|c| c == current);
    let mut shown = favs.clone();
    if !current_is_fav && !favs.is_empty() {
        shown.push(current.to_string());
    }
    let chips = shown
        .into_iter()
        .map(|tf| {
            let label = timeframe_label(&tf, i18n);
            let is_current = tf == current;
            TimeframeChip {
                aria_label: (!is_current).then(|| {
                    text_of(
                        i18n,
                        &switch_timeframe_message(),
                        &[("timeframe", MessageArg::from(label.clone()))],
                    )
                }),
                current: is_current,
                timeframe: tf,
                label,
            }
        })
        .collect();
    if favs.is_empty() {
        let label = timeframe_label(current, i18n);
        TimeframeChips {
            chips,
            caret_solo: true,
            caret_aria_label: text_of(
                i18n,
                &current_timeframe_message(),
                &[("timeframe", MessageArg::from(label.clone()))],
            ),
            caret_text: Some(label),
        }
    } else {
        TimeframeChips {
            chips,
            caret_solo: false,
            caret_text: None,
            caret_aria_label: text_of(i18n, &timeframes_message(), &[]),
        }
    }
}

pub fn timeframes_message() -> MessageLike {
    msg("widget.topbar.timeframes", "Timeframes")
}
pub fn indicators_message() -> MessageLike {
    msg("widget.topbar.indicators", "Indicators")
}
pub fn undo_message() -> MessageLike {
    msg("widget.topbar.undo", "Undo")
}
pub fn redo_message() -> MessageLike {
    msg("widget.topbar.redo", "Redo")
}
pub fn screenshot_message() -> MessageLike {
    msg("widget.topbar.screenshot", "Download screenshot")
}
pub fn alerts_message() -> MessageLike {
    msg("widget.topbar.alerts", "Alerts")
}
pub fn timeframe_message() -> MessageLike {
    msg("widget.topbar.timeframe", "Timeframe")
}
pub fn chart_style_message() -> MessageLike {
    msg("widget.topbar.chartStyle", "Chart style")
}
pub fn layout_message() -> MessageLike {
    msg("widget.topbar.layout", "Layout")
}
pub fn switch_timeframe_message() -> MessageLike {
    msg(
        "widget.topbar.switchTimeframe",
        "Switch timeframe to {timeframe}",
    )
}
pub fn current_timeframe_message() -> MessageLike {
    msg(
        "widget.topbar.currentTimeframe",
        "Timeframe \u{2014} {timeframe}",
    )
}
pub fn current_layout_message() -> MessageLike {
    msg("widget.topbar.currentLayout", "Layout \u{2014} {layout}")
}
pub fn current_style_message() -> MessageLike {
    msg("widget.topbar.currentStyle", "Chart style \u{2014} {style}")
}

/// The style button's accessible name: `Chart style \u{2014} Candles`.
pub fn style_button_label(
    registry: &ChartTypeRegistry,
    style: &str,
    i18n: Option<&dyn Translator>,
) -> String {
    text_of(
        i18n,
        &current_style_message(),
        &[(
            "style",
            MessageArg::from(price_style_label(registry, style, i18n)),
        )],
    )
}

/// The layout button's accessible name: `Layout \u{2014} 2x2`.
pub fn layout_button_label(layout_id: Option<&str>, i18n: Option<&dyn Translator>) -> String {
    text_of(
        i18n,
        &current_layout_message(),
        &[(
            "layout",
            MessageArg::from(layout_id.unwrap_or("").to_string()),
        )],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ChartTypeRegistry {
        ChartTypeRegistry::builtin()
    }

    #[test]
    fn price_style_labels_cover_built_ins_registry_labels_and_the_raw_id_fallback() {
        let reg = registry();
        assert_eq!(price_style_label(&reg, "candles", None), "Candles");
        assert_eq!(
            price_style_label(&reg, "unknown-style", None),
            "unknown-style"
        );
    }

    #[test]
    fn the_symbol_button_shows_the_bare_ticker() {
        assert_eq!(symbol_button_text("BINANCE:BTCUSDT"), "BTCUSDT");
        assert_eq!(symbol_button_text("  AAPL "), "AAPL");
        assert_eq!(symbol_button_text(""), "");
    }

    #[test]
    fn the_timeframe_dropdown_checks_the_current_value_and_stars_only_when_handled() {
        let tfs = ["1", "60", "D"];
        let plain = timeframe_menu_items(&tfs, "60", None, None);
        assert_eq!(
            plain
                .iter()
                .map(|i| (i.id.as_str(), i.label.as_str(), i.checked))
                .collect::<Vec<_>>(),
            [
                ("1", "1m", Some(false)),
                ("60", "1h", Some(true)),
                ("D", "1D", Some(false))
            ]
        );
        assert!(plain.iter().all(|i| i.favorite.is_none()));
        let favs = vec!["60".to_string()];
        let starred = timeframe_menu_items(&tfs, "60", Some(&favs), None);
        assert_eq!(
            starred.iter().map(|i| i.favorite).collect::<Vec<_>>(),
            [Some(false), Some(true), Some(false)]
        );
    }

    #[test]
    fn the_style_dropdown_lists_built_ins_then_registered_types_below_a_separator() {
        let reg = registry();
        let none = HashSet::new();
        let items = style_menu_items(
            &reg,
            "candles",
            &none,
            &|id| Some(format!("style-{id}")),
            None,
        );
        assert_eq!(items.len(), price_style_ids(&reg).len());
        assert_eq!(items[0].id, "candles");
        assert_eq!(items[0].label, "Candles");
        assert_eq!(items[0].checked, Some(true));
        assert_eq!(items[0].icon.as_deref(), Some("style-candles"));
        assert!(items.iter().all(|i| i.checked == Some(i.id == "candles")));
        // The separator opens the registered family, right after the nine built-ins.
        let separators: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.separator_before)
            .map(|(i, _)| i)
            .collect();
        assert!(separators.iter().all(|&i| i == BUILTIN_PRICE_STYLES.len()));
    }

    #[test]
    fn styles_the_data_cannot_draw_are_greyed_with_a_reason() {
        let reg = registry();
        let blocked: HashSet<String> = ["bars".to_string()].into_iter().collect();
        let items = style_menu_items(&reg, "line", &blocked, &|_| None, None);
        let bars = items.iter().find(|i| i.id == "bars").expect("bars row");
        assert!(bars.disabled);
        assert_eq!(
            bars.title.as_deref(),
            Some("Not available for this symbol's data")
        );
        let line = items.iter().find(|i| i.id == "line").expect("line row");
        assert!(!line.disabled && line.title.is_none());
    }

    #[test]
    fn a_plugin_style_icon_registers_its_markup_once() {
        let reg = registry();
        // Built-ins carry no icon of their own: nothing to register.
        assert_eq!(price_style_icon(&reg, "candles", &|_| false), None);
        assert_eq!(
            price_style_icon(&reg, "candles", &|id| id == "style-candles"),
            Some(PriceStyleIcon {
                id: "style-candles".into(),
                register: None
            })
        );
    }

    #[test]
    fn without_favorites_the_caret_doubles_as_the_current_label() {
        let chips = timeframe_chips(&[], "60", true, None);
        assert!(chips.chips.is_empty() && chips.caret_solo);
        assert_eq!(chips.caret_text.as_deref(), Some("1h"));
        assert_eq!(chips.caret_aria_label, "Timeframe \u{2014} 1h");
        // No star handler: the chips never render, whatever the favorites say.
        let handlerless = timeframe_chips(&["D".to_string()], "60", false, None);
        assert!(handlerless.chips.is_empty() && handlerless.caret_solo);
    }

    #[test]
    fn favorites_sort_by_duration_with_the_highlight_in_place() {
        let favs = vec!["D".to_string(), "15".to_string(), "60".to_string()];
        let chips = timeframe_chips(&favs, "60", true, None);
        assert!(!chips.caret_solo && chips.caret_text.is_none());
        assert_eq!(chips.caret_aria_label, "Timeframes");
        assert_eq!(
            chips
                .chips
                .iter()
                .map(|c| c.timeframe.as_str())
                .collect::<Vec<_>>(),
            ["15", "60", "D"]
        );
        let current = &chips.chips[1];
        assert!(current.current && current.aria_label.is_none());
        assert_eq!(
            chips.chips[0].aria_label.as_deref(),
            Some("Switch timeframe to 15m")
        );
    }

    #[test]
    fn an_unstarred_current_timeframe_sits_after_the_favorites_next_to_the_caret() {
        let favs = vec!["15".to_string(), "D".to_string()];
        let chips = timeframe_chips(&favs, "240", true, None);
        let order: Vec<&str> = chips.chips.iter().map(|c| c.timeframe.as_str()).collect();
        assert_eq!(order, ["15", "D", "240"]);
        assert!(chips.chips[2].current);
        assert_eq!(chips.chips[2].label, "4h");
    }

    #[test]
    fn the_button_labels_name_their_current_value() {
        let reg = registry();
        assert_eq!(
            style_button_label(&reg, "area", None),
            "Chart style \u{2014} Area"
        );
        assert_eq!(
            layout_button_label(Some("2x2"), None),
            "Layout \u{2014} 2x2"
        );
        assert_eq!(layout_button_label(None, None), "Layout \u{2014} ");
    }

    #[test]
    fn the_fixed_labels_read_in_english() {
        let t = |m: MessageLike| text_of(None, &m, &[]);
        assert_eq!(t(indicators_message()), "Indicators");
        assert_eq!(t(undo_message()), "Undo");
        assert_eq!(t(redo_message()), "Redo");
        assert_eq!(t(screenshot_message()), "Download screenshot");
        assert_eq!(t(alerts_message()), "Alerts");
        assert_eq!(t(timeframe_message()), "Timeframe");
        assert_eq!(t(chart_style_message()), "Chart style");
        assert_eq!(t(layout_message()), "Layout");
    }
}
