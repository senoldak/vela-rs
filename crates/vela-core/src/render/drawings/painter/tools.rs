//! The data-driven and annotated tools: info line, trend angle, measure box, position,
//! regression channel, anchored VWAP, fixed-range volume profile, price-action zones,
//! liquidity and the pattern family.

use std::f64::consts::PI;

use crate::drawings::effective_color::{INVALID_FILL, VALID_FILL, effective_fill_color};
use crate::drawings::hittest::line_segment_intersection;
use crate::drawings::types::measure::{DatePriceRange, PositionTool};
use crate::drawings::types::patterns::PatternDrawing;
use crate::drawings::types::price_action::{FairValueGap, Liquidity, OrderBlock};
use crate::drawings::types::stats::{AnchoredVwap, FixedRangeVolumeProfile, RegressionChannel};
use crate::drawings::{Drawing, Projector};
use crate::i18n::{MessageArg, msg};
use crate::indicators::classics::math::{js_max, js_min, js_round};
use crate::model::{BoxTextSize, LineStyle};
use crate::palette::{BEARISH, BULLISH};
use crate::render::canvas::{Canvas2d, LineJoin, TextAlign, TextBaseline};
use crate::render::core::color::with_alpha;
use crate::render::shared::drawing_geometry::{contrast_color, dash_pattern, named_font_size};
use crate::util::{js_number, to_fixed};

use super::helpers::{
    StrokeSpec, is_transparent, px_font, round_rect, set_fill, set_stroke, stroke_path,
    stroke_polyline, to_hex6,
};
use super::{BADGE_FILL, BADGE_STROKE, DrawingPainter, DrawingTheme};

impl DrawingPainter {
    /// Paint an info line: the segment plus a midpoint box reading price change, percent
    /// and bar count (green when up, red when down).
    pub(super) fn paint_info_line(
        &self,
        ctx: &dyn Canvas2d,
        d: &dyn Drawing,
        proj: &dyn Projector,
    ) {
        let pts = d.handle_points(proj);
        if pts.len() < 2 {
            return;
        }
        let (x1, y1) = pts[0];
        let (x2, y2) = pts[1];
        stroke_path(ctx, (&d.base().style).into(), || {
            ctx.move_to(x1, y1);
            ctx.line_to(x2, y2);
        });
        let anchors = &d.base().anchors;
        let (Some(a), Some(b)) = (anchors.first(), anchors.get(1)) else {
            return;
        };
        let price_change = b.price - a.price;
        let pct = if a.price != 0.0 {
            (price_change / a.price) * 100.0
        } else {
            0.0
        };
        let bars = proj
            .bars_between(a.time, b.time)
            .map_or(0.0, |n| js_round(n).abs());
        let sign = if price_change >= 0.0 { "+" } else { "" };
        let message = msg(
            "drawing.infoline.measure",
            "{change} ({percent}%)  {count, plural, one {{bars} bar} other {{bars} bars}}",
        );
        let txt = self.t_with(
            &message,
            &[
                (
                    "change",
                    MessageArg::from(self.num(&format!("{sign}{}", to_fixed(price_change, 2)))),
                ),
                (
                    "percent",
                    MessageArg::from(self.num(&format!("{sign}{}", to_fixed(pct, 2)))),
                ),
                ("count", MessageArg::from(bars)),
                ("bars", MessageArg::from(self.num(&js_number(bars)))),
            ],
        );
        let mx = (x1 + x2) / 2.0;
        let my = (y1 + y2) / 2.0;
        ctx.set_font("11px monospace");
        let bw = ctx.measure_text(&txt).width + 12.0;
        let bh = 18.0;
        let bx = mx - 2.0;
        let by = my - bh - 4.0; // sits just above the midpoint
        ctx.save();
        ctx.set_global_alpha(0.9 * ctx.global_alpha());
        round_rect(ctx, bx, by, bw, bh, 3.0);
        set_fill(ctx, BADGE_FILL);
        ctx.fill();
        set_stroke(ctx, BADGE_STROKE);
        ctx.set_line_width(1.0);
        ctx.stroke();
        ctx.restore();
        set_fill(
            ctx,
            if price_change >= 0.0 {
                BULLISH
            } else {
                BEARISH
            },
        );
        ctx.set_text_align(TextAlign::Left);
        ctx.set_text_baseline(TextBaseline::Middle);
        ctx.fill_text(&txt, bx + 6.0, by + bh / 2.0);
        ctx.set_text_baseline(TextBaseline::Alphabetic);
    }

