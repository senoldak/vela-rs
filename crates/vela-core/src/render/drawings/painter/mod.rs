//! Paints the user's drawing tools onto a canvas: the port of the native renderer's
//! `DrawingPainter`.
//!
//! The caller has already applied the device-pixel-ratio transform and cleared the canvas,
//! so everything here is in media px. [`DrawingPainter::paint_all`] dispatches by tool;
//! geometry comes from each tool's own pure methods through a [`Projector`]. Strokes and
//! fonts reuse the shared Pine-drawing helpers so user drawings match engine-drawn ones.
//!
//! The canvas call stream (calls, order, arguments, redundant ones included) follows the
//! original line by line; a check replays the original on a recording context and compares.
//!
//! Inputs the original reads from its host, here parameters: the theme
//! ([`DrawingTheme`], three strings of the chart theme), the translator
//! ([`DrawingPainter::i18n`]), the chart's series look ([`DrawingPainter::series_look`]) and
//! the chart-type registry the magnifier resolves bar transforms through
//! ([`DrawingPainter::chart_types`]).
//!
//! Differences from the original, all forced by the model:
//! - The measure and position tools produce English label text themselves; the painter
//!   uses it as is (the original passes its translator in).
//! - A [`Magnifier`] cannot be written through `&dyn Drawing`, so the chip rectangle the
//!   original stores on the drawing is kept by the painter: read it with
//!   [`DrawingPainter::chip_rect`].
//! - [`Projector::pane_rect`] answers `None` both for "no pane geometry" and "pane gone";
//!   the painter tells them apart by asking `y_of` for the pane.

mod fib;
mod helpers;
mod labels;
mod magnifier;
mod tools;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;
use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value;

use crate::chart_types::ChartTypeRegistry;
use crate::drawings::effective_color::EffectiveColorTheme;
use crate::drawings::types::annotations::{
    ArrowMark, CalloutKind, CalloutTool, GlyphStamp, PinnedKind, PinnedLabel,
};
use crate::drawings::types::channels::{
    DisjointChannel, FlatTopBottom, ParallelChannel, Pitchfork,
};
use crate::drawings::types::fib::{
    FibArcs, FibChannel, FibCircles, FibExtension, FibExtensionTrend, FibFan, FibRatios,
    FibRetracement, FibSpeedFan, FibSpiral, FibTimeZones, FibWedge, RadialFib, TrendFibTime,
};
use crate::drawings::types::gann::{GannBox, GannFan, GannSquare};
use crate::drawings::types::lines::TrendLine;
use crate::drawings::types::mach::{DedekindTessellation, MachFigure};
use crate::drawings::types::measure::{DatePriceRange, Magnifier, PositionTool};
use crate::drawings::types::patterns::PatternDrawing;
use crate::drawings::types::price_action::{FairValueGap, Liquidity, OrderBlock};
use crate::drawings::types::shapes::{
    Arc as ArcShape, BoxDrawing, Curve, Freehand, Highlighter, Path, Polyline, RotatedRect,
    Triangle,
};
use crate::drawings::types::stats::{AnchoredVwap, FixedRangeVolumeProfile, RegressionChannel};
use crate::drawings::{Bounds, Drawing, Projector, SegmentGeometry};
use crate::i18n::{MessageArg, MessageLike, Translator, english_translator, text_of};
use crate::model::DrawingExtend;
use crate::palette::{BEARISH, BULLISH, NEUTRAL, SLATE, SLATE_DEEP};
use crate::render::canvas::{Canvas2d, LineCap, LineJoin, TextAlign, TextBaseline};
use crate::render::core::color::with_alpha;
use crate::render::shared::drawing_geometry::{
    extend_endpoints, label_line_height, named_font_size,
};

pub use helpers::midline_segments;
use helpers::{
    Pt, StrokeSpec, apply_stroke, fill_of, px_font, round_rect, set_fill, set_stroke, stroke_path,
};

/// Px radius of the round drag handles.
const HANDLE_RADIUS: f64 = 4.5;
/// Handle chrome is fixed (not the drawing's line color) so tools with atypical accents,
/// regression gray or FRVP green, still match every other drawing's anchors.
const HANDLE_BORDER: &str = crate::drawings::DEFAULT_DRAWING_COLOR;
const GHOST_ALPHA: f64 = 0.7;
/// Info badges (regression R², measure deltas) float over chart content of any color, so
/// they keep a fixed dark plate instead of a themed surface.
const BADGE_FILL: &str = SLATE_DEEP;
const BADGE_STROKE: &str = SLATE;

