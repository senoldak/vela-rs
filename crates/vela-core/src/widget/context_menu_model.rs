//! Chart context-menu MODEL: the item descriptors for each right-click zone, how
//! contributed rows sort in among them, and the renderer writes a selection implies.
//! Pure functions over plain state, so the menus can be tested without a DOM; the
//! `ChartContextMenu` component (browser crate) reads the renderer, calls these and shows
//! the result.

use serde_json::{Value, json};

use super::timezones::timezone_menu_rows;
use crate::i18n::{MessageLike, Translator, english_translator, msg, text_of};
use crate::ui::MenuItemDescriptor;

fn t(i18n: Option<&dyn Translator>, m: MessageLike) -> String {
    text_of(i18n, &m, &[])
}

/// Zone a right-click landed in: the chart body, the right price scale, the bottom time axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Zone {
    Body,
    PriceAxis,
    TimeAxis,
}

impl Zone {
    pub fn as_str(self) -> &'static str {
        match self {
            Zone::Body => "body",
            Zone::PriceAxis => "price-axis",
            Zone::TimeAxis => "time-axis",
        }
    }
}

/// One pane's scale state, as reported by the renderer's `paneScales` feature.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneScaleInfo {
    pub id: String,
    pub kind: PaneKind,
    /// The pane's pixel band inside the plot, so a click y maps to a pane.
    pub top: f64,
    pub height: f64,
    pub mode: String,
    pub log: bool,
    pub invert: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneKind {
    Price,
    Study,
}

/// The settings-dialog tab each zone's settings item opens: the one holding the settings
/// that zone is about, so the menu lands where the eye already is. Tab ids, not display text.
pub fn settings_section(zone: Zone) -> &'static str {
    match zone {
        Zone::Body => "Canvas",
        Zone::PriceAxis | Zone::TimeAxis => "Scales and lines",
    }
}

/// Menu id for the settings item of a zone, carrying the tab to open.
fn settings_item(zone: Zone, label: String, icon: Option<&str>) -> MenuItemDescriptor {
    MenuItemDescriptor {
        icon: icon.map(str::to_string),
        separator_before: true,
        ..MenuItemDescriptor::new(format!("settings:{}", settings_section(zone)), label)
    }
}

/// The section a `settings:*` item asks for, or `None` for a bare `settings` id.
pub fn settings_section_of(id: &str) -> Option<String> {
    let rest: String = id.chars().skip("settings:".chars().count()).collect();
    if rest.is_empty() { None } else { Some(rest) }
}

/// The four mutually exclusive price-scale choices (log is part of the same group).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaleChoice {
    Regular,
    Percent,
    Indexed,
    Log,
}

impl ScaleChoice {
    pub const ALL: [ScaleChoice; 4] = [
        ScaleChoice::Regular,
        ScaleChoice::Percent,
        ScaleChoice::Indexed,
        ScaleChoice::Log,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ScaleChoice::Regular => "regular",
            ScaleChoice::Percent => "percent",
            ScaleChoice::Indexed => "indexed",
            ScaleChoice::Log => "log",
        }
    }

    fn label(self) -> MessageLike {
        match self {
            ScaleChoice::Regular => msg("widget.contextMenu.scale.regular", "Regular"),
            ScaleChoice::Percent => msg("widget.contextMenu.scale.percent", "Percent"),
            ScaleChoice::Indexed => msg("widget.contextMenu.scale.indexed", "Indexed to 100"),
            ScaleChoice::Log => msg("widget.contextMenu.scale.log", "Logarithmic"),
        }
    }
}

/// Which pane's scale a right-click at plot-local `y` targets, or `None` without per-pane
/// scales.
pub fn pane_scale_at(panes: &[PaneScaleInfo], y: f64) -> Option<&PaneScaleInfo> {
    panes
        .iter()
        .find(|p| y >= p.top && y < p.top + p.height)
        .or_else(|| panes.first())
}

/// A pane's active choice. Log wins over the mode: the two live in one exclusive group.
pub fn scale_choice_of(mode: Option<&str>, log: bool) -> ScaleChoice {
    if log {
        return ScaleChoice::Log;
    }
    match mode {
        Some("percent") => ScaleChoice::Percent,
        Some("indexed") => ScaleChoice::Indexed,
        _ => ScaleChoice::Regular,
    }
}

