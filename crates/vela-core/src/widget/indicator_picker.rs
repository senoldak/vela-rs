//! Indicator picker: a dialog over the widget's indicator manifest: toggle entries on/off
//! (live add/remove on the current chart; state survives rebuilds via the widget).
//!
//! This is the pure half of the original `IndicatorPicker`: the row model, search matching
//! and the list layout ([`indicator_picker_view`]: the "On chart" card, the library grouped
//! by category, the empty states). Left for the browser crate: the dialog and its search
//! field (clearing it and refreshing on each open, no auto-focus on the mobile layout), the
//! row/badge/trash elements and CSS, and the click routing (a trash click calls
//! `on_remove(instance)`, a row click `on_add(library index)`, both followed by a refresh).

use crate::i18n::{MessageArg, MessageLike, Translator, msg, text_of};

/// One row the picker shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndicatorRow {
    /// The name the picker shows, in the shell's language.
    pub name: String,
    /// Other names search matches: the English name when `name` is a translation, and the
    /// short title (`RSI`) in both languages. A reader of any language still finds an
    /// indicator by the names its literature uses.
    pub aliases: Vec<String>,
    pub language: Option<String>,
    pub category: Option<String>,
    /// Core-computed (native) indicator: accent styling + `native` routing.
    pub native: bool,
    /// Native type id (routing key for add/remove).
    pub native_type: Option<String>,
    /// Beta badge (native catalog flag).
    pub beta: bool,
}

/// The category of a row that declares none (an identity: the heading shows
/// [`general_label`]).
pub const GENERAL: &str = "General";

pub fn title_message() -> MessageLike {
    msg("widget.indicatorPicker.title", "Indicators")
}
pub fn search_placeholder() -> MessageLike {
    msg("widget.indicatorPicker.search", "Search\u{2026}")
}
pub fn beta_label() -> MessageLike {
    msg("widget.indicatorPicker.beta", "beta")
}
pub fn remove_label() -> MessageLike {
    msg("widget.indicatorPicker.remove", "Remove from chart")
}
pub fn general_label() -> MessageLike {
    msg("widget.indicatorPicker.general", "General")
}

/// Lower-case `s` by `locale`'s casing rules: Turkish and Azeri map `I` to dotless `\u{131}`
/// and `\u{130}` to `i`. Other locales use the default rules.
pub fn lower_for_locale(s: &str, locale: Option<&str>) -> String {
    let language = locale
        .map(|l| l.split(['-', '_']).next().unwrap_or(l).to_ascii_lowercase())
        .unwrap_or_default();
    if language == "tr" || language == "az" {
        s.chars()
            .map(|c| match c {
                'I' => "\u{131}".to_string(),
                '\u{130}' => "i".to_string(),
                c => c.to_lowercase().collect(),
            })
            .collect()
    } else {
        s.to_lowercase()
    }
}

/// A row matches by its shown name or by any of its aliases. Text folds by the reader's
/// casing rules and by the default ones, so a Latin name typed under Turkish rules (where
/// `I` lowers to dotless i) still matches. An empty (trimmed) query matches everything.
pub fn row_matches(row: &IndicatorRow, query: &str, locale: Option<&str>) -> bool {
    let raw = query.trim();
    if raw.is_empty() {
        return true;
    }
    let q = lower_for_locale(raw, locale);
    let q_plain = raw.to_lowercase();
    std::iter::once(&row.name)
        .chain(row.aliases.iter())
        .any(|n| lower_for_locale(n, locale).contains(&q) || n.to_lowercase().contains(&q_plain))
}

/// A row with where it sits in its source list (the library or the live instances): the
/// index routes the click.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedRow {
    pub row: IndicatorRow,
    pub index: usize,
}

/// The "On chart" card: every live instance (the same script may appear several times).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnChartCard {
    /// `On chart \u{b7} n`.
    pub title: String,
    pub rows: Vec<IndexedRow>,
}

/// One category heading and its library rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CategoryGroup {
    /// The heading: the category, or the shell's `General` for a row without one.
    pub title: String,
    pub rows: Vec<IndexedRow>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndicatorPickerView {
    pub on_chart: Option<OnChartCard>,
    pub groups: Vec<CategoryGroup>,
    /// The empty state's text, when there is nothing to list.
    pub empty: Option<String>,
}

/// What a row's trailing badge says on the "On chart" card: `vela` for a native indicator,
/// else the script's language.
pub fn instance_badge(row: &IndicatorRow) -> Option<&str> {
    if row.native {
        Some("vela")
    } else {
        row.language.as_deref().filter(|l| !l.is_empty())
    }
}

