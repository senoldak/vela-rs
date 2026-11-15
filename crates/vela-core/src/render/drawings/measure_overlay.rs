//! The transient ruler: NOT a persistent drawing. Click to start, move (no button) to
//! size, click again to finish (or press-drag-release in one go). It shows a
//! direction-tinted box, a mid dashed crosshair, a directional arrow and a label
//! (price change and percent, bars, duration), and is cleared on the next press, pan
//! or zoom.
//!
//! Coordinates are kept in MEDIA PIXELS: the overlay never survives a viewport change
//! (it is cleared on pan/zoom), so it needs no reprojection; data values for the label
//! are read off the live projector at paint time.

use serde_json::Value;

use crate::drawings::Projector;
use crate::format::{DurationFormatOptions, DurationStyle, FormatSurface};
use crate::i18n::{
    MessageArg, MessageLike, Translator, english_translator, localize_number_text, msg,
};
use crate::indicators::classics::math::{js_max, js_min, js_round};
use crate::palette::{BEARISH, BULLISH};
use crate::render::canvas::{Canvas2d, TextAlign, TextBaseline};
use crate::util::to_fixed;

use crate::render::shared::drawing_scene_renderer::{theme_str, theme_text_color};

fn price_change_message() -> MessageLike {
    msg("drawing.ruler.priceChange", "{change} ({percent}%)")
}

fn bars_and_duration_message() -> MessageLike {
    msg(
        "drawing.ruler.barsAndDuration",
        "{count, plural, one {{bars} bar} other {{bars} bars}}, {duration}",
    )
}

/// Signed value with 2 decimals (e.g. `+12.34`).
fn signed(n: f64) -> String {
    format!("{}{}", if n >= 0.0 { "+" } else { "" }, to_fixed(n, 2))
}

// The ruler's direction tint is fixed, not the theme's candle colors: it must stay
// readable over candles of any user-chosen color.
const UP: &str = BULLISH;
const DOWN: &str = BEARISH;
const DRAG_SLOP: f64 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MeasureState {
    Idle,
    Measuring,
    Finished,
}

/// A point in media pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeasurePoint {
    pub x: f64,
    pub y: f64,
}

/// The ruler's current graphic endpoints.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeasurePoints {
    pub start: MeasurePoint,
    pub end: MeasurePoint,
}

/// The ruler's state and painter. Owned by the interaction layer (`&mut self`).
#[derive(Debug)]
pub struct MeasureOverlay {
    state: MeasureState,
    start: Option<MeasurePoint>,
    end: Option<MeasurePoint>,
    press_x: f64,
    press_y: f64,
}

impl Default for MeasureOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl MeasureOverlay {
    pub fn new() -> Self {
        Self {
            state: MeasureState::Idle,
            start: None,
            end: None,
            press_x: 0.0,
            press_y: 0.0,
        }
    }

    pub fn is_active(&self) -> bool {
        self.state != MeasureState::Idle
    }

    pub fn is_finished(&self) -> bool {
        self.state == MeasureState::Finished
    }

    /// A press: begin the measurement, or finish it on the second click. `(x, y)` is the
    /// raw cursor (drag slop vs click-move-click); `graphic` is the pair of graphic
    /// endpoints, magnet-snapped when the magnet is on, `None` for the same as `(x, y)`.
    pub fn down(&mut self, x: f64, y: f64, graphic: Option<(f64, f64)>) {
        let (gx, gy) = graphic.unwrap_or((x, y));
        if self.state == MeasureState::Measuring {
            self.end = Some(MeasurePoint { x: gx, y: gy });
            self.state = MeasureState::Finished;
            return;
        }
        self.start = Some(MeasurePoint { x: gx, y: gy });
        self.end = Some(MeasurePoint { x: gx, y: gy });
        self.press_x = x;
        self.press_y = y;
        self.state = MeasureState::Measuring;
    }

    /// Size the in-progress ruler. `(x, y)` is the graphic (magnet-snapped) cursor.
    pub fn move_to(&mut self, x: f64, y: f64) {
        if self.state == MeasureState::Measuring {
            self.end = Some(MeasurePoint { x, y });
        }
    }

