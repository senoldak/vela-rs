//! The painter of the drawing objects an indicator script emits: lines, boxes, labels,
//! polylines and linefills (`crate::model::drawings`). One pass paints a whole
//! [`DrawingSet`], bottom to top: linefills, boxes, polylines, lines, labels.
//!
//! The caller supplies a canvas, the pane size and two coordinate closures (logical bar
//! index to x, price to y); the renderer owns only the drawing logic and a text-width
//! cache that persists across frames.
//!
//! Differences from the original's shape (the paint itself is call for call the same):
//! - The original's `LayerDeps` (bar-time resolver, bar lookup, theme) is rebound before
//!   every frame with closures over the frame's scene. Here a [`LayerDeps`] borrowing
//!   them is passed to [`DrawingSceneRenderer::render`] and
//!   [`DrawingSceneRenderer::price_range`] instead of being stored, so no `set_deps`.
//! - Line breaking goes through the `Intl` backend's word segmenter (`Intl.Segmenter` in
//!   the original); the renderer owns the backend, English by default.
//! - The original reads `ctx.font` back where it measures a bubble label; [`Canvas2d`]
//!   has no getter, so the font string that was just assigned is used (the browser's
//!   serialization of it, when it differs, only keys the width cache).
//! - A canvas `arc` call carries its `counterclockwise` argument (`false`) where the
//!   original omits it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::indicators::classics::math::{js_max, js_min};
use crate::intl::{Intl, default_intl};
use crate::model::drawings::{
    BoxFontFamily, BoxHAlign, BoxTextSize, BoxVAlign, DrawingBox, DrawingExtend, DrawingLabel,
    DrawingLine, DrawingLinefill, DrawingPolyline, DrawingXLoc, LabelStyle, LabelYLoc,
};
use crate::model::indicator::IndicatorModel;
use crate::render::canvas::{Canvas2d, TextAlign, TextBaseline};
use crate::util::js_number;

use super::drawing_geometry::{
    auto_font_size, contrast_color, dash_pattern, extend_endpoints, named_font_size,
};
use super::text_wrap::wrap_text_lines;

/// The full set of script drawings for one indicator, batched into one pass.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrawingSet {
    pub lines: Vec<DrawingLine>,
    pub boxes: Vec<DrawingBox>,
    pub labels: Vec<DrawingLabel>,
    pub polylines: Vec<DrawingPolyline>,
    pub linefills: Vec<DrawingLinefill>,
}

impl DrawingSet {
    /// True when the set draws nothing at all.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
            && self.boxes.is_empty()
            && self.labels.is_empty()
            && self.polylines.is_empty()
            && self.linefills.is_empty()
    }
}

/// The high and low of a bar, for label `yloc` above/below.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarHighLow {
    pub high: f64,
    pub low: f64,
}

/// Renderer-side resolvers (kept out of core); identical across backends.
pub struct LayerDeps<'a> {
    /// Drawing `bar_time` (ms) to a fractional logical bar index.
    pub time_to_logical: &'a dyn Fn(f64) -> f64,
    /// Bar high/low at a (rounded) logical index.
    pub bar_at: &'a dyn Fn(f64) -> Option<BarHighLow>,
    pub theme: &'a Value,
}

/// Drawing price range over the visible bar window (backend-neutral autoscale geometry).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawingPriceRange {
    pub min: f64,
    pub max: f64,
    pub above_margin: f64,
    pub below_margin: f64,
}

/// A model shape carrying optional script drawing arrays (structurally `IndicatorModel`).
#[derive(Clone, Copy, Debug, Default)]
pub struct DrawingSetSource<'a> {
    pub lines: &'a [DrawingLine],
    pub boxes: &'a [DrawingBox],
    pub labels: &'a [DrawingLabel],
    pub polylines: &'a [DrawingPolyline],
    pub linefills: &'a [DrawingLinefill],
}

impl<'a> From<&'a IndicatorModel> for DrawingSetSource<'a> {
    fn from(m: &'a IndicatorModel) -> Self {
        Self {
            lines: &m.lines,
            boxes: &m.boxes,
            labels: &m.labels,
            polylines: &m.polylines,
            linefills: &m.linefills,
        }
    }
}

/// A model's drawings routed ONE way: its own pane (`overlay` false) or forced onto the
/// price pane (`overlay` true, Pine's `force_overlay`). Every consumer of a model's
/// drawings splits along this same seam.
pub fn model_drawing_set<'a>(m: impl Into<DrawingSetSource<'a>>, overlay: bool) -> DrawingSet {
    let m = m.into();
    DrawingSet {
        lines: m
            .lines
            .iter()
            .filter(|d| d.overlay == overlay)
            .cloned()
            .collect(),
        boxes: m
            .boxes
            .iter()
            .filter(|d| d.overlay == overlay)
            .cloned()
            .collect(),
        labels: m
            .labels
            .iter()
            .filter(|d| d.overlay == overlay)
            .cloned()
            .collect(),
        polylines: m
            .polylines
            .iter()
            .filter(|d| d.overlay == overlay)
            .cloned()
            .collect(),
        linefills: m
            .linefills
            .iter()
            .filter(|d| d.overlay == overlay)
            .cloned()
            .collect(),
    }
}

/// True when the set draws nothing at all.
pub fn drawing_set_empty(s: &DrawingSet) -> bool {
    s.is_empty()
}

/// Hover hit-rect of one rendered label that carries a tooltip (canvas coords of the
/// last render).
#[derive(Clone, Debug, PartialEq)]
pub struct LabelTipRegion {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub text: String,
}

fn font_size_px(size: BoxTextSize) -> f64 {
    if size == BoxTextSize::Auto {
        12.0
    } else {
        named_font_size(size)
    }
}

/// A theme string property, `None` when the theme lacks it.
pub(crate) fn theme_str<'a>(theme: &'a Value, key: &str) -> Option<&'a str> {
    theme.get(key).and_then(Value::as_str)
}

/// `theme.textColor` (an absent one is the empty string, which a canvas ignores).
pub(crate) fn theme_text_color(theme: &Value) -> &str {
    theme_str(theme, "textColor").unwrap_or("")
}

/// `theme.fontFamily || 'sans-serif'`.
pub(crate) fn theme_font_family(theme: &Value) -> &str {
    match theme_str(theme, "fontFamily") {
        Some(f) if !f.is_empty() => f,
        _ => "sans-serif",
    }
}

/// JavaScript truthiness of an optional string.
pub(crate) fn truthy(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|s| !s.is_empty())
}

/// The CSS font of text drawn with a drawing's family and `text_formatting`.
pub(crate) fn drawing_font(
    theme: &Value,
    family: BoxFontFamily,
    bold: bool,
    italic: bool,
    size_px: f64,
) -> String {
    let family = if family == BoxFontFamily::Monospace {
        "monospace"
    } else {
        theme_font_family(theme)
    };
    format!(
        "{}{}{}px {}",
        if italic { "italic " } else { "" },
        if bold { "bold " } else { "" },
        js_number(size_px),
        family
    )
}

/// Whether any painted part of a line (anchor segment plus its `extend` projection)
/// crosses the visible bar window `[lo, hi]` (logical x). A vertical line (x1 == x2)
/// extends along itself, so extend adds no horizontal coverage.
fn line_covers_window(a: f64, b: f64, extend: DrawingExtend, lo: f64, hi: f64) -> bool {
    let min_x = js_min(a, b);
    let max_x = js_max(a, b);
    if a == b {
        return a >= lo && a <= hi;
    }
    match extend {
        DrawingExtend::Both => true,
        DrawingExtend::Left => max_x >= lo,
        DrawingExtend::Right => min_x <= hi,
        DrawingExtend::None => max_x >= lo && min_x <= hi,
    }
}

fn is_point_shape(style: LabelStyle) -> bool {
    matches!(
        style,
        LabelStyle::Circle
            | LabelStyle::Square
            | LabelStyle::Diamond
            | LabelStyle::Flag
            | LabelStyle::ArrowUp
            | LabelStyle::ArrowDown
            | LabelStyle::TriangleUp
            | LabelStyle::TriangleDown
            | LabelStyle::Cross
            | LabelStyle::XCross
    )
}

fn max_of(first: f64, rest: impl Iterator<Item = f64>) -> f64 {
    rest.fold(first, js_max)
}

fn min_of(values: &[f64]) -> f64 {
    values.iter().copied().fold(f64::INFINITY, js_min)
}

fn max_over(values: &[f64]) -> f64 {
    values.iter().copied().fold(f64::NEG_INFINITY, js_max)
}

/// What one frame paints with: the canvas, the pane size and the coordinate closures.
struct Frame<'a> {
    ctx: &'a dyn Canvas2d,
    w: f64,
    h: f64,
    x_of: &'a dyn Fn(f64) -> f64,
    y_of: &'a dyn Fn(f64) -> Option<f64>,
    deps: &'a LayerDeps<'a>,
}

/// Backend-agnostic renderer for all script drawing objects. Owned by one caller
/// (`&mut self`): its width cache and tooltip rects are per-renderer state.
pub struct DrawingSceneRenderer {
    intl: Arc<dyn Intl>,
    /// Text width cache, keyed `"{font} {text}"`, persisting across frames.
    width_cache: RefCell<HashMap<String, f64>>,
    /// Tooltip hit-rects captured while drawing the CURRENT set (rebuilt per render).
    tip_regions: Vec<LabelTipRegion>,
    /// Index offset of the CURRENT set's model: its `bar_index` coordinates count from
    /// the model's anchor bar, so they shift by this to land on chart logical indices.
    /// 0 (the norm) for whole-chart models. `bar_time` coordinates are unaffected.
    index_offset: f64,
    set: DrawingSet,
}

impl Default for DrawingSceneRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl DrawingSceneRenderer {
    /// A renderer with an empty set, wrapping text with the built-in English backend.
    pub fn new() -> Self {
        Self::with_intl(default_intl())
    }

    /// A renderer wrapping text with the platform's `Intl` backend.
    pub fn with_intl(intl: Arc<dyn Intl>) -> Self {
        Self {
            intl,
            width_cache: RefCell::new(HashMap::new()),
            tip_regions: Vec::new(),
            index_offset: 0.0,
            set: DrawingSet::default(),
        }
    }

