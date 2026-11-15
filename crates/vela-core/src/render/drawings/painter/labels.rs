//! Text on drawings: the optional label every tool can carry, the annotation tools built
//! from text (callouts, notes, price tags, signposts) and the fixed-size markers (arrow
//! marks, glyph stamps).

use std::f64::consts::PI;

use crate::drawings::effective_color::effective_fill_color;
use crate::drawings::types::annotations::{
    ArrowDir, ArrowMark, CalloutTool, GlyphStamp, PinnedLabel,
};
use crate::drawings::{Drawing, Projector};
use crate::format::{FormatSurface, PriceFormatOptions};
use crate::indicators::classics::math::{js_max, js_min};
use crate::model::BoxTextSize;
use crate::render::canvas::{Canvas2d, LineJoin, TextAlign, TextBaseline};
use crate::render::chrome::ticks::value_decimals;
use crate::render::core::color::with_alpha;
use crate::render::shared::drawing_geometry::{
    TEXT_FRAME_INSET, TEXT_FRAME_RISE, contrast_color, label_line_height, named_font_size,
    upright_line_angle,
};

use super::helpers::{
    Tail, TailEdge, apply_stroke, callout_bubble_path, px_font, round_rect, set_fill, set_stroke,
    stroke_path, text_font,
};
use super::{DrawingPainter, DrawingTheme};

/// Where a drawing's label paints. With `rotate`, the canvas is rotated by `angle` around
/// `(cx, cy)` first, and `x` / `top` are in that rotated frame.
pub(super) struct LabelLayout {
    pub x: f64,
    pub top: f64,
    pub align: TextAlign,
    pub rotate: Option<Rotation>,
}

pub(super) struct Rotation {
    pub angle: f64,
    pub cx: f64,
    pub cy: f64,
}

/// Where a drawing's label sits: first-line top-left plus horizontal alignment, per type.
pub(super) fn label_layout(d: &dyn Drawing, proj: &dyn Projector) -> Option<LabelLayout> {
    let text = d.base().text.as_ref()?;
    let pts = d.handle_points(proj);
    let lines = text.value.split('\n').count() as f64;
    let lh = label_line_height(named_font_size(text.size));
    if let Some(mark) = d.as_any().downcast_ref::<ArrowMark>() {
        let b = d.bounds(proj)?;
        let x = b.x + b.w / 2.0;
        return Some(match mark.dir() {
            ArrowDir::Down => LabelLayout {
                x,
                top: b.y - 4.0 - lh * lines,
                align: TextAlign::Center,
                rotate: None,
            },
            ArrowDir::Up => LabelLayout {
                x,
                top: b.y + b.h + 4.0,
                align: TextAlign::Center,
                rotate: None,
            },
        });
    }
    match d.type_key() {
        "text" => pts.first().map(|p| LabelLayout {
            x: p.0 + 2.0,
            top: p.1 + 2.0,
            align: TextAlign::Left,
            rotate: None,
        }),
        // Above the line.
        "hline" => pts.first().map(|p| LabelLayout {
            x: p.0 + 8.0,
            top: p.1 - 4.0 - lh * lines,
            align: TextAlign::Left,
            rotate: None,
        }),
        "trendline" | "ray" | "extendedline" | "infoline" | "trendangle" => {
            if pts.len() < 2 {
                return None;
            }
            let (x1, y1) = pts[0];
            let (x2, y2) = pts[1];
            // `place` slides the label along the segment (from the first anchor's end),
            // `side` puts it under the line instead of over it; both default to the
            // original middle-over.
            let t = match text.place {
                Some(crate::drawings::TextPlace::Start) => 0.12,
                Some(crate::drawings::TextPlace::End) => 0.88,
                _ => 0.5,
            };
            let below = text.side == Some(crate::drawings::TextSide::Below);
            Some(LabelLayout {
                x: 0.0,
                top: if below { 6.0 } else { -6.0 - lh * lines },
                align: TextAlign::Center,
                rotate: Some(Rotation {
                    angle: upright_line_angle(x1, y1, x2, y2),
                    cx: x1 + (x2 - x1) * t,
                    cy: y1 + (y2 - y1) * t,
                }),
            })
        }
        "box" => {
            if pts.len() < 2 {
                return None;
            }
            Some(LabelLayout {
                x: (pts[0].0 + pts[1].0) / 2.0,
                top: (pts[0].1 + pts[1].1) / 2.0 - (lh * lines) / 2.0,
                align: TextAlign::Center,
                rotate: None,
            })
        }
        _ => pts.first().map(|p| LabelLayout {
            x: p.0,
            top: p.1,
            align: TextAlign::Left,
            rotate: None,
        }),
    }
}

