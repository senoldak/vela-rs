//! The Fibonacci and wavefront families: level tools, concentric rings, Gann arcs, the
//! spiral, the Mach figures and the Dedekind tessellation.

use std::f64::consts::PI;

use crate::drawings::types::fib::{FibAlign, FibRatios, FibSpiral, RadialFib};
use crate::drawings::types::gann::{GANN_SQUARE_ARCS, GannSquare};
use crate::drawings::types::mach::{DedekindGeom, DedekindTessellation, MachFigure};
use crate::drawings::{Drawing, Projector};
use crate::indicators::classics::math::js_max;
use crate::model::BoxTextSize;
use crate::render::canvas::{Canvas2d, TextAlign, TextBaseline};
use crate::render::shared::drawing_geometry::named_font_size;
use crate::util::js_number;

use super::helpers::{
    StrokeSpec, apply_stroke, fib_font_size, format_mach_ratio, px_font, set_fill, stroke_path,
};
use super::{DrawingPainter, DrawingTheme};

fn text_align(align: FibAlign) -> TextAlign {
    match align {
        FibAlign::Left => TextAlign::Left,
        FibAlign::Center => TextAlign::Center,
        FibAlign::Right => TextAlign::Right,
    }
}

impl DrawingPainter {
    /// Paint any Fibonacci tool from its resolved entry lines: optional fill bands, then
    /// each colored line with its auto-number and optional centered custom label (both
    /// per-tool sized).
    pub(super) fn paint_fib_ratios(
        &self,
        ctx: &dyn Canvas2d,
        d: &dyn FibRatios,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let Some(lines) = d.entry_lines(proj).filter(|l| !l.is_empty()) else {
            return;
        };
        for band in d.fill_bands(proj) {
            ctx.save();
            ctx.set_global_alpha(0.06 * ctx.global_alpha());
            set_fill(ctx, &band.color);
            ctx.fill_rect(band.x, band.y, band.w, band.h);
            ctx.restore();
        }
        ctx.set_text_baseline(TextBaseline::Middle);
        let core = d.fib_core();
        let num_font = px_font(fib_font_size(core.numbers_size), &theme.font_family);
        let lbl_font = px_font(fib_font_size(core.labels_size), &theme.font_family);
        let style = &d.base().style;
        for l in &lines {
            stroke_path(ctx, StrokeSpec::colored(style, &l.color), || {
                ctx.move_to(l.x1, l.y1);
                ctx.line_to(l.x2, l.y2);
            });
            set_fill(ctx, &l.color);
            ctx.set_font(&num_font);
            ctx.set_text_align(text_align(l.number_align));
            ctx.fill_text(&self.num(&l.number_text), l.number_x, l.number_y);
            if let Some(label) = l.label.as_deref().filter(|s| !s.is_empty()) {
                ctx.set_font(&lbl_font);
                ctx.set_text_align(TextAlign::Center);
                ctx.fill_text(label, l.label_x, l.label_y);
            }
        }
        ctx.set_text_align(TextAlign::Left);
        ctx.set_text_baseline(TextBaseline::Alphabetic);
    }

    /// Paint a concentric-ring fib tool (circles / arcs / wedge): each enabled level as an
    /// arc of radius R0 times ratio over the tool's angular sweep, plus any bounding lines
    /// and a ratio label per ring.
    pub(super) fn paint_radial_fib(
        &self,
        ctx: &dyn Canvas2d,
        d: &dyn RadialFib,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let style = &d.base().style;
        if let Some(g) = d.radial(proj).filter(|g| g.r0 >= 1.0) {
            let font = px_font(fib_font_size(d.fib_core().numbers_size), &theme.font_family);
            let mid = (g.a0 + g.a1) / 2.0;
            ctx.set_text_baseline(TextBaseline::Middle);
            for lv in &d.fib_core().levels {
                if !lv.enabled {
                    continue;
                }
                let r = g.r0 * lv.ratio;
                if r < 0.5 {
                    continue;
                }
                apply_stroke(ctx, StrokeSpec::colored(style, &lv.color));
                ctx.begin_path();
                ctx.arc(g.cx, g.cy, r, g.a0, g.a1, false);
                ctx.stroke();
                ctx.set_line_dash(&[]);
                set_fill(ctx, &lv.color);
                ctx.set_font(&font);
                ctx.set_text_align(TextAlign::Left);
                ctx.fill_text(
                    &js_number(lv.ratio),
                    g.cx + mid.cos() * r + 3.0,
                    g.cy + mid.sin() * r,
                );
            }
            ctx.set_text_baseline(TextBaseline::Alphabetic);
            ctx.set_text_align(TextAlign::Left);
        }
        for b in d.bounding_lines(proj) {
            stroke_path(ctx, style.into(), || {
                ctx.move_to(b[0], b[1]);
                ctx.line_to(b[2], b[3]);
            });
        }
    }