    pub fn set_set(&mut self, set: DrawingSet, index_offset: f64) {
        self.set = set;
        self.index_offset = index_offset;
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    /// Tooltip hit-rects of the labels drawn by the LAST `render` call (same coords as
    /// its canvas).
    pub fn label_tip_regions(&self) -> &[LabelTipRegion] {
        &self.tip_regions
    }

    /// Draw the whole set into `ctx` using the supplied coordinate closures.
    pub fn render(
        &mut self,
        deps: &LayerDeps,
        ctx: &dyn Canvas2d,
        w: f64,
        h: f64,
        x_of: &dyn Fn(f64) -> f64,
        y_of: &dyn Fn(f64) -> Option<f64>,
    ) {
        self.tip_regions = Vec::new();
        if self.is_empty() {
            return;
        }
        let f = Frame {
            ctx,
            w,
            h,
            x_of,
            y_of,
            deps,
        };
        let mut tips = Vec::new();
        ctx.save();
        self.draw_linefills(&f);
        self.draw_boxes(&f);
        self.draw_polylines(&f);
        self.draw_lines(&f);
        self.draw_labels(&f, &mut tips);
        ctx.restore();
        self.tip_regions = tips;
    }

    /// Price min/max (plus above/below pixel margins) over the drawings intersecting the
    /// visible bar range `[from, to]`, for autoscale. `None` if nothing in range
    /// contributes a price.
    pub fn price_range(&self, deps: &LayerDeps, from: f64, to: f64) -> Option<DrawingPriceRange> {
        let lo = js_min(from, to);
        let hi = js_max(from, to);
        // Painted-part coverage: the anchor span plus the side(s) `extend` projects
        // toward. A one-sided extension paints nothing on its other side, so a window
        // entirely there must not inherit the drawing's prices.
        let visible = |a: f64, b: f64, extend: DrawingExtend| -> bool {
            match extend {
                DrawingExtend::Both => true,
                DrawingExtend::Left => js_max(a, b) >= lo,
                DrawingExtend::Right => js_min(a, b) <= hi,
                DrawingExtend::None => js_max(a, b) >= lo && js_min(a, b) <= hi,
            }
        };
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        let mut fold = |v: f64| {
            if v < min {
                min = v;
            }
            if v > max {
                max = v;
            }
        };

        for ln in &self.set.lines {
            if ln.invisible {
                continue;
            }
            // A line folds its ANCHOR prices (y1/y2) while some painted part of it
            // crosses the window: the extension widens WHERE the line is, but its
            // projected values never define the scale. A vertical line (x1 == x2)
            // extends along itself, with no horizontal coverage beyond its own bar, so
            // one anchored off-screen contributes nothing (it paints nothing here).
            if !line_covers_window(
                self.logical_of(deps, ln.xloc, ln.x1),
                self.logical_of(deps, ln.xloc, ln.x2),
                ln.extend,
                lo,
                hi,
            ) {
                continue;
            }
            fold(ln.y1);
            fold(ln.y2);
        }
        for bx in &self.set.boxes {
            let has_border = truthy(&bx.border_color).is_some() && bx.border_width > 0.0;
            if truthy(&bx.bg_color).is_none() && !has_border && truthy(&bx.text).is_none() {
                continue;
            }
            if !visible(
                self.logical_of(deps, bx.xloc, bx.left),
                self.logical_of(deps, bx.xloc, bx.right),
                bx.extend,
            ) {
                continue;
            }
            fold(bx.top);
            fold(bx.bottom);
        }
        for lf in &self.set.linefills {
            if truthy(&lf.color).is_none() {
                continue;
            }
            let xs = [
                self.logical_of(deps, lf.line1.xloc, lf.line1.x1),
                self.logical_of(deps, lf.line1.xloc, lf.line1.x2),
                self.logical_of(deps, lf.line2.xloc, lf.line2.x1),
                self.logical_of(deps, lf.line2.xloc, lf.line2.x2),
            ];
            let lf_ext = if lf.line1.extend != DrawingExtend::None
                || lf.line2.extend != DrawingExtend::None
            {
                DrawingExtend::Both
            } else {
                DrawingExtend::None
            };
            if !visible(min_of(&xs), max_over(&xs), lf_ext) {
                continue;
            }
            fold(lf.line1.y1);
            fold(lf.line1.y2);
            fold(lf.line2.y1);
            fold(lf.line2.y2);
        }
        for pl in &self.set.polylines {
            if pl.points.len() < 2 {
                continue;
            }
            let xs: Vec<f64> = pl
                .points
                .iter()
                .map(|p| self.logical_of(deps, p.xloc, p.x))
                .collect();
            if !visible(min_of(&xs), max_over(&xs), DrawingExtend::None) {
                continue;
            }
            for p in &pl.points {
                fold(p.price);
            }
        }
        let mut above_margin = 0.0;
        let mut below_margin = 0.0;
        for lb in &self.set.labels {
            let lx = self.logical_of(deps, lb.xloc, lb.x);
            if !visible(lx, lx, DrawingExtend::None) {
                continue;
            }
            if lb.yloc == LabelYLoc::Price {
                fold(lb.y);
                continue;
            }
            if matches!(lb.yloc, LabelYLoc::Top | LabelYLoc::Bottom) {
                continue;
            }
            let Some(bar) = (deps.bar_at)(lx) else {
                continue;
            };
            fold(if lb.yloc == LabelYLoc::AboveBar {
                bar.high
            } else {
                bar.low
            });
            let font_px = font_size_px(lb.size);
            let line_count = js_max(
                1.0,
                lb.text.as_deref().unwrap_or("").split('\n').count() as f64,
            );
            let ext = if is_point_shape(lb.style) {
                14.0 + js_max(4.0, font_px * 0.6) + 2.0
            } else {
                14.0 + font_px * 1.25 * line_count + 8.0 + 7.0
            };
            if lb.yloc == LabelYLoc::AboveBar {
                above_margin = js_max(above_margin, ext);
            } else {
                below_margin = js_max(below_margin, ext);
            }
        }

        if !min.is_finite() || !max.is_finite() {
            return None;
        }
        Some(DrawingPriceRange {
            min,
            max,
            above_margin,
            below_margin,
        })
    }

    fn logical_of(&self, deps: &LayerDeps, xloc: DrawingXLoc, x: f64) -> f64 {
        match xloc {
            DrawingXLoc::BarTime => (deps.time_to_logical)(x),
            DrawingXLoc::BarIndex => x + self.index_offset,
        }
    }

    // ── lines ────────────────────────────────────────────────────────────────

    fn draw_lines(&self, f: &Frame) {
        let ctx = f.ctx;
        for ln in &self.set.lines {
            if ln.invisible {
                continue;
            }
            let x1 = (f.x_of)(self.logical_of(f.deps, ln.xloc, ln.x1));
            let x2 = (f.x_of)(self.logical_of(f.deps, ln.xloc, ln.x2));
            let (Some(y1), Some(y2)) = ((f.y_of)(ln.y1), (f.y_of)(ln.y2)) else {
                continue;
            };

            let ext_l = matches!(ln.extend, DrawingExtend::Left | DrawingExtend::Both);
            let ext_r = matches!(ln.extend, DrawingExtend::Right | DrawingExtend::Both);
            if !ext_l && !ext_r && (js_max(x1, x2) < 0.0 || js_min(x1, x2) > f.w) {
                continue;
            }

            let [ax, ay, bx, by] = extend_endpoints(x1, y1, x2, y2, ln.extend, f.w, f.h);
            let color = ln
                .color
                .as_deref()
                .unwrap_or_else(|| theme_text_color(f.deps.theme));

            ctx.begin_path();
            ctx.move_to(ax, ay);
            ctx.line_to(bx, by);
            ctx.set_stroke_style(&color.into());
            ctx.set_line_width(ln.width);
            ctx.set_line_dash(&dash_pattern(ln.style, ln.width));
            ctx.stroke();
            ctx.set_line_dash(&[]);

            let p2_right = x2 >= x1;
            if ln.arrow_right {
                let tip = if if p2_right { ext_r } else { ext_l } {
                    if p2_right { (bx, by) } else { (ax, ay) }
                } else {
                    (x2, y2)
                };
                self.draw_arrow(ctx, tip.0, tip.1, x1, y1, ln.width, color);
            }
            if ln.arrow_left {
                let tip = if if p2_right { ext_l } else { ext_r } {
                    if p2_right { (ax, ay) } else { (bx, by) }
                } else {
                    (x1, y1)
                };
                self.draw_arrow(ctx, tip.0, tip.1, x2, y2, ln.width, color);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_arrow(
        &self,
        ctx: &dyn Canvas2d,
        tip_x: f64,
        tip_y: f64,
        from_x: f64,
        from_y: f64,
        width: f64,
        color: &str,
    ) {
        let angle = (tip_y - from_y).atan2(tip_x - from_x);
        let size = 6.0 + width * 2.0;
        let spread = 0.45;
        ctx.begin_path();
        ctx.move_to(tip_x, tip_y);
        ctx.line_to(
            tip_x - size * (angle - spread).cos(),
            tip_y - size * (angle - spread).sin(),
        );
        ctx.line_to(
            tip_x - size * (angle + spread).cos(),
            tip_y - size * (angle + spread).sin(),
        );
        ctx.close_path();
        ctx.set_fill_style(&color.into());
        ctx.fill();
    }

    // ── linefills ────────────────────────────────────────────────────────────

    fn draw_linefills(&self, f: &Frame) {
        let ctx = f.ctx;
        for lf in &self.set.linefills {
            let Some(color) = truthy(&lf.color) else {
                continue;
            };
            let (Some(a), Some(b)) = (
                self.resolve_line(&lf.line1, f),
                self.resolve_line(&lf.line2, f),
            ) else {
                continue;
            };
            let orient = |s: [f64; 4]| -> [f64; 4] {
                if s[0] > s[2] || (s[0] == s[2] && s[1] > s[3]) {
                    [s[2], s[3], s[0], s[1]]
                } else {
                    s
                }
            };
            let a2 = orient(a);
            let b2 = orient(b);
            ctx.begin_path();
            ctx.move_to(a2[0], a2[1]);
            ctx.line_to(a2[2], a2[3]);
            ctx.line_to(b2[2], b2[3]);
            ctx.line_to(b2[0], b2[1]);
            ctx.close_path();
            ctx.set_fill_style(&color.into());
            ctx.fill();
        }
    }

    fn resolve_line(&self, ln: &DrawingLine, f: &Frame) -> Option<[f64; 4]> {
        let x1 = (f.x_of)(self.logical_of(f.deps, ln.xloc, ln.x1));
        let x2 = (f.x_of)(self.logical_of(f.deps, ln.xloc, ln.x2));
        let y1 = (f.y_of)(ln.y1)?;
        let y2 = (f.y_of)(ln.y2)?;
        Some(extend_endpoints(x1, y1, x2, y2, ln.extend, f.w, f.h))
    }

    // ── polylines ────────────────────────────────────────────────────────────

    fn draw_polylines(&self, f: &Frame) {
        let ctx = f.ctx;
        for pl in &self.set.polylines {
            let mut pts: Vec<(f64, f64)> = Vec::new();
            for p in &pl.points {
                let Some(y) = (f.y_of)(p.price) else {
                    continue;
                };
                pts.push(((f.x_of)(self.logical_of(f.deps, p.xloc, p.x)), y));
            }
            if pts.len() < 2 {
                continue;
            }
            let mut min_x = f64::INFINITY;
            let mut max_x = f64::NEG_INFINITY;
            for p in &pts {
                if p.0 < min_x {
                    min_x = p.0;
                }
                if p.0 > max_x {
                    max_x = p.0;
                }
            }
            if max_x < 0.0 || min_x > f.w {
                continue;
            }

            ctx.begin_path();
            if pl.curved {
                curved_path(ctx, &pts, pl.closed);
            } else {
                ctx.move_to(pts[0].0, pts[0].1);
                for p in &pts[1..] {
                    ctx.line_to(p.0, p.1);
                }
            }
            if pl.closed {
                ctx.close_path();
            }

            if let Some(fill) = truthy(&pl.fill_color) {
                ctx.set_fill_style(&fill.into());
                ctx.fill();
            }
            if let Some(line_color) = truthy(&pl.line_color) {
                ctx.set_stroke_style(&line_color.into());
                ctx.set_line_width(pl.line_width);
                ctx.set_line_dash(&dash_pattern(pl.line_style, pl.line_width));
                ctx.stroke();
                ctx.set_line_dash(&[]);

                if pl.arrow_left || pl.arrow_right {
                    let segs = if pl.closed { pts.len() } else { pts.len() - 1 };
                    for i in 0..segs {
                        let a = pts[i];
                        let b = pts[(i + 1) % pts.len()];
                        if pl.arrow_right {
                            self.draw_arrow(ctx, b.0, b.1, a.0, a.1, pl.line_width, line_color);
                        }
                        if pl.arrow_left {
                            self.draw_arrow(ctx, a.0, a.1, b.0, b.1, pl.line_width, line_color);
                        }
                    }
                }
            }
        }
    }

    // ── boxes ────────────────────────────────────────────────────────────────

    fn draw_boxes(&self, f: &Frame) {
        let ctx = f.ctx;
        for bx in &self.set.boxes {
            let mut xl = (f.x_of)(self.logical_of(f.deps, bx.xloc, bx.left));
            let mut xr = (f.x_of)(self.logical_of(f.deps, bx.xloc, bx.right));
            if matches!(bx.extend, DrawingExtend::Left | DrawingExtend::Both) {
                xl = -2.0;
            }
            if matches!(bx.extend, DrawingExtend::Right | DrawingExtend::Both) {
                xr = f.w + 2.0;
            }
            let (Some(yt), Some(yb)) = ((f.y_of)(bx.top), (f.y_of)(bx.bottom)) else {
                continue;
            };

            let left = js_min(xl, xr);
            let right = js_max(xl, xr);
            let top = js_min(yt, yb);
            let bottom = js_max(yt, yb);
            let w = right - left;
            let h = bottom - top;

            if right < 0.0 || left > f.w || bottom < 0.0 || top > f.h {
                continue;
            }

            if let Some(bg) = truthy(&bx.bg_color) {
                ctx.set_fill_style(&bg.into());
                ctx.fill_rect(left, top, w, h);
            }
            if let Some(border) = truthy(&bx.border_color)
                && bx.border_width > 0.0
            {
                ctx.set_stroke_style(&border.into());
                ctx.set_line_width(bx.border_width);
                ctx.set_line_dash(&dash_pattern(bx.border_style, bx.border_width));
                ctx.stroke_rect(left, top, w, h);
                ctx.set_line_dash(&[]);
            }
            if truthy(&bx.text).is_some() {
                self.draw_box_text(f, bx, left, top, w, h);
            }
        }
    }

    fn measure(&self, ctx: &dyn Canvas2d, font: &str, text: &str) -> f64 {
        let key = format!("{font} {text}");
        if let Some(cached) = self.width_cache.borrow().get(&key) {
            return *cached;
        }
        ctx.set_font(font);
        let width = ctx.measure_text(text).width;
        self.width_cache.borrow_mut().insert(key, width);
        width
    }

    fn wrap_text(&self, ctx: &dyn Canvas2d, font: &str, text: &str, max_width: f64) -> Vec<String> {
        wrap_text_lines(self.intl.as_ref(), text, max_width, &|t| {
            self.measure(ctx, font, t)
        })
    }

    fn draw_box_text(&self, f: &Frame, bx: &DrawingBox, left: f64, top: f64, w: f64, h: f64) {
        let ctx = f.ctx;
        let text = bx.text.as_deref().unwrap_or("");
        let pad = 4.0;

        let mut size = named_font_size(bx.text_size);
        let lines: Vec<String>;
        if size == 0.0 {
            let reference = drawing_font(f.deps.theme, bx.font_family, bx.bold, bx.italic, 14.0);
            lines = if bx.wrap {
                self.wrap_text(ctx, &reference, text, js_max(1.0, w - pad * 2.0))
            } else {
                text.split('\n').map(String::from).collect()
            };
            let mut widest = 0.0;
            for l in &lines {
                widest = js_max(widest, self.measure(ctx, &reference, l));
            }
            size = auto_font_size(widest, lines.len() as f64, w - pad * 2.0, h);
            if size == 0.0 {
                return;
            }
        } else {
            let font = drawing_font(f.deps.theme, bx.font_family, bx.bold, bx.italic, size);
            lines = if bx.wrap {
                self.wrap_text(ctx, &font, text, js_max(1.0, w - pad * 2.0))
            } else {
                text.split('\n').map(String::from).collect()
            };
        }
        ctx.set_font(&drawing_font(
            f.deps.theme,
            bx.font_family,
            bx.bold,
            bx.italic,
            size,
        ));
        let fill = match bx.text_color.as_deref() {
            Some(c) => c,
            None => contrast_color(bx.bg_color.as_deref()),
        };
        ctx.set_fill_style(&fill.into());

        let line_h = size * 1.3;
        let block_h = line_h * lines.len() as f64;

        let tx = match bx.h_align {
            BoxHAlign::Left => {
                ctx.set_text_align(TextAlign::Left);
                left + pad
            }
            BoxHAlign::Right => {
                ctx.set_text_align(TextAlign::Right);
                left + w - pad
            }
            BoxHAlign::Center => {
                ctx.set_text_align(TextAlign::Center);
                left + w / 2.0
            }
        };
        ctx.set_text_baseline(TextBaseline::Middle);
        let block_top = match bx.v_align {
            BoxVAlign::Top => top + pad,
            BoxVAlign::Bottom => top + h - pad - block_h,
            BoxVAlign::Center => top + (h - block_h) / 2.0,
        };

        for (i, line) in lines.iter().enumerate() {
            ctx.fill_text(line, tx, block_top + (i as f64 + 0.5) * line_h);
        }
    }

    // ── labels ───────────────────────────────────────────────────────────────

    fn draw_labels(&self, f: &Frame, tips: &mut Vec<LabelTipRegion>) {
        let ctx = f.ctx;
        for lb in &self.set.labels {
            let px = (f.x_of)(self.logical_of(f.deps, lb.xloc, lb.x));
            if px < -50.0 || px > f.w + 50.0 {
                continue;
            }

            let py: Option<f64> = match lb.yloc {
                LabelYLoc::Price => (f.y_of)(lb.y),
                LabelYLoc::Top => Some(14.0),
                LabelYLoc::Bottom => Some(f.h - 14.0),
                LabelYLoc::AboveBar | LabelYLoc::BelowBar => {
                    let Some(bar) = (f.deps.bar_at)(self.logical_of(f.deps, lb.xloc, lb.x)) else {
                        continue;
                    };
                    let above = lb.yloc == LabelYLoc::AboveBar;
                    let base = (f.y_of)(if above { bar.high } else { bar.low });
                    base.map(|b| b + if above { -14.0 } else { 14.0 })
                }
            };
            let Some(py) = py else {
                continue;
            };

            let color = lb
                .color
                .as_deref()
                .unwrap_or_else(|| theme_text_color(f.deps.theme));
            let font_px = font_size_px(lb.size);

            if is_point_shape(lb.style) {
                if !lb.no_fill {
                    self.draw_label_shape(ctx, lb.style, px, py, font_px, color);
                }
                if truthy(&lb.text).is_some() {
                    self.draw_label_text(f, lb, px, py + font_px, font_px, false);
                }
                if let Some(tooltip) = truthy(&lb.tooltip) {
                    let r = js_max(4.0, font_px * 0.6) + 3.0;
                    tips.push(LabelTipRegion {
                        left: px - r,
                        top: py - r,
                        right: px + r,
                        bottom: py + r,
                        text: tooltip.to_string(),
                    });
                }
            } else if matches!(lb.style, LabelStyle::None | LabelStyle::TextOutline) {
                if truthy(&lb.text).is_some() {
                    self.draw_label_text(
                        f,
                        lb,
                        px,
                        py,
                        font_px,
                        lb.style == LabelStyle::TextOutline,
                    );
                    if let Some(tooltip) = truthy(&lb.tooltip) {
                        tips.push(self.text_region(
                            ctx,
                            f.deps.theme,
                            lb,
                            px,
                            py,
                            font_px,
                            tooltip,
                        ));
                    }
                }
            } else {
                // noFill (na color) keeps the bubble style's geometry: draw_bubble
                // places the text as if the bubble were there and skips the fill.
                let (bx, by, bw, bh) = self.draw_bubble(f, lb, px, py, font_px, color);
                if let Some(tooltip) = truthy(&lb.tooltip) {
                    tips.push(LabelTipRegion {
                        left: bx,
                        top: by,
                        right: bx + bw,
                        bottom: by + bh,
                        text: tooltip.to_string(),
                    });
                }
            }
        }
    }

    /// Hover rect of a text-only label (style none/text_outline/noFill), centered like
    /// `draw_label_text`.
    #[allow(clippy::too_many_arguments)]
    fn text_region(
        &self,
        ctx: &dyn Canvas2d,
        theme: &Value,
        lb: &DrawingLabel,
        cx: f64,
        cy: f64,
        font_px: f64,
        text: &str,
    ) -> LabelTipRegion {
        let font = drawing_font(theme, lb.font_family, lb.bold, lb.italic, font_px);
        let lines: Vec<&str> = lb.text.as_deref().unwrap_or("").split('\n').collect();
        let widths: Vec<f64> = lines.iter().map(|l| self.measure(ctx, &font, l)).collect();
        let w = max_of(1.0, widths.into_iter());
        let h = font_px * 1.25 * lines.len() as f64;
        let left = match lb.text_align {
            BoxHAlign::Left => cx,
            BoxHAlign::Right => cx - w,
            BoxHAlign::Center => cx - w / 2.0,
        };
        LabelTipRegion {
            left,
            top: cy - h / 2.0,
            right: left + w,
            bottom: cy + h / 2.0,
            text: text.to_string(),
        }
    }

    fn draw_label_text(
        &self,
        f: &Frame,
        lb: &DrawingLabel,
        cx: f64,
        cy: f64,
        font_px: f64,
        outline: bool,
    ) {
        let ctx = f.ctx;
        ctx.set_font(&drawing_font(
            f.deps.theme,
            lb.font_family,
            lb.bold,
            lb.italic,
            font_px,
        ));
        ctx.set_text_align(match lb.text_align {
            BoxHAlign::Left => TextAlign::Left,
            BoxHAlign::Right => TextAlign::Right,
            BoxHAlign::Center => TextAlign::Center,
        });
        ctx.set_text_baseline(TextBaseline::Middle);
        let lines: Vec<&str> = lb.text.as_deref().unwrap_or("").split('\n').collect();
        let line_h = font_px * 1.25;
        let start_y = cy - (line_h * (lines.len() as f64 - 1.0)) / 2.0;
        let text_color = lb
            .text_color
            .as_deref()
            .unwrap_or_else(|| theme_text_color(f.deps.theme));
        if outline {
            ctx.set_line_width(3.0);
            ctx.set_stroke_style(&contrast_color(Some(text_color)).into());
            for (i, line) in lines.iter().enumerate() {
                ctx.stroke_text(line, cx, start_y + i as f64 * line_h);
            }
        }
        ctx.set_fill_style(&text_color.into());
        for (i, line) in lines.iter().enumerate() {
            ctx.fill_text(line, cx, start_y + i as f64 * line_h);
        }
    }

    fn draw_label_shape(
        &self,
        ctx: &dyn Canvas2d,
        style: LabelStyle,
        cx: f64,
        cy: f64,
        font_px: f64,
        color: &str,
    ) {
        let r = js_max(4.0, font_px * 0.6);
        ctx.set_fill_style(&color.into());
        ctx.set_stroke_style(&color.into());
        ctx.set_line_width(js_max(1.0, r * 0.3));
        ctx.begin_path();
        match style {
            LabelStyle::Square => {
                ctx.fill_rect(cx - r, cy - r, r * 2.0, r * 2.0);
            }
            LabelStyle::Diamond => {
                ctx.move_to(cx, cy - r);
                ctx.line_to(cx + r, cy);
                ctx.line_to(cx, cy + r);
                ctx.line_to(cx - r, cy);
                ctx.close_path();
                ctx.fill();
            }
            LabelStyle::TriangleUp | LabelStyle::ArrowUp => {
                ctx.move_to(cx, cy - r);
                ctx.line_to(cx + r, cy + r);
                ctx.line_to(cx - r, cy + r);
                ctx.close_path();
                ctx.fill();
            }
            LabelStyle::TriangleDown | LabelStyle::ArrowDown => {
                ctx.move_to(cx, cy + r);
                ctx.line_to(cx + r, cy - r);
                ctx.line_to(cx - r, cy - r);
                ctx.close_path();
                ctx.fill();
            }
            LabelStyle::Flag => {
                ctx.move_to(cx - r * 0.7, cy - r);
                ctx.line_to(cx - r * 0.7, cy + r);
                ctx.move_to(cx - r * 0.7, cy - r);
                ctx.line_to(cx + r, cy - r * 0.4);
                ctx.line_to(cx - r * 0.7, cy + r * 0.2);
                ctx.stroke();
                ctx.fill();
            }
            LabelStyle::Cross => {
                ctx.move_to(cx, cy - r);
                ctx.line_to(cx, cy + r);
                ctx.move_to(cx - r, cy);
                ctx.line_to(cx + r, cy);
                ctx.stroke();
            }
            LabelStyle::XCross => {
                ctx.move_to(cx - r, cy - r);
                ctx.line_to(cx + r, cy + r);
                ctx.move_to(cx + r, cy - r);
                ctx.line_to(cx - r, cy + r);
                ctx.stroke();
            }
            // A circle, and the fallback for a style with no marker of its own.
            _ => {
                ctx.arc(cx, cy, r, 0.0, std::f64::consts::PI * 2.0, false);
                ctx.fill();
            }
        }
    }

    /// Draw a bubble label; returns the bubble body rect `(x, y, w, h)` (for tooltip
    /// hit-testing).
    fn draw_bubble(
        &self,
        f: &Frame,
        lb: &DrawingLabel,
        px: f64,
        py: f64,
        font_px: f64,
        color: &str,
    ) -> (f64, f64, f64, f64) {
        let ctx = f.ctx;
        let font = drawing_font(f.deps.theme, lb.font_family, lb.bold, lb.italic, font_px);
        ctx.set_font(&font);
        let lines: Vec<&str> = lb.text.as_deref().unwrap_or("").split('\n').collect();
        let pad_x = 6.0;
        let pad_y = 4.0;
        let line_h = font_px * 1.25;
        let widths: Vec<f64> = lines.iter().map(|l| self.measure(ctx, &font, l)).collect();
        let text_w = max_of(1.0, widths.into_iter());
        let w = text_w + pad_x * 2.0;
        let h = line_h * lines.len() as f64 + pad_y * 2.0;
        let ptr = 7.0;

        #[derive(PartialEq)]
        enum Pointer {
            Down,
            Up,
            Left,
            Right,
            None,
        }
        let (bx, by, pointer) = match lb.style {
            LabelStyle::LabelUp => (px - w / 2.0, py + ptr, Pointer::Up),
            LabelStyle::LabelLeft => (px + ptr, py - h / 2.0, Pointer::Left),
            LabelStyle::LabelRight => (px - w - ptr, py - h / 2.0, Pointer::Right),
            LabelStyle::LabelCenter => (px - w / 2.0, py - h / 2.0, Pointer::None),
            LabelStyle::LabelLowerLeft => (px + ptr, py - h - ptr, Pointer::None),
            LabelStyle::LabelLowerRight => (px - w - ptr, py - h - ptr, Pointer::None),
            LabelStyle::LabelUpperLeft => (px + ptr, py + ptr, Pointer::None),
            LabelStyle::LabelUpperRight => (px - w - ptr, py + ptr, Pointer::None),
            _ => (px - w / 2.0, py - h - ptr, Pointer::Down),
        };

        if !lb.no_fill {
            ctx.set_fill_style(&color.into());
            round_rect_path(ctx, bx, by, w, h, 4.0);
            ctx.fill();
            if pointer != Pointer::None {
                ctx.begin_path();
                match pointer {
                    Pointer::Down => {
                        ctx.move_to(px - ptr, by + h);
                        ctx.line_to(px + ptr, by + h);
                        ctx.line_to(px, py);
                    }
                    Pointer::Up => {
                        ctx.move_to(px - ptr, by);
                        ctx.line_to(px + ptr, by);
                        ctx.line_to(px, py);
                    }
                    Pointer::Left => {
                        ctx.move_to(bx, py - ptr);
                        ctx.line_to(bx, py + ptr);
                        ctx.line_to(px, py);
                    }
                    _ => {
                        ctx.move_to(bx + w, py - ptr);
                        ctx.line_to(bx + w, py + ptr);
                        ctx.line_to(px, py);
                    }
                }
                ctx.close_path();
                ctx.fill();
            }
        }

        if truthy(&lb.text).is_some() {
            // With no bubble behind it, contrast-of-bubble is meaningless: fall back to
            // the theme's text color (same default as bare label text).
            let fill = match lb.text_color.as_deref() {
                Some(c) => c,
                None if lb.no_fill => theme_text_color(f.deps.theme),
                None => contrast_color(Some(color)),
            };
            ctx.set_fill_style(&fill.into());
            ctx.set_text_baseline(TextBaseline::Middle);
            // Pine `textalign` aligns the LINES inside the bubble (the bubble itself
            // stays put).
            let tx = match lb.text_align {
                BoxHAlign::Left => {
                    ctx.set_text_align(TextAlign::Left);
                    bx + pad_x
                }
                BoxHAlign::Right => {
                    ctx.set_text_align(TextAlign::Right);
                    bx + w - pad_x
                }
                BoxHAlign::Center => {
                    ctx.set_text_align(TextAlign::Center);
                    bx + w / 2.0
                }
            };
            let start_y = by + pad_y + line_h / 2.0;
            for (i, line) in lines.iter().enumerate() {
                ctx.fill_text(line, tx, start_y + i as f64 * line_h);
            }
        }
        (bx, by, w, h)
    }
}

/// The path of a Catmull-Rom spline through `pts` as cubic beziers (end points repeated
/// when open, wrapped around when closed).
fn curved_path(ctx: &dyn Canvas2d, pts: &[(f64, f64)], closed: bool) {
    let n = pts.len() as i64;
    let at = |i: i64| -> (f64, f64) {
        let idx = if closed {
            i.rem_euclid(n)
        } else {
            i.clamp(0, n - 1)
        };
        pts[idx as usize]
    };
    ctx.move_to(pts[0].0, pts[0].1);
    let last = if closed { n } else { n - 1 };
    for i in 0..last {
        let p0 = at(i - 1);
        let p1 = at(i);
        let p2 = at(i + 1);
        let p3 = at(i + 2);
        let cp1x = p1.0 + (p2.0 - p0.0) / 6.0;
        let cp1y = p1.1 + (p2.1 - p0.1) / 6.0;
        let cp2x = p2.0 - (p3.0 - p1.0) / 6.0;
        let cp2y = p2.1 - (p3.1 - p1.1) / 6.0;
        ctx.bezier_curve_to(cp1x, cp1y, cp2x, cp2y, p2.0, p2.1);
    }
}

/// A rounded rectangle built from `arcTo` corners (not the context's `roundRect`).
fn round_rect_path(ctx: &dyn Canvas2d, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let rr = js_min(js_min(r, w / 2.0), h / 2.0);
    ctx.begin_path();
    ctx.move_to(x + rr, y);
    ctx.arc_to(x + w, y, x + w, y + h, rr);
    ctx.arc_to(x + w, y + h, x, y + h, rr);
    ctx.arc_to(x, y + h, x, y, rr);
    ctx.arc_to(x, y, x + w, y, rr);
    ctx.close_path();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::series::LineStyle;
    use crate::render::canvas::RecordingCanvas;
    use serde_json::json;

    // ── fixtures ──

    fn theme() -> Value {
        json!({ "fontFamily": "sans-serif", "textColor": "#ddd" })
    }

    fn line(over: impl FnOnce(&mut DrawingLine)) -> DrawingLine {
        let mut l = DrawingLine {
            id: "ln".into(),
            pane_id: "price".into(),
            xloc: DrawingXLoc::BarIndex,
            x1: 100.0,
            y1: 5.0,
            x2: 110.0,
            y2: 6.0,
            extend: DrawingExtend::None,
            color: Some("#0f0".into()),
            invisible: false,
            width: 1.0,
            style: LineStyle::Solid,
            arrow_left: false,
            arrow_right: false,
            overlay: false,
        };
        over(&mut l);
        l
    }

    fn boxed(over: impl FnOnce(&mut DrawingBox)) -> DrawingBox {
        let mut b = DrawingBox {
            id: "bx".into(),
            pane_id: "price".into(),
            xloc: DrawingXLoc::BarIndex,
            left: 100.0,
            top: 6.0,
            right: 110.0,
            bottom: 5.0,
            extend: DrawingExtend::None,
            bg_color: Some("#0f04".into()),
            border_color: Some("#0f0".into()),
            border_width: 1.0,
            border_style: LineStyle::Solid,
            text: None,
            text_color: None,
            text_size: BoxTextSize::Auto,
            h_align: BoxHAlign::Center,
            v_align: BoxVAlign::Center,
            wrap: false,
            font_family: BoxFontFamily::Default,
            bold: false,
            italic: false,
            overlay: false,
        };
        over(&mut b);
        b
    }

    fn label(over: impl FnOnce(&mut DrawingLabel)) -> DrawingLabel {
        let mut l = DrawingLabel {
            id: "l1".into(),
            pane_id: "price".into(),
            xloc: DrawingXLoc::BarIndex,
            x: 100.0,
            y: 100.0,
            yloc: LabelYLoc::Price,
            text: Some("AB".into()),
            style: LabelStyle::LabelDown,
            color: None,
            text_color: None,
            size: BoxTextSize::Normal, // 14px, line height 17.5
            text_align: BoxHAlign::Center,
            tooltip: None,
            font_family: BoxFontFamily::Default,
            bold: false,
            italic: false,
            no_fill: false,
            overlay: false,
        };
        over(&mut l);
        l
    }

    fn polyline(points: &[(f64, f64)], over: impl FnOnce(&mut DrawingPolyline)) -> DrawingPolyline {
        let mut p = DrawingPolyline {
            id: "pl".into(),
            pane_id: "price".into(),
            points: points
                .iter()
                .map(|(x, price)| crate::model::drawings::PolylinePoint {
                    xloc: DrawingXLoc::BarIndex,
                    x: *x,
                    price: *price,
                })
                .collect(),
            curved: false,
            closed: false,
            line_color: Some("#f00".into()),
            fill_color: None,
            line_width: 1.0,
            line_style: LineStyle::Solid,
            arrow_left: false,
            arrow_right: false,
            overlay: false,
        };
        over(&mut p);
        p
    }

    fn set_of(f: impl FnOnce(&mut DrawingSet)) -> DrawingSet {
        let mut s = DrawingSet::default();
        f(&mut s);
        s
    }

    /// Identity coordinates, `bar_time` read as a logical index.
    fn paint_with(
        set: DrawingSet,
        offset: f64,
        w: f64,
        h: f64,
        bar: Option<BarHighLow>,
    ) -> (RecordingCanvas, DrawingSceneRenderer) {
        let theme = theme();
        let time = |ms: f64| ms;
        let bar_at = move |_: f64| bar;
        let deps = LayerDeps {
            time_to_logical: &time,
            bar_at: &bar_at,
            theme: &theme,
        };
        let mut r = DrawingSceneRenderer::new();
        r.set_set(set, offset);
        let canvas = RecordingCanvas::new();
        r.render(&deps, &canvas, w, h, &|l| l, &|p| Some(p));
        (canvas, r)
    }

    fn paint(set: DrawingSet) -> (RecordingCanvas, DrawingSceneRenderer) {
        paint_with(set, 0.0, 800.0, 400.0, None)
    }

    fn range_of(set: DrawingSet, offset: f64, from: f64, to: f64) -> Option<DrawingPriceRange> {
        let theme = theme();
        let time = |_: f64| 0.0;
        let bar_at = |_: f64| None;
        let deps = LayerDeps {
            time_to_logical: &time,
            bar_at: &bar_at,
            theme: &theme,
        };
        let mut r = DrawingSceneRenderer::new();
        r.set_set(set, offset);
        r.price_range(&deps, from, to)
    }

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    // ── autoscale: anchor offsets (anchor-offset.test.ts) ──

    #[test]
    fn an_anchored_models_bar_index_coordinates_shift_by_its_offset() {
        // The model's bar_index 0..2 anchored at chart bar 50 occupies chart bars 50..52.
        let ln = line(|l| {
            l.x1 = 0.0;
            l.x2 = 2.0;
            l.y1 = 5.0;
            l.y2 = 6.0;
        });
        let set = set_of(|s| s.lines.push(ln));
        assert!(range_of(set.clone(), 50.0, 50.0, 52.0).is_some());
        assert!(range_of(set, 50.0, 0.0, 2.0).is_none());
    }

    #[test]
    fn offset_zero_keeps_raw_bar_index_semantics() {
        let ln = line(|l| {
            l.x1 = 0.0;
            l.x2 = 2.0;
        });
        let set = set_of(|s| s.lines.push(ln));
        assert!(range_of(set.clone(), 0.0, 0.0, 2.0).is_some());
        assert!(range_of(set, 0.0, 50.0, 52.0).is_none());
    }

    #[test]
    fn bar_time_coordinates_ignore_the_offset_and_use_the_time_resolver() {
        let ln = line(|l| {
            l.xloc = DrawingXLoc::BarTime;
            l.x1 = 1000.0;
            l.x2 = 2000.0;
        });
        let theme = theme();
        let time = |ms: f64| ms / 1000.0;
        let bar_at = |_: f64| None;
        let deps = LayerDeps {
            time_to_logical: &time,
            bar_at: &bar_at,
            theme: &theme,
        };
        let mut r = DrawingSceneRenderer::new();
        r.set_set(set_of(|s| s.lines.push(ln)), 50.0);
        assert!(r.price_range(&deps, 1.0, 2.0).is_some());
        assert!(r.price_range(&deps, 51.0, 52.0).is_none());
    }

    // ── autoscale: extended lines (drawing-extend-autoscale.test.ts) ──

    fn line_range(l: DrawingLine, from: f64, to: f64) -> Option<DrawingPriceRange> {
        range_of(set_of(|s| s.lines.push(l)), 0.0, from, to)
    }

    const ALL_EXTENDS: [DrawingExtend; 4] = [
        DrawingExtend::None,
        DrawingExtend::Left,
        DrawingExtend::Right,
        DrawingExtend::Both,
    ];

    #[test]
    fn a_line_folds_its_anchor_prices_while_the_segment_is_in_view() {
        for extend in ALL_EXTENDS {
            let r = line_range(line(|l| l.extend = extend), 90.0, 120.0).unwrap();
            assert_eq!((r.min, r.max), (5.0, 6.0));
        }
    }

    #[test]
    fn extend_both_keeps_folding_with_off_screen_anchors() {
        for (from, to) in [(500.0, 600.0), (0.0, 50.0)] {
            let r = line_range(line(|l| l.extend = DrawingExtend::Both), from, to).unwrap();
            assert_eq!((r.min, r.max), (5.0, 6.0));
        }
    }

    #[test]
    fn extend_left_and_right_fold_only_on_the_side_the_projection_covers() {
        let right = |from, to| line_range(line(|l| l.extend = DrawingExtend::Right), from, to);
        let left = |from, to| line_range(line(|l| l.extend = DrawingExtend::Left), from, to);
        assert!(right(500.0, 600.0).is_some());
        assert!(right(0.0, 50.0).is_none());
        assert!(left(0.0, 50.0).is_some());
        assert!(left(500.0, 600.0).is_none());
    }

    #[test]
    fn unextended_off_screen_lines_stay_excluded() {
        assert!(line_range(line(|_| {}), 500.0, 600.0).is_none());
        assert!(line_range(line(|_| {}), 0.0, 50.0).is_none());
    }

    #[test]
    fn a_vertical_line_folds_only_while_its_bar_is_in_view() {
        for extend in [
            DrawingExtend::Left,
            DrawingExtend::Right,
            DrawingExtend::Both,
        ] {
            let point = line(|l| {
                l.x1 = 100.0;
                l.x2 = 100.0;
                l.y1 = 7.0;
                l.y2 = 7.0;
                l.extend = extend;
            });
            let r = line_range(point.clone(), 90.0, 120.0).unwrap();
            assert_eq!((r.min, r.max), (7.0, 7.0));
            assert!(line_range(point.clone(), 500.0, 600.0).is_none());
            assert!(line_range(point, 0.0, 50.0).is_none());
        }
    }

    #[test]
    fn an_invisible_line_contributes_nothing() {
        assert!(line_range(line(|l| l.invisible = true), 90.0, 120.0).is_none());
    }

    #[test]
    fn a_reversed_window_is_normalized() {
        assert!(line_range(line(|_| {}), 120.0, 90.0).is_some());
    }

    // ── autoscale: extended boxes ──

    fn box_range(b: DrawingBox, from: f64, to: f64) -> Option<DrawingPriceRange> {
        range_of(set_of(|s| s.boxes.push(b)), 0.0, from, to)
    }

    #[test]
    fn a_box_folds_top_and_bottom_while_the_span_is_in_view() {
        for extend in ALL_EXTENDS {
            let r = box_range(boxed(|b| b.extend = extend), 90.0, 120.0).unwrap();
            assert_eq!((r.min, r.max), (5.0, 6.0));
        }
    }

    #[test]
    fn extend_both_boxes_fold_off_screen() {
        for (from, to) in [(500.0, 600.0), (0.0, 50.0)] {
            let r = box_range(boxed(|b| b.extend = DrawingExtend::Both), from, to).unwrap();
            assert_eq!((r.min, r.max), (5.0, 6.0));
        }
    }

    #[test]
    fn extend_left_and_right_boxes_fold_only_on_the_covered_side() {
        let right = |from, to| box_range(boxed(|b| b.extend = DrawingExtend::Right), from, to);
        let left = |from, to| box_range(boxed(|b| b.extend = DrawingExtend::Left), from, to);
        assert!(right(500.0, 600.0).is_some());
        assert!(right(0.0, 50.0).is_none());
        assert!(left(0.0, 50.0).is_some());
        assert!(left(500.0, 600.0).is_none());
    }

    #[test]
    fn unextended_off_screen_boxes_stay_excluded() {
        assert!(box_range(boxed(|_| {}), 500.0, 600.0).is_none());
        assert!(box_range(boxed(|_| {}), 0.0, 50.0).is_none());
    }

    #[test]
    fn a_box_with_no_fill_border_or_text_paints_nothing_and_folds_nothing() {
        let bare = boxed(|b| {
            b.bg_color = None;
            b.border_color = None;
        });
        assert!(box_range(bare.clone(), 90.0, 120.0).is_none());
        let texted = DrawingBox {
            text: Some("x".into()),
            ..bare.clone()
        };
        assert!(box_range(texted, 90.0, 120.0).is_some());
        let zero_border = boxed(|b| {
            b.bg_color = None;
            b.border_width = 0.0;
        });
        assert!(box_range(zero_border, 90.0, 120.0).is_none());
    }

    // ── autoscale: linefills, polylines, labels ──

    #[test]
    fn a_linefill_folds_both_lines_prices_and_extends_if_either_line_does() {
        let a = line(|l| {
            l.x1 = 100.0;
            l.x2 = 110.0;
            l.y1 = 1.0;
            l.y2 = 2.0;
        });
        let b = line(|l| {
            l.x1 = 100.0;
            l.x2 = 110.0;
            l.y1 = 8.0;
            l.y2 = 9.0;
            l.extend = DrawingExtend::Right;
        });
        let lf = DrawingLinefill {
            id: "lf".into(),
            pane_id: "price".into(),
            line1: a.clone(),
            line2: b,
            color: Some("#00f".into()),
            overlay: false,
        };
        let set = set_of(|s| s.linefills.push(lf.clone()));
        let r = range_of(set.clone(), 0.0, 500.0, 600.0).unwrap();
        assert_eq!((r.min, r.max), (1.0, 9.0));
        // either line extending makes the pair cover every window
        assert!(range_of(set, 0.0, 0.0, 50.0).is_some());
        let none = DrawingLinefill { color: None, ..lf };
        assert!(range_of(set_of(|s| s.linefills.push(none)), 0.0, 90.0, 120.0).is_none());
    }

    #[test]
    fn a_polyline_needs_two_points_in_view() {
        let pl = polyline(&[(100.0, 3.0), (110.0, 4.0), (105.0, 8.0)], |_| {});
        let set = set_of(|s| s.polylines.push(pl));
        let r = range_of(set.clone(), 0.0, 90.0, 120.0).unwrap();
        assert_eq!((r.min, r.max), (3.0, 8.0));
        assert!(range_of(set, 0.0, 500.0, 600.0).is_none());
        let one = polyline(&[(100.0, 3.0)], |_| {});
        assert!(range_of(set_of(|s| s.polylines.push(one)), 0.0, 90.0, 120.0).is_none());
    }

    #[test]
    fn labels_fold_their_price_or_the_bars_extreme_and_report_margins() {
        let theme = theme();
        let time = |_: f64| 0.0;
        let bar_at = |_: f64| {
            Some(BarHighLow {
                high: 12.0,
                low: 4.0,
            })
        };
        let deps = LayerDeps {
            time_to_logical: &time,
            bar_at: &bar_at,
            theme: &theme,
        };
        let mut r = DrawingSceneRenderer::new();
        r.set_set(
            set_of(|s| {
                s.labels.push(label(|l| {
                    l.x = 100.0;
                    l.yloc = LabelYLoc::Price;
                    l.y = 50.0;
                }));
                s.labels.push(label(|l| {
                    l.x = 100.0;
                    l.yloc = LabelYLoc::AboveBar;
                    l.text = Some("a\nb".into());
                }));
                s.labels.push(label(|l| {
                    l.x = 100.0;
                    l.yloc = LabelYLoc::BelowBar;
                    l.style = LabelStyle::Circle;
                }));
                // pinned to the pane edge: no price at all
                s.labels.push(label(|l| {
                    l.x = 100.0;
                    l.yloc = LabelYLoc::Top;
                    l.y = 9999.0;
                }));
            }),
            0.0,
        );
        let range = r.price_range(&deps, 90.0, 110.0).unwrap();
        assert_eq!((range.min, range.max), (4.0, 50.0));
        // above: 14 + 14 * 1.25 * 2 lines + 8 + 7 = 64; below (circle): 14 + max(4, 8.4) + 2
        approx(range.above_margin, 64.0);
        approx(range.below_margin, 24.4);
        // a window past the label folds nothing
        assert!(r.price_range(&deps, 200.0, 300.0).is_none());
    }

    #[test]
    fn a_nan_price_range_is_none_like_the_original() {
        let ln = line(|l| {
            l.y1 = f64::NAN;
            l.y2 = f64::NAN;
        });
        assert!(line_range(ln, 90.0, 120.0).is_none());
    }

    #[test]
    fn model_drawing_set_splits_on_the_overlay_flag() {
        let mut m = crate::model::indicator::IndicatorModel {
            id: "m".into(),
            title: "m".into(),
            shorttitle: None,
            display_title: None,
            display_shorttitle: None,
            overlay: true,
            pane_hint: crate::model::PaneHint::Price,
            native: None,
            legend: None,
            pane_axis: None,
            pane_id: None,
            own_scale: false,
            anchor_time: None,
            series: vec![],
            fills: vec![],
            backgrounds: vec![],
            price_lines: vec![],
            lines: vec![],
            boxes: vec![],
            labels: vec![],
            polylines: vec![],
            linefills: vec![],
            tables: vec![],
            bar_colors: vec![],
            trades: vec![],
            inputs: vec![],
            input_values: Default::default(),
            props: vec![],
            prop_values: Default::default(),
        };
        m.lines.push(line(|l| l.overlay = true));
        m.lines.push(line(|l| l.id = "own".into()));
        m.labels.push(label(|_| {}));
        let own = model_drawing_set(&m, false);
        assert_eq!(own.lines.len(), 1);
        assert_eq!(own.lines[0].id, "own");
        assert_eq!(own.labels.len(), 1);
        let forced = model_drawing_set(&m, true);
        assert_eq!(forced.lines.len(), 1);
        assert!(forced.labels.is_empty());
        assert!(drawing_set_empty(&DrawingSet::default()));
        assert!(!drawing_set_empty(&own));
    }

    // ── lines: complete call streams ──

    #[test]
    fn an_empty_set_paints_nothing_at_all() {
        let (c, r) = paint(DrawingSet::default());
        assert!(c.is_empty());
        assert!(r.is_empty());
        assert!(r.label_tip_regions().is_empty());
    }

    #[test]
    fn a_dashed_line_streams_path_style_dash_stroke_and_reset() {
        let ln = line(|l| {
            l.x1 = 10.0;
            l.y1 = 20.0;
            l.x2 = 110.0;
            l.y2 = 60.0;
            l.width = 2.0;
            l.style = LineStyle::Dashed;
        });
        let (c, _) = paint_with(set_of(|s| s.lines.push(ln)), 0.0, 200.0, 100.0, None);
        assert_eq!(
            c.to_json(),
            json!([
                ["save"],
                ["beginPath"],
                ["moveTo", 10.0, 20.0],
                ["lineTo", 110.0, 60.0],
                ["strokeStyle", "#0f0"],
                ["lineWidth", 2.0],
                ["setLineDash", [8.0, 6.0]],
                ["stroke"],
                ["setLineDash", []],
                ["restore"],
            ])
        );
    }

    #[test]
    fn a_line_without_a_color_takes_the_theme_text_color() {
        let ln = line(|l| l.color = None);
        let (c, _) = paint(set_of(|s| s.lines.push(ln)));
        assert_eq!(c.calls("strokeStyle"), vec![json!(["strokeStyle", "#ddd"])]);
    }

    #[test]
    fn a_right_extended_line_with_arrows_streams_the_extension_and_both_heads() {
        let ln = line(|l| {
            l.x1 = 10.0;
            l.y1 = 20.0;
            l.x2 = 110.0;
            l.y2 = 60.0;
            l.extend = DrawingExtend::Right;
            l.arrow_left = true;
            l.arrow_right = true;
        });
        let (c, _) = paint_with(set_of(|s| s.lines.push(ln)), 0.0, 200.0, 100.0, None);
        // extend right: [x1, y at x1, W + 2, y at W + 2] with slope 40 / 100.
        let slope = (60.0 - 20.0) / (110.0 - 10.0);
        let (bx, by) = (202.0, 20.0 + slope * (202.0 - 10.0));
        let ops = c.ops();
        assert_eq!(
            &ops[..8],
            &[
                json!(["save"]),
                json!(["beginPath"]),
                json!(["moveTo", 10.0, 20.0]),
                json!(["lineTo", bx, by]),
                json!(["strokeStyle", "#0f0"]),
                json!(["lineWidth", 1.0]),
                json!(["setLineDash", []]),
                json!(["stroke"]),
            ]
        );
        // p2 is right of p1 and the line extends right: the head sits at the extended end,
        // pointing away from p1; the left head sits at p1 (not extended that way),
        // pointing away from p2.
        let head = |tip: (f64, f64), from: (f64, f64)| -> Vec<serde_json::Value> {
            let angle = (tip.1 - from.1).atan2(tip.0 - from.0);
            let size = 6.0 + 1.0 * 2.0;
            vec![
                json!(["beginPath"]),
                json!(["moveTo", tip.0, tip.1]),
                json!([
                    "lineTo",
                    tip.0 - size * (angle - 0.45).cos(),
                    tip.1 - size * (angle - 0.45).sin()
                ]),
                json!([
                    "lineTo",
                    tip.0 - size * (angle + 0.45).cos(),
                    tip.1 - size * (angle + 0.45).sin()
                ]),
                json!(["closePath"]),
                json!(["fillStyle", "#0f0"]),
                json!(["fill"]),
            ]
        };
        let mut expected = vec![json!(["setLineDash", []])];
        expected.extend(head((bx, by), (10.0, 20.0)));
        expected.extend(head((10.0, 20.0), (110.0, 60.0)));
        expected.push(json!(["restore"]));
        assert_eq!(&ops[8..], &expected[..]);
    }

    #[test]
    fn lines_off_screen_or_without_a_y_are_skipped() {
        let off = line(|l| {
            l.x1 = -50.0;
            l.x2 = -10.0;
        });
        let right_off = line(|l| {
            l.x1 = 900.0;
            l.x2 = 950.0;
        });
        let (c, _) = paint(set_of(|s| {
            s.lines.push(off);
            s.lines.push(right_off);
        }));
        assert_eq!(c.names(), vec!["save", "restore"]);

        // a missing y (the pane is gone) skips too
        let theme = theme();
        let time = |ms: f64| ms;
        let bar_at = |_: f64| None;
        let deps = LayerDeps {
            time_to_logical: &time,
            bar_at: &bar_at,
            theme: &theme,
        };
        let mut r = DrawingSceneRenderer::new();
        r.set_set(set_of(|s| s.lines.push(line(|_| {}))), 0.0);
        let c = RecordingCanvas::new();
        r.render(&deps, &c, 800.0, 400.0, &|l| l, &|_| None);
        assert_eq!(c.names(), vec!["save", "restore"]);
    }

    #[test]
    fn an_extended_line_is_painted_even_when_its_anchors_are_off_screen() {
        let ln = line(|l| {
            l.x1 = -50.0;
            l.x2 = -10.0;
            l.extend = DrawingExtend::Right;
        });
        let (c, _) = paint(set_of(|s| s.lines.push(ln)));
        assert_eq!(c.calls("stroke").len(), 1);
    }

    #[test]
    fn an_invisible_line_is_not_painted() {
        let (c, _) = paint(set_of(|s| s.lines.push(line(|l| l.invisible = true))));
        assert_eq!(c.names(), vec!["save", "restore"]);
    }

    // ── linefills ──

    #[test]
    fn a_linefill_streams_the_quad_between_the_two_oriented_lines() {
        // line1 runs right to left: it is oriented left to right before the quad is built.
        let a = line(|l| {
            l.x1 = 100.0;
            l.y1 = 10.0;
            l.x2 = 20.0;
            l.y2 = 30.0;
        });
        let b = line(|l| {
            l.x1 = 20.0;
            l.y1 = 50.0;
            l.x2 = 100.0;
            l.y2 = 70.0;
        });
        let lf = DrawingLinefill {
            id: "lf".into(),
            pane_id: "price".into(),
            line1: a,
            line2: b,
            color: Some("#00f4".into()),
            overlay: false,
        };
        let (c, _) = paint(set_of(|s| s.linefills.push(lf)));
        assert_eq!(
            c.to_json(),
            json!([
                ["save"],
                ["beginPath"],
                ["moveTo", 20.0, 30.0],
                ["lineTo", 100.0, 10.0],
                ["lineTo", 100.0, 70.0],
                ["lineTo", 20.0, 50.0],
                ["closePath"],
                ["fillStyle", "#00f4"],
                ["fill"],
                ["restore"],
            ])
        );
    }

    // ── polylines ──

    #[test]
    fn a_closed_filled_polyline_streams_path_fill_then_stroke() {
        let pl = polyline(&[(10.0, 10.0), (50.0, 40.0), (90.0, 20.0)], |p| {
            p.closed = true;
            p.fill_color = Some("#0f04".into());
            p.line_width = 3.0;
            p.line_style = LineStyle::Dotted;
        });
        let (c, _) = paint(set_of(|s| s.polylines.push(pl)));
        assert_eq!(
            c.to_json(),
            json!([
                ["save"],
                ["beginPath"],
                ["moveTo", 10.0, 10.0],
                ["lineTo", 50.0, 40.0],
                ["lineTo", 90.0, 20.0],
                ["closePath"],
                ["fillStyle", "#0f04"],
                ["fill"],
                ["strokeStyle", "#f00"],
                ["lineWidth", 3.0],
                ["setLineDash", [3.0, 6.0]],
                ["stroke"],
                ["setLineDash", []],
                ["restore"],
            ])
        );
    }

    #[test]
    fn an_open_curved_polyline_streams_catmull_rom_beziers_with_repeated_end_points() {
        let pl = polyline(&[(10.0, 10.0), (50.0, 40.0), (90.0, 20.0)], |p| {
            p.curved = true
        });
        let (c, _) = paint(set_of(|s| s.polylines.push(pl)));
        let ops = c.ops();
        assert_eq!(
            &ops[..4],
            &[
                json!(["save"]),
                json!(["beginPath"]),
                json!(["moveTo", 10.0, 10.0]),
                // i = 0: p0 = p1 = A (10, 10), p2 = B (50, 40), p3 = C (90, 20)
                json!([
                    "bezierCurveTo",
                    10.0 + (50.0 - 10.0) / 6.0,
                    10.0 + (40.0 - 10.0) / 6.0,
                    50.0 - (90.0 - 10.0) / 6.0,
                    40.0 - (20.0 - 10.0) / 6.0,
                    50.0,
                    40.0
                ]),
            ]
        );
        // i = 1: p0 = A, p1 = B, p2 = p3 = C
        assert_eq!(
            ops[4],
            json!([
                "bezierCurveTo",
                50.0 + (90.0 - 10.0) / 6.0,
                40.0 + (20.0 - 10.0) / 6.0,
                90.0 - (90.0 - 50.0) / 6.0,
                20.0 - (20.0 - 40.0) / 6.0,
                90.0,
                20.0
            ])
        );
        // then strokeStyle, lineWidth, setLineDash, stroke, setLineDash, restore
        assert_eq!(ops.len(), 11);
    }

    #[test]
    fn a_closed_curved_polyline_wraps_around() {
        let pl = polyline(&[(10.0, 10.0), (50.0, 40.0), (90.0, 20.0)], |p| {
            p.curved = true;
            p.closed = true;
            p.line_color = None;
        });
        let (c, _) = paint(set_of(|s| s.polylines.push(pl)));
        let ops = c.ops();
        // three curves (one per edge incl. the closing one), then closePath
        assert_eq!(c.calls("bezierCurveTo").len(), 3);
        // the closing curve (i = 2): p0 = B, p1 = C, p2 = A (wrapped), p3 = B (wrapped)
        assert_eq!(
            c.calls("bezierCurveTo")[2],
            json!([
                "bezierCurveTo",
                90.0 + (10.0 - 50.0) / 6.0,
                20.0 + (10.0 - 40.0) / 6.0,
                10.0 - (50.0 - 90.0) / 6.0,
                10.0 - (40.0 - 20.0) / 6.0,
                10.0,
                10.0
            ])
        );
        assert_eq!(ops[ops.len() - 2], json!(["closePath"]));
        // no stroke color and no fill: only the path
        assert!(c.calls("stroke").is_empty() && c.calls("fill").is_empty());
    }

    #[test]
    fn polyline_arrows_follow_every_segment_and_wrap_when_closed() {
        let pl = polyline(&[(10.0, 10.0), (50.0, 40.0), (90.0, 20.0)], |p| {
            p.arrow_right = true;
        });
        let (open, _) = paint(set_of(|s| s.polylines.push(pl.clone())));
        assert_eq!(open.calls("fill").len(), 2); // two segments, one head each
        let closed = DrawingPolyline { closed: true, ..pl };
        let (c, _) = paint(set_of(|s| s.polylines.push(closed)));
        assert_eq!(c.calls("fill").len(), 3);
    }

    #[test]
    fn polylines_with_fewer_than_two_drawable_points_or_off_screen_are_skipped() {
        let one = polyline(&[(10.0, 10.0)], |_| {});
        let off = polyline(&[(-50.0, 10.0), (-10.0, 20.0)], |_| {});
        let (c, _) = paint(set_of(|s| {
            s.polylines.push(one);
            s.polylines.push(off);
        }));
        assert_eq!(c.names(), vec!["save", "restore"]);
    }

    // ── boxes ──

    #[test]
    fn a_box_with_centered_text_streams_fill_border_then_the_text_block() {
        let bx = boxed(|b| {
            b.left = 20.0;
            b.right = 120.0;
            b.top = 30.0;
            b.bottom = 80.0;
            b.bg_color = Some("#112233".into());
            b.border_color = Some("#fff".into());
            b.text = Some("Hi".into());
            b.text_size = BoxTextSize::Normal;
        });
        let (c, _) = paint(set_of(|s| s.boxes.push(bx)));
        // size 14 => lineH 18.2, blockH 18.2; centered: top 30 + (50 - 18.2) / 2
        let block_top = 30.0 + (50.0 - 14.0 * 1.3) / 2.0;
        assert_eq!(
            c.to_json(),
            json!([
                ["save"],
                ["fillStyle", "#112233"],
                ["fillRect", 20.0, 30.0, 100.0, 50.0],
                ["strokeStyle", "#fff"],
                ["lineWidth", 1.0],
                ["setLineDash", []],
                ["strokeRect", 20.0, 30.0, 100.0, 50.0],
                ["setLineDash", []],
                ["font", "14px sans-serif"],
                ["fillStyle", "#ffffff"],
                ["textAlign", "center"],
                ["textBaseline", "middle"],
                ["fillText", "Hi", 70.0, block_top + 0.5 * (14.0 * 1.3)],
                ["restore"],
            ])
        );
    }

    #[test]
    fn box_text_aligns_left_right_top_bottom_with_a_4px_pad_and_bold_italic_mono() {
        let bx = boxed(|b| {
            b.left = 20.0;
            b.right = 120.0;
            b.top = 30.0;
            b.bottom = 80.0;
            b.bg_color = None;
            b.border_color = None;
            b.text = Some("a\nb".into());
            b.text_size = BoxTextSize::Small; // 12px, lineH 15.6, two lines => 31.2
            b.text_color = Some("#abc".into());
            b.h_align = BoxHAlign::Right;
            b.v_align = BoxVAlign::Bottom;
            b.font_family = BoxFontFamily::Monospace;
            b.bold = true;
            b.italic = true;
        });
        let (c, _) = paint(set_of(|s| s.boxes.push(bx)));
        let line_h = 12.0 * 1.3;
        let block_h = line_h * 2.0;
        let block_top = 30.0 + 50.0 - 4.0 - block_h;
        assert_eq!(
            c.to_json(),
            json!([
                ["save"],
                ["font", "italic bold 12px monospace"],
                ["fillStyle", "#abc"],
                ["textAlign", "right"],
                ["textBaseline", "middle"],
                [
                    "fillText",
                    "a",
                    20.0 + 100.0 - 4.0,
                    block_top + 0.5 * line_h
                ],
                [
                    "fillText",
                    "b",
                    20.0 + 100.0 - 4.0,
                    block_top + 1.5 * line_h
                ],
                ["restore"],
            ])
        );
        let top_left = boxed(|b| {
            b.left = 20.0;
            b.right = 120.0;
            b.top = 30.0;
            b.bottom = 80.0;
            b.bg_color = None;
            b.border_color = None;
            b.text = Some("x".into());
            b.text_size = BoxTextSize::Tiny;
            b.h_align = BoxHAlign::Left;
            b.v_align = BoxVAlign::Top;
        });
        let (c, _) = paint(set_of(|s| s.boxes.push(top_left)));
        assert_eq!(
            c.calls("fillText"),
            vec![json!([
                "fillText",
                "x",
                24.0,
                (30.0 + 4.0) + 0.5 * (10.0 * 1.3)
            ])]
        );
    }

    #[test]
    fn an_auto_sized_wrapped_box_measures_at_14px_then_fits_the_font_to_the_box() {
        let bx = boxed(|b| {
            b.left = 20.0;
            b.right = 120.0;
            b.top = 30.0;
            b.bottom = 80.0;
            b.bg_color = None;
            b.border_color = None;
            b.text = Some("ab cd".into());
            b.text_size = BoxTextSize::Auto;
            b.wrap = true;
        });
        let (c, _) = paint(set_of(|s| s.boxes.push(bx)));
        // The reference font is set once (the first measurement misses the cache; later
        // reads of the same font and text hit it). "ab cd" is 5 * 14 * 0.6 = 42 wide.
        let widest = 5.0 * 14.0 * 0.6;
        let size = f64::min(
            f64::min((100.0 - 8.0) * 14.0 / widest, 50.0 * 0.85 / (1.0 * 1.3)),
            48.0,
        );
        let line_h = size * 1.3;
        let block_top = 30.0 + (50.0 - line_h) / 2.0;
        assert_eq!(
            c.to_json(),
            json!([
                ["save"],
                ["font", "14px sans-serif"],
                [
                    "font",
                    format!("{}px sans-serif", crate::util::js_number(size))
                ],
                ["fillStyle", "#000000"],
                ["textAlign", "center"],
                ["textBaseline", "middle"],
                ["fillText", "ab cd", 70.0, block_top + 0.5 * line_h],
                ["restore"],
            ])
        );
    }

    #[test]
    fn a_wrapped_box_breaks_lines_at_spaces_to_the_box_width() {
        let bx = boxed(|b| {
            b.left = 0.0;
            b.right = 60.0;
            b.top = 0.0;
            b.bottom = 100.0;
            b.bg_color = None;
            b.border_color = None;
            b.text = Some("aaaa bbbb".into());
            b.text_size = BoxTextSize::Normal; // 8.4 px per char: 4 chars = 33.6, 9 chars = 75.6
            b.wrap = true;
        });
        let (c, _) = paint(set_of(|s| s.boxes.push(bx)));
        assert_eq!(c.texts(), vec!["aaaa", "bbbb"]);
    }

    #[test]
    fn an_auto_box_too_small_to_read_hides_its_text() {
        let bx = boxed(|b| {
            b.left = 0.0;
            b.right = 100.0;
            b.top = 0.0;
            b.bottom = 3.0; // 3 * 0.85 / 1.3 < 6
            b.bg_color = None;
            b.border_color = None;
            b.text = Some("hidden".into());
        });
        let (c, _) = paint(set_of(|s| s.boxes.push(bx)));
        assert!(c.texts().is_empty());
    }

    #[test]
    fn an_extended_box_spans_from_minus_two_to_the_pane_width_plus_two() {
        let bx = boxed(|b| {
            b.left = 20.0;
            b.right = 120.0;
            b.top = 30.0;
            b.bottom = 80.0;
            b.border_color = None;
            b.extend = DrawingExtend::Both;
        });
        let (c, _) = paint(set_of(|s| s.boxes.push(bx)));
        assert_eq!(
            c.calls("fillRect"),
            vec![json!(["fillRect", -2.0, 30.0, 804.0, 50.0])]
        );
    }

    #[test]
    fn boxes_outside_the_pane_are_skipped() {
        let (c, _) = paint(set_of(|s| {
            s.boxes.push(boxed(|b| {
                b.left = 900.0;
                b.right = 950.0;
                b.top = 30.0;
                b.bottom = 80.0;
            }));
            s.boxes.push(boxed(|b| {
                b.left = 10.0;
                b.right = 50.0;
                b.top = -80.0;
                b.bottom = -30.0;
            }));
        }));
        assert_eq!(c.names(), vec!["save", "restore"]);
    }

    // ── labels: bubbles (drawing-scene-labels.test.ts) ──
    //
    // At the defaults, "AB" at 14px measures 16.8 (the recorder's fixed rule): the body is
    // 28.8 wide and 25.5 high, and label_down at (100, 100) puts it at left 85.6, top 67.5.

    fn bubble() -> (f64, f64, f64, f64) {
        let w = 2.0 * 14.0 * 0.6 + 6.0 * 2.0;
        let h = 14.0 * 1.25 + 4.0 * 2.0;
        (100.0 - w / 2.0, 100.0 - h - 7.0, w, h)
    }

    fn texts_with_align(c: &RecordingCanvas) -> Vec<(String, f64, String)> {
        let mut align = String::from("start");
        let mut out = Vec::new();
        for op in c.ops() {
            match op[0].as_str().unwrap() {
                "textAlign" => align = op[1].as_str().unwrap().to_string(),
                "fillText" => out.push((
                    op[1].as_str().unwrap().to_string(),
                    op[2].as_f64().unwrap(),
                    align.clone(),
                )),
                _ => {}
            }
        }
        out
    }

    #[test]
    fn a_bubble_label_centers_its_text_by_default() {
        let (c, _) = paint(set_of(|s| s.labels.push(label(|_| {}))));
        assert_eq!(
            texts_with_align(&c),
            vec![("AB".to_string(), 100.0, "center".to_string())]
        );
    }

    #[test]
    fn bubble_text_left_and_right_align_against_the_padding() {
        let (bx, _, w, _) = bubble();
        let (c, _) = paint(set_of(|s| {
            s.labels.push(label(|l| l.text_align = BoxHAlign::Left))
        }));
        assert_eq!(
            texts_with_align(&c),
            vec![("AB".to_string(), bx + 6.0, "left".to_string())]
        );
        let (c, _) = paint(set_of(|s| {
            s.labels.push(label(|l| l.text_align = BoxHAlign::Right))
        }));
        assert_eq!(
            texts_with_align(&c),
            vec![("AB".to_string(), bx + w - 6.0, "right".to_string())]
        );
    }

    #[test]
    fn label_fonts_carry_bold_and_italic() {
        let font_of = |over: fn(&mut DrawingLabel)| -> String {
            let (c, _) = paint(set_of(|s| s.labels.push(label(over))));
            c.calls("font").last().unwrap()[1]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(font_of(|_| {}), "14px sans-serif");
        assert_eq!(
            font_of(|l| {
                l.bold = true;
                l.italic = true
            }),
            "italic bold 14px sans-serif"
        );
        assert_eq!(
            font_of(|l| {
                l.style = LabelStyle::None;
                l.bold = true
            }),
            "bold 14px sans-serif"
        );
    }

    #[test]
    fn a_bubble_labels_complete_call_stream() {
        let lb = label(|l| l.color = Some("#2962ff".into()));
        let (c, _) = paint(set_of(|s| s.labels.push(lb)));
        let (bx, by, w, h) = bubble();
        let rr = 4.0;
        assert_eq!(
            c.to_json(),
            json!([
                ["save"],
                // drawBubble sets the font, then the (cache-missing) measurement sets it again
                ["font", "14px sans-serif"],
                ["font", "14px sans-serif"],
                ["fillStyle", "#2962ff"],
                ["beginPath"],
                ["moveTo", bx + rr, by],
                ["arcTo", bx + w, by, bx + w, by + h, rr],
                ["arcTo", bx + w, by + h, bx, by + h, rr],
                ["arcTo", bx, by + h, bx, by, rr],
                ["arcTo", bx, by, bx + w, by, rr],
                ["closePath"],
                ["fill"],
                // pointer down
                ["beginPath"],
                ["moveTo", 100.0 - 7.0, by + h],
                ["lineTo", 100.0 + 7.0, by + h],
                ["lineTo", 100.0, 100.0],
                ["closePath"],
                ["fill"],
                // text on the bubble's contrast color (white on #2962ff), centered
                ["fillStyle", "#ffffff"],
                ["textBaseline", "middle"],
                ["textAlign", "center"],
                ["fillText", "AB", bx + w / 2.0, by + 4.0 + 17.5 / 2.0],
                ["restore"],
            ])
        );
    }

    #[test]
    fn the_pointer_and_body_follow_the_style() {
        let (_, _, w, h) = bubble();
        let cases: [(LabelStyle, f64, f64, &str); 8] = [
            (LabelStyle::LabelUp, 100.0 - w / 2.0, 100.0 + 7.0, "up"),
            (LabelStyle::LabelLeft, 100.0 + 7.0, 100.0 - h / 2.0, "left"),
            (
                LabelStyle::LabelRight,
                100.0 - w - 7.0,
                100.0 - h / 2.0,
                "right",
            ),
            (
                LabelStyle::LabelCenter,
                100.0 - w / 2.0,
                100.0 - h / 2.0,
                "none",
            ),
            (
                LabelStyle::LabelLowerLeft,
                100.0 + 7.0,
                100.0 - h - 7.0,
                "none",
            ),
            (
                LabelStyle::LabelLowerRight,
                100.0 - w - 7.0,
                100.0 - h - 7.0,
                "none",
            ),
            (LabelStyle::LabelUpperLeft, 100.0 + 7.0, 100.0 + 7.0, "none"),
            (
                LabelStyle::LabelUpperRight,
                100.0 - w - 7.0,
                100.0 + 7.0,
                "none",
            ),
        ];
        for (style, bx, by, pointer) in cases {
            let make = |l: &mut DrawingLabel| {
                l.style = style;
                l.color = Some("#fff".into());
                l.tooltip = Some("t".into());
            };
            let (c, r) = paint(set_of(|s| s.labels.push(label(make))));
            // body rect, via the tooltip region
            let reg = &r.label_tip_regions()[0];
            approx(reg.left, bx);
            approx(reg.top, by);
            approx(reg.right, bx + w);
            approx(reg.bottom, by + h);
            // none => body fill only; otherwise a second filled triangle toward the anchor
            let fills = c.calls("fill").len();
            assert_eq!(fills, if pointer == "none" { 1 } else { 2 }, "{style:?}");
            if pointer == "left" {
                assert!(c.ops().contains(&json!(["moveTo", bx, 100.0 - 7.0])));
            }
            if pointer == "right" {
                assert!(c.ops().contains(&json!(["moveTo", bx + w, 100.0 - 7.0])));
            }
            if pointer == "up" {
                assert!(c.ops().contains(&json!(["moveTo", 100.0 - 7.0, by])));
            }
        }
    }

    // ── labels: noFill (label-nofill.test.ts) ──

    fn nofill_label(style: LabelStyle, no_fill: bool) -> DrawingLabel {
        label(|l| {
            l.x = 5.0;
            l.y = 60.0;
            l.text = Some("T".into());
            l.style = style;
            l.no_fill = no_fill;
            l.color = if no_fill {
                None
            } else {
                Some("#2962ff".into())
            };
        })
    }

    /// `(x, y, fill() count, fillStyle calls)` of the single text a label paints.
    fn text_xy_and_fills(lb: DrawingLabel) -> (f64, f64, usize, Vec<serde_json::Value>) {
        let (c, _) = paint_with(set_of(|s| s.labels.push(lb)), 0.0, 200.0, 100.0, None);
        let t = c.calls("fillText");
        assert_eq!(t.len(), 1);
        (
            t[0][2].as_f64().unwrap(),
            t[0][3].as_f64().unwrap(),
            c.calls("fill").len(),
            c.calls("fillStyle"),
        )
    }

    #[test]
    fn a_nofill_label_up_keeps_the_text_where_the_filled_bubble_puts_it() {
        let (fx, fy, filled_fills, _) = text_xy_and_fills(nofill_label(LabelStyle::LabelUp, false));
        let (nx, ny, nofill_fills, styles) =
            text_xy_and_fills(nofill_label(LabelStyle::LabelUp, true));
        assert!(filled_fills > 0);
        assert_eq!(nofill_fills, 0);
        assert_eq!((nx, ny), (fx, fy));
        // label_up hangs below its anchor (60), not centered on it
        assert!(ny > 60.0);
        // no bubble: the text takes the theme color, not a bubble contrast
        assert_eq!(styles, vec![json!(["fillStyle", "#ddd"])]);
    }

    #[test]
    fn nofill_label_up_and_label_center_place_text_differently() {
        let (_, up, _, _) = text_xy_and_fills(nofill_label(LabelStyle::LabelUp, true));
        let (_, center, _, _) = text_xy_and_fills(nofill_label(LabelStyle::LabelCenter, true));
        assert_ne!(up, center);
        // label_center is symmetric around the anchor
        approx(center, 60.0);
    }

    // ── labels: point shapes ──

    #[test]
    fn a_circle_marker_streams_style_arc_fill_and_the_text_below_it() {
        let lb = label(|l| {
            l.style = LabelStyle::Circle;
            l.color = Some("#f0f".into());
            l.text = Some("A".into());
        });
        let (c, _) = paint(set_of(|s| s.labels.push(lb)));
        let r = f64::max(4.0, 14.0 * 0.6);
        assert_eq!(
            c.to_json(),
            json!([
                ["save"],
                ["fillStyle", "#f0f"],
                ["strokeStyle", "#f0f"],
                ["lineWidth", f64::max(1.0, r * 0.3)],
                ["beginPath"],
                [
                    "arc",
                    100.0,
                    100.0,
                    r,
                    0.0,
                    std::f64::consts::PI * 2.0,
                    false
                ],
                ["fill"],
                ["font", "14px sans-serif"],
                ["textAlign", "center"],
                ["textBaseline", "middle"],
                ["fillStyle", "#ddd"],
                ["fillText", "A", 100.0, 100.0 + 14.0],
                ["restore"],
            ])
        );
    }

    #[test]
    fn every_marker_shape_streams_its_own_path() {
        // The ops after save / fillStyle / strokeStyle / lineWidth, without the final restore.
        let shape = |style: LabelStyle| {
            let lb = label(|l| {
                l.style = style;
                l.color = Some("#123".into());
                l.text = None;
            });
            let (c, _) = paint(set_of(|s| s.labels.push(lb)));
            c.ops()[4..c.ops().len() - 1].to_vec()
        };
        let r = f64::max(4.0, 14.0 * 0.6);
        let (cx, cy) = (100.0, 100.0);
        assert_eq!(
            shape(LabelStyle::Square),
            vec![
                json!(["beginPath"]),
                json!(["fillRect", cx - r, cy - r, r * 2.0, r * 2.0])
            ]
        );
        assert_eq!(
            shape(LabelStyle::Diamond),
            vec![
                json!(["beginPath"]),
                json!(["moveTo", cx, cy - r]),
                json!(["lineTo", cx + r, cy]),
                json!(["lineTo", cx, cy + r]),
                json!(["lineTo", cx - r, cy]),
                json!(["closePath"]),
                json!(["fill"]),
            ]
        );
        for up in [LabelStyle::TriangleUp, LabelStyle::ArrowUp] {
            assert_eq!(
                shape(up),
                vec![
                    json!(["beginPath"]),
                    json!(["moveTo", cx, cy - r]),
                    json!(["lineTo", cx + r, cy + r]),
                    json!(["lineTo", cx - r, cy + r]),
                    json!(["closePath"]),
                    json!(["fill"]),
                ]
            );
        }
        for down in [LabelStyle::TriangleDown, LabelStyle::ArrowDown] {
            assert_eq!(
                shape(down),
                vec![
                    json!(["beginPath"]),
                    json!(["moveTo", cx, cy + r]),
                    json!(["lineTo", cx + r, cy - r]),
                    json!(["lineTo", cx - r, cy - r]),
                    json!(["closePath"]),
                    json!(["fill"]),
                ]
            );
        }
        assert_eq!(
            shape(LabelStyle::Flag),
            vec![
                json!(["beginPath"]),
                json!(["moveTo", cx - r * 0.7, cy - r]),
                json!(["lineTo", cx - r * 0.7, cy + r]),
                json!(["moveTo", cx - r * 0.7, cy - r]),
                json!(["lineTo", cx + r, cy - r * 0.4]),
                json!(["lineTo", cx - r * 0.7, cy + r * 0.2]),
                json!(["stroke"]),
                json!(["fill"]),
            ]
        );
        assert_eq!(
            shape(LabelStyle::Cross),
            vec![
                json!(["beginPath"]),
                json!(["moveTo", cx, cy - r]),
                json!(["lineTo", cx, cy + r]),
                json!(["moveTo", cx - r, cy]),
                json!(["lineTo", cx + r, cy]),
                json!(["stroke"]),
            ]
        );
        assert_eq!(
            shape(LabelStyle::XCross),
            vec![
                json!(["beginPath"]),
                json!(["moveTo", cx - r, cy - r]),
                json!(["lineTo", cx + r, cy + r]),
                json!(["moveTo", cx + r, cy - r]),
                json!(["lineTo", cx - r, cy + r]),
                json!(["stroke"]),
            ]
        );
    }

    #[test]
    fn a_nofill_marker_draws_no_shape_but_keeps_its_text() {
        let lb = label(|l| {
            l.style = LabelStyle::Circle;
            l.no_fill = true;
        });
        let (c, _) = paint(set_of(|s| s.labels.push(lb)));
        assert!(c.calls("arc").is_empty());
        assert_eq!(c.texts(), vec!["AB"]);
    }

    // ── labels: text only ──

    #[test]
    fn a_text_only_label_streams_font_align_color_and_one_line_per_row() {
        let lb = label(|l| {
            l.style = LabelStyle::None;
            l.text = Some("one\ntwo".into());
            l.text_color = Some("#c0ffee".into());
            l.text_align = BoxHAlign::Right;
        });
        let (c, _) = paint(set_of(|s| s.labels.push(lb)));
        // two lines of 17.5: the block is centered on y = 100
        let start_y = 100.0 - (17.5 * 1.0) / 2.0;
        assert_eq!(
            c.to_json(),
            json!([
                ["save"],
                ["font", "14px sans-serif"],
                ["textAlign", "right"],
                ["textBaseline", "middle"],
                ["fillStyle", "#c0ffee"],
                ["fillText", "one", 100.0, start_y],
                ["fillText", "two", 100.0, start_y + 17.5],
                ["restore"],
            ])
        );
    }

    #[test]
    fn a_text_outline_label_strokes_a_contrast_halo_under_the_fill() {
        let lb = label(|l| {
            l.style = LabelStyle::TextOutline;
            l.text_color = Some("#ffffff".into());
        });
        let (c, _) = paint(set_of(|s| s.labels.push(lb)));
        assert_eq!(
            c.to_json(),
            json!([
                ["save"],
                ["font", "14px sans-serif"],
                ["textAlign", "center"],
                ["textBaseline", "middle"],
                ["lineWidth", 3.0],
                ["strokeStyle", "#000000"],
                ["strokeText", "AB", 100.0, 100.0],
                ["fillStyle", "#ffffff"],
                ["fillText", "AB", 100.0, 100.0],
                ["restore"],
            ])
        );
    }

    #[test]
    fn a_text_only_label_without_text_draws_nothing() {
        let lb = label(|l| {
            l.style = LabelStyle::None;
            l.text = None;
            l.tooltip = Some("t".into());
        });
        let (c, r) = paint(set_of(|s| s.labels.push(lb)));
        assert_eq!(c.names(), vec!["save", "restore"]);
        assert!(r.label_tip_regions().is_empty());
    }

    // ── labels: placement ──

    #[test]
    fn yloc_top_and_bottom_pin_to_the_pane_edges_and_bars_offset_by_14() {
        let ys = |yloc: LabelYLoc, bar: Option<BarHighLow>| -> Option<f64> {
            let lb = label(|l| {
                l.style = LabelStyle::None;
                l.yloc = yloc;
            });
            let (c, _) = paint_with(set_of(|s| s.labels.push(lb)), 0.0, 800.0, 400.0, bar);
            c.calls("fillText")
                .first()
                .map(|op| op[3].as_f64().unwrap())
        };
        let bar = Some(BarHighLow {
            high: 200.0,
            low: 150.0,
        });
        assert_eq!(ys(LabelYLoc::Top, None), Some(14.0));
        assert_eq!(ys(LabelYLoc::Bottom, None), Some(400.0 - 14.0));
        assert_eq!(ys(LabelYLoc::AboveBar, bar), Some(200.0 - 14.0));
        assert_eq!(ys(LabelYLoc::BelowBar, bar), Some(150.0 + 14.0));
        // no bar under the label: nothing to anchor to
        assert_eq!(ys(LabelYLoc::AboveBar, None), None);
    }

    #[test]
    fn labels_far_outside_the_pane_are_skipped() {
        let (c, _) = paint(set_of(|s| {
            s.labels.push(label(|l| l.x = -51.0));
            s.labels.push(label(|l| l.x = 851.0));
        }));
        assert_eq!(c.names(), vec!["save", "restore"]);
        let (c, _) = paint(set_of(|s| s.labels.push(label(|l| l.x = -50.0))));
        assert!(c.names().len() > 2);
    }

    // ── labels: tooltip regions ──

    #[test]
    fn a_bubble_with_a_tooltip_exposes_its_body_rect() {
        let (_, r) = paint(set_of(|s| {
            s.labels.push(label(|l| l.tooltip = Some("hello".into())))
        }));
        let (bx, by, w, h) = bubble();
        let regions = r.label_tip_regions();
        assert_eq!(regions.len(), 1);
        approx(regions[0].left, bx);
        approx(regions[0].top, by);
        approx(regions[0].right, bx + w);
        approx(regions[0].bottom, by + h);
        assert_eq!(regions[0].text, "hello");
    }

    #[test]
    fn point_shapes_expose_a_square_around_the_marker() {
        let (_, r) = paint(set_of(|s| {
            s.labels.push(label(|l| {
                l.style = LabelStyle::Circle;
                l.tooltip = Some("dot".into());
            }))
        }));
        let reg = &r.label_tip_regions()[0];
        // r = max(4, 14 * 0.6) + 3 = 11.4 around (100, 100)
        approx(reg.left, 88.6);
        approx(reg.right, 111.4);
        approx(reg.top, 88.6);
        approx(reg.bottom, 111.4);
        assert_eq!(reg.text, "dot");
    }

    #[test]
    fn text_only_labels_expose_the_text_block_honoring_textalign() {
        let (_, r) = paint(set_of(|s| {
            s.labels.push(label(|l| {
                l.style = LabelStyle::None;
                l.text_align = BoxHAlign::Left;
                l.tooltip = Some("txt".into());
            }))
        }));
        let reg = &r.label_tip_regions()[0];
        assert_eq!(reg.left, 100.0); // left-aligned text starts at the anchor
        approx(reg.right, 100.0 + 16.8);
        assert_eq!(reg.text, "txt");
        // (100, 100) centered on the line block: 17.5 high
        approx(reg.top, 100.0 - 8.75);
        approx(reg.bottom, 100.0 + 8.75);
    }

    #[test]
    fn a_text_only_labels_complete_stream_with_its_tooltip_measurement() {
        let lb = label(|l| {
            l.style = LabelStyle::None;
            l.text_align = BoxHAlign::Left;
            l.tooltip = Some("txt".into());
        });
        let (c, _) = paint(set_of(|s| s.labels.push(lb)));
        assert_eq!(
            c.to_json(),
            json!([
                ["save"],
                ["font", "14px sans-serif"],
                ["textAlign", "left"],
                ["textBaseline", "middle"],
                ["fillStyle", "#ddd"],
                ["fillText", "AB", 100.0, 100.0],
                // the tooltip rect measures the text (a width-cache miss sets the font)
                ["font", "14px sans-serif"],
                ["restore"],
            ])
        );
    }

    #[test]
    fn labels_without_a_tooltip_contribute_nothing_and_regions_reset_per_render() {
        let theme = theme();
        let time = |ms: f64| ms;
        let bar_at = |_: f64| None;
        let deps = LayerDeps {
            time_to_logical: &time,
            bar_at: &bar_at,
            theme: &theme,
        };
        let mut r = DrawingSceneRenderer::new();
        let canvas = RecordingCanvas::new();
        let render = |r: &mut DrawingSceneRenderer| {
            r.render(&deps, &canvas, 800.0, 400.0, &|l| l, &|p| Some(p))
        };
        r.set_set(set_of(|s| s.labels.push(label(|_| {}))), 0.0);
        render(&mut r);
        assert!(r.label_tip_regions().is_empty());
        r.set_set(
            set_of(|s| s.labels.push(label(|l| l.tooltip = Some("x".into())))),
            0.0,
        );
        render(&mut r);
        assert_eq!(r.label_tip_regions().len(), 1);
        r.set_set(set_of(|s| s.labels.push(label(|_| {}))), 0.0);
        render(&mut r);
        assert!(r.label_tip_regions().is_empty());
        // an empty set also clears them
        r.set_set(
            set_of(|s| s.labels.push(label(|l| l.tooltip = Some("x".into())))),
            0.0,
        );
        render(&mut r);
        r.set_set(DrawingSet::default(), 0.0);
        render(&mut r);
        assert!(r.label_tip_regions().is_empty());
    }

    // ── layering and caching ──

    #[test]
    fn the_layers_paint_linefills_boxes_polylines_lines_labels_in_that_order() {
        let set = set_of(|s| {
            s.labels.push(label(|l| {
                l.style = LabelStyle::None;
                l.x = 20.0;
            }));
            s.lines.push(line(|l| {
                l.x1 = 10.0;
                l.x2 = 50.0;
            }));
            s.polylines
                .push(polyline(&[(10.0, 10.0), (50.0, 40.0)], |_| {}));
            s.boxes.push(boxed(|b| {
                b.left = 10.0;
                b.right = 50.0;
                b.top = 30.0;
                b.bottom = 80.0;
                b.border_color = None;
            }));
            s.linefills.push(DrawingLinefill {
                id: "lf".into(),
                pane_id: "price".into(),
                line1: line(|l| {
                    l.x1 = 10.0;
                    l.x2 = 50.0;
                }),
                line2: line(|l| {
                    l.x1 = 10.0;
                    l.x2 = 50.0;
                    l.y1 = 9.0;
                }),
                color: Some("#00f".into()),
                overlay: false,
            });
        });
        let (c, _) = paint(set);
        let marks: Vec<String> = c
            .names()
            .into_iter()
            .filter(|n| matches!(n.as_str(), "fill" | "fillRect" | "stroke" | "fillText"))
            .collect();
        assert_eq!(
            marks,
            vec!["fill", "fillRect", "stroke", "stroke", "fillText"]
        );
    }

    #[test]
    fn text_widths_are_cached_across_frames() {
        let theme = theme();
        let time = |ms: f64| ms;
        let bar_at = |_: f64| None;
        let deps = LayerDeps {
            time_to_logical: &time,
            bar_at: &bar_at,
            theme: &theme,
        };
        let mut r = DrawingSceneRenderer::new();
        r.set_set(set_of(|s| s.labels.push(label(|_| {}))), 0.0);
        let first = RecordingCanvas::new();
        r.render(&deps, &first, 800.0, 400.0, &|l| l, &|p| Some(p));
        let second = RecordingCanvas::new();
        r.render(&deps, &second, 800.0, 400.0, &|l| l, &|p| Some(p));
        // the first frame measured (a second font assignment); the second reuses the width
        assert_eq!(first.calls("font").len(), 2);
        assert_eq!(second.calls("font").len(), 1);
    }

    #[test]
    fn a_missing_theme_font_falls_back_to_sans_serif_and_a_monospace_box_ignores_it() {
        assert_eq!(theme_font_family(&json!({})), "sans-serif");
        assert_eq!(
            theme_font_family(&json!({ "fontFamily": "" })),
            "sans-serif"
        );
        assert_eq!(
            theme_font_family(&json!({ "fontFamily": "Inter" })),
            "Inter"
        );
        assert_eq!(
            drawing_font(
                &json!({ "fontFamily": "Inter" }),
                BoxFontFamily::Monospace,
                false,
                false,
                12.5
            ),
            "12.5px monospace"
        );
    }
}
