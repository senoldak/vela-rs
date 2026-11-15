//! More drawer: the mobile three-dots bottom sheet: undo/redo/screenshot actions, the chart
//! type, layout, panels, alerts and the contributed actions, with sub-views for the chart
//! type, the layout and the alerts.
//!
//! The pure half of the original `MoreDrawer`: the sub-view navigation ([`MoreView`]), the
//! main screen's content ([`main_view`]: action buttons and list rows with their values),
//! the chart-type rows and the layout sub-view's state. Left for the browser crate: the
//! drawer sheet, the element rows and CSS, hiding the drawer after an action, the layout
//! grid canvas ([`grid_cells`](super::layout_picker::grid_cells) and
//! [`paint_layout_grid`](super::layout_picker::paint_layout_grid)) and the alert rows.

use super::layout_picker::{LayoutPickerShape, PickerRow, layout_grid_label};
use crate::i18n::{MessageArg, MessageLike, Translator, msg, text_of};

pub fn chart_type_label() -> MessageLike {
    msg("widget.moreDrawer.chartType", "Chart type")
}
pub fn layout_label() -> MessageLike {
    msg("widget.moreDrawer.layout", "Layout")
}
pub fn alerts_label() -> MessageLike {
    msg("widget.moreDrawer.alerts", "Alerts")
}
pub fn undo_label() -> MessageLike {
    msg("widget.moreDrawer.undo", "Undo")
}
pub fn redo_label() -> MessageLike {
    msg("widget.moreDrawer.redo", "Redo")
}
pub fn screenshot_label() -> MessageLike {
    msg("widget.moreDrawer.screenshot", "Screenshot")
}
pub fn sync_label() -> MessageLike {
    msg("widget.moreDrawer.sync", "Sync")
}
pub fn no_alerts_label() -> MessageLike {
    msg("widget.moreDrawer.noAlerts", "No alerts yet.")
}

/// An alert row: `{title}: {message}`.
pub fn alert_text(title: &str, message: &str, i18n: Option<&dyn Translator>) -> String {
    text_of(
        i18n,
        &msg("widget.moreDrawer.alert", "{title}: {message}"),
        &[
            ("title", MessageArg::from(title.to_string())),
            ("message", MessageArg::from(message.to_string())),
        ],
    )
}

/// The sheet's screens. Everything but `Main` has a back button reading its own title.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MoreView {
    #[default]
    Main,
    Style,
    Layout,
    Alerts,
}

impl MoreView {
    /// The back button's text on a sub-view; `None` on the main screen.
    pub fn back_label(self) -> Option<MessageLike> {
        match self {
            MoreView::Main => None,
            MoreView::Style => Some(chart_type_label()),
            MoreView::Layout => Some(layout_label()),
            MoreView::Alerts => Some(alerts_label()),
        }
    }
}

/// An undo/redo/screenshot button of the actions strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionButton {
    pub icon: &'static str,
    pub label: String,
    pub enabled: bool,
}

/// A contributed action's row (a label and an optional icon).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionRow {
    pub label: String,
    pub icon: Option<String>,
}

/// One docked panel the sheet lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelRow {
    pub id: String,
    pub title: String,
    pub icon: String,
}

/// A chart style the sheet lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleChoice {
    pub id: String,
    pub label: String,
    pub icon: Option<String>,
    /// Listed greyed, with `title` as its tooltip, and cannot be picked.
    pub unavailable: bool,
    pub title: Option<String>,
}

/// What the shell feeds the main screen (everything read live at open). An omitted
/// handler is `false` / `None`: that surface is absent (the shell's topbar composition
/// removed it).
#[derive(Clone, Debug, Default)]
pub struct MainInputs {
    pub has_undo: bool,
    pub has_redo: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub has_screenshot: bool,
    pub styles: Vec<StyleChoice>,
    pub current_style: String,
    pub panels: Vec<PanelRow>,
    /// `Some(count)` shows the Alerts row; `None` (composition removed `alerts`) hides it.
    pub alert_count: Option<usize>,
    /// Contributed topbar actions, projected as plain rows at the end of the list.
    pub actions: Vec<ActionRow>,
    /// Contributed PRIMARY actions (left cluster, menu-placed): rows right after Layout.
    pub primary_actions: Vec<ActionRow>,
    /// The layout row, hidden when `None` (single-chart shells).
    pub layout: Option<LayoutInputs>,
}