    /// A release: finish if the press was actually dragged (press-drag-release), else
    /// wait for the second click (click-move-click). `(x, y)` is the raw cursor (slop);
    /// `graphic` the graphic end (magnet-snapped when the magnet is on), `None` for the
    /// same as `(x, y)`.
    pub fn up(&mut self, x: f64, y: f64, graphic: Option<(f64, f64)>) {
        let (gx, gy) = graphic.unwrap_or((x, y));
        if self.state == MeasureState::Measuring
            && (x - self.press_x).hypot(y - self.press_y) > DRAG_SLOP
        {
            self.end = Some(MeasurePoint { x: gx, y: gy });
            self.state = MeasureState::Finished;
        }
    }

    /// Current graphic endpoints in media pixels, or `None` when idle.
    pub fn points(&self) -> Option<MeasurePoints> {
        Some(MeasurePoints {
            start: self.start?,
            end: self.end?,
        })
    }

    pub fn clear(&mut self) {
        self.state = MeasureState::Idle;
        self.start = None;
        self.end = None;
    }

    /// Paint the ruler. `i18n` defaults to the English translator.
    pub fn paint(
        &self,
        ctx: &dyn Canvas2d,
        proj: &dyn Projector,
        theme: &Value,
        i18n: Option<&dyn Translator>,
    ) {
        let (Some(start), Some(end)) = (self.start, self.end) else {
            return;
        };
        let i18n: &dyn Translator = i18n.unwrap_or_else(|| english_translator());
        let (x1, y1) = (start.x, start.y);
        let (x2, y2) = (end.x, end.y);
        let Some(pane_id) = proj.pane_id_at_y(y1).or_else(|| proj.pane_id_at_y(y2)) else {
            return;
        };
        let p1 = proj.px_to_point(x1, y1, &pane_id);
        let p2 = proj.px_to_point(x2, y2, &pane_id);
        let price_diff = p2.price - p1.price;
        let pct = if p1.price != 0.0 {
            (price_diff / p1.price) * 100.0
        } else {
            0.0
        };
        let bars = proj.bars_between(p1.time, p2.time).map(js_round);
        let up = price_diff >= 0.0;
        let color = if up { UP } else { DOWN };
        let left = js_min(x1, x2);
        let right = js_max(x1, x2);
        let top = js_min(y1, y2);
        let bot = js_max(y1, y2);
        let mid_x = (x1 + x2) / 2.0;
        let mid_y = (y1 + y2) / 2.0;

        ctx.save();
        ctx.set_global_alpha(0.12);
        ctx.set_fill_style(&color.into());
        ctx.fill_rect(left, top, right - left, bot - top);
        ctx.set_global_alpha(1.0);
        ctx.set_stroke_style(&color.into());
        ctx.set_line_width(1.0);
        ctx.set_line_dash(&[4.0, 4.0]);
        ctx.begin_path();
        ctx.move_to(mid_x, y1);
        ctx.line_to(mid_x, y2);
        ctx.move_to(x1, mid_y);
        ctx.line_to(x2, mid_y);
        ctx.stroke();
        ctx.set_line_dash(&[]);
        // a filled arrow at the destination edge, pointing the way price moved
        let ay = if up { top } else { bot };
        let dir = if up { 1.0 } else { -1.0 };
        ctx.set_fill_style(&color.into());
        ctx.begin_path();
        ctx.move_to(mid_x, ay);
        ctx.line_to(mid_x - 5.0, ay + dir * 7.0);
        ctx.line_to(mid_x + 5.0, ay + dir * 7.0);
        ctx.close_path();
        ctx.fill();
        ctx.restore();

        let duration = i18n.format().duration(
            p2.time - p1.time,
            &DurationFormatOptions {
                style: Some(DurationStyle::Span),
                surface: Some(FormatSurface::Drawing),
            },
        );
        let change = i18n.t_with(
            &price_change_message(),
            &[
                (
                    "change",
                    MessageArg::from(localize_number_text(&signed(price_diff), Some(i18n))),
                ),
                (
                    "percent",
                    MessageArg::from(localize_number_text(&signed(pct), Some(i18n))),
                ),
            ],
        );
        let second = match bars {
            Some(bars) => i18n.t_with(
                &bars_and_duration_message(),
                &[
                    ("count", MessageArg::from(bars)),
                    (
                        "bars",
                        MessageArg::from(localize_number_text(&bars_text(bars), Some(i18n))),
                    ),
                    ("duration", MessageArg::from(duration)),
                ],
            ),
            None => duration,
        };
        self.paint_label(ctx, proj, theme, mid_x, top, bot, color, &[change, second]);
    }