    /// Paint a trend angle: the segment plus a faint dashed baseline, an arc, and a degree
    /// readout at the start anchor. The angle is the on-screen (pixel) inclination, so it
    /// changes with zoom.
    pub(super) fn paint_trend_angle(
        &self,
        ctx: &dyn Canvas2d,
        d: &dyn Drawing,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let pts = d.handle_points(proj);
        if pts.len() < 2 {
            return;
        }
        let (x1, y1) = pts[0];
        let (x2, y2) = pts[1];
        let style = &d.base().style;
        stroke_path(ctx, style.into(), || {
            ctx.move_to(x1, y1);
            ctx.line_to(x2, y2);
        });
        let dx = x2 - x1;
        let dy = y2 - y1;
        let angle_rad = (-dy).atan2(dx); // screen Y is inverted: negate for a natural angle
        let angle_deg = angle_rad * (180.0 / PI);
        let arc_r = js_min(30.0, dx.hypot(dy) * 0.3);
        let color = &style.line_color;
        // Faint dashed horizontal baseline (the 0 degree reference).
        ctx.save();
        ctx.set_global_alpha(0.4 * ctx.global_alpha());
        set_stroke(ctx, color);
        ctx.set_line_width(1.0);
        ctx.set_line_dash(&[4.0, 4.0]);
        ctx.begin_path();
        ctx.move_to(x1, y1);
        ctx.line_to(x1 + js_max(dx.abs(), arc_r + 20.0), y1);
        ctx.stroke();
        ctx.restore();
        ctx.set_line_dash(&[]);
        // The angle arc, swept from the baseline to the line.
        if arc_r > 1.0 {
            let end = -angle_rad; // canvas-space angle of the line
            set_stroke(ctx, color);
            ctx.set_line_width(1.5);
            ctx.begin_path();
            ctx.arc(x1, y1, arc_r, js_min(0.0, end), js_max(0.0, end), false);
            ctx.stroke();
        }
        // The readout sits on the side of the baseline the line leaves empty (under it for
        // a rising line, over it for a falling one) so the line never runs through the
        // digits.
        let label = format!("{}\u{b0}", self.num(&to_fixed(angle_deg, 1)));
        let lx = x1 + arc_r + 6.0;
        let ly = if dy < 0.0 { y1 + 14.0 } else { y1 - 6.0 };
        ctx.set_font(&px_font(11.0, &theme.font_family));
        ctx.set_text_align(TextAlign::Left);
        ctx.set_text_baseline(TextBaseline::Alphabetic);
        ctx.save();
        ctx.set_line_join(LineJoin::Round);
        ctx.set_line_width(3.0);
        set_stroke(ctx, &theme.background);
        ctx.stroke_text(&label, lx, ly); // a halo in the plot's color keeps the digits legible over candles
        ctx.restore();
        set_fill(ctx, color);
        ctx.fill_text(&label, lx, ly);
    }

    /// Paint a measurement box: a direction-tinted (green up / red down) shaded rect with a
    /// centered, multi-line label of the computed price/time deltas.
    pub(super) fn paint_measure_box(
        &self,
        ctx: &dyn Canvas2d,
        d: &DatePriceRange,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let pts = d.handle_points(proj);
        if pts.len() < 2 {
            return;
        }
        let x = js_min(pts[0].0, pts[1].0);
        let y = js_min(pts[0].1, pts[1].1);
        let w = (pts[1].0 - pts[0].0).abs();
        let h = (pts[1].1 - pts[0].1).abs();
        let color = if d.is_up() { BULLISH } else { BEARISH };
        let style = &d.base().style;
        ctx.save();
        ctx.set_global_alpha(0.12 * ctx.global_alpha());
        set_fill(ctx, color);
        ctx.fill_rect(x, y, w, h);
        ctx.restore();
        ctx.set_line_dash(&dash_pattern(style.line_style, style.line_width));
        set_stroke(ctx, color);
        ctx.set_line_width(style.line_width);
        ctx.stroke_rect(x, y, w, h);
        ctx.set_line_dash(&[]);
        let lines = d.measure_label(proj);
        let text = d.base().text.as_ref();
        let fs = named_font_size(text.map_or(BoxTextSize::Normal, |t| t.size));
        set_fill(
            ctx,
            text.and_then(|t| t.color.as_deref())
                .unwrap_or(&theme.text_color),
        );
        ctx.set_font(&px_font(fs, &theme.font_family));
        ctx.set_text_align(TextAlign::Center);
        ctx.set_text_baseline(TextBaseline::Middle);
        let lh = js_round(fs * 1.35);
        let start_y = y + h / 2.0 - ((lines.len() as f64 - 1.0) * lh) / 2.0;
        for (i, line) in lines.iter().enumerate() {
            ctx.fill_text(line, x + w / 2.0, start_y + i as f64 * lh);
        }
        ctx.set_text_align(TextAlign::Left);
        ctx.set_text_baseline(TextBaseline::Alphabetic);
    }