/// `Math.max(min, ...widths of lines)` at the context's current font.
fn widest(ctx: &dyn Canvas2d, lines: &[&str], min: f64) -> f64 {
    lines
        .iter()
        .fold(min, |acc, line| js_max(acc, ctx.measure_text(line).width))
}

fn text_flags(text: Option<&crate::drawings::DrawingText>) -> (bool, bool, BoxTextSize) {
    match text {
        Some(t) => (t.bold == Some(true), t.italic == Some(true), t.size),
        None => (false, false, BoxTextSize::Normal),
    }
}

impl DrawingPainter {
    /// Draw a drawing's optional text label at a type-appropriate anchor (multi-line aware).
    /// Two-point line tools rotate the glyphs to follow the segment, kept upright.
    pub(super) fn paint_label(
        &self,
        ctx: &dyn Canvas2d,
        d: &dyn Drawing,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let base = d.base();
        let Some(text) = base.text.as_ref().filter(|t| !t.value.is_empty()) else {
            return;
        };
        if self.targets.muted_label.as_deref() == Some(base.id.as_str()) {
            return;
        }
        let Some(layout) = label_layout(d, proj) else {
            return;
        };
        let fs = named_font_size(text.size);
        ctx.save();
        if let Some(r) = &layout.rotate {
            ctx.translate(r.cx, r.cy);
            ctx.rotate(r.angle);
        }
        ctx.set_font(&text_font(
            text.bold == Some(true),
            text.italic == Some(true),
            fs,
            &theme.font_family,
        ));
        ctx.set_text_baseline(TextBaseline::Top);
        ctx.set_text_align(layout.align);
        let lh = label_line_height(fs);
        let lines: Vec<&str> = text.value.split('\n').collect();
        // A halo in the chart background keeps the words readable where a line (its own, or
        // another drawing's) runs through them. Free text and a box's centered label sit
        // clear of any line and keep their plain look.
        let key = d.type_key();
        if key != "text" && key != "box" {
            ctx.set_line_join(LineJoin::Round);
            ctx.set_line_width(3.0);
            set_stroke(ctx, &theme.background);
            for (i, line) in lines.iter().enumerate() {
                ctx.stroke_text(line, layout.x, layout.top + i as f64 * lh);
            }
        }
        set_fill(ctx, text.color.as_deref().unwrap_or(&theme.text_color));
        for (i, line) in lines.iter().enumerate() {
            ctx.fill_text(line, layout.x, layout.top + i as f64 * lh);
        }
        if key == "text" {
            self.paint_text_frame(ctx, &base.id, layout.x, layout.top, &lines, fs, theme);
        }
        ctx.restore();
    }

