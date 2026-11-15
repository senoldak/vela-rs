//! Price scale drawer: the mobile bottom sheet a long-press on the price axis opens. It is
//! the desktop price-axis menu ([`price_axis_items`](super::context_menu_model::price_axis_items))
//! re-laid as touch rows, and each row's tap resolves to a renderer write.
//!
//! The pure half of the original `PriceScaleDrawer`: the sheet's rows ([`sheet_rows`]) and
//! what a tap does ([`tap_action`]). Left for the browser crate: the drawer sheet, the row
//! and section elements and CSS, reading and writing the renderer's features (`autoScale`,
//! `scalePriceOnly`, `invertScale`, `axisLabels`, ...; each write re-renders the sheet, and
//! after an invert or a scale choice the pane snapshot is re-read from `paneScales`).

use super::context_menu_model::{
    PaneScaleInfo, ScaleChoice, invert_write, scale_writes, settings_section_of,
};
use crate::ui::MenuItemDescriptor;
use serde_json::Value;

pub fn title_message() -> crate::i18n::MessageLike {
    crate::i18n::msg("widget.priceScaleDrawer.title", "Price scale")
}

/// One row of the sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SheetRow {
    /// A section heading (`Labels`, `Levels`).
    Section(String),
    /// A tappable row.
    Item {
        id: String,
        label: String,
        checked: bool,
        /// A hairline above (the settings row, the first scale choice).
        separator: bool,
    },
}

/// The rows: a `labels` / `levels` submenu becomes a section heading followed by its
/// toggles; the settings row is separated; everything else is a plain (checkable) row.
pub fn sheet_rows(items: &[MenuItemDescriptor]) -> Vec<SheetRow> {
    let mut rows = Vec::new();
    for item in items {
        if (item.id == "labels" || item.id == "levels")
            && let Some(sub) = &item.submenu
        {
            rows.push(SheetRow::Section(item.label.clone()));
            for s in sub {
                rows.push(SheetRow::Item {
                    id: s.id.clone(),
                    label: s.label.clone(),
                    checked: s.checked == Some(true),
                    separator: false,
                });
            }
            continue;
        }
        let is_settings = item.id.starts_with("settings");
        rows.push(SheetRow::Item {
            id: item.id.clone(),
            label: item.label.clone(),
            checked: item.checked == Some(true),
            separator: is_settings || (item.id.starts_with("scale:") && item.separator_before),
        });
    }
    rows
}

/// What tapping a row asks of the renderer.
#[derive(Clone, Debug, PartialEq)]
pub enum TapAction {
    /// Flip a boolean feature (`toggle:<feature>`, `price-only`, `auto`).
    ToggleFeature(String),
    /// `autoScale` flips against its own default (on): off exactly when it reads `false`.
    ToggleAutoScale,
    /// Write these features (invert, a scale choice), then re-read the pane snapshot.
    Write(Vec<(String, Value)>),
    /// Open the settings dialog on this tab (`None`: its first), then close the sheet.
    OpenSettings(Option<String>),
}