    /// Paint a position tool: a reward zone (entry to target) and risk zone (entry to stop)
    /// in the tool's own zone colors, the three level lines, and the direction / R:R / % /
    /// loss+size labels (styled by `text`, hidden wholesale by `show_text`).
    pub(super) fn paint_position(
        &self,
        ctx: &dyn Canvas2d,
        d: &PositionTool,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let Some(layout) = d.layout(proj) else { return };
        let (x1, x2, ey, sy, ty) = (layout.x1, layout.x2, layout.ey, layout.sy, layout.ty);
        let w = x2 - x1;
        let zone = |ya: f64, yb: f64, color: &str| {
            ctx.save();
            ctx.set_global_alpha(0.13 * ctx.global_alpha());
            set_fill(ctx, color);
            ctx.fill_rect(x1, js_min(ya, yb), w, (yb - ya).abs());
            ctx.restore();
        };
        zone(ey, ty, &d.profit_color); // reward
        zone(ey, sy, &d.loss_color); // risk
        let style = &d.base().style;
        let line = |y: f64, color: &str| {
            stroke_path(ctx, StrokeSpec::colored(style, color), || {
                ctx.move_to(x1, y);
                ctx.line_to(x2, y);
            });
        };
        line(ty, &d.profit_color);
        line(sy, &d.loss_color);
        line(ey, &style.line_color); // entry: accent
        if !d.show_text {
            return;
        }
        let text = d.base().text.as_ref();
        let fs = named_font_size(text.map_or(BoxTextSize::Normal, |t| t.size));
        set_fill(
            ctx,
            text.and_then(|t| t.color.as_deref())
                .unwrap_or(&theme.text_color),
        );
        ctx.set_text_align(TextAlign::Center);
        ctx.set_text_baseline(TextBaseline::Middle);
        ctx.set_font(&px_font(fs, &theme.font_family));
        let cx = x1 + w / 2.0;
        // Header + loss/size stack above the box (whichever is shown alone takes the bottom
        // slot); target/stop % stay centered in their zones.
        let top_y = js_min(js_min(ey, sy), ty) - 9.0;
        if d.show_header {
            let y = if d.show_loss_size {
                top_y - js_round(fs * 1.15)
            } else {
                top_y
            };
            ctx.fill_text(&d.header_label(), cx, y);
        }
        ctx.set_font(&px_font(js_max(10.0, fs - 1.0), &theme.font_family));
        if d.show_loss_size {
            ctx.fill_text(&d.loss_size_label(), cx, top_y);
        }
        if d.show_target_label {
            ctx.fill_text(&d.target_label(), cx, (ey + ty) / 2.0);
        }
        if d.show_stop_label {
            ctx.fill_text(&d.stop_label(), cx, (ey + sy) / 2.0);
        }
        ctx.set_text_align(TextAlign::Left);
        ctx.set_text_baseline(TextBaseline::Alphabetic);
    }

