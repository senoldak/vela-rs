//! Canvas painting of the timeline-mark lane: one outlined token per glyph (circle,
//! square, diamond or pin) drawn in the mark's color on the plot background, carrying a
//! letter or a registry icon in that same color; a hairline stem down to the time-axis
//! line; and a background-colored halo so the cards of a collapsed deck separate. The
//! token the pointer lands on swells once and settles; the one whose popup is open (or
//! that was just clicked) paints filled: color for the body and outline, white ink.
//!
//! Left for the browser crate: the original's `MarkIconRaster` turns a registry icon's SVG
//! markup into a canvas-drawable image through the DOM (`document`, `Image`,
//! `XMLSerializer`), caches it per (icon, ink, size, dpr) and asks the owner for a chrome
//! repaint when the image lands. Here the painter asks a [`MarkIcons`] for the image
//! instead; the browser crate implements it with that raster and cache, and
//! [`NoMarkIcons`] answers `None` (the bare token). The grapheme-aware letter helpers take
//! the `Intl` backend, which the original reads from the platform.

// A NaN operand must fail these checks, as the original's `!(a > b)` does.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

use std::f64::consts::PI;

use crate::i18n::slice_graphemes;
use crate::indicators::classics::math::{js_max, js_min, js_round};
use crate::intl::Intl;
use crate::marks::MarkShape;
use crate::render::canvas::{Canvas2d, CanvasImage, TextAlign, TextBaseline};
use crate::util::js_number;

use super::marks_layout::MarkLaneLayout;

/// The hover pulse (one swell-and-settle when the pointer lands on a token), in ms.
pub const MARK_PULSE_MS: f64 = 360.0;
/// How much the token grows at the pulse's peak (a fraction of its size).
pub const MARK_PULSE_AMPLITUDE: f64 = 0.1;
/// Ink of a filled (active) token.
const ACTIVE_INK: &str = "#ffffff";

/// Registry icons as canvas-drawable images, keyed by (icon, ink, px, dpr). A miss starts
/// the load and answers `None`; the owner repaints when the image lands.
pub trait MarkIcons {
    fn get(&self, icon: &str, ink: &str, px: f64, dpr: f64) -> Option<CanvasImage>;
}

/// An icon source with no images: every icon token paints bare.
pub struct NoMarkIcons;

impl MarkIcons for NoMarkIcons {
    fn get(&self, _icon: &str, _ink: &str, _px: f64, _dpr: f64) -> Option<CanvasImage> {
        None
    }
}

pub struct MarkPaintDeps<'a> {
    /// The time-axis line's y: every stack's base glyph drops a stem onto it.
    pub axis_y: f64,
    /// The plot background: the idle token's fill and the separating halo.
    pub background: &'a str,
    /// Stem color (the axis border).
    pub stem_color: &'a str,
    pub font_family: &'a str,
    pub dpr: f64,
    pub icons: &'a dyn MarkIcons,
    pub intl: &'a dyn Intl,
    /// The cluster under the pointer (it pulsed once when the pointer landed) and when
    /// that hover began, frame-clock ms.
    pub hover_key: Option<&'a str>,
    pub hover_since: f64,
    /// The cluster whose popup is open: painted filled.
    pub active_key: Option<&'a str>,
    /// A just-clicked content-less cluster: painted filled for a moment.
    pub flash_key: Option<&'a str>,
    /// The frame clock, ms.
    pub now_ms: f64,
}

/// The hovered token's size multiplier `elapsed_ms` ms into the hover: one swell to
/// `1 + amplitude` at mid-pulse, back to 1 at its end, 1 ever after.
pub fn pulse_scale(elapsed_ms: f64) -> f64 {
    if !(elapsed_ms > 0.0) || elapsed_ms >= MARK_PULSE_MS {
        return 1.0;
    }
    1.0 + MARK_PULSE_AMPLITUDE * (PI * elapsed_ms / MARK_PULSE_MS).sin()
}