/// Lay the picker out for `query`: the live instances that match, then the library grouped
/// by category (first-seen order; rows without a category join `General`, which the picker
/// names in the shell's language), or the empty state.
pub fn indicator_picker_view(
    library: &[IndicatorRow],
    on_chart: &[IndicatorRow],
    query: &str,
    i18n: Option<&dyn Translator>,
) -> IndicatorPickerView {
    let locale = i18n.map(|t| t.locale());
    let t = |m: MessageLike, args: &[(&str, MessageArg)]| text_of(i18n, &m, args);
    let live: Vec<IndexedRow> = on_chart
        .iter()
        .enumerate()
        .filter(|(_, r)| row_matches(r, query, locale))
        .map(|(index, r)| IndexedRow {
            row: r.clone(),
            index,
        })
        .collect();
    let mut view = IndicatorPickerView::default();
    if !live.is_empty() {
        view.on_chart = Some(OnChartCard {
            title: t(
                msg("widget.indicatorPicker.onChart", "On chart \u{b7} {count}"),
                &[("count", MessageArg::from(live.len()))],
            ),
            rows: live,
        });
    }
    let mut groups: Vec<(String, Vec<IndexedRow>)> = Vec::new();
    for (index, r) in library.iter().enumerate() {
        if !row_matches(r, query, locale) {
            continue;
        }
        let category = r.category.clone().unwrap_or_else(|| GENERAL.to_string());
        let entry = IndexedRow {
            row: r.clone(),
            index,
        };
        match groups.iter_mut().find(|(c, _)| *c == category) {
            Some((_, bucket)) => bucket.push(entry),
            None => groups.push((category, vec![entry])),
        }
    }
    view.groups = groups
        .into_iter()
        .map(|(category, rows)| CategoryGroup {
            title: if category == GENERAL {
                t(general_label(), &[])
            } else {
                category
            },
            rows,
        })
        .collect();
    if view.on_chart.is_none() && view.groups.is_empty() {
        view.empty = Some(t(
            if library.is_empty() {
                msg(
                    "widget.indicatorPicker.empty",
                    "No indicators in the manifest.",
                )
            } else {
                msg("widget.indicatorPicker.noMatch", "No indicators match.")
            },
            &[],
        ));
    }
    view
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, category: Option<&str>) -> IndicatorRow {
        IndicatorRow {
            name: name.into(),
            category: category.map(str::to_string),
            ..IndicatorRow::default()
        }
    }

    fn titles(view: &IndicatorPickerView) -> Vec<&str> {
        view.groups.iter().map(|g| g.title.as_str()).collect()
    }

    fn names(group: &CategoryGroup) -> Vec<&str> {
        group.rows.iter().map(|r| r.row.name.as_str()).collect()
    }

    #[test]
    fn the_library_groups_by_category_in_first_seen_order_with_general_for_the_rest() {
        let library = vec![
            row("EMA", Some("Trend")),
            row("Mine", None),
            row("RSI", Some("Momentum")),
            row("SMA", Some("Trend")),
        ];
        let view = indicator_picker_view(&library, &[], "", None);
        assert_eq!(titles(&view), ["Trend", "General", "Momentum"]);
        assert_eq!(names(&view.groups[0]), ["EMA", "SMA"]);
        // The index routes the click back to the library.
        assert_eq!(
            view.groups[0]
                .rows
                .iter()
                .map(|r| r.index)
                .collect::<Vec<_>>(),
            [0, 3]
        );
        assert!(view.on_chart.is_none() && view.empty.is_none());
    }

    #[test]
    fn the_on_chart_card_lists_every_matching_live_instance_with_its_count() {
        let live = vec![row("EMA", None), row("RSI", None), row("EMA", None)];
        let view = indicator_picker_view(&[], &live, "ema", None);
        let card = view.on_chart.expect("a card");
        assert_eq!(card.title, "On chart \u{b7} 2");
        assert_eq!(
            card.rows.iter().map(|r| r.index).collect::<Vec<_>>(),
            [0, 2]
        );
    }

    #[test]
    fn search_matches_names_and_aliases_case_insensitively() {
        let mut rsi = row("Relative Strength Index", None);
        rsi.aliases = vec!["RSI".into()];
        assert!(row_matches(&rsi, "rsi", None));
        assert!(row_matches(&rsi, "  STRENGTH ", None));
        assert!(!row_matches(&rsi, "macd", None));
        assert!(row_matches(&rsi, "", None));
        assert!(row_matches(&rsi, "   ", None));
    }

    #[test]
    fn a_latin_name_typed_under_turkish_rules_still_matches() {
        let rsi = row("RSI", None);
        // Under Turkish casing `I` lowers to dotless i: the plain fold keeps it findable.
        assert!(row_matches(&rsi, "RSI", Some("tr")));
        assert!(row_matches(&rsi, "rsi", Some("tr")));
        assert_eq!(lower_for_locale("I\u{130}", Some("tr-TR")), "\u{131}i");
        assert_eq!(lower_for_locale("Iİ", Some("en")), "ii̇");
        assert_eq!(lower_for_locale("ABC", None), "abc");
    }

    #[test]
    fn the_empty_states_tell_an_empty_manifest_from_a_query_that_matched_nothing() {
        assert_eq!(
            indicator_picker_view(&[], &[], "", None).empty.as_deref(),
            Some("No indicators in the manifest.")
        );
        let library = vec![row("EMA", None)];
        assert_eq!(
            indicator_picker_view(&library, &[], "zzz", None)
                .empty
                .as_deref(),
            Some("No indicators match.")
        );
        assert!(
            indicator_picker_view(&library, &[], "ema", None)
                .empty
                .is_none()
        );
    }

    #[test]
    fn an_on_chart_match_alone_is_not_empty() {
        let live = vec![row("Custom", None)];
        let view = indicator_picker_view(&[row("EMA", None)], &live, "custom", None);
        assert!(view.on_chart.is_some() && view.groups.is_empty() && view.empty.is_none());
    }

    #[test]
    fn instance_badges_name_native_or_the_language() {
        let mut native = row("Volume", None);
        native.native = true;
        native.language = Some("pine".into());
        assert_eq!(instance_badge(&native), Some("vela"));
        let mut script = row("EMA", None);
        script.language = Some("pine".into());
        assert_eq!(instance_badge(&script), Some("pine"));
        assert_eq!(instance_badge(&row("X", None)), None);
        script.language = Some(String::new());
        assert_eq!(instance_badge(&script), None);
    }

    #[test]
    fn the_labels_read_in_english() {
        let t = |m: MessageLike| text_of(None, &m, &[]);
        assert_eq!(t(title_message()), "Indicators");
        assert_eq!(t(search_placeholder()), "Search\u{2026}");
        assert_eq!(t(beta_label()), "beta");
        assert_eq!(t(remove_label()), "Remove from chart");
    }
}