    /// Paint a linear-regression channel: the two shaded bands (mid to upper, mid to lower),
    /// the three lines (upper/mid/lower, each with its own color and dash), and the R²
    /// readout at the start.
    pub(super) fn paint_regression_channel(
        &self,
        ctx: &dyn Canvas2d,
        d: &RegressionChannel,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let Some(l) = d.layout(proj) else { return };
        let s = &d.reg;
        let quad = |color: &str, y0: f64, y1: f64, yy0: f64, yy1: f64| {
            ctx.save();
            set_fill(ctx, color);
            ctx.begin_path();
            ctx.move_to(l.x0, y0);
            ctx.line_to(l.x1, y1);
            ctx.line_to(l.x1, yy1);
            ctx.line_to(l.x0, yy0);
            ctx.close_path();
            ctx.fill();
            ctx.restore();
        };
        quad(&s.upper_fill, l.mid_y0, l.mid_y1, l.upper_y0, l.upper_y1); // mid to upper (green by default)
        quad(&s.lower_fill, l.mid_y0, l.mid_y1, l.lower_y0, l.lower_y1); // mid to lower (red by default)
        let width = d.base().style.line_width;
        let line = |color: &str, line_style: LineStyle, y0: f64, y1: f64| {
            stroke_path(ctx, StrokeSpec::new(color, width, line_style), || {
                ctx.move_to(l.x0, y0);
                ctx.line_to(l.x1, y1);
            });
        };
        line(&s.upper_color, s.upper_style, l.upper_y0, l.upper_y1);
        line(&s.lower_color, s.lower_style, l.lower_y0, l.lower_y1);
        line(&s.mid_color, s.mid_style, l.mid_y0, l.mid_y1);
        if l.show_r2 {
            let txt = format!("R\u{b2} {}%", js_number(js_round(l.r2 * 100.0)));
            ctx.set_font(&px_font(11.0, &theme.font_family));
            let tw = ctx.measure_text(&txt).width;
            let pad = 5.0;
            let gap = 4.0;
            let box_w = tw + pad * 2.0;
            let bx = l.x0 - gap - box_w; // badge sits left of the channel start, not inside it
            let by = l.mid_y0 - 9.0;
            ctx.save();
            ctx.set_global_alpha(0.9 * ctx.global_alpha());
            round_rect(ctx, bx, by - 8.0, box_w, 16.0, 3.0);
            set_fill(ctx, BADGE_FILL);
            ctx.fill();
            ctx.restore();
            set_fill(ctx, &theme.text_color);
            ctx.set_text_align(TextAlign::Left);
            ctx.set_text_baseline(TextBaseline::Middle);
            ctx.fill_text(&txt, bx + pad, by);
            ctx.set_text_baseline(TextBaseline::Alphabetic);
        }
    }

    /// Paint a fixed-range volume profile: horizontal histogram rows (up/down split)
    /// anchored to the left or right of the time span, optional VAH / VAL / POC levels
    /// across the range, and optional developing POC / VA polylines. Recomputes from the two
    /// anchors on every paint.
    pub(super) fn paint_fixed_range_vp(
        &self,
        ctx: &dyn Canvas2d,
        d: &FixedRangeVolumeProfile,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let Some(l) = d.layout(proj) else { return };
        let s = &d.frvp;
        let profile = &l.profile;
        if profile.max_total <= 0.0 || l.max_w <= 0.0 {
            return;
        }
        for (k, row) in profile.rows.iter().enumerate() {
            let total = row.up + row.down;
            if total <= 0.0 {
                continue;
            }
            // y_edges[k] = lower price bound; canvas y grows downward so the top edge is the
            // higher index.
            let ya = l.y_edges.get(k).copied().unwrap_or(f64::NAN);
            let yb = l.y_edges.get(k + 1).copied().unwrap_or(f64::NAN);
            let y_top = js_min(ya, yb);
            let h = js_max(1.0, (yb - ya).abs() - 1.0);
            let w = (total / profile.max_total) * l.max_w;
            let up_w = (row.up / total) * w;
            let in_va = k >= profile.va_from && k <= profile.va_to;
            let up_color = if in_va { &s.va_up_color } else { &s.up_color };
            let down_color = if in_va {
                &s.va_down_color
            } else {
                &s.down_color
            };
            let x0 = if l.grow == 1 {
                l.anchor_x
            } else {
                l.anchor_x - w
            };
            if up_w > 0.0 {
                set_fill(ctx, up_color);
                ctx.fill_rect(x0, y_top, up_w, h);
            }
            if w - up_w > 0.0 {
                set_fill(ctx, down_color);
                ctx.fill_rect(x0 + up_w, y_top, w - up_w, h);
            }
        }

        let w = d.base().style.line_width;
        let h_line = |show: bool, color: &str, style: LineStyle, y: Option<f64>| {
            let Some(y) = y.filter(|_| show && !is_transparent(color)) else {
                return;
            };
            stroke_path(ctx, StrokeSpec::new(color, w, style), || {
                ctx.move_to(l.x0, y);
                ctx.line_to(l.x1, y);
            });
        };
        h_line(s.show_vah, &s.vah_color, s.vah_style, l.vah_y);
        h_line(s.show_val, &s.val_color, s.val_style, l.val_y);
        // No explicit POC ink: the theme's contrast color, resolved per paint so the line
        // follows theme switches live (white on dark, black on light).
        let poc_color = match &s.poc_color {
            Some(c) => c.as_str(),
            None => contrast_color(Some(&theme.background)),
        };
        h_line(s.show_poc, poc_color, s.poc_style, l.poc_y);

        // Developing levels sit on top of the histogram so they stay readable.
        let dev_w = js_max(1.0, w);
        if s.show_developing_poc {
            stroke_polyline(
                ctx,
                &l.developing_poc,
                StrokeSpec::new(&s.developing_poc_color, dev_w, s.developing_poc_style),
            );
        }
        if s.show_developing_va {
            let dev = StrokeSpec::new(&s.developing_va_color, dev_w, s.developing_va_style);
            stroke_polyline(ctx, &l.developing_va_high, dev);
            stroke_polyline(ctx, &l.developing_va_low, dev);
        }
    }