/// The main (price-pane) scale is the chart-level setting keyboard shortcuts and the
/// persisted config also drive; a study pane carries its own, so panes stay independent.
fn main_scale(pane: Option<&PaneScaleInfo>) -> bool {
    pane.is_none_or(|p| p.kind == PaneKind::Price)
}

/// Renderer writes that selecting a scale choice implies: picking one clears the others.
pub fn scale_writes(choice: ScaleChoice, pane: Option<&PaneScaleInfo>) -> Vec<(String, Value)> {
    let mode = match choice {
        ScaleChoice::Percent => "percent",
        ScaleChoice::Indexed => "indexed",
        _ => "price",
    };
    let log = choice == ScaleChoice::Log;
    match pane.filter(|p| p.kind != PaneKind::Price) {
        None => vec![
            ("scaleMode".to_string(), json!(mode)),
            ("logScale".to_string(), json!(log)),
        ],
        Some(p) => vec![
            (
                "scaleMode".to_string(),
                json!({ "pane": p.id, "mode": mode }),
            ),
            (
                "logScale".to_string(),
                json!({ "pane": p.id, "value": log }),
            ),
        ],
    }
}

/// The price-only row's state for the axis under the pointer: the price pane's setting, or
/// `None` (no row) on a study pane, whose window has no bars to fit.
pub fn price_only_state(pane: Option<&PaneScaleInfo>, flag: bool) -> Option<bool> {
    main_scale(pane).then_some(flag)
}