/// The layout row's inputs.
#[derive(Clone, Debug, Default)]
pub struct LayoutInputs {
    /// Current layout's canvas shape (`None`: a preset the canvas cannot express).
    pub shape: Option<LayoutPickerShape>,
    /// Registered layouts NOT expressible on the canvas.
    pub presets: Vec<PickerRow>,
    /// The current layout's 16px glyph (`<svg>` markup) for the Layout row.
    pub glyph: Option<String>,
}

/// A list row of the main screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MainRow {
    ChartType {
        value: Option<String>,
    },
    Layout {
        value: Option<String>,
        glyph: Option<String>,
    },
    /// A contributed primary action (right after Layout).
    PrimaryAction(ActionRow),
    Panel(PanelRow),
    Alerts {
        value: Option<String>,
    },
    /// A contributed action (the end of the list).
    Action(ActionRow),
}

/// The main screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MainView {
    /// The strip of undo/redo/screenshot buttons (empty: no strip).
    pub actions: Vec<ActionButton>,
    pub rows: Vec<MainRow>,
}

/// Lay the main screen out.
pub fn main_view(inputs: &MainInputs, i18n: Option<&dyn Translator>) -> MainView {
    let t = |m: MessageLike| text_of(i18n, &m, &[]);
    let mut actions = Vec::new();
    if inputs.has_undo {
        actions.push(ActionButton {
            icon: "undo",
            label: t(undo_label()),
            enabled: inputs.can_undo,
        });
    }
    if inputs.has_redo {
        actions.push(ActionButton {
            icon: "redo",
            label: t(redo_label()),
            enabled: inputs.can_redo,
        });
    }
    if inputs.has_screenshot {
        actions.push(ActionButton {
            icon: "camera",
            label: t(screenshot_label()),
            enabled: true,
        });
    }
    let mut rows = vec![MainRow::ChartType {
        value: inputs
            .styles
            .iter()
            .find(|s| s.id == inputs.current_style)
            .map(|s| s.label.clone()),
    }];
    if let Some(layout) = &inputs.layout {
        let value = match layout.shape {
            Some(shape) => Some(layout_grid_label(shape.cols, shape.rows, i18n)),
            None => layout
                .presets
                .iter()
                .find(|p| p.checked)
                .map(|p| p.label.clone()),
        };
        rows.push(MainRow::Layout {
            value,
            glyph: layout.glyph.clone(),
        });
    }
    rows.extend(
        inputs
            .primary_actions
            .iter()
            .cloned()
            .map(MainRow::PrimaryAction),
    );
    rows.extend(inputs.panels.iter().cloned().map(MainRow::Panel));
    if let Some(count) = inputs.alert_count {
        rows.push(MainRow::Alerts {
            value: (count > 0).then(|| count.to_string()),
        });
    }
    rows.extend(inputs.actions.iter().cloned().map(MainRow::Action));
    MainView { actions, rows }
}

/// A style row's click: the id to apply, or `None` for an unavailable (greyed) style.
pub fn pick_style(style: &StyleChoice) -> Option<&str> {
    (!style.unavailable).then_some(style.id.as_str())
}