/// Route a row id. `invert_now` is the current invert state of the targeted axis (the
/// pane's, else the chart-level one); `pane` is the targeted pane when per-pane scales are
/// reported. `None` for an id the sheet does not act on.
pub fn tap_action(id: &str, invert_now: bool, pane: Option<&PaneScaleInfo>) -> Option<TapAction> {
    if let Some(feature) = id.strip_prefix("toggle:") {
        return Some(TapAction::ToggleFeature(feature.to_string()));
    }
    if id.starts_with("settings") {
        return Some(TapAction::OpenSettings(settings_section_of(id)));
    }
    match id {
        "auto" => Some(TapAction::ToggleAutoScale),
        "price-only" => Some(TapAction::ToggleFeature("scalePriceOnly".to_string())),
        "invert" => {
            let (feature, value) = invert_write(!invert_now, pane);
            Some(TapAction::Write(vec![(feature, value)]))
        }
        _ => {
            let choice = match id.strip_prefix("scale:")? {
                "regular" => ScaleChoice::Regular,
                "percent" => ScaleChoice::Percent,
                "indexed" => ScaleChoice::Indexed,
                "log" => ScaleChoice::Log,
                _ => return None,
            };
            Some(TapAction::Write(scale_writes(choice, pane)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::context_menu_model::{PaneKind, PriceAxisState, price_axis_items};
    use serde_json::json;

    fn state() -> PriceAxisState {
        PriceAxisState {
            auto: true,
            price_only: Some(false),
            invert: false,
            choice: ScaleChoice::Percent,
            axis_labels: true,
            price_label: false,
            countdown: false,
            price_line: true,
        }
    }

    #[test]
    fn submenus_become_sections_with_their_toggles_and_the_settings_row_is_set_apart() {
        let rows = sheet_rows(&price_axis_items(&state(), None));
        let shape: Vec<String> = rows
            .iter()
            .map(|r| match r {
                SheetRow::Section(s) => format!("[{s}]"),
                SheetRow::Item { id, separator, .. } => {
                    format!("{}{id}", if *separator { "| " } else { "" })
                }
            })
            .collect();
        assert_eq!(
            shape,
            [
                "auto",
                "price-only",
                "invert",
                "| scale:regular",
                "scale:percent",
                "scale:indexed",
                "scale:log",
                "[Labels]",
                "toggle:axisLabels",
                "toggle:priceLabel",
                "toggle:countdown",
                "[Levels]",
                "toggle:currentPriceLine",
                "| settings:Scales and lines",
            ]
        );
    }

    #[test]
    fn rows_carry_the_check_marks() {
        let rows = sheet_rows(&price_axis_items(&state(), None));
        let checked: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                SheetRow::Item {
                    id, checked: true, ..
                } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            checked,
            [
                "auto",
                "scale:percent",
                "toggle:axisLabels",
                "toggle:currentPriceLine"
            ]
        );
    }

    #[test]
    fn taps_route_to_renderer_work() {
        assert_eq!(
            tap_action("toggle:countdown", false, None),
            Some(TapAction::ToggleFeature("countdown".into()))
        );
        assert_eq!(
            tap_action("price-only", false, None),
            Some(TapAction::ToggleFeature("scalePriceOnly".into()))
        );
        assert_eq!(
            tap_action("auto", false, None),
            Some(TapAction::ToggleAutoScale)
        );
        assert_eq!(
            tap_action("settings:Scales and lines", false, None),
            Some(TapAction::OpenSettings(Some("Scales and lines".into())))
        );
        assert_eq!(
            tap_action("settings", false, None),
            Some(TapAction::OpenSettings(None))
        );
        assert_eq!(tap_action("labels", false, None), None);
        assert_eq!(tap_action("scale:sideways", false, None), None);
    }

    #[test]
    fn invert_and_scale_choices_write_through_the_main_or_the_study_pane() {
        assert_eq!(
            tap_action("invert", false, None),
            Some(TapAction::Write(vec![("invertScale".into(), json!(true))]))
        );
        let study = PaneScaleInfo {
            id: "p2".into(),
            kind: PaneKind::Study,
            top: 0.0,
            height: 100.0,
            mode: "price".into(),
            log: false,
            invert: true,
        };
        assert_eq!(
            tap_action("invert", true, Some(&study)),
            Some(TapAction::Write(vec![(
                "invertScale".into(),
                json!({ "pane": "p2", "value": false })
            )]))
        );
        let Some(TapAction::Write(writes)) = tap_action("scale:log", false, Some(&study)) else {
            panic!("a write");
        };
        assert_eq!(
            writes[1],
            (
                "logScale".to_string(),
                json!({ "pane": "p2", "value": true })
            )
        );
        assert_eq!(
            crate::i18n::text_of(None, &title_message(), &[]),
            "Price scale"
        );
    }
}
