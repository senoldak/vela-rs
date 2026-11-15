//! Bottom bar: the range chips, the clock + time-zone trigger, the RTH/ETH session toggle
//! and the chart-settings button.
//!
//! The pure half of the original `Bottombar`: the range chips ([`RANGE_PRESETS`],
//! [`range_label`]), the session toggle's state ([`session_toggle_state`]), the zone the
//! clock and offset label render in ([`display_zone`]), and the zone menu rows
//! ([`timezone_menu_items`]). Left for the browser crate: the element tree and CSS, the
//! zone dropdown menu, the settings tooltip, the second-aligned clock tick (it formats
//! `Date.now()` through the translator's `clock` formatter in [`display_zone`]) and the
//! button handlers (a range click first marks that chip active, then reports the preset;
//! a session click is optimistic: the reload confirms).

use crate::i18n::{MessageLike, Translator, english_translator, msg, text_of};
use crate::ui::MenuItemDescriptor;
use crate::visible_range::VisibleRangePreset;

use super::timezones::{resolve_timezone, timezone_menu_rows};

/// A range chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RangePreset {
    /// Button label (the chip's identity).
    pub id: &'static str,
    /// Timeframe to switch to for this range.
    pub tf: &'static str,
    /// The core visible-range preset framed once the chart is ready.
    pub preset: VisibleRangePreset,
    /// Bars the window needs AT `tf`: the fetch budget for the rebuild. Without it the chart
    /// loads its default depth and the framed window is clipped to whatever history happens
    /// to be loaded (a "1D" that only shows 16 hours). Includes a small margin; `ALL` asks
    /// for as much history as the provider will serve.
    pub bars: u32,
}

/// Range chips: each pairs a timeframe, a visible window, and the fetch depth that window
/// needs. Resolutions follow the reference: the shorter the range, the finer the bars
/// (1 day of 1-minute bars to 5 years of weekly bars).
pub const RANGE_PRESETS: [RangePreset; 9] = [
    RangePreset {
        id: "1D",
        tf: "1",
        preset: VisibleRangePreset::Day1,
        bars: 1500,
    }, //   1 day  @ 1m  = 1440 bars
    RangePreset {
        id: "7D",
        tf: "5",
        preset: VisibleRangePreset::Week1,
        bars: 2100,
    }, //   7 days @ 5m  = 2016
    RangePreset {
        id: "1M",
        tf: "30",
        preset: VisibleRangePreset::Month1,
        bars: 1500,
    }, //  30 days @ 30m = 1440
    RangePreset {
        id: "3M",
        tf: "60",
        preset: VisibleRangePreset::Month3,
        bars: 2200,
    }, //  90 days @ 1h  = 2160
    RangePreset {
        id: "6M",
        tf: "240",
        preset: VisibleRangePreset::Month6,
        bars: 1150,
    }, // 180 days @ 4h  = 1080
    RangePreset {
        id: "YTD",
        tf: "D",
        preset: VisibleRangePreset::YearToDate,
        bars: 400,
    }, //  366 days @ 1D
    RangePreset {
        id: "1Y",
        tf: "D",
        preset: VisibleRangePreset::Year1,
        bars: 400,
    }, //    365 days @ 1D
    RangePreset {
        id: "5Y",
        tf: "W",
        preset: VisibleRangePreset::Year5,
        bars: 300,
    }, //    5 years  @ 1W = 261
    RangePreset {
        id: "ALL",
        tf: "W",
        preset: VisibleRangePreset::All,
        bars: 5000,
    }, // everything the provider serves
];

/// The range chips' labels; a chip's id stays its identity.
fn range_message(id: &str) -> Option<MessageLike> {
    Some(match id {
        "1D" => msg("widget.bottombar.range.1d", "1D"),
        "7D" => msg("widget.bottombar.range.7d", "7D"),
        "1M" => msg("widget.bottombar.range.1m", "1M"),
        "3M" => msg("widget.bottombar.range.3m", "3M"),
        "6M" => msg("widget.bottombar.range.6m", "6M"),
        "YTD" => msg("widget.bottombar.range.ytd", "YTD"),
        "1Y" => msg("widget.bottombar.range.1y", "1Y"),
        "5Y" => msg("widget.bottombar.range.5y", "5Y"),
        "ALL" => msg("widget.bottombar.range.all", "ALL"),
        _ => return None,
    })
}

/// A range chip's label in the shell's language; an unknown preset shows its id.
pub fn range_label(id: &str, i18n: Option<&dyn Translator>) -> String {
    match range_message(id) {
        Some(m) => text_of(i18n, &m, &[]),
        None => id.to_string(),
    }
}

pub fn time_zone_label() -> MessageLike {
    msg("widget.bottombar.timeZone", "Time zone")
}
pub fn session_label() -> MessageLike {
    msg(
        "widget.bottombar.session",
        "Session \u{2014} regular (RTH) vs extended (ETH) hours",
    )
}
pub fn regular_label() -> MessageLike {
    msg("widget.bottombar.regular", "RTH")
}
pub fn extended_label() -> MessageLike {
    msg("widget.bottombar.extended", "ETH")
}
pub fn chart_settings_label() -> MessageLike {
    msg("widget.bottombar.chartSettings", "Chart settings")
}

/// The session the toggle can pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionKey {
    Regular,
    Extended,
}

/// What the RTH/ETH toggle shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionToggleState {
    /// The toggle is shown at all (sessions only apply to markets that have them).
    pub visible: bool,
    /// The buttons are clickable.
    pub enabled: bool,
    /// The lit chip.
    pub active: SessionKey,
}

