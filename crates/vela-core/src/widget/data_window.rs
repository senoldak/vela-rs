//! Data window: a docked side panel, the object tree's sibling, showing the bar under the
//! crosshair: its date and time, its OHLCV, then every indicator's value there, one section
//! per indicator. The readout arrives pre-formatted from the renderer
//! (`renderer.data_window_readout`), so the panel only lays it out.
//!
//! This is the pure half of the original `DataWindow`: [`data_window_sections`] (the layout
//! of a readout into sections), the panel title and the events that refresh it. Left for the
//! browser crate: the `SidePanel` element, the section/row elements and their CSS, and the
//! chart event and crosshair subscriptions (hover updates ride the crosshair seam; resting
//! updates come from [`REFRESH_EVENTS`]). It refreshes only while the panel is visible and a
//! chart is bound, and shows [`no_data_message`] for a readout with no sections, or
//! [`no_readout_message`] when the renderer provides none.

use crate::events::types::ChartEventName;
use crate::i18n::{MessageLike, Translator, msg, text_of};
use crate::ports::chart_renderer::DataWindowReadout;

/// The panel's title (also its toggle's tooltip).
pub fn data_window_title() -> MessageLike {
    msg("widget.dataWindow.title", "Data window")
}

/// Shown when the readout holds no section.
pub fn no_data_message() -> MessageLike {
    msg("widget.dataWindow.noData", "No data")
}

/// Shown when the renderer provides no readout at all.
pub fn no_readout_message() -> MessageLike {
    msg(
        "widget.dataWindow.noReadout",
        "This renderer provides no data readout.",
    )
}

/// Events that change what the readout holds: indicator values, plots, or the live bar.
pub const REFRESH_EVENTS: [ChartEventName; 9] = [
    ChartEventName::Bar,
    ChartEventName::ContextChanged,
    ChartEventName::IndicatorAdded,
    ChartEventName::IndicatorRemoved,
    ChartEventName::IndicatorVisibility,
    ChartEventName::IndicatorMoved,
    ChartEventName::PaneChanged,
    ChartEventName::MarketChanged,
    ChartEventName::HistoryComplete,
];

/// One readout line. `color` empty: the value inherits the panel's text color.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataWindowLine {
    pub label: String,
    pub value: String,
    pub color: String,
}

/// A titled block of readout lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataWindowSection {
    pub title: String,
    pub lines: Vec<DataWindowLine>,
}

const EM_DASH: &str = "\u{2014}";

fn line(label: String, value: &str, color: &str) -> DataWindowLine {
    DataWindowLine {
        label,
        value: value.to_string(),
        color: color.to_string(),
    }
}

