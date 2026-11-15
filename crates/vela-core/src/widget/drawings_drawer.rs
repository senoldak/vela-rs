//! Drawings drawer: the mobile bottom sheet of drawing tools: a search field, one tab per
//! toolbar group, and the tool rows with a favorite star.
//!
//! The pure half of the original `DrawingsDrawer`: [`DrawerModel`] holds the toolbar read at
//! open, the active group and the query, and lays the list out ([`DrawerModel::list`]): a
//! flat search over every group, or the active group (with its sections). Left for the
//! browser crate: the drawer sheet, the search field, the tab strip (picking a tab, the
//! active one included, clears the query), the row elements with glyph and star, and the
//! swipe handler that calls [`DrawerModel::step_group`].

use crate::drawings::toolbar::{ToolGroupSource, ToolSource, ToolbarSource};
use crate::i18n::{MessageArg, MessageLike, Translator, msg, source_text, text_of};

use super::indicator_picker::lower_for_locale;

pub fn title_message() -> MessageLike {
    msg("widget.drawingsDrawer.title", "Drawings")
}
pub fn search_placeholder() -> MessageLike {
    msg("widget.drawingsDrawer.search", "Search tools\u{2026}")
}
pub fn no_match_message() -> MessageLike {
    msg("widget.drawingsDrawer.noMatch", "No tools match.")
}

/// A star's accessible name: `Favorite {tool}`.
pub fn favorite_label(tool: &str, i18n: Option<&dyn Translator>) -> String {
    text_of(
        i18n,
        &msg("widget.drawingsDrawer.favorite", "Favorite {tool}"),
        &[("tool", MessageArg::from(tool.to_string()))],
    )
}

/// One tool row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRow {
    pub type_key: String,
    pub label: String,
    /// Inline SVG markup of the glyph.
    pub icon: String,
    /// The currently armed tool.
    pub active: bool,
}

/// A section heading and its rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListSection {
    /// `None` for the unlabeled run of a group without sections.
    pub title: Option<String>,
    pub rows: Vec<ToolRow>,
}

/// What the list shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DrawerList {
    /// Nothing matched the search.
    Empty(String),
    Sections(Vec<ListSection>),
}

/// The drawer's state.
#[derive(Clone, Debug, Default)]
pub struct DrawerModel {
    definition: ToolbarSource,
    active_group: String,
    query: String,
}

