//! Status line MODEL: the segment visibility the parts config implies, plus the item
//! descriptors for the status line's right-click menu. Pure functions over plain state,
//! so both can be tested without a DOM; the `Statusline` component (browser crate)
//! projects them onto its elements.

use crate::i18n::{MessageLike, Translator, msg, text_of};
use crate::ui::MenuItemDescriptor;

/// The status line's toggleable segments: the settings dialog's Status line tab and
/// the status line's own right-click menu drive these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatuslinePart {
    Logo,
    Name,
    Market,
    Ohlc,
    Change,
}

impl StatuslinePart {
    pub const ALL: [StatuslinePart; 5] = [
        StatuslinePart::Logo,
        StatuslinePart::Name,
        StatuslinePart::Market,
        StatuslinePart::Ohlc,
        StatuslinePart::Change,
    ];

    /// The part's key as the config spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            StatuslinePart::Logo => "logo",
            StatuslinePart::Name => "name",
            StatuslinePart::Market => "market",
            StatuslinePart::Ohlc => "ohlc",
            StatuslinePart::Change => "change",
        }
    }

    /// The part's name: the status line's right-click menu and the settings rows that
    /// toggle it.
    pub fn label(self) -> MessageLike {
        match self {
            StatuslinePart::Logo => msg("widget.statusline.part.logo", "Symbol logo"),
            StatuslinePart::Name => msg("widget.statusline.part.name", "Symbol name"),
            StatuslinePart::Market => msg("widget.statusline.part.market", "Market status"),
            StatuslinePart::Ohlc => msg("widget.statusline.part.ohlc", "OHLC values"),
            StatuslinePart::Change => msg("widget.statusline.part.change", "Bar change values"),
        }
    }
}

/// Which parts show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatuslineParts {
    pub logo: bool,
    pub name: bool,
    pub market: bool,
    pub ohlc: bool,
    pub change: bool,
}

impl StatuslineParts {
    pub fn get(&self, part: StatuslinePart) -> bool {
        match part {
            StatuslinePart::Logo => self.logo,
            StatuslinePart::Name => self.name,
            StatuslinePart::Market => self.market,
            StatuslinePart::Ohlc => self.ohlc,
            StatuslinePart::Change => self.change,
        }
    }
}

impl Default for StatuslineParts {
    fn default() -> Self {
        Self {
            logo: true,
            name: true,
            market: true,
            ohlc: true,
            change: true,
        }
    }
}

/// Per-segment visibility the parts config implies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentVisibility {
    pub avatar: bool,
    pub symbol: bool,
    pub meta: bool,
    pub market: bool,
    pub ohlc: bool,
    pub change: bool,
    pub eye: bool,
}

/// Per-segment visibility the parts config implies. The venue/timeframe meta carries no
/// part of its own: it reads as an extension of the symbol name ("BTCUSDT · BINANCE ·
/// 1h" is one identity readout), so it follows `name`. A hidden chart (the price series
/// eye) drops the whole value readout (OHLC and bar change, values of a series that
/// isn't painted) without touching the parts themselves, so showing the chart brings
/// them straight back; the eye affordance that re-shows the chart is out only then.
pub fn segment_visibility(parts: &StatuslineParts, chart_hidden: bool) -> SegmentVisibility {
    SegmentVisibility {
        avatar: parts.logo,
        symbol: parts.name,
        meta: parts.name,
        market: parts.market,
        ohlc: parts.ohlc && !chart_hidden,
        change: parts.change && !chart_hidden,
        eye: chart_hidden,
    }
}

/// How much of the value readout a width step keeps: every value the price style reads
/// out (`Full`), just the close and the bar change (`Compact`), or the close and the
/// percent delta alone (`Minimal`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatuslineLevel {
    Full,
    Compact,
    Minimal,
}

/// One rung of the width ladder: whether the value readout sits on its own row under
/// the symbol line, and how much of it is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatuslineLayout {
    pub stacked: bool,
    pub level: StatuslineLevel,
}

/// The width ladder the status line walks down until its row fits, widest first: the
/// values first move under the symbol line, then shed O/H/L, then the absolute change.
/// Every status line (single chart, phone, multi-chart cell) walks the same rungs; in
/// fit mode whatever still overflows after the last one is hidden segment by segment.
pub const STATUSLINE_LADDER: [StatuslineLayout; 4] = [
    StatuslineLayout {
        stacked: false,
        level: StatuslineLevel::Full,
    },
    StatuslineLayout {
        stacked: true,
        level: StatuslineLevel::Full,
    },
    StatuslineLayout {
        stacked: true,
        level: StatuslineLevel::Compact,
    },
    StatuslineLayout {
        stacked: true,
        level: StatuslineLevel::Minimal,
    },
];