/// Whether the style row is the current one.
pub fn style_checked(style: &StyleChoice, current: &str) -> bool {
    style.id == current
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(id: &str, label: &str, unavailable: bool) -> StyleChoice {
        StyleChoice {
            id: id.into(),
            label: label.into(),
            icon: None,
            unavailable,
            title: unavailable.then(|| "Not available".to_string()),
        }
    }

    fn action(label: &str) -> ActionRow {
        ActionRow {
            label: label.into(),
            icon: None,
        }
    }

    fn base() -> MainInputs {
        MainInputs {
            styles: vec![
                style("candles", "Candles", false),
                style("bars", "Bars", true),
            ],
            current_style: "candles".into(),
            ..MainInputs::default()
        }
    }

    #[test]
    fn the_bare_main_screen_has_only_the_chart_type_row() {
        let view = main_view(&base(), None);
        assert!(view.actions.is_empty());
        assert_eq!(
            view.rows,
            [MainRow::ChartType {
                value: Some("Candles".into())
            }]
        );
    }

    #[test]
    fn the_actions_strip_follows_the_composition_and_the_history_state() {
        let inputs = MainInputs {
            has_undo: true,
            has_redo: true,
            can_undo: true,
            can_redo: false,
            has_screenshot: true,
            ..base()
        };
        let view = main_view(&inputs, None);
        assert_eq!(
            view.actions
                .iter()
                .map(|a| (a.icon, a.label.as_str(), a.enabled))
                .collect::<Vec<_>>(),
            [
                ("undo", "Undo", true),
                ("redo", "Redo", false),
                ("camera", "Screenshot", true)
            ]
        );
        // Composition removed undo-redo: no buttons for them.
        let no_history = MainInputs {
            has_screenshot: true,
            ..base()
        };
        assert_eq!(main_view(&no_history, None).actions.len(), 1);
    }

    #[test]
    fn the_rows_follow_the_original_order() {
        let inputs = MainInputs {
            layout: Some(LayoutInputs {
                shape: Some(LayoutPickerShape { rows: 2, cols: 3 }),
                presets: vec![],
                glyph: Some("<svg/>".into()),
            }),
            primary_actions: vec![action("Primary")],
            panels: vec![PanelRow {
                id: "objects".into(),
                title: "Objects".into(),
                icon: "tree".into(),
            }],
            alert_count: Some(2),
            actions: vec![action("Custom")],
            ..base()
        };
        let rows = main_view(&inputs, None).rows;
        assert_eq!(rows.len(), 6);
        assert!(matches!(rows[0], MainRow::ChartType { .. }));
        assert_eq!(
            rows[1],
            MainRow::Layout {
                value: Some("3 \u{d7} 2".into()),
                glyph: Some("<svg/>".into())
            }
        );
        assert!(matches!(&rows[2], MainRow::PrimaryAction(a) if a.label == "Primary"));
        assert!(matches!(&rows[3], MainRow::Panel(p) if p.id == "objects"));
        assert_eq!(
            rows[4],
            MainRow::Alerts {
                value: Some("2".into())
            }
        );
        assert!(matches!(&rows[5], MainRow::Action(a) if a.label == "Custom"));
    }

    #[test]
    fn the_layout_value_falls_back_to_the_checked_preset_and_alerts_show_a_count_only_when_nonzero()
    {
        let inputs = MainInputs {
            layout: Some(LayoutInputs {
                shape: None,
                presets: vec![
                    PickerRow {
                        id: "a".into(),
                        label: "Wide".into(),
                        checked: false,
                    },
                    PickerRow {
                        id: "b".into(),
                        label: "Stack".into(),
                        checked: true,
                    },
                ],
                glyph: None,
            }),
            alert_count: Some(0),
            ..base()
        };
        let rows = main_view(&inputs, None).rows;
        assert_eq!(
            rows[1],
            MainRow::Layout {
                value: Some("Stack".into()),
                glyph: None
            }
        );
        assert_eq!(rows[2], MainRow::Alerts { value: None });
        // A preset-less, shape-less layout shows no value at all.
        let bare = MainInputs {
            layout: Some(LayoutInputs::default()),
            ..base()
        };
        assert_eq!(
            main_view(&bare, None).rows[1],
            MainRow::Layout {
                value: None,
                glyph: None
            }
        );
    }

    #[test]
    fn an_unknown_current_style_shows_no_value_and_unavailable_styles_cannot_be_picked() {
        let inputs = MainInputs {
            current_style: "nope".into(),
            ..base()
        };
        assert_eq!(
            main_view(&inputs, None).rows[0],
            MainRow::ChartType { value: None }
        );
        let styles = base().styles;
        assert_eq!(pick_style(&styles[0]), Some("candles"));
        assert_eq!(pick_style(&styles[1]), None);
        assert!(style_checked(&styles[0], "candles") && !style_checked(&styles[1], "candles"));
    }

    #[test]
    fn sub_views_have_a_back_button_named_after_themselves() {
        assert!(MoreView::Main.back_label().is_none());
        let name = |v: MoreView| v.back_label().map(|m| text_of(None, &m, &[]));
        assert_eq!(name(MoreView::Style).as_deref(), Some("Chart type"));
        assert_eq!(name(MoreView::Layout).as_deref(), Some("Layout"));
        assert_eq!(name(MoreView::Alerts).as_deref(), Some("Alerts"));
        assert_eq!(MoreView::default(), MoreView::Main);
    }

    #[test]
    fn alerts_read_as_title_and_message() {
        assert_eq!(alert_text("BTC", "crossed 70k", None), "BTC: crossed 70k");
        assert_eq!(text_of(None, &no_alerts_label(), &[]), "No alerts yet.");
        assert_eq!(text_of(None, &sync_label(), &[]), "Sync");
    }
}
