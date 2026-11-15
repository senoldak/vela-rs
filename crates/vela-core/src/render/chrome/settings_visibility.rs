//! The settings-dialog VISIBILITY POLICY: the pure logic behind `settings: { hidden }`.
//!
//! Every tab, group, and row the dialog renders carries a stable hierarchical id
//! (dot-separated, kebab-case). The policy is a flat list of ids to hide; an id hides its
//! whole subtree, and hiding is presentation-only: hidden values keep being stored,
//! delivered, and applied. Default: everything visible.
//!
//! Id derivation is IMPLICIT so contributors need no extra wiring:
//! - built-in tabs, groups and rows: the static ids stamped by the settings dialog
//!   (enumerated in [`BUILTIN_SETTINGS_IDS`]; keep the two in sync);
//! - chart-type SDK sections: `type:<typeId>` for the tab, the row's own bag key for value
//!   rows, the label's slug for headings and headers, the title's slug for subsections;
//! - host sections: the section or row `id` field when given, else the title or label
//!   slug.
//!
//! Kept free of DOM so the policy is unit-testable. The settings dialog itself is the
//! browser crate's.

use std::collections::HashSet;

use crate::chart_types::{BasePainting, ChartTypeRegistry};
use crate::chart_types::{SettingsInlineControl, SettingsRowDescriptor, normalize_settings_row};
use crate::i18n::{MessageLike, source_text};

/// The built-in price styles, which share the `candles` block instead of carrying their
/// own candle cosmetics.
const BUILTIN_PRICE_STYLES: [&str; 9] = [
    "candles",
    "bars",
    "line",
    "linemarkers",
    "stepline",
    "area",
    "baseline",
    "columns",
    "heikinashi",
];

/// Kebab-case slug of a display label: the implicit id of label-only elements. A message
/// slugs its English source, so an id never changes with the display language.
pub fn settings_id_slug(label: &MessageLike) -> String {
    slug_of(&source_text(label))
}

fn slug_of(text: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for c in text.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(c);
        } else {
            pending_dash = true;
        }
    }
    out
}

/// The Events tab: timeline-mark visibility; present once marks name groups.
pub const MARKS_SETTINGS_ID: &str = "events";
/// Its "Visible events" section: one checkbox per mark group (the id names the groups,
/// not the label).
pub const MARKS_GROUPS_SETTINGS_ID: &str = "events.groups";

/// A mark group's settings-row id (`events.groups.<group-id>`, the id kebab-cased).
pub fn mark_group_settings_id(group_id: &str) -> String {
    format!("{MARKS_GROUPS_SETTINGS_ID}.{}", slug_of(group_id))
}

/// True when `id`, or any of its dot-path ancestors, is in the hidden set.
pub fn settings_id_hidden(id: &str, hidden: &HashSet<String>) -> bool {
    if hidden.is_empty() {
        return false;
    }
    let mut path = id;
    loop {
        if hidden.contains(path) {
            return true;
        }
        match path.rfind('.') {
            Some(cut) => path = &path[..cut],
            None => return false,
        }
    }
}

/// The key a control stores under (`None` for display-only hints).
fn control_key(control: &SettingsInlineControl) -> Option<&str> {
    match control {
        SettingsInlineControl::Number { key, .. }
        | SettingsInlineControl::Color { key, .. }
        | SettingsInlineControl::Width { key, .. }
        | SettingsInlineControl::Select { key, .. } => Some(key),
        SettingsInlineControl::Hint { .. } => None,
    }
}

/// The implicit id of one chart-type settings row: the boolean toggle's key for composite
/// rows, else the first stored key (that key already being the row's stable persistence
/// contract), else the label's slug (headings and headers, key-less rows).
pub fn settings_row_id(r: &SettingsRowDescriptor) -> String {
    let (SettingsRowDescriptor::Heading { label, .. }
    | SettingsRowDescriptor::Header { label, .. }) = r
    else {
        let Some(n) = normalize_settings_row(r) else {
            return String::new();
        };
        if let Some(toggle) = &n.toggle {
            return toggle.key.clone();
        }
        if let Some(key) = n.controls.iter().find_map(control_key) {
            return key.to_string();
        }
        return settings_id_slug(&n.label);
    };
    settings_id_slug(label)
}