    /// A bordered, two-line label centered under the box (flipped above if it would
    /// overflow).
    #[allow(clippy::too_many_arguments)]
    fn paint_label(
        &self,
        ctx: &dyn Canvas2d,
        proj: &dyn Projector,
        theme: &Value,
        mid_x: f64,
        box_top: f64,
        box_bot: f64,
        color: &str,
        lines: &[String],
    ) {
        ctx.save();
        // The original interpolates `theme.fontFamily` as is ("undefined" when absent).
        ctx.set_font(&format!(
            "12px {}",
            theme_str(theme, "fontFamily").unwrap_or("undefined")
        ));
        let widest = lines
            .iter()
            .map(|l| ctx.measure_text(l).width)
            .fold(f64::NEG_INFINITY, js_max);
        let w = widest + 16.0;
        let lh = 16.0;
        let h = lines.len() as f64 * lh + 8.0;
        let mut y = box_bot + 8.0;
        if y + h > proj.height() {
            y = box_top - h - 8.0;
        }
        let x = mid_x - w / 2.0;
        ctx.set_fill_style(&theme_str(theme, "background").unwrap_or("").into());
        ctx.set_stroke_style(&color.into());
        ctx.set_line_width(1.0);
        ctx.begin_path();
        let r = 4.0;
        ctx.round_rect(x, y, w, h, r);
        ctx.fill();
        ctx.stroke();
        ctx.set_fill_style(&theme_text_color(theme).into());
        ctx.set_text_align(TextAlign::Center);
        ctx.set_text_baseline(TextBaseline::Middle);
        for (i, line) in lines.iter().enumerate() {
            ctx.fill_text(line, mid_x, y + 8.0 + lh / 2.0 + i as f64 * lh);
        }
        ctx.restore();
    }
}