/// Lay a readout out as sections: the bar's Time, its Price block (tinted with the bar's
/// direction), then one section per indicator with a line per plot in the plot's own color.
/// Empty when the chart holds no bar to read.
pub fn data_window_sections(
    readout: &DataWindowReadout,
    i18n: Option<&dyn Translator>,
) -> Vec<DataWindowSection> {
    let t = |m: MessageLike| text_of(i18n, &m, &[]);
    let or_dash = |s: &str| {
        if s.is_empty() {
            EM_DASH.to_string()
        } else {
            s.to_string()
        }
    };
    let mut sections = Vec::new();
    if !readout.date.is_empty() || !readout.time.is_empty() {
        sections.push(DataWindowSection {
            title: t(msg("widget.dataWindow.timeSection", "Time")),
            lines: vec![
                line(
                    t(msg("widget.dataWindow.date", "Date")),
                    &or_dash(&readout.date),
                    "",
                ),
                line(
                    t(msg("widget.dataWindow.time", "Time")),
                    &or_dash(&readout.time),
                    "",
                ),
            ],
        });
    }
    if let Some(ohlc) = &readout.ohlc {
        let color = if ohlc.up {
            "var(--vela-up)"
        } else {
            "var(--vela-down)"
        };
        let mut lines = vec![
            line(t(msg("widget.dataWindow.open", "Open")), &ohlc.o, color),
            line(t(msg("widget.dataWindow.high", "High")), &ohlc.h, color),
            line(t(msg("widget.dataWindow.low", "Low")), &ohlc.l, color),
            line(t(msg("widget.dataWindow.close", "Close")), &ohlc.c, color),
        ];
        if let Some(vol) = &ohlc.vol {
            lines.push(line(
                t(msg("widget.dataWindow.volume", "Volume")),
                vol,
                color,
            ));
        }
        sections.push(DataWindowSection {
            title: t(msg("widget.dataWindow.priceSection", "Price")),
            lines,
        });
    }
    for group in &readout.groups {
        sections.push(DataWindowSection {
            title: group.name.clone(),
            lines: group
                .rows
                .iter()
                .map(|r| line(r.label.clone(), &r.value, &r.color))
                .collect(),
        });
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::chart_renderer::{DataWindowGroup, DataWindowOhlc, DataWindowRow};

    fn empty() -> DataWindowReadout {
        DataWindowReadout {
            date: String::new(),
            time: String::new(),
            ohlc: None,
            groups: Vec::new(),
        }
    }

    fn row(label: &str, value: &str, color: &str) -> DataWindowRow {
        DataWindowRow {
            label: label.into(),
            value: value.into(),
            color: color.into(),
        }
    }

    fn l(label: &str, value: &str, color: &str) -> DataWindowLine {
        line(label.to_string(), value, color)
    }

    #[test]
    fn has_no_sections_when_the_chart_holds_no_bar() {
        assert!(data_window_sections(&empty(), None).is_empty());
    }

    #[test]
    fn lays_the_readout_out_as_time_price_then_one_section_per_indicator() {
        let readout = DataWindowReadout {
            date: "2023-11-14".into(),
            time: "22:13".into(),
            ohlc: Some(DataWindowOhlc {
                o: "104.00".into(),
                h: "107.00".into(),
                l: "103.00".into(),
                c: "105.00".into(),
                vol: Some("1.23K".into()),
                up: true,
            }),
            groups: vec![
                DataWindowGroup {
                    name: "Moving Average".into(),
                    rows: vec![row("MA", "102.50", "#2962ff")],
                },
                DataWindowGroup {
                    name: "RSI".into(),
                    rows: vec![
                        row("RSI", "61.20", "#7e57c2"),
                        row("Signal", "58.90", "#ff9800"),
                    ],
                },
            ],
        };
        let sections = data_window_sections(&readout, None);
        let titles: Vec<&str> = sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Time", "Price", "Moving Average", "RSI"]);
        assert_eq!(
            sections[0].lines,
            [l("Date", "2023-11-14", ""), l("Time", "22:13", "")]
        );
        let labels: Vec<&str> = sections[1].lines.iter().map(|x| x.label.as_str()).collect();
        assert_eq!(labels, ["Open", "High", "Low", "Close", "Volume"]);
        assert!(
            sections[1]
                .lines
                .iter()
                .all(|x| x.color == "var(--vela-up)")
        );
        assert_eq!(
            sections[3].lines,
            [
                l("RSI", "61.20", "#7e57c2"),
                l("Signal", "58.90", "#ff9800")
            ]
        );
    }

    #[test]
    fn tints_a_down_bar_with_the_down_color_and_drops_volume_without_it() {
        let readout = DataWindowReadout {
            date: "2023-11-14".into(),
            time: "22:13".into(),
            ohlc: Some(DataWindowOhlc {
                o: "110.00".into(),
                h: "111.00".into(),
                l: "98.00".into(),
                c: "99.00".into(),
                vol: None,
                up: false,
            }),
            groups: Vec::new(),
        };
        let sections = data_window_sections(&readout, None);
        let labels: Vec<&str> = sections[1].lines.iter().map(|x| x.label.as_str()).collect();
        assert_eq!(labels, ["Open", "High", "Low", "Close"]);
        assert!(
            sections[1]
                .lines
                .iter()
                .all(|x| x.color == "var(--vela-down)")
        );
    }

    #[test]
    fn shows_a_dash_for_a_missing_date_or_time_rather_than_an_empty_row() {
        let readout = DataWindowReadout {
            date: "2023-11-14".into(),
            ..empty()
        };
        let sections = data_window_sections(&readout, None);
        assert_eq!(
            sections[0].lines,
            [l("Date", "2023-11-14", ""), l("Time", "\u{2014}", "")]
        );
        let time_only = DataWindowReadout {
            time: "09:30".into(),
            ..empty()
        };
        assert_eq!(
            data_window_sections(&time_only, None)[0].lines[0],
            l("Date", "\u{2014}", "")
        );
    }

    #[test]
    fn the_messages_read_in_english_without_a_translator() {
        assert_eq!(text_of(None, &data_window_title(), &[]), "Data window");
        assert_eq!(text_of(None, &no_data_message(), &[]), "No data");
        assert_eq!(
            text_of(None, &no_readout_message(), &[]),
            "This renderer provides no data readout."
        );
        assert_eq!(REFRESH_EVENTS.len(), 9);
    }
}