/// Renderer write inverting one pane's axis (high at the bottom).
pub fn invert_write(next: bool, pane: Option<&PaneScaleInfo>) -> (String, Value) {
    match pane.filter(|p| p.kind != PaneKind::Price) {
        None => ("invertScale".to_string(), json!(next)),
        Some(p) => (
            "invertScale".to_string(),
            json!({ "pane": p.id, "value": next }),
        ),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PriceAxisState {
    /// Autoscale on: the axis fits the visible data instead of a frozen window.
    pub auto: bool,
    /// Autoscale fits the price bars only. `None` for a study pane's axis, which has no bars.
    pub price_only: Option<bool>,
    pub invert: bool,
    pub choice: ScaleChoice,
    pub axis_labels: bool,
    pub price_label: bool,
    pub countdown: bool,
    pub price_line: bool,
}

fn checked(id: &str, label: String, checked: bool) -> MenuItemDescriptor {
    MenuItemDescriptor {
        checked: Some(checked),
        ..MenuItemDescriptor::new(id, label)
    }
}

/// The price-axis rows; `i18n` names them in the chart's language (English when absent).
pub fn price_axis_items(
    s: &PriceAxisState,
    i18n: Option<&dyn Translator>,
) -> Vec<MenuItemDescriptor> {
    let mut items = vec![checked(
        "auto",
        t(
            i18n,
            msg("widget.contextMenu.auto", "Auto (fits data to screen)"),
        ),
        s.auto,
    )];
    if let Some(price_only) = s.price_only {
        items.push(checked(
            "price-only",
            t(
                i18n,
                msg(
                    "widget.contextMenu.scalePriceOnly",
                    "Scale price chart only",
                ),
            ),
            price_only,
        ));
    }
    items.push(checked(
        "invert",
        t(i18n, msg("widget.contextMenu.invert", "Invert scale")),
        s.invert,
    ));
    for (i, choice) in ScaleChoice::ALL.into_iter().enumerate() {
        items.push(MenuItemDescriptor {
            separator_before: i == 0,
            ..checked(
                &format!("scale:{}", choice.as_str()),
                t(i18n, choice.label()),
                s.choice == choice,
            )
        });
    }
    items.push(MenuItemDescriptor {
        separator_before: true,
        submenu: Some(vec![
            checked(
                "toggle:axisLabels",
                t(
                    i18n,
                    msg("widget.contextMenu.axisLabels", "Price axis labels"),
                ),
                s.axis_labels,
            ),
            checked(
                "toggle:priceLabel",
                t(
                    i18n,
                    msg("widget.contextMenu.priceLabel", "Last price label"),
                ),
                s.price_label,
            ),
            checked(
                "toggle:countdown",
                t(
                    i18n,
                    msg("widget.contextMenu.countdown", "Countdown to bar close"),
                ),
                s.countdown,
            ),
        ]),
        ..MenuItemDescriptor::new(
            "labels",
            t(i18n, msg("widget.contextMenu.labels", "Labels")),
        )
    });
    items.push(MenuItemDescriptor {
        submenu: Some(vec![checked(
            "toggle:currentPriceLine",
            t(i18n, msg("widget.contextMenu.priceLine", "Last price line")),
            s.price_line,
        )]),
        ..MenuItemDescriptor::new(
            "levels",
            t(i18n, msg("widget.contextMenu.levels", "Levels")),
        )
    });
    items.push(settings_item(
        Zone::PriceAxis,
        t(
            i18n,
            msg("widget.contextMenu.moreSettings", "More settings\u{2026}"),
        ),
        None,
    ));
    items
}

/// The time-axis rows. `timezone` is the host's STORED choice: an IANA zone, or the
/// exchange rule. `at_ms` is the instant the zone offsets are read at (now).
pub fn time_axis_items(
    timezone: &str,
    i18n: Option<&dyn Translator>,
    at_ms: f64,
) -> Vec<MenuItemDescriptor> {
    let translator: &dyn Translator = i18n.unwrap_or_else(|| english_translator());
    let rows = timezone_menu_rows(timezone, translator, at_ms);
    vec![
        MenuItemDescriptor {
            submenu: Some(
                rows.into_iter()
                    .map(|r| checked(&format!("tz:{}", r.value), r.label, r.checked))
                    .collect(),
            ),
            ..MenuItemDescriptor::new(
                "timezone",
                t(i18n, msg("widget.contextMenu.timezone", "Time zone")),
            )
        },
        settings_item(
            Zone::TimeAxis,
            t(
                i18n,
                msg("widget.contextMenu.moreSettings", "More settings\u{2026}"),
            ),
            None,
        ),
    ]
}

/// Chart-body items. The two removals stay VISIBLE with nothing to remove (disabled, so
/// the menu keeps its shape).
pub fn body_items(
    drawings: usize,
    indicators: usize,
    i18n: Option<&dyn Translator>,
) -> Vec<MenuItemDescriptor> {
    vec![
        MenuItemDescriptor {
            icon: Some("reset".into()),
            ..MenuItemDescriptor::new(
                "reset-view",
                t(
                    i18n,
                    msg("widget.contextMenu.resetView", "Reset chart view"),
                ),
            )
        },
        MenuItemDescriptor {
            icon: Some("eraser".into()),
            disabled: drawings == 0,
            separator_before: true,
            ..MenuItemDescriptor::new(
                "remove-drawings",
                t(
                    i18n,
                    msg("widget.contextMenu.removeDrawings", "Remove drawings"),
                ),
            )
        },
        MenuItemDescriptor {
            icon: Some("indicators".into()),
            disabled: indicators == 0,
            ..MenuItemDescriptor::new(
                "remove-indicators",
                t(
                    i18n,
                    msg("widget.contextMenu.removeIndicators", "Remove indicators"),
                ),
            )
        },
        settings_item(
            Zone::Body,
            t(i18n, msg("widget.contextMenu.settings", "Settings\u{2026}")),
            Some("gear"),
        ),
    ]
}

/// The rank of every built-in context-menu row, by zone and row id (`settings` is each
/// zone's settings row). Contributed `context:*` actions sort TOGETHER with these rows by
/// their `order` (default 0), ascending; on a tie the built-in row comes first, then the
/// actions in registration order. The built-in actions rank below 0 and the settings row at
/// 1000, so an action without `order` lands after the built-in actions and before the
/// settings row, and `order: -100` leads the menu.
pub const CONTEXT_MENU_BUILTIN_ORDER: [(Zone, &[(&str, f64)]); 3] = [
    (
        Zone::Body,
        &[
            ("reset-view", -30.0),
            ("remove-drawings", -20.0),
            ("remove-indicators", -10.0),
            ("settings", 1000.0),
        ],
    ),
    (
        Zone::PriceAxis,
        &[
            ("auto", -80.0),
            ("price-only", -75.0),
            ("invert", -70.0),
            ("scale:regular", -60.0),
            ("scale:percent", -50.0),
            ("scale:indexed", -40.0),
            ("scale:log", -30.0),
            ("labels", -20.0),
            ("levels", -10.0),
            ("settings", 1000.0),
        ],
    ),
    (Zone::TimeAxis, &[("timezone", -10.0), ("settings", 1000.0)]),
];

/// The built-in ranks of one zone.
pub fn builtin_ranks(zone: Zone) -> &'static [(&'static str, f64)] {
    CONTEXT_MENU_BUILTIN_ORDER
        .iter()
        .find(|(z, _)| *z == zone)
        .map_or(&[], |(_, ranks)| ranks)
}