    /// Frame a plain text label while it is the target, so the text reads as a clickable
    /// object even though it has no shape of its own: a firm frame once selected, a fainter
    /// one under the cursor. Placed to coincide with the inline editor's frame, so clicking
    /// into the text does not shift it.
    #[allow(clippy::too_many_arguments)]
    fn paint_text_frame(
        &self,
        ctx: &dyn Canvas2d,
        id: &str,
        x: f64,
        top: f64,
        lines: &[&str],
        fs: f64,
        theme: &DrawingTheme,
    ) {
        let t = &self.targets;
        let alpha = if t.selected.contains(id) {
            0.3
        } else if t.hovered.as_deref() == Some(id) {
            0.12
        } else {
            0.0
        };
        if alpha == 0.0 {
            return;
        }
        let lh = label_line_height(fs);
        let w = widest(ctx, lines, 8.0);
        ctx.save();
        ctx.set_line_dash(&[]);
        ctx.set_line_width(1.0);
        set_stroke(ctx, &with_alpha(&theme.text_color, alpha));
        round_rect(
            ctx,
            x - TEXT_FRAME_INSET,
            top - (lh - fs) / 2.0 - TEXT_FRAME_RISE,
            w + TEXT_FRAME_INSET * 2.0,
            lines.len() as f64 * lh + TEXT_FRAME_RISE * 2.0,
            4.0,
        );
        ctx.stroke();
        ctx.restore();
    }

    /// A price for a Price Label tag or a Price Note: decimals scaled to the magnitude, with
    /// the locale's grouping.
    fn price_tag(&self, p: f64) -> String {
        let options = PriceFormatOptions {
            surface: Some(FormatSurface::Drawing),
            grouping: Some(true),
            ..PriceFormatOptions::new(value_decimals(p))
        };
        self.translator().format().price(p, &options)
    }

    /// Paint a callout as a speech bubble: a rounded text box with a tapered triangular tail
    /// flush on the box edge facing the target, its apex at the target point. A comment is
    /// the same with fully rounded (balloon) corners: `max_radius` is clamped to
    /// `min(w, h) / 2`.
    pub(super) fn paint_bubble(
        &self,
        ctx: &dyn Canvas2d,
        d: &CalloutTool,
        proj: &dyn Projector,
        theme: &DrawingTheme,
        max_radius: f64,
    ) {
        let Some(p) = d.points(proj) else { return };
        let (tx, ty) = p[0];
        let (cx, cy) = p[1];
        let text = d.base().text.as_ref();
        let (bold, italic, size) = text_flags(text);
        let fs = named_font_size(size);
        ctx.set_font(&text_font(bold, italic, fs, &theme.font_family));
        let lines: Vec<&str> = text.map_or("", |t| t.value.as_str()).split('\n').collect();
        let tw = widest(ctx, &lines, 8.0);
        let pad_x = 9.0;
        let pad_y = 6.0;
        let lh = label_line_height(fs);
        let w = tw + pad_x * 2.0;
        let h = lines.len() as f64 * lh + pad_y * 2.0;
        let x = cx - w / 2.0;
        let y = cy - h / 2.0;
        let hw = w / 2.0;
        let hh = h / 2.0;
        let dx = tx - cx;
        let dy = ty - cy;
        let r = js_min(js_min(max_radius, w / 2.0), h / 2.0);

        // The tail: which box edge it exits, where along that edge, and its apex at the
        // target, only when the target is outside the box.
        let mut tail: Option<Tail> = None;
        if dx.abs() > hw || dy.abs() > hh {
            let half = js_max(6.0, js_min(w, h) * 0.18);
            if dx.abs() / hw >= dy.abs() / hh {
                let ex = if dx > 0.0 { x + w } else { x };
                let at = js_max(
                    y + r + half,
                    js_min(y + h - r - half, cy + (dy * (ex - cx)) / or_one(dx)),
                );
                tail = Some(Tail {
                    edge: if dx > 0.0 {
                        TailEdge::Right
                    } else {
                        TailEdge::Left
                    },
                    at,
                    apex: (tx, ty),
                    half,
                });
            } else {
                let ey = if dy > 0.0 { y + h } else { y };
                let at = js_max(
                    x + r + half,
                    js_min(x + w - r - half, cx + (dx * (ey - cy)) / or_one(dy)),
                );
                tail = Some(Tail {
                    edge: if dy > 0.0 {
                        TailEdge::Bottom
                    } else {
                        TailEdge::Top
                    },
                    at,
                    apex: (tx, ty),
                    half,
                });
            }
        }

        // One continuous outline for the box + tail, so the border never crosses the tail
        // base (the pointer side is "open": the path detours out to the apex and back).
        let fill = effective_fill_color(d, &theme.fill_theme());
        let plate = fill.as_deref().unwrap_or(&theme.background);
        ctx.save();
        set_fill(ctx, plate); // alpha lives in the fill color itself
        callout_bubble_path(ctx, x, y, w, h, r, tail);
        ctx.fill();
        ctx.restore();
        apply_stroke(ctx, (&d.base().style).into());
        callout_bubble_path(ctx, x, y, w, h, r, tail);
        ctx.stroke();
        ctx.set_line_dash(&[]);

        // No explicit ink: auto-contrast against the plate the text actually sits on (the
        // safety net for documents that predate the creation-time ink fixation).
        let ink = match text.and_then(|t| t.color.as_deref()) {
            Some(c) => c,
            None => contrast_color(Some(plate)),
        };
        set_fill(ctx, ink);
        ctx.set_text_baseline(TextBaseline::Top);
        ctx.set_text_align(TextAlign::Left);
        for (i, line) in lines.iter().enumerate() {
            ctx.fill_text(line, x + pad_x, y + pad_y + i as f64 * lh);
        }
    }