    /// Paint an anchored VWAP: the shaded band between the upper/lower sigma curves, the
    /// two band edges (transparent by default), then the VWAP midline on top. All three are
    /// polylines that recompute from the anchor time forward, so the shape follows the bars
    /// live.
    pub(super) fn paint_anchored_vwap(
        &self,
        ctx: &dyn Canvas2d,
        d: &AnchoredVwap,
        proj: &dyn Projector,
    ) {
        let Some(l) = d.layout(proj) else { return };
        let s = &d.vwap;
        let w = d.base().style.line_width;
        if l.upper.len() >= 2 && !s.band_fill.is_empty() {
            ctx.save();
            set_fill(ctx, &s.band_fill); // opacity lives in the fill color's own alpha
            ctx.begin_path();
            ctx.move_to(l.upper[0].0, l.upper[0].1);
            for p in &l.upper[1..] {
                ctx.line_to(p.0, p.1);
            }
            for p in l.lower.iter().rev() {
                ctx.line_to(p.0, p.1);
            }
            ctx.close_path();
            ctx.fill();
            ctx.restore();
        }
        stroke_polyline(
            ctx,
            &l.upper,
            StrokeSpec::new(&s.upper_color, w, s.upper_style),
        );
        stroke_polyline(
            ctx,
            &l.lower,
            StrokeSpec::new(&s.lower_color, w, s.lower_style),
        );
        stroke_polyline(ctx, &l.mid, StrokeSpec::new(&s.mid_color, w, s.mid_style));
    }