/// Drop the hidden rows of a chart-type section, `scope` being the id prefix
/// (`type:<typeId>`). Hiding a `Heading` drops its whole group (every row up to the next
/// heading); hiding a `Header` drops its subgroup (up to the next header or heading).
/// Gates (`when`) and value delivery are untouched: callers must still SEED from the
/// unfiltered rows so conditions keep reading their defaults.
pub fn filter_hidden_rows(
    rows: &[SettingsRowDescriptor],
    scope: &str,
    hidden: &HashSet<String>,
) -> Vec<SettingsRowDescriptor> {
    if hidden.is_empty() {
        return rows.to_vec();
    }
    let mut out = Vec::new();
    let mut skip_group = false;
    let mut skip_sub = false;
    for r in rows {
        let id = format!("{scope}.{}", settings_row_id(r));
        match r {
            SettingsRowDescriptor::Heading { .. } => {
                skip_group = settings_id_hidden(&id, hidden);
                skip_sub = false;
                if !skip_group {
                    out.push(r.clone());
                }
            }
            SettingsRowDescriptor::Header { .. } => {
                if skip_group {
                    continue;
                }
                skip_sub = settings_id_hidden(&id, hidden);
                if !skip_sub {
                    out.push(r.clone());
                }
            }
            _ => {
                if skip_group || skip_sub || settings_id_hidden(&id, hidden) {
                    continue;
                }
                out.push(r.clone());
            }
        }
    }
    out
}

/// The structural slice of a host row this module needs (no callbacks).
#[derive(Clone, Debug, PartialEq)]
pub struct HostRowIdSource {
    pub kind: String,
    pub label: MessageLike,
    /// Explicit stable id: labels are display text; set this to survive renames.
    pub id: Option<String>,
}

/// What [`filter_hidden_host_rows`] needs of a host row (the original is generic over any
/// row with these fields).
pub trait HostRowId {
    fn kind(&self) -> &str;
    fn label(&self) -> &MessageLike;
    fn explicit_id(&self) -> Option<&str>;
}