/// A contributed row on its way into a context menu, with the action's `order`.
#[derive(Clone, Debug)]
pub struct ContributedRow {
    pub item: MenuItemDescriptor,
    pub order: Option<f64>,
}

/// One zone's menu: the built-in rows and the contributed ones interleaved by rank (see
/// [`CONTEXT_MENU_BUILTIN_ORDER`]). Separators fall between groups only, never at the
/// ends: each built-in group is a run the builder already separates, and consecutive
/// contributed rows form one group. `contributed` arrives in registration order within
/// equal `order`s (what the action registry returns) and keeps it.
pub fn compose_menu(
    zone: Zone,
    builtin: &[MenuItemDescriptor],
    contributed: &[ContributedRow],
) -> Vec<MenuItemDescriptor> {
    let ranks = builtin_ranks(zone);
    struct Row {
        item: MenuItemDescriptor,
        rank: f64,
        group: i64,
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut group = 0;
    for item in builtin {
        if item.separator_before {
            group += 1;
        }
        let key = if item.id.starts_with("settings") {
            "settings"
        } else {
            item.id.as_str()
        };
        let rank = ranks
            .iter()
            .find(|(k, _)| *k == key)
            .map_or(0.0, |(_, r)| *r);
        rows.push(Row {
            item: item.clone(),
            rank,
            group,
        });
    }
    for c in contributed {
        rows.push(Row {
            item: c.item.clone(),
            rank: c.order.unwrap_or(0.0),
            group: -1,
        });
    }
    rows.sort_by(|a, b| a.rank.total_cmp(&b.rank));
    let groups: Vec<i64> = rows.iter().map(|r| r.group).collect();
    rows.into_iter()
        .enumerate()
        .map(|(i, r)| MenuItemDescriptor {
            separator_before: i > 0 && groups[i - 1] != r.group,
            ..r.item
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::timezones::timezones;

    fn pane(id: &str, kind: PaneKind, top: f64, height: f64) -> PaneScaleInfo {
        PaneScaleInfo {
            id: id.into(),
            kind,
            top,
            height,
            mode: "price".into(),
            log: false,
            invert: false,
        }
    }

    fn axis_state() -> PriceAxisState {
        PriceAxisState {
            auto: true,
            price_only: None,
            invert: false,
            choice: ScaleChoice::Regular,
            axis_labels: true,
            price_label: true,
            countdown: false,
            price_line: true,
        }
    }

    fn labels(items: &[MenuItemDescriptor]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    fn ids(items: &[MenuItemDescriptor]) -> Vec<&str> {
        items.iter().map(|i| i.id.as_str()).collect()
    }

    fn find<'a>(items: &'a [MenuItemDescriptor], id: &str) -> &'a MenuItemDescriptor {
        items.iter().find(|i| i.id == id).expect("item present")
    }

    #[test]
    fn pane_scale_at_maps_a_click_y_to_the_pane_whose_band_contains_it() {
        let panes = [
            pane("price", PaneKind::Price, 0.0, 300.0),
            pane("p2", PaneKind::Study, 300.0, 100.0),
        ];
        let at = |y| pane_scale_at(&panes, y).map(|p| p.id.as_str());
        assert_eq!(at(10.0), Some("price"));
        assert_eq!(at(299.0), Some("price"));
        assert_eq!(at(300.0), Some("p2"));
        assert_eq!(at(399.0), Some("p2"));
    }

    #[test]
    fn pane_scale_at_falls_back_to_the_first_pane_and_to_none_without_panes() {
        let panes = [
            pane("price", PaneKind::Price, 0.0, 300.0),
            pane("p2", PaneKind::Study, 300.0, 100.0),
        ];
        assert_eq!(
            pane_scale_at(&panes, 5000.0).map(|p| p.id.as_str()),
            Some("price")
        );
        assert!(pane_scale_at(&[], 10.0).is_none());
    }

    #[test]
    fn log_wins_over_the_mode() {
        assert_eq!(scale_choice_of(Some("price"), false), ScaleChoice::Regular);
        assert_eq!(
            scale_choice_of(Some("percent"), false),
            ScaleChoice::Percent
        );
        assert_eq!(
            scale_choice_of(Some("indexed"), false),
            ScaleChoice::Indexed
        );
        assert_eq!(scale_choice_of(Some("percent"), true), ScaleChoice::Log);
        assert_eq!(scale_choice_of(None, false), ScaleChoice::Regular);
    }

    fn w(pairs: &[(&str, Value)]) -> Vec<(String, Value)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn picking_one_choice_clears_the_others_on_the_main_scale() {
        assert_eq!(
            scale_writes(ScaleChoice::Percent, None),
            w(&[("scaleMode", json!("percent")), ("logScale", json!(false))])
        );
        assert_eq!(
            scale_writes(ScaleChoice::Log, None),
            w(&[("scaleMode", json!("price")), ("logScale", json!(true))])
        );
        let price = pane("price", PaneKind::Price, 0.0, 100.0);
        assert_eq!(
            scale_writes(ScaleChoice::Indexed, Some(&price)),
            w(&[("scaleMode", json!("indexed")), ("logScale", json!(false))])
        );
    }

    #[test]
    fn a_study_pane_is_targeted_pane_scoped() {
        let study = pane("p2", PaneKind::Study, 300.0, 100.0);
        assert_eq!(
            scale_writes(ScaleChoice::Log, Some(&study)),
            w(&[
                ("scaleMode", json!({ "pane": "p2", "mode": "price" })),
                ("logScale", json!({ "pane": "p2", "value": true })),
            ])
        );
    }

    #[test]
    fn invert_follows_the_same_main_scale_per_pane_split() {
        assert_eq!(
            invert_write(true, None),
            ("invertScale".to_string(), json!(true))
        );
        let price = pane("price", PaneKind::Price, 0.0, 100.0);
        assert_eq!(
            invert_write(false, Some(&price)),
            ("invertScale".to_string(), json!(false))
        );
        let study = pane("p2", PaneKind::Study, 0.0, 100.0);
        assert_eq!(
            invert_write(true, Some(&study)),
            (
                "invertScale".to_string(),
                json!({ "pane": "p2", "value": true })
            )
        );
    }

    #[test]
    fn the_price_axis_menu_offers_every_row() {
        let items = price_axis_items(&axis_state(), None);
        assert_eq!(
            labels(&items),
            [
                "Auto (fits data to screen)",
                "Invert scale",
                "Regular",
                "Percent",
                "Indexed to 100",
                "Logarithmic",
                "Labels",
                "Levels",
                "More settings\u{2026}"
            ]
        );
        let separated: Vec<&str> = items
            .iter()
            .filter(|i| i.separator_before)
            .map(|i| i.label.as_str())
            .collect();
        assert_eq!(separated, ["Regular", "Labels", "More settings\u{2026}"]);
        let sub = |id: &str| -> Vec<String> {
            find(&items, id)
                .submenu
                .as_ref()
                .expect("submenu")
                .iter()
                .map(|i| i.label.clone())
                .collect()
        };
        assert_eq!(
            sub("labels"),
            [
                "Price axis labels",
                "Last price label",
                "Countdown to bar close"
            ]
        );
        assert_eq!(
            find(&items, "levels")
                .submenu
                .as_ref()
                .expect("submenu")
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            ["toggle:currentPriceLine"]
        );
    }

    #[test]
    fn check_marks_track_the_state_with_exactly_one_scale_mode() {
        let items = price_axis_items(
            &PriceAxisState {
                auto: false,
                invert: true,
                choice: ScaleChoice::Indexed,
                ..axis_state()
            },
            None,
        );
        assert_eq!(find(&items, "auto").checked, Some(false));
        assert_eq!(find(&items, "invert").checked, Some(true));
        let on: Vec<&str> = items
            .iter()
            .filter(|i| i.id.starts_with("scale:") && i.checked == Some(true))
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(on, ["scale:indexed"]);
    }

    #[test]
    fn scale_price_chart_only_sits_under_auto_on_the_price_scale() {
        let off = price_axis_items(
            &PriceAxisState {
                price_only: Some(false),
                ..axis_state()
            },
            None,
        );
        assert_eq!(
            labels(&off)[..3],
            [
                "Auto (fits data to screen)",
                "Scale price chart only",
                "Invert scale"
            ]
        );
        assert_eq!(find(&off, "price-only").checked, Some(false));
        let on = price_axis_items(
            &PriceAxisState {
                price_only: Some(true),
                ..axis_state()
            },
            None,
        );
        assert_eq!(find(&on, "price-only").checked, Some(true));
    }

    #[test]
    fn a_study_panes_axis_does_not_offer_the_price_only_row() {
        assert_eq!(price_only_state(None, true), Some(true));
        let price = pane("price", PaneKind::Price, 0.0, 100.0);
        assert_eq!(price_only_state(Some(&price), false), Some(false));
        let study = pane("p2", PaneKind::Study, 0.0, 100.0);
        assert_eq!(price_only_state(Some(&study), true), None);
        let items = price_axis_items(
            &PriceAxisState {
                price_only: price_only_state(Some(&study), true),
                ..axis_state()
            },
            None,
        );
        assert!(!items.iter().any(|i| i.id == "price-only"));
    }

    #[test]
    fn the_submenu_toggles_mirror_the_feature_states() {
        let items = price_axis_items(
            &PriceAxisState {
                axis_labels: false,
                countdown: true,
                price_line: false,
                ..axis_state()
            },
            None,
        );
        let labels_menu = find(&items, "labels").submenu.as_ref().expect("submenu");
        assert_eq!(find(labels_menu, "toggle:axisLabels").checked, Some(false));
        assert_eq!(find(labels_menu, "toggle:priceLabel").checked, Some(true));
        assert_eq!(find(labels_menu, "toggle:countdown").checked, Some(true));
        assert_eq!(
            find(&items, "levels").submenu.as_ref().expect("submenu")[0].checked,
            Some(false)
        );
    }

    fn zone_translator() -> crate::i18n::LocaleState {
        use crate::i18n::{LocaleRegistry, LocalizationOptions, create_translator};
        let intl = crate::widget::test_support::ZoneIntl::arc();
        let registry = LocaleRegistry::new(std::sync::Arc::clone(&intl));
        create_translator(LocalizationOptions::default(), intl, registry)
    }

    const NOW: f64 = 1_709_553_600_000.0; // 2024-03-04T12:00:00Z

    #[test]
    fn the_time_axis_lists_utc_the_exchange_rule_then_the_zones_and_checks_the_active_one() {
        let tr = zone_translator();
        let items = time_axis_items("Europe/Paris", Some(&tr), NOW);
        assert_eq!(ids(&items), ["timezone", "settings:Scales and lines"]);
        let zones = items[0].submenu.as_ref().expect("zones");
        assert_eq!(zones.len(), timezones().len() + 1);
        assert_eq!(ids(zones)[..2], ["tz:Etc/UTC", "tz:exchange"]);
        let checked_ids: Vec<&str> = zones
            .iter()
            .filter(|z| z.checked == Some(true))
            .map(|z| z.id.as_str())
            .collect();
        assert_eq!(checked_ids, ["tz:Europe/Paris"]);
        assert_eq!(find(zones, "tz:Asia/Tokyo").label, "(UTC+9) Tokyo");
    }

    #[test]
    fn a_renderers_bare_utc_checks_the_same_row_as_etc_utc() {
        for tz in ["UTC", "Etc/UTC"] {
            let items = time_axis_items(tz, None, NOW);
            let zones = items[0].submenu.as_ref().expect("zones");
            let checked_ids: Vec<&str> = zones
                .iter()
                .filter(|z| z.checked == Some(true))
                .map(|z| z.id.as_str())
                .collect();
            assert_eq!(checked_ids, ["tz:Etc/UTC"]);
        }
    }

    #[test]
    fn the_exchange_row_reads_plain_exchange_and_checks_under_the_rule() {
        let zones = time_axis_items("exchange", None, NOW)[0]
            .submenu
            .clone()
            .expect("zones");
        let checked_ids: Vec<&str> = zones
            .iter()
            .filter(|z| z.checked == Some(true))
            .map(|z| z.id.as_str())
            .collect();
        assert_eq!(checked_ids, ["tz:exchange"]);
        assert_eq!(zones[1].label, "Exchange");
        let paris = time_axis_items("Europe/Paris", None, NOW)[0]
            .submenu
            .clone()
            .expect("zones");
        assert_eq!(paris[1].checked, Some(false));
    }

    #[test]
    fn the_body_menu_keeps_the_removals_visible_but_disabled_when_nothing_is_there() {
        let empty = body_items(0, 0, None);
        assert_eq!(
            ids(&empty),
            [
                "reset-view",
                "remove-drawings",
                "remove-indicators",
                "settings:Canvas"
            ]
        );
        let disabled: Vec<&str> = empty
            .iter()
            .filter(|i| i.disabled)
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(disabled, ["remove-drawings", "remove-indicators"]);
        assert!(!body_items(2, 1, None).iter().any(|i| i.disabled));
    }

    #[test]
    fn body_rows_carry_icons_and_the_axis_menus_stay_text_only() {
        let body = body_items(0, 0, None);
        let icons: Vec<(&str, Option<&str>)> = body
            .iter()
            .map(|i| (i.id.as_str(), i.icon.as_deref()))
            .collect();
        assert_eq!(
            icons,
            [
                ("reset-view", Some("reset")),
                ("remove-drawings", Some("eraser")),
                ("remove-indicators", Some("indicators")),
                ("settings:Canvas", Some("gear")),
            ]
        );
        assert!(
            !price_axis_items(&axis_state(), None)
                .iter()
                .any(|i| i.icon.is_some())
        );
        assert!(
            !time_axis_items("Etc/UTC", None, NOW)
                .iter()
                .any(|i| i.icon.is_some())
        );
    }

    fn row(id: &str, order: Option<f64>) -> ContributedRow {
        ContributedRow {
            item: MenuItemDescriptor::new(id, id),
            order,
        }
    }

    /// Ids with a `|` where a separator is drawn.
    fn shape(items: &[MenuItemDescriptor]) -> String {
        items
            .iter()
            .map(|i| {
                if i.separator_before {
                    format!("| {}", i.id)
                } else {
                    i.id.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn every_built_in_row_has_a_documented_rank() {
        let zones = [
            (Zone::Body, body_items(1, 1, None)),
            (
                Zone::PriceAxis,
                price_axis_items(
                    &PriceAxisState {
                        price_only: Some(false),
                        ..axis_state()
                    },
                    None,
                ),
            ),
            (Zone::TimeAxis, time_axis_items("Etc/UTC", None, NOW)),
        ];
        for (zone, items) in zones {
            let ranks = builtin_ranks(zone);
            for item in &items {
                let key = if item.id.starts_with("settings") {
                    "settings"
                } else {
                    item.id.as_str()
                };
                assert!(
                    ranks.iter().any(|(k, _)| *k == key),
                    "{} {}",
                    zone.as_str(),
                    item.id
                );
            }
            assert_eq!(ranks.len(), items.len());
        }
    }

    #[test]
    fn with_nothing_contributed_every_menu_keeps_its_rows_and_separators() {
        let body = body_items(1, 1, None);
        assert_eq!(compose_menu(Zone::Body, &body, &[]), body);
        let axis = price_axis_items(&axis_state(), None);
        assert_eq!(
            shape(&compose_menu(Zone::PriceAxis, &axis, &[])),
            shape(&axis)
        );
        let time = time_axis_items("Etc/UTC", None, NOW);
        assert_eq!(
            shape(&compose_menu(Zone::TimeAxis, &time, &[])),
            "timezone | settings:Scales and lines"
        );
    }

    #[test]
    fn an_action_without_order_lands_after_the_built_in_actions_and_before_settings() {
        let body = body_items(1, 1, None);
        assert_eq!(
            shape(&compose_menu(Zone::Body, &body, &[row("a", None)])),
            "reset-view | remove-drawings remove-indicators | a | settings:Canvas"
        );
    }

    #[test]
    fn a_negative_order_leads_the_menu_separated_from_the_built_in_rows() {
        let body = body_items(1, 1, None);
        assert_eq!(
            shape(&compose_menu(
                Zone::Body,
                &body,
                &[row("copy", Some(-100.0))]
            )),
            "copy | reset-view | remove-drawings remove-indicators | settings:Canvas"
        );
    }

    #[test]
    fn an_order_between_two_built_in_ranks_lands_between_those_rows() {
        let body = body_items(1, 1, None);
        let out = compose_menu(
            Zone::Body,
            &body,
            &[row("x", Some(-25.0)), row("y", Some(1001.0))],
        );
        assert_eq!(
            ids(&out),
            [
                "reset-view",
                "x",
                "remove-drawings",
                "remove-indicators",
                "settings:Canvas",
                "y"
            ]
        );
        assert_eq!(
            shape(&compose_menu(Zone::Body, &body, &[row("x", Some(-15.0))])),
            "reset-view | remove-drawings | x | remove-indicators | settings:Canvas"
        );
    }

    #[test]
    fn ties_keep_registration_order_with_the_built_in_row_first() {
        let body = body_items(1, 1, None);
        let tied = compose_menu(
            Zone::Body,
            &body,
            &[row("b", None), row("a", None), row("c", Some(0.0))],
        );
        assert_eq!(ids(&tied)[3..6], ["b", "a", "c"]);
        let lead = compose_menu(Zone::Body, &body, &[row("tie", Some(-30.0))]);
        assert_eq!(ids(&lead)[..2], ["reset-view", "tie"]);
        let last = compose_menu(Zone::Body, &body, &[row("last", Some(1000.0))]);
        assert_eq!(ids(&last)[last.len() - 2..], ["settings:Canvas", "last"]);
    }

    #[test]
    fn consecutive_contributed_rows_form_one_group_and_no_separator_opens_or_closes_the_menu() {
        let body = body_items(1, 1, None);
        let items = compose_menu(
            Zone::Body,
            &body,
            &[
                row("a", Some(-200.0)),
                row("b", Some(-100.0)),
                row("c", None),
                row("d", None),
            ],
        );
        assert_eq!(
            shape(&items),
            "a b | reset-view | remove-drawings remove-indicators | c d | settings:Canvas"
        );
        assert!(!items[0].separator_before);
        let only = compose_menu(Zone::Body, &[], &[row("a", None), row("b", None)]);
        assert!(!only.iter().any(|i| i.separator_before));
    }

    #[test]
    fn the_axis_menus_keep_their_built_in_order_and_groups() {
        let axis = price_axis_items(&axis_state(), None);
        assert_eq!(
            shape(&compose_menu(Zone::PriceAxis, &axis, &[row("p", None)])),
            "auto invert | scale:regular scale:percent scale:indexed scale:log | labels levels | p | settings:Scales and lines"
        );
        assert!(
            shape(&compose_menu(
                Zone::PriceAxis,
                &axis,
                &[row("lead", Some(-100.0))]
            ))
            .starts_with("lead | auto invert |")
        );
        let time = time_axis_items("Etc/UTC", None, NOW);
        assert_eq!(
            shape(&compose_menu(Zone::TimeAxis, &time, &[row("t", None)])),
            "timezone | t | settings:Scales and lines"
        );
    }

    #[test]
    fn rows_pass_through_untouched_apart_from_their_separator() {
        let item = MenuItemDescriptor {
            icon: Some("clone".into()),
            separator_before: true,
            ..MenuItemDescriptor::new("action:x", "Copy price")
        };
        let body = body_items(1, 1, None);
        let out = compose_menu(
            Zone::Body,
            &body,
            &[ContributedRow {
                item,
                order: Some(-100.0),
            }],
        );
        assert_eq!(
            out[0],
            MenuItemDescriptor {
                icon: Some("clone".into()),
                ..MenuItemDescriptor::new("action:x", "Copy price")
            }
        );
        let settings = &compose_menu(Zone::Body, &body, &[])[3];
        assert_eq!(
            (
                settings.id.as_str(),
                settings.icon.as_deref(),
                settings.separator_before
            ),
            ("settings:Canvas", Some("gear"), true)
        );
    }

    #[test]
    fn each_zone_opens_the_dialog_tab_holding_its_own_settings() {
        let last = |items: &[MenuItemDescriptor]| settings_section_of(&items[items.len() - 1].id);
        assert_eq!(last(&body_items(0, 0, None)).as_deref(), Some("Canvas"));
        assert_eq!(
            last(&price_axis_items(&axis_state(), None)).as_deref(),
            Some("Scales and lines")
        );
        assert_eq!(
            last(&time_axis_items("Etc/UTC", None, NOW)).as_deref(),
            Some("Scales and lines")
        );
    }

    #[test]
    fn a_bare_settings_id_asks_for_no_particular_tab() {
        assert_eq!(settings_section_of("settings"), None);
        assert_eq!(settings_section_of("settings:"), None);
    }
}
