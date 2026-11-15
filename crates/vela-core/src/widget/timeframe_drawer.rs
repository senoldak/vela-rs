//! Timeframe drawer: the mobile bottom sheet of range chips and timeframe chips.
//!
//! The pure half of the original `TimeframeDrawer`: the headings and the chips with their
//! active flags ([`timeframe_drawer_view`]). Left for the browser crate: the sheet, the chip
//! elements and CSS, and hiding the sheet after a pick (a range pick reports the preset, a
//! timeframe pick the timeframe).

use super::bottombar::{RangePreset, range_label};
use super::timeframe::timeframe_label;
use crate::i18n::{MessageLike, Translator, msg, text_of};

pub fn date_range_heading() -> MessageLike {
    msg("widget.timeframeDrawer.dateRange", "Date Range")
}
pub fn timeframes_heading() -> MessageLike {
    msg("widget.timeframeDrawer.timeframes", "Timeframes")
}

/// One chip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrawerChip {
    /// The chip's identity: the range id, or the timeframe value.
    pub id: String,
    pub label: String,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeframeDrawerView {
    pub range_heading: String,
    pub ranges: Vec<DrawerChip>,
    pub timeframes_heading: String,
    pub timeframes: Vec<DrawerChip>,
}

/// The sheet's content at open. The active range chip is lit; a timeframe chip is lit only
/// while NO range is active (the range owns the timeframe then).
pub fn timeframe_drawer_view<S: AsRef<str>>(
    ranges: &[RangePreset],
    timeframes: &[S],
    current_timeframe: &str,
    active_range: Option<&str>,
    i18n: Option<&dyn Translator>,
) -> TimeframeDrawerView {
    TimeframeDrawerView {
        range_heading: text_of(i18n, &date_range_heading(), &[]),
        ranges: ranges
            .iter()
            .map(|r| DrawerChip {
                id: r.id.to_string(),
                label: range_label(r.id, i18n),
                active: active_range == Some(r.id),
            })
            .collect(),
        timeframes_heading: text_of(i18n, &timeframes_heading(), &[]),
        timeframes: timeframes
            .iter()
            .map(|tf| DrawerChip {
                id: tf.as_ref().to_string(),
                label: timeframe_label(tf.as_ref(), i18n),
                active: active_range.is_none() && tf.as_ref() == current_timeframe,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::bottombar::RANGE_PRESETS;

    fn lit(chips: &[DrawerChip]) -> Vec<&str> {
        chips
            .iter()
            .filter(|c| c.active)
            .map(|c| c.id.as_str())
            .collect()
    }

    #[test]
    fn with_no_active_range_the_current_timeframe_chip_is_lit() {
        let view = timeframe_drawer_view(&RANGE_PRESETS, &["1", "60", "D"], "60", None, None);
        assert_eq!(lit(&view.timeframes), ["60"]);
        assert!(lit(&view.ranges).is_empty());
        assert_eq!(view.range_heading, "Date Range");
        assert_eq!(view.timeframes_heading, "Timeframes");
    }

    #[test]
    fn an_active_range_lights_its_chip_and_leaves_the_timeframes_dark() {
        let view = timeframe_drawer_view(&RANGE_PRESETS, &["1", "60", "D"], "60", Some("3M"), None);
        assert_eq!(lit(&view.ranges), ["3M"]);
        assert!(lit(&view.timeframes).is_empty());
    }

    #[test]
    fn chips_are_labeled_compactly() {
        let view = timeframe_drawer_view(&RANGE_PRESETS, &["60", "D", "240"], "D", None, None);
        assert_eq!(
            view.timeframes
                .iter()
                .map(|c| c.label.as_str())
                .collect::<Vec<_>>(),
            ["1h", "1D", "4h"]
        );
        assert_eq!(view.ranges.len(), 9);
        assert_eq!(view.ranges[5].label, "YTD");
        // An unknown active range lights nothing (and still mutes the timeframes).
        let odd = timeframe_drawer_view(&RANGE_PRESETS, &["60"], "60", Some("custom"), None);
        assert!(lit(&odd.ranges).is_empty() && lit(&odd.timeframes).is_empty());
    }
}