    /// Paint a price-action zone (fair value gap, order block): the box and its halfway
    /// line, quieter once it has run its course, a dot where it stopped, and, once price
    /// flips it, the dashed box it carries on as. With no zone under the anchor, a faint
    /// tick marks it.
    pub(super) fn paint_price_zone(
        &self,
        ctx: &dyn Canvas2d,
        d: &dyn Drawing,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let any = d.as_any();
        let zone = if let Some(f) = any.downcast_ref::<FairValueGap>() {
            f.zone(proj)
        } else {
            any.downcast_ref::<OrderBlock>().and_then(|o| o.zone(proj))
        };
        let Some(z) = zone else {
            let Some(a) = d.base().anchors.first() else {
                return;
            };
            let x = js_round(proj.x_of(a.time)) + 0.5;
            let faint = with_alpha(&theme.text_color, 0.35);
            stroke_path(ctx, StrokeSpec::new(&faint, 1.0, LineStyle::Dashed), || {
                ctx.move_to(x, 0.0);
                ctx.line_to(x, proj.height());
            });
            return;
        };
        let h = z.y_bottom - z.y_top;
        let stroke = to_hex6(&z.fill, &z.fill); // the fill's hue, opaque
        ctx.save();
        ctx.set_global_alpha(ctx.global_alpha() * if z.faded { 0.6 } else { 1.0 });
        set_fill(ctx, &z.fill);
        ctx.fill_rect(z.x1, z.y_top, z.x2 - z.x1, h);
        if z.border {
            ctx.set_line_width(1.0);
            ctx.set_line_dash(&[]);
            set_stroke(ctx, &stroke);
            ctx.stroke_rect(
                js_round(z.x1) + 0.5,
                js_round(z.y_top) + 0.5,
                js_round(z.x2 - z.x1),
                js_round(h),
            );
        }
        if let Some(mid_y) = z.mid_y {
            let y = js_round(mid_y) + 0.5;
            stroke_path(
                ctx,
                StrokeSpec::new(&stroke, 1.0, LineStyle::Dashed),
                || {
                    ctx.move_to(z.x1, y);
                    ctx.line_to(z.x2, y);
                },
            );
        }
        ctx.restore();
        if let Some(f) = &z.flip {
            ctx.save();
            set_fill(ctx, &f.fill);
            ctx.fill_rect(f.x1, z.y_top, f.x2 - f.x1, h);
            ctx.set_line_width(1.0);
            set_stroke(ctx, &to_hex6(&f.fill, &f.fill));
            ctx.set_line_dash(&[5.0, 4.0]);
            ctx.stroke_rect(
                js_round(f.x1) + 0.5,
                js_round(z.y_top) + 0.5,
                js_round(f.x2 - f.x1),
                js_round(h),
            );
            ctx.restore();
        }
        if let Some(m) = &z.marker {
            ctx.begin_path();
            ctx.arc(m.x, m.y, 3.5, 0.0, PI * 2.0, false);
            set_fill(ctx, &stroke);
            ctx.fill();
        }
    }

    /// Paint a liquidity level: the line to where it stopped, and a cross where it was
    /// swept.
    pub(super) fn paint_liquidity(&self, ctx: &dyn Canvas2d, d: &Liquidity, proj: &dyn Projector) {
        let Some(l) = d.line(proj) else { return };
        let style = &d.base().style;
        let y = js_round(l.y) + 0.5;
        stroke_path(ctx, style.into(), || {
            ctx.move_to(l.x1, y);
            ctx.line_to(l.x2, y);
        });
        if let Some(sweep) = l.sweep {
            let (x, sy) = (sweep.x, sweep.y);
            stroke_path(
                ctx,
                StrokeSpec::new(&style.line_color, 1.75, LineStyle::Solid),
                || {
                    ctx.move_to(x - 4.0, sy - 4.0);
                    ctx.line_to(x + 4.0, sy + 4.0);
                    ctx.move_to(x + 4.0, sy - 4.0);
                    ctx.line_to(x - 4.0, sy + 4.0);
                },
            );
        }
    }