    /// Shared text-box body for the note/signpost plates: a rounded filled and stroked rect
    /// with multi-line text, anchored at (x, y) top-left.
    #[allow(clippy::too_many_arguments)]
    fn paint_text_plate(
        &self,
        ctx: &dyn Canvas2d,
        d: &dyn Drawing,
        theme: &DrawingTheme,
        rect: (f64, f64, f64, f64),
        lines: &[String],
        lh: f64,
        pad: (f64, f64),
    ) {
        let (x, y, w, h) = rect;
        let (pad_x, pad_y) = pad;
        let fill = effective_fill_color(d, &theme.fill_theme());
        let plate = fill.as_deref().unwrap_or(&theme.background);
        ctx.save();
        set_fill(ctx, plate);
        round_rect(ctx, x, y, w, h, 5.0);
        ctx.fill();
        ctx.restore();
        apply_stroke(ctx, (&d.base().style).into());
        round_rect(ctx, x, y, w, h, 5.0);
        ctx.stroke();
        ctx.set_line_dash(&[]);
        let ink = match d.base().text.as_ref().and_then(|t| t.color.as_deref()) {
            Some(c) => c,
            None => contrast_color(Some(plate)),
        };
        set_fill(ctx, ink);
        ctx.set_text_baseline(TextBaseline::Top);
        ctx.set_text_align(TextAlign::Left);
        for (i, line) in lines.iter().enumerate() {
            ctx.fill_text(line, x + pad_x, y + pad_y + i as f64 * lh);
        }
    }

    /// Measure a drawing's text into wrapped lines + a box size at its named font.
    fn measure_lines(
        &self,
        ctx: &dyn Canvas2d,
        d: &dyn Drawing,
        theme: &DrawingTheme,
        fallback: &str,
    ) -> (Vec<String>, f64, f64) {
        let text = d.base().text.as_ref();
        let (bold, italic, size) = text_flags(text);
        let fs = named_font_size(size);
        ctx.set_font(&text_font(bold, italic, fs, &theme.font_family));
        let value = text
            .map(|t| t.value.as_str())
            .filter(|v| !v.is_empty())
            .unwrap_or(fallback);
        let lines: Vec<String> = value.split('\n').map(str::to_string).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let tw = widest(ctx, &refs, 8.0);
        (lines, tw, label_line_height(fs))
    }