    /// Paint a Gann square's concentric quarter-ellipse arcs, centered on the origin corner
    /// and sized as fractions of the box (so they match its pixel aspect).
    pub(super) fn paint_gann_arcs(&self, ctx: &dyn Canvas2d, d: &GannSquare, proj: &dyn Projector) {
        let Some(g) = d
            .arc_geom(proj)
            .filter(|g| !(g.bx.abs() < 1.0 || g.py.abs() < 1.0))
        else {
            return;
        };
        let style = &d.base().style;
        const N: usize = 24; // samples per quarter
        for arc in GANN_SQUARE_ARCS.iter() {
            apply_stroke(ctx, StrokeSpec::colored(style, arc.color));
            ctx.begin_path();
            for i in 0..=N {
                let t = (i as f64 / N as f64) * (PI / 2.0);
                let x = g.ox + arc.k * g.bx * t.cos();
                let y = g.oy + arc.k * g.py * t.sin();
                if i == 0 {
                    ctx.move_to(x, y);
                } else {
                    ctx.line_to(x, y);
                }
            }
            ctx.stroke();
        }
        ctx.set_line_dash(&[]);
    }

    /// Paint a Fibonacci spiral as a sampled polyline.
    pub(super) fn paint_fib_spiral(&self, ctx: &dyn Canvas2d, d: &FibSpiral, proj: &dyn Projector) {
        let Some(pts) = d.spiral_points(proj).filter(|p| p.len() >= 2) else {
            return;
        };
        stroke_path(ctx, (&d.base().style).into(), || {
            ctx.move_to(pts[0].0, pts[0].1);
            for p in &pts[1..] {
                ctx.line_to(p.0, p.1);
            }
        });
    }

    /// Paint a Mach wavefront figure (sonic wall or supersonic cone) plus the Huygens
    /// circles.
    pub(super) fn paint_mach_figure(
        &self,
        ctx: &dyn Canvas2d,
        d: &MachFigure,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let Some(g) = d.geom(proj) else { return };
        let style = &d.base().style;
        for c in &g.circles {
            apply_stroke(ctx, StrokeSpec::colored(style, &c.color));
            ctx.begin_path();
            ctx.arc(c.cx, c.cy, c.r, 0.0, PI * 2.0, false);
            ctx.stroke();
        }
        // Envelope (shock wall or Mach-cone generators) slightly stronger, in the line color.
        apply_stroke(ctx, style.into());
        ctx.set_line_width(style.line_width + 0.5);
        for r in &g.rays {
            ctx.begin_path();
            ctx.move_to(r[0], r[1]);
            ctx.line_to(r[2], r[3]);
            ctx.stroke();
        }
        // Nose marker.
        ctx.begin_path();
        ctx.arc(
            g.nose_x,
            g.nose_y,
            js_max(2.5, style.line_width + 1.5),
            0.0,
            PI * 2.0,
            false,
        );
        set_fill(ctx, &style.line_color);
        ctx.fill();
        ctx.set_line_dash(&[]);
        // Ratio labels sit just outside each circle along the expansion direction.
        if d.show_ratios {
            ctx.set_font(&px_font(
                named_font_size(BoxTextSize::Small),
                &theme.font_family,
            ));
            ctx.set_text_baseline(TextBaseline::Middle);
            ctx.set_text_align(TextAlign::Left);
            for c in &g.circles {
                let lx = c.cx + g.fx * c.r + 4.0;
                let ly = c.cy + g.fy * c.r;
                set_fill(ctx, &c.color);
                ctx.fill_text(&format_mach_ratio(c.ratio), lx, ly);
            }
            ctx.set_text_baseline(TextBaseline::Alphabetic);
            ctx.set_text_align(TextAlign::Left);
        }
    }

    /// Paint a Dedekind tessellation: box border + clipped semicircles/verticals (modular
    /// tiling).
    pub(super) fn paint_dedekind(
        &self,
        ctx: &dyn Canvas2d,
        d: &DedekindTessellation,
        proj: &dyn Projector,
    ) {
        let (Some(b), Some(geoms)) = (d.layout(proj), d.geodesics(proj)) else {
            return;
        };
        let style = &d.base().style;
        // Light border so the user-defined range stays visible.
        ctx.save();
        apply_stroke(ctx, style.into());
        ctx.set_global_alpha(ctx.global_alpha() * 0.45);
        ctx.stroke_rect(b.left, b.top, b.right - b.left, b.bot - b.top);
        ctx.restore();
        ctx.save();
        ctx.begin_path();
        ctx.rect(b.left, b.top, b.right - b.left, b.bot - b.top);
        ctx.clip();
        apply_stroke(ctx, style.into());
        for g in &geoms {
            ctx.begin_path();
            match *g {
                DedekindGeom::VLine { x, y0, y1 } => {
                    ctx.move_to(x, y0);
                    ctx.line_to(x, y1);
                }
                // Clockwise pi to 0 on a y-down canvas sweeps the upper semicircle (into
                // the box).
                DedekindGeom::Arc { cx, cy, r } => ctx.arc(cx, cy, r, PI, 0.0, false),
            }
            ctx.stroke();
        }
        ctx.set_line_dash(&[]);
        ctx.restore();
    }
}