impl HostRowId for HostRowIdSource {
    fn kind(&self) -> &str {
        &self.kind
    }
    fn label(&self) -> &MessageLike {
        &self.label
    }
    fn explicit_id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

/// The structural slice of a host section this module needs.
#[derive(Clone, Debug, PartialEq)]
pub struct HostSectionIdSource {
    pub title: MessageLike,
    pub rows: Vec<HostRowIdSource>,
    pub id: Option<String>,
}

/// A host section's id: the explicit `id` when given, else the title's slug.
pub fn host_section_id(s: &HostSectionIdSource) -> String {
    s.id.clone().unwrap_or_else(|| settings_id_slug(&s.title))
}

/// A host row's leaf id: the explicit `id` when given, else the label's slug.
pub fn host_row_id<R: HostRowId>(r: &R) -> String {
    r.explicit_id()
        .map(str::to_string)
        .unwrap_or_else(|| settings_id_slug(r.label()))
}

/// Drop the hidden rows of a host section (same semantics as [`filter_hidden_rows`]:
/// hiding a `heading` drops every row up to the next heading).
pub fn filter_hidden_host_rows<T: HostRowId + Clone>(
    rows: &[T],
    scope: &str,
    hidden: &HashSet<String>,
) -> Vec<T> {
    if hidden.is_empty() {
        return rows.to_vec();
    }
    let mut out = Vec::new();
    let mut skip_group = false;
    for r in rows {
        let id = format!("{scope}.{}", host_row_id(r));
        if r.kind() == "heading" {
            skip_group = settings_id_hidden(&id, hidden);
            if !skip_group {
                out.push(r.clone());
            }
            continue;
        }
        if skip_group || settings_id_hidden(&id, hidden) {
            continue;
        }
        out.push(r.clone());
    }
    out
}

/// The BUILT-IN ids the settings dialog stamps: tab, then its groups, then their rows.
/// Must match the `sid(...)` literals in the dialog; the catalog test guards the obvious
/// drifts (shape, prefixes), the browser probe the rest.
pub const BUILTIN_SETTINGS_IDS: &[&str] = &[
    "symbol",
    "symbol.type",
    "symbol.style.candles",
    "symbol.style.candles.body",
    "symbol.style.candles.borders",
    "symbol.style.candles.wick",
    "symbol.style.candles.spacing",
    "symbol.style.bars",
    "symbol.style.bars.up-color",
    "symbol.style.bars.down-color",
    "symbol.style.bars.spacing",
    "symbol.style.line",
    "symbol.style.line.color",
    "symbol.style.line.width",
    "symbol.style.area",
    "symbol.style.area.line-color",
    "symbol.style.area.width",
    "symbol.style.area.top-fill",
    "symbol.style.area.bottom-fill",
    "symbol.style.baseline",
    "symbol.style.baseline.top-line",
    "symbol.style.baseline.bottom-line",
    "symbol.style.baseline.fill-top",
    "symbol.style.baseline.fill-bottom",
    "symbol.style.baseline.base-level",
    "symbol.style.baseline.width",
    "symbol.style.columns",
    "symbol.style.columns.up-color",
    "symbol.style.columns.down-color",
    "symbol.style.columns.spacing",
    "symbol.animation",
    "symbol.animation.zoom",
    "symbol.animation.pan",
    "symbol.animation.autoscale",
    "symbol.animation.price-changes",
    "symbol.animation.intro",
    "symbol.timezone",
    "scales",
    "scales.price-scale",
    "scales.price-scale.mode",
    "scales.price-scale.invert",
    "scales.price-scale.price-only",
    "scales.price-scale.free-pan",
    "scales.price-scale.last-price-line",
    "scales.price-scale.last-price-label",
    "scales.price-scale.countdown",
    "scales.price-scale.axis-labels",
    "scales.price-scale.border-color",
    "scales.crosshair",
    "scales.crosshair.color",
    "scales.crosshair.width",
    "scales.crosshair.style",
    "canvas",
    "canvas.background",
    "canvas.background.color",
    "canvas.background.text-color",
    "canvas.background.text-size",
    "canvas.background.pane-separator",
    "canvas.grid",
    "canvas.grid.vertical",
    "canvas.grid.horizontal",
    "canvas.margins",
    "canvas.margins.top",
    "canvas.margins.bottom",
    "canvas.margins.right",
    "canvas.theme",
];

/// Whether a style carries its OWN candle cosmetics: a registered plugin type whose base
/// painting is candles. Built-ins (heikin-ashi included) share the `candles` block
/// instead.
fn has_own_candle_paint(registry: &ChartTypeRegistry, style: &str) -> bool {
    if BUILTIN_PRICE_STYLES.contains(&style) {
        return false;
    }
    registry
        .all()
        .iter()
        .find(|t| t.id == style)
        .is_some_and(|t| t.base_painting() == BasePainting::Candles)
}

/// Every addressable setting id of a chart instance: the built-ins, the registered chart
/// types' sections (rows enumerated across flat rows, instances, and subsections), the
/// given host sections, and the timeline-mark groups (the Events tab, present once marks
/// name groups). The discovery surface behind `chart.renderer.listSettingsIds()`: hosts
/// enumerate this instead of reading contributor source.
pub fn settings_id_catalog(
    registry: &ChartTypeRegistry,
    host_sections: &[HostSectionIdSource],
    mark_groups: &[&str],
) -> Vec<String> {
    let mut ids = IdSet::default();
    for id in BUILTIN_SETTINGS_IDS {
        ids.add((*id).to_string());
    }
    // Dynamic rows (one per mark group) enumerate here, not in the static list.
    if !mark_groups.is_empty() {
        ids.add(MARKS_SETTINGS_ID.to_string());
        ids.add(MARKS_GROUPS_SETTINGS_ID.to_string());
        for g in mark_groups {
            ids.add(mark_group_settings_id(g));
        }
    }
    for def in registry.all() {
        // A plugin style painting its own candles gets the per-style cosmetics group in
        // the Symbol tab (settings section or not): same row set as the built-in.
        if has_own_candle_paint(registry, &def.id) {
            let style = format!("symbol.style.{}", def.id);
            for leaf in ["", ".body", ".borders", ".wick", ".spacing"] {
                ids.add(format!("{style}{leaf}"));
            }
        }
        let Some(section) = &def.settings else {
            continue;
        };
        let scope = format!("type:{}", def.id);
        ids.add(scope.clone());
        ids.add_rows(&scope, &section.rows);
        for inst in &section.instances {
            ids.add_rows(&scope, &inst.rows);
        }
        for sub in &section.subsections {
            ids.add(format!("{scope}.{}", settings_id_slug(&sub.title)));
            ids.add_rows(&scope, &sub.rows);
        }
    }
    for hs in host_sections {
        let scope = host_section_id(hs);
        ids.add(scope.clone());
        for r in &hs.rows {
            ids.add(format!("{scope}.{}", host_row_id(r)));
        }
    }
    ids.ids
}

/// An insertion-ordered set of ids.
#[derive(Default)]
struct IdSet {
    ids: Vec<String>,
    seen: HashSet<String>,
}

impl IdSet {
    fn add(&mut self, id: String) {
        if self.seen.insert(id.clone()) {
            self.ids.push(id);
        }
    }

