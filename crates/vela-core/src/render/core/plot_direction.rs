//! The plot never mirrors: time runs left to right and the price scale stays
//! on the right in a page of either direction, so the text painted in chart
//! coordinates (axis labels, crosshair chips, script labels and tables,
//! drawing text, marks) reads left to right too. A 2D context's `direction`
//! defaults to `inherit`, which follows the page: inside a right-to-left page
//! the bidi algorithm would carry a number's sign or percent sign to its far
//! end and `textAlign: 'start'` would anchor text on its right.
//!
//! Left for the browser crate: `pinPlotCanvas` (it sets `canvas.dir = 'ltr'`
//! on the `HTMLCanvasElement`, which outlives the state reset a resize causes
//! and reaches every context opened on the canvas). [`begin_plot_frame`] sets
//! the context's own `direction` at the start of every frame, because a resize
//! resets a context's state, `direction` included.

use crate::render::canvas::Canvas2d;

/// The value a plot context's `direction` property must hold while painting.
pub const PLOT_TEXT_DIRECTION: &str = "ltr";

/// Start a frame on a plot canvas: the device-pixel transform and the
/// left-to-right text direction.
pub fn begin_plot_frame(ctx: &dyn Canvas2d, dpr: f64) {
    ctx.set_transform(dpr, 0.0, 0.0, dpr, 0.0, 0.0);
    ctx.set_direction(PLOT_TEXT_DIRECTION);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::canvas::RecordingCanvas;
    use serde_json::json;

    #[test]
    fn a_frame_starts_with_the_device_pixel_transform_then_pins_the_direction() {
        let canvas = RecordingCanvas::new();
        begin_plot_frame(&canvas, 2.0);
        assert_eq!(
            canvas.to_json(),
            json!([
                ["setTransform", 2.0, 0.0, 0.0, 2.0, 0.0, 0.0],
                ["direction", "ltr"]
            ])
        );
    }

    #[test]
    fn the_text_direction_is_left_to_right() {
        assert_eq!(PLOT_TEXT_DIRECTION, "ltr");
    }
}