/// The price style's readout shape: a bar (`Ohlc`) or a single line value (`Value`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadoutShape {
    Ohlc,
    Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadoutCellKey {
    Open,
    High,
    Low,
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadoutCell {
    pub key: ReadoutCellKey,
    pub label: String,
}

/// Which of the O/H/L/C cells a level keeps, given the price style's readout shape.
/// One-line styles (`Value`) only ever plot the close, so their readout is the single
/// unlabeled value at every level; bar-shaped styles drop O/H/L below `Full` and keep
/// the labeled close, whose `C` the `Minimal` level drops too.
pub fn readout_cells(
    level: StatuslineLevel,
    readout: ReadoutShape,
    i18n: Option<&dyn Translator>,
) -> Vec<ReadoutCell> {
    let cell = |key, label: String| ReadoutCell { key, label };
    if readout == ReadoutShape::Value {
        return vec![cell(ReadoutCellKey::Close, String::new())];
    }
    let label = |m: MessageLike| text_of(i18n, &m, &[]);
    let open = || label(msg("widget.statusline.openShort", "O"));
    let high = || label(msg("widget.statusline.highShort", "H"));
    let low = || label(msg("widget.statusline.lowShort", "L"));
    let close = || label(msg("widget.statusline.closeShort", "C"));
    match level {
        StatuslineLevel::Full => vec![
            cell(ReadoutCellKey::Open, open()),
            cell(ReadoutCellKey::High, high()),
            cell(ReadoutCellKey::Low, low()),
            cell(ReadoutCellKey::Close, close()),
        ],
        StatuslineLevel::Compact => vec![cell(ReadoutCellKey::Close, close())],
        StatuslineLevel::Minimal => vec![cell(ReadoutCellKey::Close, String::new())],
    }
}

/// Right-click menu rows: one checkable toggle per part (same labels as the settings
/// dialog's Status line tab), then the chart itself: the same hide/show of the price
/// series the object tree's eye drives.
pub fn statusline_menu_items(
    parts: &StatuslineParts,
    chart_visible: bool,
    i18n: Option<&dyn Translator>,
) -> Vec<MenuItemDescriptor> {
    let t = |m: MessageLike| text_of(i18n, &m, &[]);
    let mut items: Vec<MenuItemDescriptor> = StatuslinePart::ALL
        .iter()
        .map(|part| MenuItemDescriptor {
            checked: Some(parts.get(*part)),
            ..MenuItemDescriptor::new(format!("part:{}", part.as_str()), t(part.label()))
        })
        .collect();
    items.push(MenuItemDescriptor {
        separator_before: true,
        ..MenuItemDescriptor::new("chart", t(show_chart_label(chart_visible)))
    });
    items
}

fn show_chart_label(chart_visible: bool) -> MessageLike {
    if chart_visible {
        msg("widget.statusline.hideChart", "Hide chart")
    } else {
        msg("widget.statusline.showChart", "Show chart")
    }
}

/// The show-chart eye's label (the same text as the menu row).
pub fn show_chart_message() -> MessageLike {
    show_chart_label(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_ON: StatuslineParts = StatuslineParts {
        logo: true,
        name: true,
        market: true,
        ohlc: true,
        change: true,
    };

    #[test]
    fn everything_on_shows_every_segment_and_keeps_the_eye_stowed() {
        assert_eq!(
            segment_visibility(&ALL_ON, false),
            SegmentVisibility {
                avatar: true,
                symbol: true,
                meta: true,
                market: true,
                ohlc: true,
                change: true,
                eye: false
            }
        );
    }

    #[test]
    fn logo_gates_only_the_avatar() {
        let seg = segment_visibility(
            &StatuslineParts {
                logo: false,
                ..ALL_ON
            },
            false,
        );
        assert!(!seg.avatar && seg.symbol && seg.meta);
    }

    #[test]
    fn name_hides_the_venue_meta_along_with_the_symbol_text() {
        let seg = segment_visibility(
            &StatuslineParts {
                name: false,
                ..ALL_ON
            },
            false,
        );
        assert!(!seg.symbol && !seg.meta);
        assert!(seg.avatar); // the logo keeps its own toggle
    }

    #[test]
    fn the_value_parts_stay_independent() {
        let seg = segment_visibility(
            &StatuslineParts {
                ohlc: false,
                change: false,
                market: false,
                ..ALL_ON
            },
            false,
        );
        assert!(!seg.ohlc && !seg.change && !seg.market && seg.symbol);
    }

    #[test]
    fn a_hidden_chart_drops_the_value_readout_and_puts_the_eye_out() {
        let seg = segment_visibility(&ALL_ON, true);
        assert!(!seg.ohlc && !seg.change && seg.eye);
        assert!(seg.avatar && seg.symbol && seg.market);
    }

    #[test]
    fn showing_the_chart_again_restores_the_value_parts_as_configured() {
        let seg = segment_visibility(&ALL_ON, false);
        assert!(seg.ohlc && seg.change && !seg.eye);
        assert!(
            !segment_visibility(
                &StatuslineParts {
                    ohlc: false,
                    ..ALL_ON
                },
                false
            )
            .ohlc
        );
    }

    fn cells(level: StatuslineLevel, shape: ReadoutShape) -> Vec<(ReadoutCellKey, String)> {
        readout_cells(level, shape, None)
            .into_iter()
            .map(|c| (c.key, c.label))
            .collect()
    }

    #[test]
    fn bar_shaped_styles_read_out_all_four_values_at_the_full_level() {
        let labels: Vec<String> = readout_cells(StatuslineLevel::Full, ReadoutShape::Ohlc, None)
            .into_iter()
            .map(|c| c.label)
            .collect();
        assert_eq!(labels, ["O", "H", "L", "C"]);
    }

    #[test]
    fn compact_keeps_the_labeled_close_and_minimal_drops_the_label_too() {
        assert_eq!(
            cells(StatuslineLevel::Compact, ShapeOhlc),
            [(ReadoutCellKey::Close, "C".to_string())]
        );
        assert_eq!(
            cells(StatuslineLevel::Minimal, ShapeOhlc),
            [(ReadoutCellKey::Close, String::new())]
        );
    }

    use ReadoutShape::Ohlc as ShapeOhlc;

    #[test]
    fn one_line_styles_only_ever_read_out_the_unlabeled_close() {
        for level in [
            StatuslineLevel::Full,
            StatuslineLevel::Compact,
            StatuslineLevel::Minimal,
        ] {
            assert_eq!(
                cells(level, ReadoutShape::Value),
                [(ReadoutCellKey::Close, String::new())]
            );
        }
    }

    #[test]
    fn the_ladder_walks_stacked_then_shed() {
        assert_eq!(STATUSLINE_LADDER.len(), 4);
        assert!(!STATUSLINE_LADDER[0].stacked);
        assert!(STATUSLINE_LADDER[1..].iter().all(|r| r.stacked));
        assert_eq!(STATUSLINE_LADDER[3].level, StatuslineLevel::Minimal);
    }

    #[test]
    fn the_menu_offers_one_checkable_toggle_per_part_after_the_current_state() {
        let items = statusline_menu_items(
            &StatuslineParts {
                logo: false,
                ohlc: false,
                ..ALL_ON
            },
            true,
            None,
        );
        let checked = |id: &str| items.iter().find(|i| i.id == id).and_then(|i| i.checked);
        assert_eq!(checked("part:logo"), Some(false));
        assert_eq!(checked("part:name"), Some(true));
        assert_eq!(checked("part:market"), Some(true));
        assert_eq!(checked("part:ohlc"), Some(false));
        assert_eq!(checked("part:change"), Some(true));
    }

    #[test]
    fn the_menu_ends_with_the_chart_toggle_worded_after_the_current_visibility() {
        let shown = statusline_menu_items(&ALL_ON, true, None);
        let last = shown.last().expect("chart row");
        assert_eq!(
            (last.id.as_str(), last.label.as_str()),
            ("chart", "Hide chart")
        );
        assert!(last.separator_before);
        let hidden = statusline_menu_items(&ALL_ON, false, None);
        assert_eq!(hidden.last().map(|i| i.label.as_str()), Some("Show chart"));
        assert_eq!(shown.len(), 6);
    }

    #[test]
    fn part_labels_read_as_the_menu_and_settings_name_them() {
        let names: Vec<String> = StatuslinePart::ALL
            .iter()
            .map(|p| text_of(None, &p.label(), &[]))
            .collect();
        assert_eq!(
            names,
            [
                "Symbol logo",
                "Symbol name",
                "Market status",
                "OHLC values",
                "Bar change values"
            ]
        );
        assert_eq!(text_of(None, &show_chart_message(), &[]), "Show chart");
    }
}
