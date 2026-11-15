//! What the native renderer supports.

use crate::ports::chart_renderer::{BarColorSupport, RendererCapabilities, Support};

/// What the native renderer supports. The original declared the eventual target from its
/// first phase so capability-gated core logic behaves; features not yet drawn are simply
/// blank until their phase ships.
///
/// `supportsWebGL2` of the original (a probe that creates a throwaway canvas and asks for
/// a `webgl2` context, once) is the browser crate's.
pub const NATIVE_CAPABILITIES: RendererCapabilities = RendererCapabilities {
    panes: true,
    // move/merge indicators, reorder + collapse/maximize panes
    pane_management: true,
    // first-class band primitive (no per-frame polygon rebuild)
    fills: Support::Native,
    bgcolor: Support::Native,
    hline: Support::Native,
    markers: true,
    // per-bar candle color channel
    barcolor: BarColorSupport::Native,
    // per-point color as a first-class attribute (no series split)
    per_point_color: true,
    drawings: true,
    // interactive drawing tools (toolbar + hit-test + handles)
    user_drawings: true,
    // drawings share the series' z space (backend-composited interleave layers)
    drawing_depth: true,
    // canvas-painted into the owning indicator's interleave slice
    tables: true,
    // strategy order-fill markers (arrows + labels + fill-price ticks)
    trades: true,
    // host events on a lane above the time axis (glyphs + detail popup)
    timeline_marks: true,
    // reuses the DOM InputsUI
    inputs_ui: true,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_native_renderer_declares_the_full_target() {
        let c = NATIVE_CAPABILITIES;
        assert!(c.panes && c.pane_management && c.markers && c.per_point_color);
        assert!(c.drawings && c.user_drawings && c.drawing_depth && c.tables);
        assert!(c.trades && c.timeline_marks && c.inputs_ui);
        assert_eq!(c.fills, Support::Native);
        assert_eq!(c.bgcolor, Support::Native);
        assert_eq!(c.hline, Support::Native);
        assert_eq!(c.barcolor, BarColorSupport::Native);
    }

    #[test]
    fn it_serializes_under_the_original_keys() {
        let v = serde_json::to_value(NATIVE_CAPABILITIES).unwrap();
        assert_eq!(v["paneManagement"], json!(true));
        assert_eq!(v["perPointColor"], json!(true));
        assert_eq!(v["drawingDepth"], json!(true));
        assert_eq!(v["timelineMarks"], json!(true));
        assert_eq!(v["inputsUI"], json!(true));
        assert_eq!(v["fills"], json!("native"));
    }
}