    /// A Note: free text on a rounded plate anchored at the point (top-left).
    pub(super) fn paint_note(
        &self,
        ctx: &dyn Canvas2d,
        d: &PinnedLabel,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let pts = d.handle_points(proj);
        let Some(&(ax, ay)) = pts.first() else { return };
        let placeholder = self.t(&d.placeholder_text().unwrap_or_default());
        let (lines, tw, lh) = self.measure_lines(ctx, d, theme, &placeholder);
        let pad_x = 8.0;
        let pad_y = 5.0;
        let h = lines.len() as f64 * lh + pad_y * 2.0;
        self.paint_text_plate(
            ctx,
            d,
            theme,
            (ax, ay, tw + pad_x * 2.0, h),
            &lines,
            lh,
            (pad_x, pad_y),
        );
    }

    /// A Price Note: a box joined to the pinned point by a leader and a dot, showing that
    /// point's auto-formatted price (not free text).
    pub(super) fn paint_price_note(
        &self,
        ctx: &dyn Canvas2d,
        d: &CalloutTool,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let (Some(p), Some(a)) = (d.points(proj), d.base().anchors.first()) else {
            return;
        };
        let (tx, ty) = p[0];
        let (cx, cy) = p[1];
        let price_str = self.price_tag(a.price);
        let text = d.base().text.as_ref();
        let (bold, italic, size) = text_flags(text);
        let fs = named_font_size(size);
        ctx.set_font(&text_font(bold, italic, fs, &theme.font_family));
        let pad_x = 9.0;
        let pad_y = 6.0;
        let w = ctx.measure_text(&price_str).width + pad_x * 2.0;
        let h = label_line_height(fs) + pad_y * 2.0;
        let x = cx - w / 2.0;
        let y = cy - h / 2.0;
        let style = &d.base().style;
        stroke_path(ctx, style.into(), || {
            ctx.move_to(tx, ty);
            ctx.line_to(cx, cy);
        });
        set_fill(ctx, &style.line_color);
        ctx.begin_path();
        ctx.arc(tx, ty, 3.0, 0.0, PI * 2.0, false);
        ctx.fill();
        let fill = effective_fill_color(d, &theme.fill_theme());
        let plate = fill.as_deref().unwrap_or(&theme.background);
        ctx.save();
        set_fill(ctx, plate);
        round_rect(ctx, x, y, w, h, 5.0);
        ctx.fill();
        ctx.restore();
        apply_stroke(ctx, style.into());
        round_rect(ctx, x, y, w, h, 5.0);
        ctx.stroke();
        ctx.set_line_dash(&[]);
        let ink = match text.and_then(|t| t.color.as_deref()) {
            Some(c) => c,
            None => contrast_color(Some(plate)),
        };
        set_fill(ctx, ink);
        ctx.set_text_baseline(TextBaseline::Middle);
        ctx.set_text_align(TextAlign::Center);
        ctx.fill_text(&price_str, cx, cy + 0.5);
    }

    /// A Signpost: a sign plate on a pole rising from a pinned level (a base nub).
    pub(super) fn paint_signpost(
        &self,
        ctx: &dyn Canvas2d,
        d: &CalloutTool,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let Some(p) = d.points(proj) else { return };
        let (bx, by) = p[0];
        let (sx, sy) = p[1];
        let placeholder = self.t(&d.placeholder_text().unwrap_or_default());
        let (lines, tw, lh) = self.measure_lines(ctx, d, theme, &placeholder);
        let pad_x = 9.0;
        let pad_y = 6.0;
        let w = tw + pad_x * 2.0;
        let h = lines.len() as f64 * lh + pad_y * 2.0;
        let style = &d.base().style;
        stroke_path(ctx, style.into(), || {
            ctx.move_to(bx, by);
            ctx.line_to(sx, sy);
        });
        set_fill(ctx, &style.line_color);
        ctx.begin_path();
        ctx.arc(bx, by, 3.5, 0.0, PI * 2.0, false);
        ctx.fill();
        self.paint_text_plate(
            ctx,
            d,
            theme,
            (sx - w / 2.0, sy - h / 2.0, w, h),
            &lines,
            lh,
            (pad_x, pad_y),
        );
    }

