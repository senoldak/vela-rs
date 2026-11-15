//! Floating drawing pill (mobile): the on-chart remnant of the hidden drawing toolbar.
//! While a tool is armed (or the eraser is on) it floats over the plot with the armed
//! tool's glyph, the magnet cycle, stay-in-drawing-mode, the eraser, and a close button to
//! disarm. Chrome over the PUBLIC drawings facade only: it drives `chart.drawings` and
//! follows the `drawing:*` events, so any path that arms/disarms a tool (drawer, keyboard
//! shortcut, API) keeps it truthful. Only shown in the mobile size class: desktop keeps the
//! docked toolbar.
//!
//! This is the pure half of the original `DrawingPill`: the labels, the magnet cycle and
//! the state the pill's elements are synced to ([`drawing_pill_state`]). Left for the
//! browser crate: the element tree and CSS, the button handlers, and the `drawing:tool`,
//! `drawing:snap`, `drawing:stay`, `drawing:mode` subscriptions that call for a re-sync.

use crate::drawings::geometry::SnapMode;
use crate::i18n::{MessageLike, msg};
use crate::ports::chart_renderer::DrawingMode;

/// The pill's four button labels (each button's accessible name).
pub fn magnet_label() -> MessageLike {
    msg("widget.drawingPill.magnet", "Magnet snap")
}
pub fn stay_label() -> MessageLike {
    msg("widget.drawingPill.stay", "Stay in drawing mode")
}
pub fn eraser_label() -> MessageLike {
    msg("widget.drawingPill.eraser", "Eraser")
}
pub fn exit_label() -> MessageLike {
    msg("widget.drawingPill.exit", "Exit drawing mode")
}

/// The magnet button cycles off, weak, strong, off.
pub fn next_snap_mode(current: SnapMode) -> SnapMode {
    match current {
        SnapMode::Off => SnapMode::Weak,
        SnapMode::Weak => SnapMode::Strong,
        SnapMode::Strong => SnapMode::Off,
    }
}

/// The eraser button toggles the eraser mode: on when off, off when on.
pub fn next_eraser_mode(current: Option<DrawingMode>) -> Option<DrawingMode> {
    if current == Some(DrawingMode::Eraser) {
        None
    } else {
        Some(DrawingMode::Eraser)
    }
}

/// The glyph in the pill's leading slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PillGlyph {
    /// The armed tool's icon (resolved from the drawing type registry by its key).
    Tool(String),
    /// The eraser icon.
    Eraser,
}

/// The magnet strength badge's bars: `None` (no badge) while snapping is off, one lit bar
/// for weak and both for strong (a letter cannot tell them apart in languages where both
/// words share an initial).
pub fn magnet_strength(snap: SnapMode) -> Option<&'static str> {
    match snap {
        SnapMode::Off => None,
        SnapMode::Weak => Some("weak"),
        SnapMode::Strong => Some("strong"),
    }
}

/// What the pill's elements show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrawingPillState {
    /// The whole pill is hidden (no armed tool, no eraser, or drawings unsupported).
    pub hidden: bool,
    pub glyph: Option<PillGlyph>,
    pub magnet_on: bool,
    pub magnet_strength: Option<&'static str>,
    pub stay_on: bool,
    pub eraser_on: bool,
}

/// The state the pill syncs to. `supported` is `chart.drawings.supported()`; a renderer
/// without drawings hides the pill.
pub fn drawing_pill_state(
    supported: bool,
    tool: Option<&str>,
    mode: Option<DrawingMode>,
    snap: SnapMode,
    stay: bool,
) -> DrawingPillState {
    let eraser = mode == Some(DrawingMode::Eraser);
    let hidden = !supported || (tool.is_none() && !eraser);
    if hidden {
        return DrawingPillState {
            hidden: true,
            glyph: None,
            magnet_on: false,
            magnet_strength: None,
            stay_on: false,
            eraser_on: false,
        };
    }
    DrawingPillState {
        hidden: false,
        glyph: Some(match tool {
            Some(key) => PillGlyph::Tool(key.to_string()),
            None => PillGlyph::Eraser,
        }),
        magnet_on: snap != SnapMode::Off,
        magnet_strength: magnet_strength(snap),
        stay_on: stay,
        eraser_on: eraser,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::text_of;

    #[test]
    fn the_magnet_shows_its_strength_as_bars_through_the_off_weak_strong_cycle() {
        let mut snap = SnapMode::Off;
        let mut shown = Vec::new();
        for _ in 0..3 {
            snap = next_snap_mode(snap);
            let state = drawing_pill_state(true, Some("trendline"), None, snap, false);
            shown.push((snap, state.magnet_strength));
        }
        assert_eq!(
            shown,
            [
                (SnapMode::Weak, Some("weak")),
                (SnapMode::Strong, Some("strong")),
                (SnapMode::Off, None),
            ]
        );
        assert_eq!(magnet_strength(SnapMode::Off), None);
    }

    #[test]
    fn the_pill_hides_without_an_armed_tool_or_the_eraser() {
        assert!(drawing_pill_state(true, None, None, SnapMode::Off, false).hidden);
        assert!(
            drawing_pill_state(true, None, Some(DrawingMode::Measure), SnapMode::Off, false).hidden
        );
        assert!(!drawing_pill_state(true, Some("trendline"), None, SnapMode::Off, false).hidden);
        assert!(
            !drawing_pill_state(true, None, Some(DrawingMode::Eraser), SnapMode::Off, false).hidden
        );
    }

    #[test]
    fn a_renderer_without_drawings_hides_the_pill_whatever_is_armed() {
        let state = drawing_pill_state(
            false,
            Some("trendline"),
            Some(DrawingMode::Eraser),
            SnapMode::Strong,
            true,
        );
        assert!(state.hidden && state.glyph.is_none());
        assert!(!state.magnet_on && !state.stay_on && !state.eraser_on);
    }

    #[test]
    fn the_glyph_is_the_tool_or_the_eraser_and_the_toggles_follow_the_state() {
        let tool = drawing_pill_state(true, Some("hline"), None, SnapMode::Weak, true);
        assert_eq!(tool.glyph, Some(PillGlyph::Tool("hline".into())));
        assert!(tool.magnet_on && tool.stay_on && !tool.eraser_on);
        let eraser =
            drawing_pill_state(true, None, Some(DrawingMode::Eraser), SnapMode::Off, false);
        assert_eq!(eraser.glyph, Some(PillGlyph::Eraser));
        assert!(eraser.eraser_on && !eraser.magnet_on);
        // An armed tool wins the glyph slot over the eraser.
        let both = drawing_pill_state(
            true,
            Some("hline"),
            Some(DrawingMode::Eraser),
            SnapMode::Off,
            false,
        );
        assert_eq!(both.glyph, Some(PillGlyph::Tool("hline".into())));
        assert!(both.eraser_on);
    }

    #[test]
    fn the_eraser_button_toggles_the_mode() {
        assert_eq!(next_eraser_mode(None), Some(DrawingMode::Eraser));
        assert_eq!(next_eraser_mode(Some(DrawingMode::Eraser)), None);
        assert_eq!(
            next_eraser_mode(Some(DrawingMode::Measure)),
            Some(DrawingMode::Eraser)
        );
    }

    #[test]
    fn the_labels_read_in_english() {
        let t = |m: MessageLike| text_of(None, &m, &[]);
        assert_eq!(t(magnet_label()), "Magnet snap");
        assert_eq!(t(stay_label()), "Stay in drawing mode");
        assert_eq!(t(eraser_label()), "Eraser");
        assert_eq!(t(exit_label()), "Exit drawing mode");
    }
}