    /// Paint a pattern: optional tinted body triangles, the labelled polyline, an extended
    /// neckline (head and shoulders), and consecutive-leg ratios (harmonic patterns).
    pub(super) fn paint_pattern(
        &self,
        ctx: &dyn Canvas2d,
        d: &PatternDrawing,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let pts = d.handle_points(proj);
        if pts.len() < 2 {
            return;
        }
        let style = &d.base().style;
        let valid = d.valid(); // harmonic validity (None for plain patterns)
        // Shared resolution (so the settings swatch matches): an explicit fill color wins,
        // else the validity tint / line-color wash. The user color carries its own alpha;
        // the wash stays light.
        let fill_color = effective_fill_color(d, &theme.fill_theme())
            .unwrap_or_else(|| style.line_color.clone());
        let fill_alpha = if super::helpers::fill_of(style).is_some() {
            1.0
        } else {
            0.1
        };
        for &[i, j, k] in d.fill_triangles() {
            let (Some(&a), Some(&b), Some(&c)) = (pts.get(i), pts.get(j), pts.get(k)) else {
                continue;
            };
            ctx.save();
            ctx.set_global_alpha(fill_alpha * ctx.global_alpha());
            set_fill(ctx, &fill_color);
            ctx.begin_path();
            ctx.move_to(a.0, a.1);
            ctx.line_to(b.0, b.1);
            ctx.line_to(c.0, c.1);
            ctx.close_path();
            ctx.fill();
            ctx.restore();
        }
        stroke_path(ctx, style.into(), || {
            ctx.move_to(pts[0].0, pts[0].1);
            for p in &pts[1..] {
                ctx.line_to(p.0, p.1);
            }
        });
        if let Some([ia, ib]) = d.neckline_indices() {
            let n = pts.len();
            if let (Some(&a), Some(&b)) = (pts.get(ia), pts.get(ib)) {
                // Clip the neckline to where it crosses the first (start to LS) and last
                // (RS to end) legs, falling back to the troughs themselves when a leg does
                // not reach the neckline.
                let left = line_segment_intersection(
                    a.0, a.1, b.0, b.1, pts[0].0, pts[0].1, pts[1].0, pts[1].1,
                )
                .unwrap_or(a);
                let right = line_segment_intersection(
                    a.0,
                    a.1,
                    b.0,
                    b.1,
                    pts[n - 2].0,
                    pts[n - 2].1,
                    pts[n - 1].0,
                    pts[n - 1].1,
                )
                .unwrap_or(b);
                stroke_path(ctx, StrokeSpec::styled(style, LineStyle::Dashed), || {
                    ctx.move_to(left.0, left.1);
                    ctx.line_to(right.0, right.1);
                });
            }
        }
        let labels = d.vertex_labels();
        set_fill(ctx, &theme.text_color);
        ctx.set_font(&format!("bold 12px {}", theme.font_family));
        ctx.set_text_align(TextAlign::Center);
        for (i, p) in pts.iter().enumerate() {
            let Some(lbl) = labels.get(i) else { continue };
            if matches!(lbl, crate::i18n::MessageLike::Text(t) if t.is_empty()) {
                continue;
            }
            let prev_y = if i > 0 { pts[i - 1].1 } else { p.1 };
            let next_y = pts.get(i + 1).map_or(p.1, |n| n.1);
            let ny = (prev_y + next_y) / 2.0;
            let above = p.1 <= ny; // a local peak: label above, a trough: below
            ctx.set_text_baseline(if above {
                TextBaseline::Bottom
            } else {
                TextBaseline::Top
            });
            ctx.fill_text(&self.t(lbl), p.0, if above { p.1 - 7.0 } else { p.1 + 7.0 });
        }
        if d.leg_ratios() {
            ctx.set_font(&px_font(11.0, &theme.font_family));
            ctx.set_text_align(TextAlign::Left);
            ctx.set_text_baseline(TextBaseline::Middle);
            for i in 2..pts.len() {
                let (a, b) = (pts[i - 1], pts[i]);
                let Some(r) = d.ratio_at(i) else { continue };
                let ok = d.ratio_ok(i); // green in-band / red out / theme color when no rule
                set_fill(
                    ctx,
                    match ok {
                        Some(true) => VALID_FILL,
                        Some(false) => INVALID_FILL,
                        None => &theme.text_color,
                    },
                );
                ctx.fill_text(
                    &self.num(&to_fixed(r, 3)),
                    (a.0 + b.0) / 2.0 + 6.0,
                    (a.1 + b.1) / 2.0,
                );
            }
        }
        // Harmonic name + check/cross badge at the completion point (D).
        if let Some(name) = d.pattern_name() {
            let d_pt = pts[pts.len() - 1];
            let prev = pts[pts.len() - 2];
            let above = d_pt.1 <= prev.1; // D a peak: label above, else below
            ctx.set_font(&format!("bold 12px {}", theme.font_family));
            set_fill(
                ctx,
                if valid == Some(false) {
                    INVALID_FILL
                } else {
                    VALID_FILL
                },
            );
            ctx.set_text_align(TextAlign::Center);
            ctx.set_text_baseline(if above {
                TextBaseline::Bottom
            } else {
                TextBaseline::Top
            });
            let mark = if valid == Some(true) {
                '\u{2713}'
            } else {
                '\u{2717}'
            };
            ctx.fill_text(
                &format!("{} {mark}", self.t(&name)),
                d_pt.0,
                if above { d_pt.1 - 22.0 } else { d_pt.1 + 22.0 },
            );
        }
        ctx.set_text_align(TextAlign::Left);
        ctx.set_text_baseline(TextBaseline::Alphabetic);
    }
}