pub fn paint_mark_lane(ctx: &dyn Canvas2d, layout: &MarkLaneLayout, deps: &MarkPaintDeps) {
    if layout.glyphs.is_empty() {
        return;
    }
    ctx.save();
    ctx.set_line_dash(&[]);
    ctx.set_text_align(TextAlign::Center);
    ctx.set_text_baseline(TextBaseline::Middle);
    for g in &layout.glyphs {
        let Some(mark) = g.cluster.marks.first() else {
            continue;
        };
        let key = g.cluster.key.as_str();
        let color = mark.glyph.color.as_str();
        let active = Some(key) == deps.active_key || Some(key) == deps.flash_key;
        let size = g.size
            * if Some(key) == deps.hover_key {
                pulse_scale(deps.now_ms - deps.hover_since)
            } else {
                1.0
            };
        if g.depth == 0 {
            // The stem ties the stack to its bar on the axis line.
            let sx = js_round(g.x) + 0.5;
            ctx.set_line_width(1.0);
            ctx.set_stroke_style(&deps.stem_color.into());
            ctx.begin_path();
            ctx.move_to(sx, g.y + g.size / 2.0);
            ctx.line_to(sx, deps.axis_y);
            ctx.stroke();
        }
        let shape = mark.glyph.shape.unwrap_or(MarkShape::Circle);
        // Halo: a background-colored ring under the outline separates deck cards and
        // lifts the token off the candles.
        let (cx, cy) = trace_shape(ctx, shape, g.x, g.y, size);
        ctx.set_line_width(4.0);
        ctx.set_stroke_style(&deps.background.into());
        ctx.stroke();
        ctx.set_fill_style(&(if active { color } else { deps.background }).into());
        ctx.fill();
        ctx.set_line_width(1.5);
        ctx.set_stroke_style(&color.into());
        ctx.stroke();
        let ink = if active { ACTIVE_INK } else { color };
        let symbol_px = js_round(size * 0.62);
        if let Some(icon) = mark.glyph.icon.as_deref().filter(|i| !i.is_empty()) {
            if let Some(img) = deps.icons.get(icon, ink, symbol_px, deps.dpr) {
                ctx.draw_image(
                    &img,
                    cx - symbol_px / 2.0,
                    cy - symbol_px / 2.0,
                    symbol_px,
                    symbol_px,
                );
            }
        } else if let Some(letter) = mark.glyph.letter.as_deref().filter(|l| !l.is_empty()) {
            // Two characters at most, never half of one (an accented letter, an emoji).
            let letter = slice_graphemes(deps.intl, letter, 2);
            ctx.set_fill_style(&ink.into());
            let mut px = letter_font_px(deps.intl, size, &letter);
            ctx.set_font(&format!("600 {}px {}", js_number(px), deps.font_family));
            if is_letter_pair(deps.intl, &letter) {
                // The pair ratio was measured in one font and the host may set another:
                // measure the pair as drawn and shrink it if it would still run over the
                // outline.
                px = fit_letter_px(
                    px,
                    ctx.measure_text(&letter).width,
                    symbol_inner_width(shape, size),
                );
                ctx.set_font(&format!("600 {}px {}", js_number(px), deps.font_family));
            }
            ctx.fill_text(&letter, cx, cy + 0.5);
        }
    }
    ctx.restore();
}

/// The font size for a token's letter(s), px. One character fills the token; two must
/// share the same width, so they drop to a size where a wide pair of capitals still sits
/// inside a 16 px pin head instead of running over its outline. The ratio comes from
/// measuring `CH` in the default host font at 600 weight: 9.5 px at 6 px against a head
/// that is 16 x 0.82 - 2 x 1.5 (outline) = 10.1 px wide; 6.5 px already overflows. On the
/// 20 px cluster token the same ratio gives 8 px, 12.4 px against 13.4. Wider pairs (`MM`)
/// or a wider host font are caught by [`fit_letter_px`] at paint time.
pub fn letter_font_px(intl: &dyn Intl, size: f64, letter: &str) -> f64 {
    js_round(
        size * if is_letter_pair(intl, letter) {
            0.38
        } else {
            0.58
        },
    )
}

/// Two characters rather than one (graphemes: `e` written with a combining accent is one).
fn is_letter_pair(intl: &dyn Intl, letter: &str) -> bool {
    slice_graphemes(intl, letter, 1) != letter
}

/// The width a token's symbol may use: the head for a pin, the whole token otherwise, less
/// the 1.5 px outline each side.
pub fn symbol_inner_width(shape: MarkShape, size: f64) -> f64 {
    (if shape == MarkShape::Pin {
        size * 0.82
    } else {
        size
    }) - 3.0
}

/// Shrink a font size so text that measures `width` at `px` fits inside `inner`; text that
/// already fits is left alone, and nothing goes below 4 px (unreadable either way, but
/// never zero or negative).
pub fn fit_letter_px(px: f64, width: f64, inner: f64) -> f64 {
    if !(width > inner) || !(width > 0.0) {
        return px;
    }
    js_max(4.0, f64::floor((px * inner) / width))
}