/// The slice of the chart theme the painter reads (the original's `VelaTheme` fields
/// `background`, `textColor`, `fontFamily`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrawingTheme {
    pub background: String,
    pub text_color: String,
    pub font_family: String,
}

impl DrawingTheme {
    /// Read the three fields off a theme value; a missing field is empty.
    pub fn from_value(theme: &Value) -> Self {
        let field = |key: &str| {
            theme
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Self {
            background: field("background"),
            text_color: field("textColor"),
            font_family: field("fontFamily"),
        }
    }

    fn fill_theme(&self) -> EffectiveColorTheme {
        EffectiveColorTheme {
            background: self.background.clone(),
        }
    }
}

/// The frame's transient interaction state, as the painter needs it: handles show for the
/// selected, dragged and hovered drawings; a targeted text label wears its frame; a label
/// owned by an open inline editor is left unpainted so the two do not draw on top of each
/// other.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaintTargets {
    pub selected: HashSet<String>,
    pub hovered: Option<String>,
    pub dragged: Option<String>,
    pub muted_label: Option<String>,
}

/// The ids whose handles show for `targets`: selected, dragged and hovered, minus the muted
/// label (while an inline editor owns the label the user is typing, not dragging, so the
/// anchor grip on the editor's corner would read as a resize handle on a floating window).
pub fn handle_ids_for(targets: &PaintTargets) -> HashSet<String> {
    let mut ids = targets.selected.clone();
    for id in [&targets.dragged, &targets.hovered].into_iter().flatten() {
        if !id.is_empty() {
            ids.insert(id.clone());
        }
    }
    if let Some(muted) = targets.muted_label.as_deref().filter(|m| !m.is_empty()) {
        ids.remove(muted);
    }
    ids
}

/// The chart's active series look: style plus resolved series colors. The magnifier's inset
/// mirrors both: candles, bars, line or area restyle the paint (bar-transform styles like
/// Heikin Ashi transform the fetched bars; unknown styles fall back to candles), and the
/// colors default to the main series' own so the inset reads as a finer copy of the chart.
#[derive(Clone, Debug, PartialEq)]
pub struct SeriesLook {
    pub style: String,
    pub up_color: String,
    pub down_color: String,
    pub line_color: String,
}

impl Default for SeriesLook {
    fn default() -> Self {
        Self {
            style: "candles".to_string(),
            up_color: BULLISH.to_string(),
            down_color: BEARISH.to_string(),
            line_color: BULLISH.to_string(),
        }
    }
}

/// Paints user drawings (see the module doc).
#[derive(Default)]
pub struct DrawingPainter {
    /// The current `paint_all` call's interaction state, visible to the per-type painters.
    targets: PaintTargets,
    /// The chart's translator: labels painted on drawings read in its language and their
    /// numbers in its digits. `None` is English.
    pub i18n: Option<Rc<dyn Translator>>,
    /// The chart's series look, pushed before each paint.
    pub series_look: SeriesLook,
    /// The registry the magnifier resolves a bar-transform style through; without one every
    /// style paints its raw bars.
    pub chart_types: Option<Arc<ChartTypeRegistry>>,
    /// Pixel rect of each painted magnifier's timeframe chip (caret included) as of the last
    /// paint, by drawing id; `None` when the chip was not painted.
    chip_rects: HashMap<String, Option<Bounds>>,
}

