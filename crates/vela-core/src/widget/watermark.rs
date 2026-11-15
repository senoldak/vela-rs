//! Symbol watermark: large faded "SYMBOL \u{b7} TF" centered on the price pane, with a smaller
//! "Replay" line under it while the chart replays past bars.
//!
//! The pure half of the original `Watermark`: the visibility rules ([`WatermarkModel`]), the
//! mark text ([`mark_text`]) and the font fit ([`watermark_font_px`]). Left for the browser
//! crate: the element (opted into the PNG export beneath the canvases), the replay icon, the
//! CSS, and the `ResizeObserver` that measures the text at the cap (`MAX_FONT_PX`) and
//! applies the fitted size.

use crate::data::provider_registry::parse_symbol;
use crate::i18n::{MessageArg, MessageLike, Translator, msg, text_of};
use crate::indicators::classics::math::{js_max, js_min};

use super::timeframe::timeframe_label;

/// The watermark's largest type size: a lone full-size chart renders at this cap.
pub const MAX_FONT_PX: f64 = 36.0;
/// Floor for tiny cells: the mark is a 5%-opacity background, small is fine.
pub const MIN_FONT_PX: f64 = 12.0;
/// Share of the chart width the text may occupy (breathing room at both edges).
const FILL: f64 = 0.9;

/// The font size that fits a mark whose text measures `text_px_at_max` at the cap into
/// `avail_px` of chart width. Text width scales linearly with font size, so one measurement
/// at the cap decides the whole fit; the size is bounded to
/// [[`MIN_FONT_PX`], [`MAX_FONT_PX`]]. The chart's OWN width is what bounds the mark: a
/// viewport-relative size overflows small cells in a multi-chart grid.
pub fn watermark_font_px(avail_px: f64, text_px_at_max: f64) -> f64 {
    if text_px_at_max <= 0.0 {
        return MAX_FONT_PX;
    }
    js_max(
        MIN_FONT_PX,
        js_min(
            MAX_FONT_PX,
            (MAX_FONT_PX * avail_px * FILL / text_px_at_max).floor(),
        ),
    )
}

pub fn replay_message() -> MessageLike {
    msg("widget.watermark.replay", "Replay")
}

/// The mark: the bare ticker (the venue prefix is routing identity, not something to
/// watermark) and the timeframe; empty without a symbol.
pub fn mark_text(symbol: &str, timeframe: &str, i18n: Option<&dyn Translator>) -> String {
    if symbol.is_empty() {
        return String::new();
    }
    text_of(
        i18n,
        &msg("widget.watermark.mark", "{symbol} \u{b7} {timeframe}"),
        &[
            ("symbol", MessageArg::from(parse_symbol(symbol).ticker)),
            (
                "timeframe",
                MessageArg::from(timeframe_label(timeframe, i18n)),
            ),
        ],
    )
}

/// What the watermark shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatermarkVisibility {
    /// The symbol line shows.
    pub text: bool,
    /// The replay line shows.
    pub replay_line: bool,
    /// The whole element shows (`display` is not `none`).
    pub shown: bool,
}

/// The watermark's visibility state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatermarkModel {
    /// The host's visibility preference (the persisted watermark toggle).
    shown: bool,
    /// The host's preference for the replay line (its own persisted toggle).
    replay_shown: bool,
    /// The chart is replaying past bars.
    replaying: bool,
    /// A bar load is in flight with nothing painted: the loading affordance owns the canvas,
    /// so the mark stays out of its way. Starts true: the FIRST `load:start` fires during
    /// chart construction, before any subscriber can see it.
    loading: bool,
}

impl Default for WatermarkModel {
    fn default() -> Self {
        Self {
            shown: true,
            replay_shown: true,
            replaying: false,
            loading: true,
        }
    }
}

impl WatermarkModel {
    pub fn set_visible(&mut self, visible: bool) {
        self.shown = visible;
    }

    /// Show/hide the replay line (the host's toggle); it only ever shows while replaying.
    pub fn set_replay_visible(&mut self, visible: bool) {
        self.replay_shown = visible;
    }

    /// The chart entered or left a bar replay.
    pub fn set_replaying(&mut self, replaying: bool) {
        self.replaying = replaying;
    }

    /// Loading and the watermark never share the canvas: hidden while a load is up.
    pub fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
    }

    pub fn visibility(&self) -> WatermarkVisibility {
        let replay = self.replay_shown && self.replaying;
        WatermarkVisibility {
            text: self.shown,
            replay_line: replay,
            shown: (self.shown || replay) && !self.loading,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_cap_when_the_text_already_fits_with_room_to_spare() {
        // Text is 500px wide at the 36px cap; a 900px chart holds it (900*0.9 = 810 >= 500).
        assert_eq!(watermark_font_px(900.0, 500.0), 36.0);
    }

    #[test]
    fn shrinks_proportionally_when_the_chart_is_narrower_than_the_text() {
        // A 400px multichart cell: 36 * (400*0.9)/500 = 25.92, floored.
        assert_eq!(watermark_font_px(400.0, 500.0), 25.0);
        // Half the cell again: the font floors at MIN.
        assert_eq!(watermark_font_px(200.0, 500.0), 12.0);
    }

    #[test]
    fn never_drops_below_the_floor_nor_exceeds_the_cap() {
        assert_eq!(watermark_font_px(30.0, 500.0), 12.0); // tiny cell: floor
        assert_eq!(watermark_font_px(100_000.0, 10.0), 36.0); // huge chart: cap
        assert_eq!(watermark_font_px(-50.0, 500.0), 12.0);
    }

    #[test]
    fn an_unmeasurable_text_keeps_the_cap_instead_of_dividing_by_zero() {
        assert_eq!(watermark_font_px(400.0, 0.0), 36.0);
        assert_eq!(watermark_font_px(400.0, -3.0), 36.0);
    }

    #[test]
    fn the_mark_is_the_bare_ticker_and_the_timeframe() {
        assert_eq!(mark_text("BTCUSDT", "60", None), "BTCUSDT \u{b7} 1h");
        assert_eq!(mark_text("BINANCE:BTCUSDT", "D", None), "BTCUSDT \u{b7} 1D");
        assert_eq!(mark_text("", "60", None), "");
    }

    #[test]
    fn the_replay_line_shows_under_the_symbol_line_only_while_replaying() {
        let mut m = WatermarkModel::default();
        m.set_loading(false);
        let v = m.visibility();
        assert!(v.text && !v.replay_line && v.shown);
        m.set_replaying(true);
        assert!(m.visibility().replay_line && m.visibility().shown);
        m.set_replaying(false);
        assert!(!m.visibility().replay_line);
    }

    #[test]
    fn the_replay_toggle_hides_it_and_it_stands_alone_when_the_symbol_line_is_off() {
        let mut m = WatermarkModel::default();
        m.set_loading(false);
        m.set_replaying(true);
        m.set_replay_visible(false);
        assert!(!m.visibility().replay_line);
        m.set_replay_visible(true);
        m.set_visible(false);
        let v = m.visibility();
        assert!(!v.text && v.replay_line && v.shown);
        m.set_replaying(false);
        assert!(!m.visibility().shown, "nothing left to show");
    }

    #[test]
    fn a_load_in_flight_hides_the_whole_mark_and_a_fresh_one_starts_loading() {
        let mut m = WatermarkModel::default();
        assert!(
            !m.visibility().shown,
            "starts hidden: the first load starts before any subscriber"
        );
        m.set_loading(false);
        assert!(m.visibility().shown);
        m.set_loading(true);
        assert!(!m.visibility().shown);
    }
}