/// `String(bars)` of a rounded bar count.
fn bars_text(bars: f64) -> String {
    crate::util::js_number(bars)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawings::LinearProjector;
    use crate::render::canvas::RecordingCanvas;
    use serde_json::json;

    fn pt(x: f64, y: f64) -> MeasurePoint {
        MeasurePoint { x, y }
    }

    // state machine (drawings-measure-overlay.test.ts)

    #[test]
    fn click_move_click_a_press_starts_the_cursor_sizes_the_second_press_finishes() {
        let mut m = MeasureOverlay::new();
        assert!(!m.is_active());
        m.down(10.0, 20.0, None);
        assert!(m.is_active());
        assert!(!m.is_finished());
        m.move_to(50.0, 60.0);
        assert!(!m.is_finished());
        m.down(50.0, 60.0, None);
        assert!(m.is_finished());
    }

    #[test]
    fn a_release_near_the_press_does_not_finish() {
        let mut m = MeasureOverlay::new();
        m.down(10.0, 20.0, None);
        m.up(11.0, 21.0, None);
        assert!(!m.is_finished());
        assert!(m.is_active());
    }

    #[test]
    fn press_drag_release_finishes_in_one_gesture() {
        let mut m = MeasureOverlay::new();
        m.down(10.0, 20.0, None);
        m.move_to(80.0, 90.0);
        m.up(80.0, 90.0, None);
        assert!(m.is_finished());
    }

    #[test]
    fn clear_resets_it_to_idle() {
        let mut m = MeasureOverlay::new();
        m.down(10.0, 20.0, None);
        m.move_to(50.0, 60.0);
        m.down(50.0, 60.0, None);
        assert!(m.is_finished());
        m.clear();
        assert!(!m.is_active());
        assert!(!m.is_finished());
        assert_eq!(m.points(), None);
    }

    #[test]
    fn graphic_endpoints_follow_snapped_coords_and_slop_is_judged_from_the_raw_press() {
        let mut m = MeasureOverlay::new();
        m.down(13.0, 88.0, Some((10.0, 90.0)));
        assert_eq!(
            m.points(),
            Some(MeasurePoints {
                start: pt(10.0, 90.0),
                end: pt(10.0, 90.0)
            })
        );
        m.up(14.0, 89.0, Some((10.0, 90.0)));
        assert!(!m.is_finished());
        m.move_to(50.0, 40.0);
        assert_eq!(m.points().unwrap().end, pt(50.0, 40.0));
        m.down(47.0, 43.0, Some((50.0, 40.0)));
        assert!(m.is_finished());
        assert_eq!(
            m.points(),
            Some(MeasurePoints {
                start: pt(10.0, 90.0),
                end: pt(50.0, 40.0)
            })
        );
    }

    #[test]
    fn press_drag_release_finishes_at_the_snapped_release() {
        let mut m = MeasureOverlay::new();
        m.down(13.0, 88.0, Some((10.0, 90.0)));
        m.move_to(50.0, 40.0);
        m.up(80.0, 90.0, Some((80.0, 90.0)));
        assert!(m.is_finished());
        assert_eq!(m.points().unwrap().end, pt(80.0, 90.0));
    }

    #[test]
    fn a_finished_ruler_ignores_moves_and_a_new_press_restarts_it() {
        let mut m = MeasureOverlay::new();
        m.down(10.0, 20.0, None);
        m.down(50.0, 60.0, None);
        m.move_to(99.0, 99.0);
        assert_eq!(m.points().unwrap().end, pt(50.0, 60.0));
        m.down(1.0, 2.0, None);
        assert!(!m.is_finished());
        assert_eq!(m.points().unwrap().start, pt(1.0, 2.0));
    }

    // painting

    fn theme() -> Value {
        json!({ "background": "#101010", "textColor": "#eeeeee", "fontFamily": "Inter" })
    }

    #[test]
    fn an_idle_overlay_paints_nothing() {
        let c = RecordingCanvas::new();
        let proj = LinearProjector::sample();
        MeasureOverlay::new().paint(&c, &proj, &theme(), None);
        assert!(c.is_empty());
    }

    #[test]
    fn a_ruler_outside_every_pane_paints_nothing() {
        let c = RecordingCanvas::new();
        let proj = LinearProjector::sample(); // panes span y in 0..=600
        let mut m = MeasureOverlay::new();
        m.down(10.0, 700.0, None);
        m.move_to(60.0, 800.0);
        m.paint(&c, &proj, &theme(), None);
        assert!(c.is_empty());
    }

    #[test]
    fn a_rising_ruler_paints_the_complete_call_stream() {
        // LinearProjector::sample: x = minutes from 0 (60_000 ms per px), price = 200 - y / 4.
        let proj = LinearProjector::sample();
        let c = RecordingCanvas::new();
        let mut m = MeasureOverlay::new();
        m.down(100.0, 300.0, None);
        m.move_to(160.0, 200.0);
        m.paint(&c, &proj, &theme(), None);
        // p1 = (6_000_000 ms, 125), p2 = (9_600_000 ms, 150): +25.00 (+20.00%), 60 bars
        // (the sample projector rounds bars_between), duration 1h.
        // Label: 12px => 7.2 px per char. "+25.00 (+20.00%)" is 16 chars => 115.2;
        // the second line is "60 bars, 1h" (11 chars) => 79.2. w = 115.2 + 16 = 131.2.
        // box left 100, right 160, top 200, bot 300; midX 130, midY 250.
        // h = 2 * 16 + 8 = 40; y = 308 (300 + 8 fits in 600); x = 130 - 65.6 = 64.4.
        let ops = c.ops();
        assert_eq!(
            &ops[..22],
            &[
                json!(["save"]),
                json!(["globalAlpha", 0.12]),
                json!(["fillStyle", "#089981"]),
                json!(["fillRect", 100.0, 200.0, 60.0, 100.0]),
                json!(["globalAlpha", 1.0]),
                json!(["strokeStyle", "#089981"]),
                json!(["lineWidth", 1.0]),
                json!(["setLineDash", [4.0, 4.0]]),
                json!(["beginPath"]),
                json!(["moveTo", 130.0, 300.0]),
                json!(["lineTo", 130.0, 200.0]),
                json!(["moveTo", 100.0, 250.0]),
                json!(["lineTo", 160.0, 250.0]),
                json!(["stroke"]),
                json!(["setLineDash", []]),
                json!(["fillStyle", "#089981"]),
                json!(["beginPath"]),
                json!(["moveTo", 130.0, 200.0]),
                json!(["lineTo", 125.0, 207.0]),
                json!(["lineTo", 135.0, 207.0]),
                json!(["closePath"]),
                json!(["fill"]),
            ]
        );
        assert_eq!(ops[22], json!(["restore"]));
        assert_eq!(c.texts(), vec!["+25.00 (+20.00%)", "60 bars, 1h"]);
    }

    #[test]
    fn the_label_stream_is_complete_and_follows_the_box() {
        let proj = LinearProjector::sample();
        let c = RecordingCanvas::new();
        let mut m = MeasureOverlay::new();
        m.down(100.0, 300.0, None);
        m.move_to(160.0, 200.0);
        m.paint(&c, &proj, &theme(), None);
        let ops = c.ops();
        let w = 16.0 * 7.2 + 16.0;
        let x = 130.0 - w / 2.0;
        assert_eq!(
            &ops[23..],
            &[
                json!(["save"]),
                json!(["font", "12px Inter"]),
                json!(["fillStyle", "#101010"]),
                json!(["strokeStyle", "#089981"]),
                json!(["lineWidth", 1.0]),
                json!(["beginPath"]),
                json!(["roundRect", x, 308.0, w, 40.0, 4.0]),
                json!(["fill"]),
                json!(["stroke"]),
                json!(["fillStyle", "#eeeeee"]),
                json!(["textAlign", "center"]),
                json!(["textBaseline", "middle"]),
                json!(["fillText", "+25.00 (+20.00%)", 130.0, 324.0]),
                json!(["fillText", "60 bars, 1h", 130.0, 340.0]),
                json!(["restore"]),
            ]
        );
    }

    #[test]
    fn a_falling_ruler_is_tinted_down_with_the_arrow_at_the_bottom_edge() {
        let proj = LinearProjector::sample();
        let c = RecordingCanvas::new();
        let mut m = MeasureOverlay::new();
        m.down(100.0, 200.0, None);
        m.move_to(160.0, 300.0);
        m.paint(&c, &proj, &theme(), None);
        let ops = c.ops();
        assert_eq!(ops[2], json!(["fillStyle", "#f23645"]));
        // down arrow: apex on the bottom edge (300), base 7px above it
        assert_eq!(ops[17], json!(["moveTo", 130.0, 300.0]));
        assert_eq!(ops[18], json!(["lineTo", 125.0, 293.0]));
        assert_eq!(c.texts()[0], "-25.00 (-16.67%)");
    }

    #[test]
    fn a_label_that_would_overflow_the_plot_flips_above_the_box() {
        let proj = LinearProjector::sample(); // height 600
        let c = RecordingCanvas::new();
        let mut m = MeasureOverlay::new();
        m.down(100.0, 500.0, None);
        m.move_to(160.0, 580.0);
        m.paint(&c, &proj, &theme(), None);
        // 580 + 8 + 40 > 600: y = top(500) - 40 - 8 = 452
        let rr = c.calls("roundRect");
        assert_eq!(rr[0][2], json!(452.0));
    }

    #[test]
    fn a_zero_start_price_gives_a_zero_percent() {
        let mut proj = LinearProjector::sample();
        proj.price_top = 0.0;
        proj.price_per_px = 0.25; // price = -y / 4: y = 0 gives 0
        let c = RecordingCanvas::new();
        let mut m = MeasureOverlay::new();
        m.down(100.0, 0.0, None);
        m.move_to(160.0, 100.0);
        m.paint(&c, &proj, &theme(), None);
        assert_eq!(c.texts()[0], "-25.00 (+0.00%)");
    }
}