    /// A Price Label: a left-pointing tag at the anchor showing the auto-formatted price.
    pub(super) fn paint_price_label(
        &self,
        ctx: &dyn Canvas2d,
        d: &PinnedLabel,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let pts = d.handle_points(proj);
        let (Some(&(ax, ay)), Some(a)) = (pts.first(), d.base().anchors.first()) else {
            return;
        };
        let price_str = self.price_tag(a.price);
        let text = d.base().text.as_ref();
        let (bold, italic, size) = text_flags(text);
        let fs = named_font_size(size);
        ctx.set_font(&text_font(bold, italic, fs, &theme.font_family));
        let tw = ctx.measure_text(&price_str).width;
        let pad_x = 8.0;
        let pad_y = 4.0;
        let ptr = 6.0; // left pointer width
        let w = tw + pad_x * 2.0;
        let h = label_line_height(fs) + pad_y * 2.0;
        let y = ay - h / 2.0;
        let fill = effective_fill_color(d, &theme.fill_theme());
        let plate = fill.as_deref().unwrap_or(&theme.background);
        ctx.save();
        set_fill(ctx, plate);
        ctx.begin_path(); // pointer triangle from the anchor pixel
        ctx.move_to(ax, ay);
        ctx.line_to(ax + ptr, ay - 5.0);
        ctx.line_to(ax + ptr, ay + 5.0);
        ctx.close_path();
        ctx.fill();
        round_rect(ctx, ax + ptr, y, w, h, 4.0);
        ctx.fill();
        ctx.restore();
        apply_stroke(ctx, (&d.base().style).into());
        round_rect(ctx, ax + ptr, y, w, h, 4.0);
        ctx.stroke();
        ctx.set_line_dash(&[]);
        let ink = match text.and_then(|t| t.color.as_deref()) {
            Some(c) => c,
            None => contrast_color(Some(plate)),
        };
        set_fill(ctx, ink);
        ctx.set_text_baseline(TextBaseline::Middle);
        ctx.set_text_align(TextAlign::Left);
        ctx.fill_text(&price_str, ax + ptr + pad_x, ay + 0.5);
    }

    /// Paint a fixed-size arrow marker as a filled glyph at its anchor.
    pub(super) fn paint_arrow_mark(&self, ctx: &dyn Canvas2d, d: &ArrowMark, proj: &dyn Projector) {
        let Some(g) = d.glyph_points(proj).filter(|g| g.len() >= 3) else {
            return;
        };
        ctx.save();
        ctx.set_line_dash(&[]);
        set_fill(ctx, &d.base().style.line_color);
        ctx.begin_path();
        ctx.move_to(g[0].0, g[0].1);
        for p in &g[1..] {
            ctx.line_to(p.0, p.1);
        }
        ctx.close_path();
        ctx.fill();
        ctx.restore();
    }

    /// A glyph/icon stamp: a fixed-pixel unicode symbol centered on the anchor (tints with
    /// the stamp color; emoji render in their own color).
    pub(super) fn paint_glyph_stamp(
        &self,
        ctx: &dyn Canvas2d,
        d: &GlyphStamp,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let Some(c) = d.center(proj) else { return };
        ctx.save();
        ctx.set_line_dash(&[]);
        ctx.set_font(&px_font(d.size_px(), &theme.font_family));
        set_fill(ctx, &d.base().style.line_color);
        ctx.set_text_align(TextAlign::Center);
        ctx.set_text_baseline(TextBaseline::Middle);
        ctx.fill_text(&d.glyph, c.0, c.1);
        ctx.set_text_align(TextAlign::Left);
        ctx.set_text_baseline(TextBaseline::Alphabetic);
        ctx.restore();
    }
}

/// `x || 1`: a zero (or NaN) divisor reads as one.
fn or_one(v: f64) -> f64 {
    if v == 0.0 || v.is_nan() { 1.0 } else { v }
}