impl DrawerModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// The drawer opens: read the toolbar live (custom sets and late-registered types
    /// apply), keep the active group when it still exists (else the first), clear the query.
    pub fn open(&mut self, toolbar: ToolbarSource) {
        self.definition = toolbar;
        if !self
            .definition
            .groups
            .iter()
            .any(|g| g.id == self.active_group)
        {
            self.active_group = self
                .definition
                .groups
                .first()
                .map(|g| g.id.clone())
                .unwrap_or_default();
        }
        self.query.clear();
    }

    pub fn groups(&self) -> &[ToolGroupSource] {
        &self.definition.groups
    }

    pub fn active_group(&self) -> &str {
        &self.active_group
    }

    /// Pick a group tab. Leaves search mode (picking a tab, the active one included).
    pub fn set_group(&mut self, id: &str) {
        self.active_group = id.to_string();
        self.query.clear();
    }

    pub fn set_query(&mut self, query: &str) {
        self.query = query.to_string();
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Move the active group tab by `step` (a horizontal swipe on the list). Search mode
    /// shows the flattened results: no tabs to page through, so the swipe is inert. Returns
    /// whether the group changed.
    pub fn step_group(&mut self, step: i64) -> bool {
        if !self.query.trim().is_empty() {
            return false;
        }
        let groups = &self.definition.groups;
        let Some(at) = groups.iter().position(|g| g.id == self.active_group) else {
            return false;
        };
        let target = at as i64 + step;
        if target < 0 {
            return false;
        }
        match groups.get(target as usize) {
            Some(next) => {
                self.active_group = next.id.clone();
                true
            }
            None => false,
        }
    }

    /// The group tabs' labels.
    pub fn tab_labels(&self, i18n: Option<&dyn Translator>) -> Vec<(String, String)> {
        self.definition
            .groups
            .iter()
            .map(|g| (g.id.clone(), text_of(i18n, &g.label, &[])))
            .collect()
    }

    /// Lay the list out for the query and the armed tool.
    pub fn list(&self, active_tool: Option<&str>, i18n: Option<&dyn Translator>) -> DrawerList {
        let locale = i18n.map(|t| t.locale());
        let lower = |s: &str| lower_for_locale(s, locale);
        let row = |tool: &ToolSource| ToolRow {
            type_key: tool.type_key.clone(),
            label: text_of(i18n, &tool.label, &[]),
            icon: tool.icon.clone(),
            active: active_tool == Some(tool.type_key.as_str()),
        };
        let query = lower(self.query.trim());
        if !query.is_empty() {
            // Search flattens every group; group labels become the section headers.
            let mut sections = Vec::new();
            for group in &self.definition.groups {
                // A tool matches by its name as shown, or by its English name.
                let hits: Vec<ToolRow> = group
                    .tools
                    .iter()
                    .filter(|t| {
                        lower(&text_of(i18n, &t.label, &[])).contains(&query)
                            || source_text(&t.label).to_lowercase().contains(&query)
                    })
                    .map(row)
                    .collect();
                if !hits.is_empty() {
                    sections.push(ListSection {
                        title: Some(text_of(i18n, &group.label, &[])),
                        rows: hits,
                    });
                }
            }
            if sections.is_empty() {
                return DrawerList::Empty(text_of(i18n, &no_match_message(), &[]));
            }
            return DrawerList::Sections(sections);
        }
        let Some(group) = self
            .definition
            .groups
            .iter()
            .find(|g| g.id == self.active_group)
        else {
            return DrawerList::Sections(Vec::new());
        };
        match group.sections.as_deref() {
            Some(sections) if !sections.is_empty() => DrawerList::Sections(
                sections
                    .iter()
                    .map(|s| ListSection {
                        title: Some(text_of(i18n, &s.label, &[])),
                        rows: s.tools.iter().map(row).collect(),
                    })
                    .collect(),
            ),
            _ => DrawerList::Sections(vec![ListSection {
                title: None,
                rows: group.tools.iter().map(row).collect(),
            }]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawings::toolbar::ToolSectionSource;

    fn tool(key: &str, label: &'static str) -> ToolSource {
        ToolSource {
            type_key: key.into(),
            label: label.into(),
            icon: format!("<svg id=\"{key}\"/>"),
        }
    }

    fn toolbar() -> ToolbarSource {
        ToolbarSource {
            groups: vec![
                ToolGroupSource {
                    id: "lines".into(),
                    label: "Lines".into(),
                    tools: vec![
                        tool("trendline", "Trend Line"),
                        tool("hline", "Horizontal Line"),
                    ],
                    sections: None,
                },
                ToolGroupSource {
                    id: "fib".into(),
                    label: "Fib".into(),
                    tools: vec![tool("fibretracement", "Fib Retracement")],
                    sections: Some(vec![ToolSectionSource {
                        label: "Fibonacci".into(),
                        tools: vec![tool("fibretracement", "Fib Retracement")],
                    }]),
                },
                ToolGroupSource {
                    id: "text".into(),
                    label: "Text".into(),
                    tools: vec![tool("note", "Note")],
                    sections: None,
                },
            ],
        }
    }

    fn model() -> DrawerModel {
        let mut m = DrawerModel::new();
        m.open(toolbar());
        m
    }

    fn keys(list: &DrawerList) -> Vec<String> {
        match list {
            DrawerList::Sections(s) => s
                .iter()
                .flat_map(|s| s.rows.iter().map(|r| r.type_key.clone()))
                .collect(),
            DrawerList::Empty(_) => Vec::new(),
        }
    }

    #[test]
    fn opening_selects_the_first_group_and_keeps_a_still_valid_one() {
        let mut m = model();
        assert_eq!(m.active_group(), "lines");
        m.set_group("text");
        m.open(toolbar());
        assert_eq!(m.active_group(), "text");
        let mut shrunk = toolbar();
        shrunk.groups.truncate(1);
        m.open(shrunk);
        assert_eq!(m.active_group(), "lines");
        let mut empty = DrawerModel::new();
        empty.open(ToolbarSource::default());
        assert_eq!(empty.active_group(), "");
        assert_eq!(empty.list(None, None), DrawerList::Sections(Vec::new()));
    }

    #[test]
    fn the_active_group_lists_its_tools_flat_and_marks_the_armed_one() {
        let list = model().list(Some("hline"), None);
        let DrawerList::Sections(sections) = list else {
            panic!("sections")
        };
        assert_eq!(sections.len(), 1);
        assert!(sections[0].title.is_none());
        assert_eq!(
            sections[0]
                .rows
                .iter()
                .map(|r| (r.type_key.as_str(), r.active))
                .collect::<Vec<_>>(),
            [("trendline", false), ("hline", true)]
        );
    }

    #[test]
    fn a_group_with_sections_lists_them_under_their_headings() {
        let mut m = model();
        m.set_group("fib");
        let DrawerList::Sections(sections) = m.list(None, None) else {
            panic!("sections")
        };
        assert_eq!(sections[0].title.as_deref(), Some("Fibonacci"));
        assert_eq!(sections[0].rows[0].label, "Fib Retracement");
    }

    #[test]
    fn search_flattens_every_group_under_its_label_and_matches_case_insensitively() {
        let mut m = model();
        m.set_query("  LINE ");
        let DrawerList::Sections(sections) = m.list(None, None) else {
            panic!("sections")
        };
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].title.as_deref(), Some("Lines"));
        assert_eq!(keys(&m.list(None, None)), ["trendline", "hline"]);
        m.set_query("fib");
        assert_eq!(keys(&m.list(None, None)), ["fibretracement"]);
    }

    #[test]
    fn nothing_matching_shows_the_empty_message() {
        let mut m = model();
        m.set_query("zzz");
        assert_eq!(
            m.list(None, None),
            DrawerList::Empty("No tools match.".into())
        );
    }

    #[test]
    fn picking_a_tab_leaves_search_mode() {
        let mut m = model();
        m.set_query("fib");
        m.set_group("lines");
        assert_eq!(m.query(), "");
        assert_eq!(keys(&m.list(None, None)), ["trendline", "hline"]);
    }

    #[test]
    fn swiping_pages_through_the_tabs_and_is_inert_in_search_mode_or_past_the_ends() {
        let mut m = model();
        assert!(!m.step_group(-1), "before the first tab");
        assert!(m.step_group(1));
        assert_eq!(m.active_group(), "fib");
        assert!(m.step_group(1));
        assert!(!m.step_group(1), "past the last tab");
        assert_eq!(m.active_group(), "text");
        m.set_query("x");
        assert!(!m.step_group(-1), "search mode shows no tabs");
        assert_eq!(m.active_group(), "text");
    }

    #[test]
    fn tabs_and_stars_are_labeled() {
        assert_eq!(
            model().tab_labels(None),
            [
                ("lines".into(), "Lines".into()),
                ("fib".into(), "Fib".into()),
                ("text".into(), "Text".into())
            ]
        );
        assert_eq!(favorite_label("Trend Line", None), "Favorite Trend Line");
        assert_eq!(text_of(None, &title_message(), &[]), "Drawings");
        assert_eq!(
            text_of(None, &search_placeholder(), &[]),
            "Search tools\u{2026}"
        );
    }
}