/// Reflect the ACTIVE chart's session posture. `enabled: false` (a continuous market, or
/// metadata not landed yet) HIDES the toggle entirely: RTH/ETH is meaningless there.
/// Enabled, the chips appear and the active one tracks the chart's current session; while
/// disabled the regular chip stays lit.
pub fn session_toggle_state(session: SessionKey, enabled: bool) -> SessionToggleState {
    SessionToggleState {
        visible: enabled,
        enabled,
        active: if enabled {
            session
        } else {
            SessionKey::Regular
        },
    }
}

/// A click on a session chip: the session to report, or `None` while the toggle is disabled.
pub fn session_click(key: SessionKey, enabled: bool) -> Option<SessionKey> {
    enabled.then_some(key)
}

/// The zone the bar's clock and label render in: the stored choice resolved against the
/// active chart's market zone (the exchange rule).
pub fn display_zone(timezone: &str, exchange_timezone: Option<&str>) -> String {
    resolve_timezone(timezone, exchange_timezone)
}

/// The zone dropdown's rows for the stored choice.
pub fn timezone_menu_items(
    timezone: &str,
    i18n: Option<&dyn Translator>,
    at_ms: f64,
) -> Vec<MenuItemDescriptor> {
    let translator: &dyn Translator = i18n.unwrap_or_else(|| english_translator());
    timezone_menu_rows(timezone, translator, at_ms)
        .into_iter()
        .map(|r| MenuItemDescriptor {
            checked: Some(r.checked),
            ..MenuItemDescriptor::new(r.value, r.label)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_the_reference_chip_set_finest_bars_on_the_shortest_range() {
        let ids: Vec<&str> = RANGE_PRESETS.iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            ["1D", "7D", "1M", "3M", "6M", "YTD", "1Y", "5Y", "ALL"]
        );
        let tfs: Vec<&str> = RANGE_PRESETS.iter().map(|r| r.tf).collect();
        assert_eq!(tfs, ["1", "5", "30", "60", "240", "D", "D", "W", "W"]);
    }

    #[test]
    fn every_chip_fetches_enough_bars_to_fill_its_window() {
        const MIN: f64 = 60_000.0;
        let tf_ms = |tf: &str| match tf {
            "1" => MIN,
            "5" => 5.0 * MIN,
            "30" => 30.0 * MIN,
            "60" => 60.0 * MIN,
            "240" => 240.0 * MIN,
            "D" => 1440.0 * MIN,
            "W" => 7.0 * 1440.0 * MIN,
            other => panic!("unexpected timeframe {other}"),
        };
        let span_days = |id: &str| match id {
            "1D" => 1.0,
            "7D" => 7.0,
            "1M" => 30.0,
            "3M" => 90.0,
            "6M" => 180.0,
            "YTD" => 366.0,
            "1Y" => 365.0,
            "5Y" => 5.0 * 365.0,
            other => panic!("unexpected chip {other}"),
        };
        for r in RANGE_PRESETS {
            if r.id == "ALL" {
                continue; // ALL frames whatever history exists
            }
            let needed = span_days(r.id) * 1440.0 * MIN / tf_ms(r.tf);
            // The budget must cover the window (what a fixed 1000-bar load got wrong)...
            assert!(f64::from(r.bars) >= needed, "{}", r.id);
            // ...without fetching absurd depth.
            assert!(f64::from(r.bars) < needed * 2.0, "{}", r.id);
        }
    }

    #[test]
    fn every_range_chip_maps_to_a_core_visible_range_preset() {
        for r in RANGE_PRESETS {
            assert!(VisibleRangePreset::ALL.contains(&r.preset));
            assert!(!r.tf.is_empty());
        }
    }

    #[test]
    fn chip_labels_follow_the_language_and_an_unknown_preset_shows_its_id() {
        assert_eq!(range_label("YTD", None), "YTD");
        assert_eq!(range_label("7D", None), "7D");
        assert_eq!(range_label("custom", None), "custom");
        for r in RANGE_PRESETS {
            assert_eq!(range_label(r.id, None), r.id);
        }
    }

    #[test]
    fn the_session_toggle_hides_when_disabled_and_keeps_the_regular_chip_lit() {
        let off = session_toggle_state(SessionKey::Extended, false);
        assert_eq!(
            off,
            SessionToggleState {
                visible: false,
                enabled: false,
                active: SessionKey::Regular
            }
        );
        let on = session_toggle_state(SessionKey::Extended, true);
        assert_eq!(
            on,
            SessionToggleState {
                visible: true,
                enabled: true,
                active: SessionKey::Extended
            }
        );
        assert_eq!(
            session_click(SessionKey::Extended, true),
            Some(SessionKey::Extended)
        );
        assert_eq!(session_click(SessionKey::Extended, false), None);
    }

    #[test]
    fn the_clock_zone_resolves_the_exchange_rule_against_the_active_market() {
        assert_eq!(
            display_zone("exchange", Some("America/Chicago")),
            "America/Chicago"
        );
        assert_eq!(display_zone("exchange", None), "Etc/UTC");
        assert_eq!(
            display_zone("Europe/Paris", Some("America/Chicago")),
            "Europe/Paris"
        );
    }

    #[test]
    fn the_zone_menu_checks_the_stored_choice() {
        let items = timezone_menu_items("exchange", None, 0.0);
        let checked: Vec<&str> = items
            .iter()
            .filter(|i| i.checked == Some(true))
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(checked, ["exchange"]);
        assert_eq!(items[0].id, "Etc/UTC");
    }

    #[test]
    fn the_fixed_labels_read_in_english() {
        let t = |m: MessageLike| text_of(None, &m, &[]);
        assert_eq!(t(time_zone_label()), "Time zone");
        assert_eq!(t(regular_label()), "RTH");
        assert_eq!(t(extended_label()), "ETH");
        assert_eq!(t(chart_settings_label()), "Chart settings");
        assert!(t(session_label()).contains("RTH"));
    }
}