impl DrawingPainter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The timeframe chip rect the last paint of magnifier `id` left (what the original
    /// stores on the drawing as `chipRect`).
    pub fn chip_rect(&self, id: &str) -> Option<Bounds> {
        self.chip_rects.get(id).copied().flatten()
    }

    fn translator(&self) -> &dyn Translator {
        match &self.i18n {
            Some(t) => t.as_ref(),
            None => english_translator(),
        }
    }

    fn t(&self, message: &MessageLike) -> String {
        text_of(self.i18n.as_deref(), message, &[])
    }

    fn t_with(&self, message: &MessageLike, values: &[(&str, MessageArg)]) -> String {
        text_of(self.i18n.as_deref(), message, values)
    }

    /// ASCII number text (`0.618 (123.45)`) in the chart's digits and separators.
    fn num(&self, text: &str) -> String {
        crate::i18n::localize_number_text(text, self.i18n.as_deref())
    }

    /// Paint every visible drawing, then selection handles for the targeted ones. Each
    /// drawing is clipped to its own pane's rect (and skipped entirely while that pane is
    /// hidden: collapsed, or zeroed by another pane's maximize) so panes stay separated.
    pub fn paint_all(
        &mut self,
        ctx: &dyn Canvas2d,
        drawings: &[&dyn Drawing],
        proj: &dyn Projector,
        theme: &DrawingTheme,
        targets: &PaintTargets,
    ) {
        self.targets = targets.clone();
        for &d in drawings {
            if !d.base().visible {
                continue;
            }
            self.paint_clipped(ctx, d, proj, |this| this.paint_one(ctx, d, proj, theme));
        }
        self.targets = PaintTargets::default();
        self.paint_highlights(ctx, drawings, proj, &handle_ids_for(targets));
    }

    /// Selection handles alone, for drawings whose body was painted elsewhere: the ones
    /// sent behind the series paint on the layer under the data, and handles left down
    /// there would be buried under the candles.
    pub fn paint_highlights(
        &mut self,
        ctx: &dyn Canvas2d,
        drawings: &[&dyn Drawing],
        proj: &dyn Projector,
        highlight_ids: &HashSet<String>,
    ) {
        if highlight_ids.is_empty() {
            return;
        }
        for &d in drawings {
            if d.base().visible && highlight_ids.contains(&d.base().id) {
                self.paint_clipped(ctx, d, proj, |this| {
                    this.paint_handles(ctx, &d.handle_points(proj));
                });
            }
        }
    }

    /// Run `paint` clipped to the drawing's pane rect; a hidden pane (height 0) or a gone one
    /// paints nothing.
    fn paint_clipped(
        &mut self,
        ctx: &dyn Canvas2d,
        d: &dyn Drawing,
        proj: &dyn Projector,
        paint: impl FnOnce(&mut Self),
    ) {
        let pane_id = &d.base().pane_id;
        let Some(rect) = proj.pane_rect(pane_id) else {
            // No pane geometry from the projector: unclipped. A pane that is gone: nothing.
            if proj.y_of(0.0, pane_id).is_some() {
                paint(self);
            }
            return;
        };
        if rect.height <= 0.0 {
            return;
        }
        ctx.save();
        ctx.begin_path();
        ctx.rect(0.0, rect.top, proj.width(), rect.height);
        ctx.clip();
        paint(self);
        ctx.restore();
    }

    /// Paint the in-progress drawing (during placing) at reduced opacity.
    pub fn paint_ghost(
        &mut self,
        ctx: &dyn Canvas2d,
        ghost: &dyn Drawing,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        ctx.set_global_alpha(GHOST_ALPHA);
        let any = ghost.as_any();
        // Two-time-anchor tools (regression channel, fixed-range VP) preview their TIME SPAN
        // while placing (two vertical guides + a connecting line) rather than the finished
        // shape.
        if let Some(reg) = any.downcast_ref::<RegressionChannel>() {
            let color = if reg.reg.mid_color.is_empty() {
                NEUTRAL
            } else {
                reg.reg.mid_color.as_str()
            };
            self.paint_time_span_ghost(ctx, ghost, proj, color);
        } else if any.is::<FixedRangeVolumeProfile>() {
            self.paint_time_span_ghost(ctx, ghost, proj, NEUTRAL);
        } else if let Some(m) = any.downcast_ref::<Magnifier>() {
            // Outline only while dragging: the finished shape fetches series data, and a
            // placement preview must never kick fetches on every cursor move.
            self.paint_magnifier_ghost(ctx, m, proj, theme);
        } else {
            self.paint_one(ctx, ghost, proj, theme);
        }
        ctx.set_global_alpha(1.0);
    }

    /// Placement preview for two time-anchor tools: a full-height dashed vertical guide at
    /// each anchor (the left/right time bounds) plus a line from the first anchor to the
    /// cursor. Neutral gray guides (same as the regression channel midline), not the tool's
    /// accent color.
    fn paint_time_span_ghost(
        &self,
        ctx: &dyn Canvas2d,
        d: &dyn Drawing,
        proj: &dyn Projector,
        color: &str,
    ) {
        let base = d.base();
        let Some(a) = base.anchors.first() else {
            return;
        };
        let b = base.anchors.get(1);
        let guide = |time: f64| {
            let x = proj.x_of(time);
            ctx.save();
            set_stroke(ctx, color);
            ctx.set_global_alpha(0.6 * ctx.global_alpha());
            ctx.set_line_width(1.0);
            ctx.set_line_dash(&[4.0, 4.0]);
            ctx.begin_path();
            ctx.move_to(x, 0.0);
            ctx.line_to(x, proj.height());
            ctx.stroke();
            ctx.restore();
        };
        guide(a.time);
        if let Some(b) = b {
            guide(b.time);
        }
        let ya = proj.y_of(a.price, &base.pane_id);
        let yb = b.and_then(|b| proj.y_of(b.price, &base.pane_id));
        let mut pts: Vec<Pt> = Vec::new();
        if let Some(ya) = ya {
            pts.push((proj.x_of(a.time), ya));
        }
        if let (Some(b), Some(yb)) = (b, yb) {
            // The connecting line (first anchor to cursor).
            ctx.save();
            set_stroke(ctx, color);
            ctx.set_line_width(1.0);
            ctx.set_line_dash(&[]);
            ctx.begin_path();
            ctx.move_to(proj.x_of(a.time), ya.unwrap_or(yb));
            ctx.line_to(proj.x_of(b.time), yb);
            ctx.stroke();
            ctx.restore();
            pts.push((proj.x_of(b.time), yb));
        }
        self.paint_handles(ctx, &pts);
    }

    /// The selection box a Ctrl/Cmd-drag on the empty plot sweeps: a faint fill in the handle
    /// accent under a thin dashed outline.
    pub fn paint_marquee(&self, ctx: &dyn Canvas2d, x: f64, y: f64, w: f64, h: f64) {
        ctx.save();
        set_fill(ctx, &with_alpha(HANDLE_BORDER, 0.08));
        ctx.fill_rect(x, y, w, h);
        set_stroke(ctx, HANDLE_BORDER);
        ctx.set_line_width(1.0);
        ctx.set_line_dash(&[4.0, 3.0]);
        ctx.stroke_rect(x + 0.5, y + 0.5, w, h);
        ctx.restore();
    }

    /// A hollow ring marking the candle point the magnet (Ctrl) will snap the next anchor to.
    pub fn paint_snap_ring(&self, ctx: &dyn Canvas2d, x: f64, y: f64, theme: &DrawingTheme) {
        ctx.set_line_dash(&[]);
        ctx.set_global_alpha(0.9);
        ctx.begin_path();
        ctx.arc(x, y, 5.5, 0.0, PI * 2.0, false);
        set_stroke(ctx, &theme.text_color);
        ctx.set_line_width(1.5);
        ctx.stroke();
        ctx.set_global_alpha(1.0);
    }

    /// Round selection/placement handles: blue ring + gray fill (shared chrome, not the
    /// drawing's line color).
    pub fn paint_handles(&self, ctx: &dyn Canvas2d, points: &[Pt]) {
        ctx.set_line_dash(&[]);
        ctx.set_line_width(1.5);
        for &(x, y) in points {
            ctx.begin_path();
            ctx.arc(x, y, HANDLE_RADIUS, 0.0, PI * 2.0, false);
            set_fill(ctx, &with_alpha(NEUTRAL, 0.55));
            ctx.fill();
            set_stroke(ctx, HANDLE_BORDER);
            ctx.stroke();
        }
    }

    /// A bottom-center pill prompting the armed tool's placement gesture (e.g. the
    /// magnifier's "drag an area"). Painted while the tool is armed and no placement is in
    /// progress; chart-background fill so it reads as chrome over any content.
    pub fn paint_placement_hint(
        &self,
        ctx: &dyn Canvas2d,
        text: &str,
        theme: &DrawingTheme,
        width: f64,
        height: f64,
    ) {
        ctx.save();
        ctx.set_font(&px_font(11.0, &theme.font_family));
        let tw = ctx.measure_text(text).width;
        let pill_w = tw + 24.0;
        let pill_h = 24.0;
        let x = (width - pill_w) / 2.0;
        let y = height - pill_h - 14.0;
        round_rect(ctx, x, y, pill_w, pill_h, pill_h / 2.0);
        set_fill(ctx, &theme.background);
        ctx.fill();
        set_stroke(ctx, &with_alpha(&theme.text_color, 0.28));
        ctx.set_line_width(1.0);
        ctx.set_line_dash(&[]);
        ctx.stroke();
        set_fill(ctx, &theme.text_color);
        ctx.set_text_align(TextAlign::Center);
        ctx.set_text_baseline(TextBaseline::Middle);
        ctx.fill_text(text, width / 2.0, y + pill_h / 2.0 + 0.5);
        ctx.restore();
    }

    fn paint_one(
        &mut self,
        ctx: &dyn Canvas2d,
        d: &dyn Drawing,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let key = d.type_key();
        if let Some(geometry) = segment_geometry(d, proj) {
            if key == "highlighter" {
                self.paint_highlighter(ctx, d, geometry.as_ref());
            } else {
                self.paint_segments(ctx, d, geometry.as_ref());
                if key == "path" {
                    // A path ends in an arrowhead along its last segment.
                    let pts = d.handle_points(proj);
                    if pts.len() >= 2 {
                        paint_arrowhead(
                            ctx,
                            pts[pts.len() - 2],
                            pts[pts.len() - 1],
                            (&d.base().style).into(),
                        );
                    }
                }
            }
            self.paint_label(ctx, d, proj, theme);
            return;
        }
        let any = d.as_any();
        if let Some(m) = any.downcast_ref::<ArrowMark>() {
            self.paint_arrow_mark(ctx, m, proj);
            self.paint_label(ctx, d, proj, theme);
            return;
        }
        if let Some(g) = any.downcast_ref::<GlyphStamp>() {
            self.paint_glyph_stamp(ctx, g, proj, theme);
            self.paint_label(ctx, d, proj, theme);
            return;
        }
        if let Some(r) = as_radial(d) {
            self.paint_radial_fib(ctx, r, proj, theme);
            self.paint_label(ctx, d, proj, theme);
            return;
        }
        if let Some(s) = any.downcast_ref::<FibSpiral>() {
            self.paint_fib_spiral(ctx, s, proj);
            self.paint_label(ctx, d, proj, theme);
            return;
        }
        if let Some(t) = any.downcast_ref::<DedekindTessellation>() {
            self.paint_dedekind(ctx, t, proj);
            self.paint_label(ctx, d, proj, theme);
            return;
        }
        if let Some(m) = any.downcast_ref::<MachFigure>() {
            self.paint_mach_figure(ctx, m, proj, theme);
            self.paint_label(ctx, d, proj, theme);
            return;
        }
        if let Some(g) = any.downcast_ref::<GannSquare>() {
            // Grid + Gann angle fan (straight entry lines), then the concentric
            // quarter-ellipse arcs from the origin corner.
            self.paint_fib_ratios(ctx, g, proj, theme);
            self.paint_gann_arcs(ctx, g, proj);
            self.paint_label(ctx, d, proj, theme);
            return;
        }
        if let Some(f) = as_fib_ratios(d) {
            self.paint_fib_ratios(ctx, f, proj, theme);
            self.paint_label(ctx, d, proj, theme);
            return;
        }
        if let Some(m) = any.downcast_ref::<DatePriceRange>() {
            self.paint_measure_box(ctx, m, proj, theme);
            return;
        }
        if let Some(p) = any.downcast_ref::<PositionTool>() {
            self.paint_position(ctx, p, proj, theme);
            return;
        }
        if let Some(r) = any.downcast_ref::<RegressionChannel>() {
            self.paint_regression_channel(ctx, r, proj, theme);
            self.paint_label(ctx, d, proj, theme);
            return;
        }
        if let Some(v) = any.downcast_ref::<AnchoredVwap>() {
            self.paint_anchored_vwap(ctx, v, proj);
            self.paint_label(ctx, d, proj, theme);
            return;
        }
        if let Some(f) = any.downcast_ref::<FixedRangeVolumeProfile>() {
            self.paint_fixed_range_vp(ctx, f, proj, theme);
            self.paint_label(ctx, d, proj, theme);
            return;
        }
        if let Some(m) = any.downcast_ref::<Magnifier>() {
            self.paint_magnifier(ctx, m, proj, theme);
            return;
        }
        if any.is::<FairValueGap>() || any.is::<OrderBlock>() {
            self.paint_price_zone(ctx, d, proj, theme);
            self.paint_label(ctx, d, proj, theme);
            return;
        }
        if let Some(l) = any.downcast_ref::<Liquidity>() {
            self.paint_liquidity(ctx, l, proj);
            self.paint_label(ctx, d, proj, theme);
            return;
        }
        if let Some(p) = any.downcast_ref::<PatternDrawing>() {
            self.paint_pattern(ctx, p, proj, theme);
            return;
        }
        if let Some(c) = any.downcast_ref::<CalloutTool>() {
            match c.kind() {
                CalloutKind::Comment => self.paint_bubble(ctx, c, proj, theme, 999.0),
                CalloutKind::PriceNote => self.paint_price_note(ctx, c, proj, theme),
                CalloutKind::Signpost => self.paint_signpost(ctx, c, proj, theme),
                CalloutKind::Callout => self.paint_bubble(ctx, c, proj, theme, 5.0),
            }
            return;
        }
        if let Some(p) = any.downcast_ref::<PinnedLabel>() {
            match p.kind() {
                PinnedKind::Note => {
                    self.paint_note(ctx, p, proj, theme);
                    return;
                }
                PinnedKind::PriceLabel => {
                    self.paint_price_label(ctx, p, proj, theme);
                    return;
                }
                PinnedKind::Text => {}
            }
        }
        self.paint_basic(ctx, d, proj, theme);
    }

    /// The tools that are just a few strokes, then the label every drawing may carry. An
    /// early return (too few points) skips the label too.
    fn paint_basic(
        &self,
        ctx: &dyn Canvas2d,
        d: &dyn Drawing,
        proj: &dyn Projector,
        theme: &DrawingTheme,
    ) {
        let style = &d.base().style;
        match d.type_key() {
            "hline" => {
                let pts = d.handle_points(proj);
                let Some(p0) = pts.first() else { return };
                stroke_path(ctx, style.into(), || {
                    ctx.move_to(0.0, p0.1);
                    ctx.line_to(proj.width(), p0.1);
                });
            }
            "trendline" | "arrow" => {
                let pts = d.handle_points(proj);
                if pts.len() < 2 {
                    return;
                }
                let ext = d
                    .as_any()
                    .downcast_ref::<TrendLine>()
                    .map_or(DrawingExtend::None, TrendLine::extension);
                let [ex1, ey1, ex2, ey2] = extend_endpoints(
                    pts[0].0,
                    pts[0].1,
                    pts[1].0,
                    pts[1].1,
                    ext,
                    proj.width(),
                    proj.height(),
                );
                stroke_path(ctx, style.into(), || {
                    ctx.move_to(ex1, ey1);
                    ctx.line_to(ex2, ey2);
                });
                if style.arrow_right == Some(true) {
                    paint_arrowhead(ctx, pts[0], pts[1], style.into());
                }
                if style.arrow_left == Some(true) {
                    paint_arrowhead(ctx, pts[1], pts[0], style.into());
                }
            }
            "ellipse" => {
                let pts = d.handle_points(proj);
                if pts.len() < 2 {
                    return;
                }
                let cx = (pts[0].0 + pts[1].0) / 2.0;
                let cy = (pts[0].1 + pts[1].1) / 2.0;
                let rx = (pts[1].0 - pts[0].0).abs() / 2.0;
                let ry = (pts[1].1 - pts[0].1).abs() / 2.0;
                if let Some(fill) = fill_of(style) {
                    ctx.save();
                    set_fill(ctx, fill); // alpha lives in the fill color itself
                    ctx.begin_path();
                    ctx.ellipse(cx, cy, rx, ry, 0.0, 0.0, PI * 2.0, false);
                    ctx.fill();
                    ctx.restore();
                }
                apply_stroke(ctx, style.into());
                ctx.begin_path();
                ctx.ellipse(cx, cy, rx, ry, 0.0, 0.0, PI * 2.0, false);
                ctx.stroke();
                ctx.set_line_dash(&[]);
            }
            key @ ("ray" | "extendedline") => {
                let pts = d.handle_points(proj);
                if pts.len() < 2 {
                    return;
                }
                let ext = if key == "ray" {
                    DrawingExtend::Right
                } else {
                    DrawingExtend::Both
                };
                let [ex1, ey1, ex2, ey2] = extend_endpoints(
                    pts[0].0,
                    pts[0].1,
                    pts[1].0,
                    pts[1].1,
                    ext,
                    proj.width(),
                    proj.height(),
                );
                stroke_path(ctx, style.into(), || {
                    ctx.move_to(ex1, ey1);
                    ctx.line_to(ex2, ey2);
                });
            }
            "vline" => {
                let pts = d.handle_points(proj);
                let Some(p0) = pts.first() else { return };
                stroke_path(ctx, style.into(), || {
                    ctx.move_to(p0.0, 0.0);
                    ctx.line_to(p0.0, proj.height());
                });
            }
            "hray" => {
                let pts = d.handle_points(proj);
                let Some(p0) = pts.first() else { return };
                stroke_path(ctx, style.into(), || {
                    ctx.move_to(p0.0, p0.1);
                    ctx.line_to(proj.width(), p0.1);
                });
            }
            "crossline" => {
                let pts = d.handle_points(proj);
                let Some(p0) = pts.first() else { return };
                stroke_path(ctx, style.into(), || {
                    ctx.move_to(0.0, p0.1); // horizontal arm: full width
                    ctx.line_to(proj.width(), p0.1);
                    ctx.move_to(p0.0, 0.0); // vertical arm: full height
                    ctx.line_to(p0.0, proj.height());
                });
            }
            "infoline" => self.paint_info_line(ctx, d, proj),
            "trendangle" => self.paint_trend_angle(ctx, d, proj, theme),
            "circle" => {
                let pts = d.handle_points(proj);
                if pts.len() < 2 {
                    return;
                }
                let (cx, cy) = pts[0];
                let r = (pts[1].0 - cx).hypot(pts[1].1 - cy);
                if r < 1.0 {
                    return;
                }
                if let Some(fill) = fill_of(style) {
                    ctx.save();
                    set_fill(ctx, fill); // alpha lives in the fill color itself
                    ctx.begin_path();
                    ctx.arc(cx, cy, r, 0.0, PI * 2.0, false);
                    ctx.fill();
                    ctx.restore();
                }
                apply_stroke(ctx, style.into());
                ctx.begin_path();
                ctx.arc(cx, cy, r, 0.0, PI * 2.0, false);
                ctx.stroke();
                ctx.set_line_dash(&[]);
            }
            "box" => {
                let pts = d.handle_points(proj);
                if pts.len() < 2 {
                    return;
                }
                let x = pts[0].0.min(pts[1].0);
                let y = pts[0].1.min(pts[1].1);
                let w = (pts[1].0 - pts[0].0).abs();
                let h = (pts[1].1 - pts[0].1).abs();
                if let Some(fill) = fill_of(style) {
                    ctx.save();
                    set_fill(ctx, fill); // alpha lives in the fill color itself
                    ctx.fill_rect(x, y, w, h);
                    ctx.restore();
                }
                // The midline sits over the fill and under the label (painted after the
                // shape). It breaks where the box's centered label sits, so the text reads
                // clear of it.
                let mid = d
                    .as_any()
                    .downcast_ref::<BoxDrawing>()
                    .and_then(|b| b.midline(proj));
                if let Some(mid) = mid {
                    let gap = self.box_label_gap(ctx, d, proj, theme, mid.y);
                    stroke_path(
                        ctx,
                        StrokeSpec::new(&mid.color, style.line_width, mid.line_style),
                        || {
                            for (from, to) in midline_segments(mid.x1, mid.x2, gap) {
                                ctx.move_to(from, mid.y);
                                ctx.line_to(to, mid.y);
                            }
                        },
                    );
                }
                apply_stroke(ctx, style.into());
                ctx.stroke_rect(x, y, w, h);
                ctx.set_line_dash(&[]);
            }
            _ => {} // text has no geometry: its label is drawn below
        }
        self.paint_label(ctx, d, proj, theme); // every drawing can carry an optional label
    }

    /// The span `[left, right]` a box's centered label covers on the row at `y`, padded so
    /// the line stops just short of the text; `None` when the box has no label or the label
    /// does not reach that row. Measured with the same font `paint_label` draws it in.
    fn box_label_gap(
        &self,
        ctx: &dyn Canvas2d,
        d: &dyn Drawing,
        proj: &dyn Projector,
        theme: &DrawingTheme,
        y: f64,
    ) -> Option<(f64, f64)> {
        let text = d.base().text.as_ref().filter(|t| !t.value.is_empty())?;
        let layout = labels::label_layout(d, proj)?;
        let fs = named_font_size(text.size);
        let lines: Vec<&str> = text.value.split('\n').collect();
        if y < layout.top || y > layout.top + label_line_height(fs) * lines.len() as f64 {
            return None;
        }
        ctx.save();
        ctx.set_font(&helpers::text_font(
            text.bold == Some(true),
            text.italic == Some(true),
            fs,
            &theme.font_family,
        ));
        let width = lines
            .iter()
            .map(|line| ctx.measure_text(line).width)
            .fold(f64::NEG_INFINITY, crate::indicators::classics::math::js_max);
        ctx.restore();
        let half = width / 2.0 + helpers::MIDLINE_LABEL_PAD;
        Some((layout.x - half, layout.x + half))
    }

    /// Paint a multi-line drawing (channel/pitchfork): optional fill polygon, then each line.
    fn paint_segments(
        &self,
        ctx: &dyn Canvas2d,
        d: &dyn Drawing,
        geometry: Option<&SegmentGeometry>,
    ) {
        let Some(g) = geometry else { return };
        let style = &d.base().style;
        if let (Some(fill), Some(color)) = (g.fill.as_ref(), fill_of(style))
            && fill.len() >= 3
        {
            ctx.save();
            set_fill(ctx, color); // alpha lives in the fill color itself
            ctx.begin_path();
            ctx.move_to(fill[0].0, fill[0].1);
            for p in &fill[1..] {
                ctx.line_to(p.0, p.1);
            }
            ctx.close_path();
            ctx.fill();
            ctx.restore();
        }
        for s in &g.segments {
            stroke_path(ctx, style.into(), || {
                ctx.move_to(s[0], s[1]);
                ctx.line_to(s[2], s[3]);
            });
        }
    }

    /// Paint a highlighter: the captured path as ONE wide, round-capped translucent swath.
    /// Drawing it as a single continuous stroke (not per segment) keeps a stroke's overlaps
    /// from compounding alpha, so it reads as one even highlight; the see-through comes from
    /// the color's own alpha.
    fn paint_highlighter(
        &self,
        ctx: &dyn Canvas2d,
        d: &dyn Drawing,
        geometry: Option<&SegmentGeometry>,
    ) {
        let Some(g) = geometry else { return };
        let Some(first) = g.segments.first() else {
            return;
        };
        let style = &d.base().style;
        ctx.save();
        set_stroke(ctx, &style.line_color); // alpha lives in the color itself
        ctx.set_line_width(style.line_width);
        ctx.set_line_cap(LineCap::Round);
        ctx.set_line_join(LineJoin::Round);
        ctx.set_line_dash(&[]);
        ctx.begin_path();
        ctx.move_to(first[0], first[1]);
        for s in &g.segments {
            ctx.line_to(s[2], s[3]);
        }
        ctx.stroke();
        ctx.restore();
    }
}