    fn add_rows(&mut self, scope: &str, rows: &[SettingsRowDescriptor]) {
        for r in rows {
            self.add(format!("{scope}.{}", settings_row_id(r)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chart_types::ChartTypeDefinition;
    use crate::chart_types::{
        ChartTypeSettingsInstance, ChartTypeSettingsSection, ChartTypeSettingsSubsection,
        SettingsRowToggle, SettingsSelectOption,
    };
    use crate::i18n::msg;

    fn label(s: &str) -> MessageLike {
        MessageLike::Text(s.to_string().into())
    }

    fn set(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| (*s).to_string()).collect()
    }

    fn heading(l: &str) -> SettingsRowDescriptor {
        SettingsRowDescriptor::Heading {
            label: label(l),
            when: None,
        }
    }

    fn header(l: &str) -> SettingsRowDescriptor {
        SettingsRowDescriptor::Header {
            label: label(l),
            when: None,
        }
    }

    fn toggle(key: &str, l: &str) -> SettingsRowDescriptor {
        SettingsRowDescriptor::Toggle {
            key: key.into(),
            label: label(l),
            defval: true,
            number: None,
            colors: vec![],
            width: None,
            when: None,
        }
    }

    fn number(key: &str, l: &str) -> SettingsRowDescriptor {
        SettingsRowDescriptor::Number {
            key: key.into(),
            label: label(l),
            defval: 20.0,
            min: None,
            max: None,
            step: None,
            when: None,
        }
    }

    fn color(key: &str, l: &str) -> SettingsRowDescriptor {
        SettingsRowDescriptor::Color {
            key: key.into(),
            label: label(l),
            defval: "#fff".into(),
            when: None,
        }
    }

    fn select(key: &str, l: &str) -> SettingsRowDescriptor {
        SettingsRowDescriptor::Select {
            key: key.into(),
            label: label(l),
            options: vec![
                SettingsSelectOption::Plain("a".into()),
                SettingsSelectOption::Plain("b".into()),
            ],
            defval: "a".into(),
            when: None,
        }
    }

    #[test]
    fn slug_kebab_cases_display_labels() {
        assert_eq!(settings_id_slug(&label("Bars to fetch")), "bars-to-fetch");
        assert_eq!(settings_id_slug(&label("Pre-market")), "pre-market");
        assert_eq!(settings_id_slug(&label("  Base level % ")), "base-level");
        assert_eq!(settings_id_slug(&label("OHLC values")), "ohlc-values");
        assert_eq!(settings_id_slug(&label("")), "");
        assert_eq!(settings_id_slug(&label("---")), "");
    }

    #[test]
    fn a_message_slugs_its_english_source() {
        assert_eq!(
            settings_id_slug(&msg("settings.x", "Bars to fetch")),
            "bars-to-fetch"
        );
    }

    #[test]
    fn hidden_matches_exact_ids_and_dot_path_ancestors() {
        let hidden = set(&["advanced", "canvas.grid", "type:vp"]);
        assert!(settings_id_hidden("advanced", &hidden));
        assert!(settings_id_hidden("advanced.bars", &hidden));
        assert!(settings_id_hidden("canvas.grid.vertical", &hidden));
        assert!(!settings_id_hidden("canvas.background", &hidden));
        assert!(settings_id_hidden("type:vp.levels", &hidden));
    }

    #[test]
    fn hidden_never_matches_by_string_prefix_alone() {
        let hidden = set(&["canvas"]);
        assert!(!settings_id_hidden("canvas-extra", &hidden));
        assert!(settings_id_hidden("canvas.grid", &hidden));
    }

    #[test]
    fn hidden_is_a_no_op_on_an_empty_policy() {
        assert!(!settings_id_hidden("anything", &HashSet::new()));
    }

    #[test]
    fn row_ids_use_the_stable_bag_key_of_value_rows_and_the_label_slug_of_titles() {
        assert_eq!(
            settings_row_id(&toggle("highlights", "Highlights")),
            "highlights"
        );
        assert_eq!(settings_row_id(&number("levels", "Max levels")), "levels");
        let range = SettingsRowDescriptor::Range {
            label: label("Volume"),
            min_key: "minVol".into(),
            max_key: "maxVol".into(),
            defval: 0.0,
            min: None,
            max: None,
            step: None,
            placeholder: None,
            when: None,
        };
        assert_eq!(settings_row_id(&range), "minVol");
        assert_eq!(settings_row_id(&heading("My Group")), "my-group");
        let composite = SettingsRowDescriptor::Row {
            label: label("Imbalance"),
            toggle: Some(SettingsRowToggle {
                key: "imb".into(),
                defval: false,
            }),
            controls: vec![SettingsInlineControl::Select {
                key: "imbMode".into(),
                label: label("Mode"),
                options: vec![SettingsSelectOption::Plain("a".into())],
                defval: "a".into(),
                when: None,
            }],
            when: None,
        };
        assert_eq!(settings_row_id(&composite), "imb");
    }

    #[test]
    fn a_row_without_a_toggle_or_keyed_control_falls_back_to_its_label() {
        let hint_only = SettingsRowDescriptor::Row {
            label: label("Just a note"),
            toggle: None,
            controls: vec![SettingsInlineControl::Hint { text: label("x") }],
            when: None,
        };
        assert_eq!(settings_row_id(&hint_only), "just-a-note");
    }

    fn rows() -> Vec<SettingsRowDescriptor> {
        vec![
            heading("Levels"),
            toggle("highlights", "Highlights"),
            header("Colors"),
            color("buyColor", "Buy color"),
            heading("Display"),
            select("mode", "Mode"),
        ]
    }

    fn kinds(rows: &[SettingsRowDescriptor]) -> Vec<&'static str> {
        rows.iter()
            .map(|r| match r {
                SettingsRowDescriptor::Heading { .. } => "heading",
                SettingsRowDescriptor::Header { .. } => "header",
                SettingsRowDescriptor::Toggle { .. } => "toggle",
                SettingsRowDescriptor::Color { .. } => "color",
                SettingsRowDescriptor::Select { .. } => "select",
                _ => "other",
            })
            .collect()
    }

    #[test]
    fn filter_keeps_everything_under_an_empty_policy() {
        assert_eq!(
            filter_hidden_rows(&rows(), "type:t", &HashSet::new()),
            rows()
        );
    }

    #[test]
    fn filter_drops_a_single_row_by_its_key_id() {
        let out = filter_hidden_rows(&rows(), "type:t", &set(&["type:t.buyColor"]));
        assert_eq!(
            kinds(&out),
            ["heading", "toggle", "header", "heading", "select"]
        );
    }

    #[test]
    fn a_hidden_heading_takes_its_whole_group_a_hidden_header_its_subgroup() {
        let group = filter_hidden_rows(&rows(), "type:t", &set(&["type:t.levels"]));
        assert_eq!(group, rows()[4..].to_vec());
        let sub = filter_hidden_rows(&rows(), "type:t", &set(&["type:t.colors"]));
        assert_eq!(kinds(&sub), ["heading", "toggle", "heading", "select"]);
    }

    #[test]
    fn hiding_the_whole_scope_empties_the_section() {
        assert!(filter_hidden_rows(&rows(), "type:t", &set(&["type:t"])).is_empty());
    }

    fn host_row(kind: &str, l: &str, id: &str) -> HostRowIdSource {
        HostRowIdSource {
            kind: kind.into(),
            label: label(l),
            id: Some(id.into()),
        }
    }

    fn status_section() -> HostSectionIdSource {
        HostSectionIdSource {
            title: label("Status line"),
            id: Some("status-line".into()),
            rows: vec![
                host_row("heading", "Status line", "parts"),
                host_row("toggle", "Symbol name", "name"),
                host_row("heading", "Indicators", "indicators"),
                host_row("toggle", "Titles", "indicator-titles"),
                host_row("toggle", "Values", "indicator-values"),
            ],
        }
    }

    fn row_ids(rows: &[HostRowIdSource]) -> Vec<&str> {
        rows.iter().filter_map(|r| r.id.as_deref()).collect()
    }

    #[test]
    fn host_ids_explicit_wins_label_slugs_are_the_fallback() {
        assert_eq!(host_section_id(&status_section()), "status-line");
        let bare = HostSectionIdSource {
            title: label("My Panel"),
            rows: vec![],
            id: None,
        };
        assert_eq!(host_section_id(&bare), "my-panel");
        let row = HostRowIdSource {
            kind: "toggle".into(),
            label: label("Bars to fetch"),
            id: None,
        };
        assert_eq!(host_row_id(&row), "bars-to-fetch");
    }

    #[test]
    fn host_filtering_hides_one_row_and_a_heading_with_its_group() {
        let section = status_section();
        let one =
            filter_hidden_host_rows(&section.rows, "status-line", &set(&["status-line.name"]));
        assert_eq!(
            row_ids(&one),
            [
                "parts",
                "indicators",
                "indicator-titles",
                "indicator-values"
            ]
        );
        let group = filter_hidden_host_rows(
            &section.rows,
            "status-line",
            &set(&["status-line.indicators"]),
        );
        assert_eq!(row_ids(&group), ["parts", "name"]);
        let all = filter_hidden_host_rows(&section.rows, "status-line", &HashSet::new());
        assert_eq!(all, section.rows);
    }

    fn viz_registry() -> ChartTypeRegistry {
        let mut registry = ChartTypeRegistry::new();
        let mut def = ChartTypeDefinition::new("viz-test");
        def.settings = Some(ChartTypeSettingsSection {
            title: label("Viz Test"),
            rows: vec![heading("Levels"), number("levels", "Max levels")],
            subsections: vec![ChartTypeSettingsSubsection {
                title: label("Overlay"),
                rows: vec![toggle("ovl", "Overlay")],
                enable_key: None,
            }],
            ..ChartTypeSettingsSection::default()
        });
        registry.register(def);
        registry
    }

    #[test]
    fn the_catalog_lists_builtins_host_sections_and_registered_chart_type_sections() {
        let host = HostSectionIdSource {
            title: label("Advanced"),
            id: Some("advanced".into()),
            rows: vec![host_row("select", "Bars to fetch", "bars")],
        };
        let ids = settings_id_catalog(&viz_registry(), &[host], &[]);
        for builtin in BUILTIN_SETTINGS_IDS {
            assert!(ids.iter().any(|i| i == builtin), "{builtin}");
        }
        for want in [
            "type:viz-test",
            "type:viz-test.levels",
            "type:viz-test.overlay",
            "type:viz-test.ovl",
            "advanced",
            "advanced.bars",
        ] {
            assert!(ids.iter().any(|i| i == want), "{want}");
        }
    }

    #[test]
    fn the_catalog_enumerates_instance_rows_too() {
        let mut registry = ChartTypeRegistry::new();
        let mut def = ChartTypeDefinition::new("inst");
        def.settings = Some(ChartTypeSettingsSection {
            title: label("Inst"),
            instances: vec![ChartTypeSettingsInstance {
                label: label("A"),
                enable_key: None,
                rows: vec![number("depth", "Depth")],
            }],
            ..ChartTypeSettingsSection::default()
        });
        registry.register(def);
        let ids = settings_id_catalog(&registry, &[], &[]);
        assert!(ids.iter().any(|i| i == "type:inst.depth"));
    }

    #[test]
    fn a_candle_painting_plugin_style_gets_the_per_style_cosmetics_ids() {
        let mut registry = ChartTypeRegistry::new();
        registry.register(ChartTypeDefinition::new("footprint"));
        let mut none = ChartTypeDefinition::new("ghost");
        none.base_painting = Some(BasePainting::None);
        registry.register(none);
        registry.register(ChartTypeDefinition::new("heikinashi"));
        let ids = settings_id_catalog(&registry, &[], &[]);
        assert!(ids.iter().any(|i| i == "symbol.style.footprint.wick"));
        assert!(!ids.iter().any(|i| i.starts_with("symbol.style.ghost")));
        assert!(!ids.iter().any(|i| i.starts_with("symbol.style.heikinashi")));
    }

    #[test]
    fn the_catalog_lists_the_events_tab_and_one_row_per_group_only_while_groups_exist() {
        let registry = ChartTypeRegistry::new();
        assert!(
            !settings_id_catalog(&registry, &[], &[])
                .iter()
                .any(|i| i == "events")
        );
        let ids = settings_id_catalog(&registry, &[], &["dividends", "stock splits"]);
        for want in [
            "events",
            "events.groups",
            "events.groups.dividends",
            "events.groups.stock-splits",
        ] {
            assert!(ids.iter().any(|i| i == want), "{want}");
        }
        assert_eq!(
            mark_group_settings_id("Stock Splits"),
            "events.groups.stock-splits"
        );
    }

    #[test]
    fn built_in_ids_are_unique_and_rows_nest_under_their_tab() {
        let real: Vec<&str> = BUILTIN_SETTINGS_IDS.to_vec();
        let unique: HashSet<&str> = real.iter().copied().collect();
        assert_eq!(unique.len(), real.len());
        let tabs = ["symbol", "scales", "canvas"];
        for id in real {
            assert!(
                tabs.iter()
                    .any(|t| id == *t || id.starts_with(&format!("{t}.")))
            );
        }
    }

    #[test]
    fn the_catalog_never_repeats_an_id() {
        let ids = settings_id_catalog(&viz_registry(), &[], &["a", "a"]);
        let unique: HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
    }
}