/// Trace a token outline centered at (x, y) and return where its symbol centers (a pin's
/// head sits above its tail).
fn trace_shape(ctx: &dyn Canvas2d, shape: MarkShape, x: f64, y: f64, size: f64) -> (f64, f64) {
    let r = size / 2.0;
    ctx.begin_path();
    match shape {
        MarkShape::Square => {
            let c = js_min(3.0, r / 2.0);
            rounded_rect(ctx, x - r, y - r, size, size, c);
            (x, y)
        }
        MarkShape::Diamond => {
            ctx.move_to(x, y - r);
            ctx.line_to(x + r, y);
            ctx.line_to(x, y + r);
            ctx.line_to(x - r, y);
            ctx.close_path();
            (x, y)
        }
        MarkShape::Pin => {
            // A round head over a short tail that points at the bar.
            let hr = r * 0.82;
            let hy = y - r + hr;
            ctx.arc(x, hy, hr, PI * 0.75, PI * 0.25, false);
            ctx.line_to(x, y + r);
            ctx.close_path();
            (x, hy)
        }
        MarkShape::Circle => {
            ctx.arc(x, y, r, 0.0, PI * 2.0, false);
            (x, y)
        }
    }
}

fn rounded_rect(ctx: &dyn Canvas2d, x: f64, y: f64, w: f64, h: f64, radius: f64) {
    ctx.move_to(x + radius, y);
    ctx.line_to(x + w - radius, y);
    ctx.quadratic_curve_to(x + w, y, x + w, y + radius);
    ctx.line_to(x + w, y + h - radius);
    ctx.quadratic_curve_to(x + w, y + h, x + w - radius, y + h);
    ctx.line_to(x + radius, y + h);
    ctx.quadratic_curve_to(x, y + h, x, y + h - radius);
    ctx.line_to(x, y + radius);
    ctx.quadratic_curve_to(x, y, x + radius, y);
    ctx.close_path();
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use serde_json::json;

    use super::*;
    use crate::intl::default_intl;
    use crate::marks::{MarkGlyph, TimelineMark};
    use crate::render::canvas::RecordingCanvas;
    use crate::render::chrome::marks_layout::{MarkCluster, PlacedGlyph};

    // ── sizing helpers (marks-paint.test.ts) ──

    #[test]
    fn a_single_letter_fills_the_token_a_pair_shares_its_width_and_shrinks_to_stay_inside_the_outline()
     {
        let intl = default_intl();
        assert_eq!(letter_font_px(&*intl, 16.0, "N"), 9.0);
        assert_eq!(letter_font_px(&*intl, 16.0, "US"), 6.0);
        assert_eq!(letter_font_px(&*intl, 20.0, "N"), 12.0);
        assert_eq!(letter_font_px(&*intl, 20.0, "EU"), 8.0);
    }

    #[test]
    fn a_pair_that_would_still_overrun_the_outline_in_the_host_font_is_shrunk_to_fit() {
        let pin_inner = symbol_inner_width(MarkShape::Pin, 16.0); // 10.12
        assert_eq!(fit_letter_px(6.0, 9.5, pin_inner), 6.0); // `CH` in the default font fits
        assert_eq!(fit_letter_px(6.0, 12.0, pin_inner), 5.0); // a wide face or `MM`: 6 * 10.12 / 12 -> 5
        assert_eq!(fit_letter_px(6.0, 30.0, pin_inner), 4.0); // never below 4 px
        assert_eq!(fit_letter_px(6.0, 0.0, pin_inner), 6.0); // a zero measurement changes nothing
        assert_eq!(fit_letter_px(6.0, f64::NAN, pin_inner), 6.0);
    }

    #[test]
    fn a_pin_gives_its_letters_only_the_head_and_the_other_tokens_the_whole_width_less_the_outline()
    {
        assert!((symbol_inner_width(MarkShape::Pin, 16.0) - (16.0 * 0.82 - 3.0)).abs() < 1e-6);
        assert_eq!(symbol_inner_width(MarkShape::Circle, 16.0), 13.0);
        assert_eq!(symbol_inner_width(MarkShape::Square, 20.0), 17.0);
        assert_eq!(symbol_inner_width(MarkShape::Diamond, 20.0), 17.0);
    }

    #[test]
    fn the_pulse_plays_once() {
        assert_eq!(pulse_scale(0.0), 1.0);
        assert_eq!(pulse_scale(-50.0), 1.0);
        assert_eq!(pulse_scale(f64::NAN), 1.0);
        assert!((pulse_scale(MARK_PULSE_MS / 2.0) - (1.0 + MARK_PULSE_AMPLITUDE)).abs() < 1e-6);
        assert_eq!(pulse_scale(MARK_PULSE_MS), 1.0);
        assert_eq!(pulse_scale(MARK_PULSE_MS * 1.5), 1.0);
        assert_eq!(pulse_scale(MARK_PULSE_MS * 10.0), 1.0);
    }

    /// A slight, brief grow: never a jump or a second beat.
    const _: () = assert!(MARK_PULSE_AMPLITUDE <= 0.12 && MARK_PULSE_MS <= 500.0);

    #[test]
    fn the_pulse_swells_and_settles_smoothly_within_its_amplitude() {
        let mut prev = 1.0;
        let mut t = 0.0;
        while t <= MARK_PULSE_MS / 2.0 {
            let s = pulse_scale(t);
            assert!(s >= prev); // rising half
            prev = s;
            t += 10.0;
        }
        let mut t = MARK_PULSE_MS / 2.0;
        while t <= MARK_PULSE_MS {
            let s = pulse_scale(t);
            assert!(s <= prev + 1e-9); // settling half
            prev = s;
            t += 10.0;
        }
        let mut t = 0.0;
        while t <= MARK_PULSE_MS * 3.0 {
            let s = pulse_scale(t);
            assert!((1.0..=1.0 + MARK_PULSE_AMPLITUDE + 1e-9).contains(&s));
            t += 7.0;
        }
    }

    // ── the painter ──

    fn glyph(
        key: &str,
        shape: Option<MarkShape>,
        color: &str,
        letter: Option<&str>,
        icon: Option<&str>,
        (x, y, size): (f64, f64, f64),
        depth: usize,
    ) -> PlacedGlyph {
        let mark = TimelineMark::new(
            key,
            0,
            MarkGlyph {
                shape,
                color: color.into(),
                letter: letter.map(str::to_string),
                icon: icon.map(str::to_string),
            },
        );
        PlacedGlyph {
            cluster: Rc::new(MarkCluster {
                key: key.into(),
                bar: 0,
                group: None,
                marks: vec![Rc::new(mark)],
            }),
            x,
            y,
            size,
            stack: 0,
            depth,
            decked: false,
        }
    }

    fn deps<'a>(icons: &'a dyn MarkIcons, intl: &'a dyn Intl) -> MarkPaintDeps<'a> {
        MarkPaintDeps {
            axis_y: 100.0,
            background: "#101010",
            stem_color: "#333333",
            font_family: "sans-serif",
            dpr: 1.0,
            icons,
            intl,
            hover_key: None,
            hover_since: 0.0,
            active_key: None,
            flash_key: None,
            now_ms: 0.0,
        }
    }

    fn layout_of(glyphs: Vec<PlacedGlyph>) -> MarkLaneLayout {
        MarkLaneLayout {
            stacks: vec![],
            glyphs,
        }
    }

    #[test]
    fn a_circle_with_a_letter_and_an_active_pin_with_a_pair_paint_this_exact_stream() {
        let intl = default_intl();
        let layout = layout_of(vec![
            glyph(
                "a",
                None,
                "#e91e63",
                Some("D"),
                None,
                (100.4, 80.0, 16.0),
                0,
            ),
            glyph(
                "b",
                Some(MarkShape::Pin),
                "#0000ff",
                Some("NY"),
                None,
                (150.0, 80.0, 20.0),
                1,
            ),
        ]);
        let canvas = RecordingCanvas::new();
        let mut d = deps(&NoMarkIcons, &*intl);
        d.active_key = Some("b");
        paint_mark_lane(&canvas, &layout, &d);
        // Glyph a: depth 0, so a stem from its bottom edge (80 + 8) to the axis on a pixel
        // middle (round(100.4) + 0.5); halo 4 px in the background, fill, 1.5 px outline;
        // the letter at 0.58 of 16 = round(9.28) = 9 px, a half pixel below center.
        // Glyph b: a pin of size 20 (r = 10, head radius 8.2 centered at 80 - 10 + 8.2);
        // active, so filled with its color, white ink; two letters at round(20 * 0.38) = 8
        // px, measured at 2 x 8 x 0.6 = 9.6 (inside the 13.4 head), so the size stays.
        let head_y = 80.0 - 10.0 + 10.0 * 0.82;
        let tau = std::f64::consts::PI * 2.0;
        assert_eq!(
            canvas.to_json(),
            json!([
                ["save"],
                ["setLineDash", []],
                ["textAlign", "center"],
                ["textBaseline", "middle"],
                // a
                ["lineWidth", 1.0],
                ["strokeStyle", "#333333"],
                ["beginPath"],
                ["moveTo", 100.5, 88.0],
                ["lineTo", 100.5, 100.0],
                ["stroke"],
                ["beginPath"],
                ["arc", 100.4, 80.0, 8.0, 0.0, tau, false],
                ["lineWidth", 4.0],
                ["strokeStyle", "#101010"],
                ["stroke"],
                ["fillStyle", "#101010"],
                ["fill"],
                ["lineWidth", 1.5],
                ["strokeStyle", "#e91e63"],
                ["stroke"],
                ["fillStyle", "#e91e63"],
                ["font", "600 9px sans-serif"],
                ["fillText", "D", 100.4, 80.5],
                // b
                ["beginPath"],
                [
                    "arc",
                    150.0,
                    head_y,
                    10.0 * 0.82,
                    std::f64::consts::PI * 0.75,
                    std::f64::consts::PI * 0.25,
                    false
                ],
                ["lineTo", 150.0, 90.0],
                ["closePath"],
                ["lineWidth", 4.0],
                ["strokeStyle", "#101010"],
                ["stroke"],
                ["fillStyle", "#0000ff"],
                ["fill"],
                ["lineWidth", 1.5],
                ["strokeStyle", "#0000ff"],
                ["stroke"],
                ["fillStyle", "#ffffff"],
                ["font", "600 8px sans-serif"],
                ["font", "600 8px sans-serif"],
                ["fillText", "NY", 150.0, head_y + 0.5],
                ["restore"],
            ])
        );
    }

    #[test]
    fn a_pair_that_fits_keeps_its_ratio_size_and_sets_the_same_font_again() {
        let intl = default_intl();
        let layout = layout_of(vec![glyph(
            "a",
            Some(MarkShape::Pin),
            "#f00",
            Some("MM"),
            None,
            (50.0, 50.0, 60.0),
            1,
        )]);
        let canvas = RecordingCanvas::new();
        paint_mark_lane(&canvas, &layout, &deps(&NoMarkIcons, &*intl));
        // px = round(60 * 0.38) = 23; width = 2 x 23 x 0.6 = 27.6 vs inner 60 * 0.82 - 3 = 46.2:
        // fits, so no shrink; the second font call repeats the first.
        let fonts = canvas.calls("font");
        assert_eq!(
            fonts,
            vec![
                json!(["font", "600 23px sans-serif"]),
                json!(["font", "600 23px sans-serif"])
            ]
        );
    }

    #[test]
    fn a_hovered_token_swells_with_the_pulse() {
        let intl = default_intl();
        let layout = layout_of(vec![glyph(
            "a",
            None,
            "#f00",
            None,
            None,
            (100.0, 80.0, 16.0),
            1,
        )]);
        let canvas = RecordingCanvas::new();
        let mut d = deps(&NoMarkIcons, &*intl);
        d.hover_key = Some("a");
        d.hover_since = 1000.0;
        d.now_ms = 1000.0 + MARK_PULSE_MS / 2.0;
        paint_mark_lane(&canvas, &layout, &d);
        let arc = &canvas.calls("arc")[0];
        let r = arc[3].as_f64().unwrap();
        assert!((r - 8.0 * (1.0 + MARK_PULSE_AMPLITUDE)).abs() < 1e-9);
    }

    #[test]
    fn a_flashed_token_paints_filled_and_only_the_base_of_a_stack_drops_a_stem() {
        let intl = default_intl();
        let layout = layout_of(vec![
            glyph("deep", None, "#0f0", None, None, (50.0, 70.0, 16.0), 1),
            glyph("base", None, "#f00", None, None, (50.0, 80.0, 16.0), 0),
        ]);
        let canvas = RecordingCanvas::new();
        let mut d = deps(&NoMarkIcons, &*intl);
        d.flash_key = Some("deep");
        paint_mark_lane(&canvas, &layout, &d);
        assert_eq!(canvas.calls("moveTo").len(), 1);
        // The flashed (deep) token fills with its own color, the base with the background.
        assert_eq!(
            canvas.calls("fillStyle"),
            vec![
                json!(["fillStyle", "#0f0"]),
                json!(["fillStyle", "#101010"])
            ]
        );
    }

    #[test]
    fn square_and_diamond_tokens_trace_their_outlines() {
        let intl = default_intl();
        let canvas = RecordingCanvas::new();
        paint_mark_lane(
            &canvas,
            &layout_of(vec![glyph(
                "s",
                Some(MarkShape::Square),
                "#f00",
                None,
                None,
                (50.0, 50.0, 16.0),
                1,
            )]),
            &deps(&NoMarkIcons, &*intl),
        );
        // A rounded square: corner radius min(3, r / 2) = 3, four quadratic corners.
        assert_eq!(canvas.calls("quadraticCurveTo").len(), 4);
        assert_eq!(canvas.calls("moveTo")[0], json!(["moveTo", 45.0, 42.0]));
        let canvas = RecordingCanvas::new();
        paint_mark_lane(
            &canvas,
            &layout_of(vec![glyph(
                "d",
                Some(MarkShape::Diamond),
                "#f00",
                None,
                None,
                (50.0, 50.0, 16.0),
                1,
            )]),
            &deps(&NoMarkIcons, &*intl),
        );
        assert_eq!(
            canvas.calls("lineTo"),
            vec![
                json!(["lineTo", 58.0, 50.0]),
                json!(["lineTo", 50.0, 58.0]),
                json!(["lineTo", 42.0, 50.0]),
            ]
        );
        assert_eq!(canvas.calls("closePath").len(), 1);
    }

    struct OneIcon;

    impl MarkIcons for OneIcon {
        fn get(&self, icon: &str, ink: &str, px: f64, dpr: f64) -> Option<CanvasImage> {
            assert_eq!((icon, ink, px, dpr), ("star", "#ff0000", 10.0, 1.0));
            Some(Rc::new(()))
        }
    }

    #[test]
    fn an_icon_wins_over_the_letter_and_is_drawn_centered_when_its_image_is_ready() {
        let intl = default_intl();
        let layout = layout_of(vec![glyph(
            "i",
            None,
            "#ff0000",
            Some("X"),
            Some("star"),
            (50.0, 50.0, 16.0),
            1,
        )]);
        let canvas = RecordingCanvas::new();
        paint_mark_lane(&canvas, &layout, &deps(&OneIcon, &*intl));
        assert_eq!(
            canvas.calls("drawImage"),
            vec![json!(["drawImage", 45.0, 45.0, 10.0, 10.0])]
        );
        assert!(canvas.texts().is_empty());
        // Before the image lands the token paints bare: no icon, and no letter either.
        let canvas = RecordingCanvas::new();
        paint_mark_lane(&canvas, &layout, &deps(&NoMarkIcons, &*intl));
        assert!(canvas.calls("drawImage").is_empty());
        assert!(canvas.texts().is_empty());
    }

    #[test]
    fn a_long_letter_keeps_two_characters_and_an_empty_lane_paints_nothing() {
        let intl = default_intl();
        let canvas = RecordingCanvas::new();
        paint_mark_lane(&canvas, &layout_of(vec![]), &deps(&NoMarkIcons, &*intl));
        assert!(canvas.is_empty());
        paint_mark_lane(
            &canvas,
            &layout_of(vec![glyph(
                "l",
                None,
                "#f00",
                Some("ABC"),
                None,
                (50.0, 50.0, 16.0),
                1,
            )]),
            &deps(&NoMarkIcons, &*intl),
        );
        assert_eq!(canvas.texts(), vec!["AB"]);
    }

    #[test]
    fn a_cluster_without_marks_is_skipped() {
        let intl = default_intl();
        let mut g = glyph("e", None, "#f00", None, None, (50.0, 50.0, 16.0), 0);
        g.cluster = Rc::new(MarkCluster {
            key: "e".into(),
            bar: 0,
            group: None,
            marks: vec![],
        });
        let canvas = RecordingCanvas::new();
        paint_mark_lane(&canvas, &layout_of(vec![g]), &deps(&NoMarkIcons, &*intl));
        // Only the state frame: save, dash, alignment, baseline, restore.
        assert_eq!(
            canvas.names(),
            vec![
                "save",
                "setLineDash",
                "textAlign",
                "textBaseline",
                "restore"
            ]
        );
    }
}