/// The pixel geometry of a tool the original derives from `SegmentDrawing`, or `None` when
/// the drawing is not one. The inner option is the tool's own `geometry(proj)`.
fn segment_geometry(d: &dyn Drawing, proj: &dyn Projector) -> Option<Option<SegmentGeometry>> {
    let any = d.as_any();
    macro_rules! try_types {
        ($($t:ty),+ $(,)?) => {
            $(if let Some(x) = any.downcast_ref::<$t>() {
                return Some(x.geometry(proj));
            })+
        };
    }
    try_types!(
        Triangle,
        RotatedRect,
        ArcShape,
        Curve,
        Polyline,
        Path,
        Freehand,
        Highlighter,
        ParallelChannel,
        DisjointChannel,
        FlatTopBottom,
        Pitchfork,
    );
    None
}

/// The concentric-ring Fibonacci tools of a drawing.
fn as_radial(d: &dyn Drawing) -> Option<&dyn RadialFib> {
    let any = d.as_any();
    macro_rules! try_types {
        ($($t:ty),+ $(,)?) => {
            $(if let Some(x) = any.downcast_ref::<$t>() {
                return Some(x);
            })+
        };
    }
    try_types!(FibCircles, FibArcs, FibWedge);
    None
}

/// Every Fibonacci-style tool (the Gann tools included) of a drawing.
fn as_fib_ratios(d: &dyn Drawing) -> Option<&dyn FibRatios> {
    let any = d.as_any();
    macro_rules! try_types {
        ($($t:ty),+ $(,)?) => {
            $(if let Some(x) = any.downcast_ref::<$t>() {
                return Some(x);
            })+
        };
    }
    try_types!(
        FibRetracement,
        FibExtension,
        FibExtensionTrend,
        FibFan,
        FibTimeZones,
        FibChannel,
        FibSpeedFan,
        TrendFibTime,
        GannFan,
        GannBox,
        GannSquare,
    );
    None
}

/// A filled triangular arrowhead at `to`, pointing along the `from` to `to` direction.
fn paint_arrowhead(ctx: &dyn Canvas2d, from: Pt, to: Pt, style: StrokeSpec) {
    let ang = (to.1 - from.1).atan2(to.0 - from.0);
    let len = crate::indicators::classics::math::js_max(9.0, style.width * 3.5);
    let spread = PI / 7.0;
    ctx.save();
    ctx.set_line_dash(&[]);
    set_fill(ctx, style.color);
    ctx.begin_path();
    ctx.move_to(to.0, to.1);
    ctx.line_to(
        to.0 - len * (ang - spread).cos(),
        to.1 - len * (ang - spread).sin(),
    );
    ctx.line_to(
        to.0 - len * (ang + spread).cos(),
        to.1 - len * (ang + spread).sin(),
    );
    ctx.close_path();
    ctx.fill();
    ctx.restore();
}
