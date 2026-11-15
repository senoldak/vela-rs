//! The Canvas 2D geometry backend (L0): the primary native backend and the WebGL2
//! backend's permanent fallback. It renders immediate-mode from the scene each frame,
//! culled to the visible bar range: bgcolor, fills (flat, conditional, gradient),
//! candles, line/area/step/histogram/columns/circles/cross with per-point and per-bar
//! color, hline. Per-pane price scales are computed by the renderer BEFORE this runs (it
//! reads `pane.scale`). Pine drawings, axes, the current-price line and the crosshair are
//! NOT its concern: they live on the chrome and cursor layers above, and the grid and
//! session highlights on the backdrop canvas below.
//!
//! The call stream (same canvas calls, same order, same arguments) is that of the
//! original; redundant calls (a style set to the value it already has) are kept on
//! purpose, a later check compares the streams call by call.
//!
//! Left for the browser crate: the original's `mount(canvas)` reads
//! `canvas.getContext('2d')` and the backing-store size (`canvas.width`, `canvas.height`,
//! in device pixels). Here [`Canvas2dBackend::mount`] takes the context as a shared
//! [`Canvas2d`] and the backing-store size as values; the shell keeps the size current
//! with [`Canvas2dBackend::set_backing_size`] whenever it resizes the canvas (it owns the
//! device-pixel-ratio sizing of the element).
//!
//! Index bookkeeping: bar indices are `f64` (integral values), as the original's are,
//! so a model offset can be negative and a viewport can start before the first bar; a
//! slot outside an array reads as absent, like JavaScript's `undefined`.
//!
//! Deviation: a NaN visible range (a corrupt viewport) paints the cleared frame and
//! nothing else; the original would run its loops with NaN bounds, which also draw
//! nothing, but would still stroke the price lines.
#![allow(clippy::too_many_arguments)]
// A NaN operand must fail these checks, as the original's `!(a > b)` does.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

use std::collections::HashMap;
use std::rc::Rc;

use serde_json::Value;

use crate::chart_types::BasePainting;
use crate::indicators::classics::math::{js_max, js_min, js_round};
use crate::model::{
    Background, CandleBarColor, CandleKind, CandleStyle, Fill, IndicatorModel, LineLikeKind,
    LineLikeStyle, LineStyle, Ohlcv, PaneKind, PriceLine, SeriesData, SeriesPoint, SeriesSpec,
    SeriesSurface,
};
use crate::ports::chart_renderer::VelaTheme;
use crate::render::canvas::{Canvas2d, LineCap, LineJoin, LinearGradient};
use crate::render::core::chart_config::{
    BASELINE_BOTTOM_LINE, BASELINE_FILL_ALPHA, BASELINE_FILL_ALPHA_FAR, BASELINE_TOP_LINE,
    COLUMNS_FILL_ALPHA, effective_candle_paint,
};
use crate::render::core::color::with_alpha;
use crate::render::core::plot_direction::begin_plot_frame;
use crate::render::core::{CoordinateSystem, PaneBounds, PaneNode, PriceScale, SceneGraph};

use super::candle_lod::{
    CandleTier, aggregate_candle_columns, candle_geometry, candle_tier, snap_y, wick_width,
};
use super::close_styles::{
    ClosePoint, close_runs, column_geometry, column_is_up, line_marker_radius,
    line_markers_visible, snap_stroke, step_vertices,
};
use super::render_backend::{BackendKind, RenderBackend};

/// A theme color by key; a missing or non-string entry reads as the empty string, which a
/// canvas ignores as a style.
pub fn theme_str<'a>(theme: &'a Value, key: &str) -> &'a str {
    theme.get(key).and_then(Value::as_str).unwrap_or("")
}

/// `setDash`: the dash pattern of a line style.
pub fn set_line_dash(ctx: &dyn Canvas2d, style: LineStyle) {
    match style {
        LineStyle::Dashed => ctx.set_line_dash(&[6.0, 4.0]),
        LineStyle::Dotted => ctx.set_line_dash(&[2.0, 3.0]),
        LineStyle::Solid => ctx.set_line_dash(&[]),
    }
}

/// `array[index]` with a JavaScript number index: `None` for a hole, a negative or
/// fractional index, or one past the end.
fn at<T>(items: &[T], index: f64) -> Option<&T> {
    if index >= 0.0 && index.fract() == 0.0 && index < items.len() as f64 {
        items.get(index as usize)
    } else {
        None
    }
}

/// The scale and pixel extent a series paints against (the original's `effPane`: the pane
/// with only its scale swapped for a merged indicator's own).
#[derive(Clone, Copy)]
struct Plane {
    scale: PriceScale,
    bounds: PaneBounds,
}

impl Plane {
    fn of(pane: &PaneNode) -> Self {
        Self {
            scale: pane.scale,
            bounds: pane.bounds,
        }
    }
}

fn y_of(coords: &CoordinateSystem, plane: &Plane, price: f64) -> f64 {
    coords.price_to_y(price, &plane.scale, &plane.bounds)
}

/// The canvas a layer paints on: its 2d context and its backing-store size in device
/// pixels (`canvas.width`, `canvas.height`), which the original reads off the element.
/// Shared by every painter that owns a canvas (backdrop, chrome, cursor, volume, VPVR).
#[derive(Clone)]
pub struct PlotSurface {
    pub ctx: Rc<dyn Canvas2d>,
    pub width: f64,
    pub height: f64,
}

impl PlotSurface {
    pub fn new(ctx: Rc<dyn Canvas2d>, backing_width: f64, backing_height: f64) -> Self {
        Self {
            ctx,
            width: backing_width,
            height: backing_height,
        }
    }
}

pub struct Canvas2dBackend {
    model_alpha: f64,
    candle_body_alpha: f64,
    candle_structure_alpha: f64,
    candle_body_scale: f64,
    surface: Option<PlotSurface>,
}

impl Default for Canvas2dBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Canvas2dBackend {
    pub fn new() -> Self {
        Self {
            model_alpha: 1.0,
            candle_body_alpha: 1.0,
            candle_structure_alpha: 1.0,
            candle_body_scale: 1.0,
            surface: None,
        }
    }

    /// Bind the backend to its data canvas: the 2d context and the backing-store size in
    /// device pixels.
    pub fn mount(&mut self, ctx: Rc<dyn Canvas2d>, backing_width: f64, backing_height: f64) {
        self.surface = Some(PlotSurface::new(ctx, backing_width, backing_height));
    }

    /// The canvas was resized: its new backing-store size in device pixels.
    pub fn set_backing_size(&mut self, backing_width: f64, backing_height: f64) {
        if let Some(surface) = &mut self.surface {
            surface.width = backing_width;
            surface.height = backing_height;
        }
    }

    // ── base price series (chart type) ──

    /// Draw the base price series in the configured style: candlesticks, OHLC bars, line
    /// (plain, with markers, or stepped), area, baseline, or columns. All stay
    /// time-indexed.
    fn draw_price_series(
        &self,
        ctx: &dyn Canvas2d,
        scene: &SceneGraph,
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        pane: &PaneNode,
        theme: &VelaTheme,
        bar_colors: &HashMap<i64, String>,
        data_w: f64,
    ) {
        if scene.base_painting == BasePainting::None {
            return; // a plugin style fully replaces the price series
        }
        let bars = &scene.bars;
        let st = &scene.style;
        let plane = Plane::of(pane);
        let up_color = theme_str(theme, "upColor");
        let down_color = theme_str(theme, "downColor");
        match scene.price_style.as_str() {
            "bars" => {
                let up = st.bars.up_color.as_deref().unwrap_or(up_color);
                let down = st.bars.down_color.as_deref().unwrap_or(down_color);
                self.draw_bars(ctx, bars, i0, i1, coords, &plane, up, down, bar_colors);
            }
            "line" => {
                let color = st.line.color.as_deref().unwrap_or(up_color);
                self.draw_price_line(ctx, bars, i0, i1, coords, &plane, color, st.line.width);
            }
            "linemarkers" => {
                let color = st.line.color.as_deref().unwrap_or(up_color);
                self.draw_price_line(ctx, bars, i0, i1, coords, &plane, color, st.line.width);
                self.draw_line_markers(ctx, bars, i0, i1, coords, &plane, color, st.line.width);
            }
            "stepline" => {
                let color = st.line.color.as_deref().unwrap_or(up_color);
                self.draw_step_line(ctx, bars, i0, i1, coords, &plane, color, st.line.width);
            }
            "area" => {
                let line_color = st.area.line_color.as_deref().unwrap_or(up_color);
                self.draw_price_area(
                    ctx,
                    bars,
                    i0,
                    i1,
                    coords,
                    &plane,
                    st.area.top_color.as_deref().unwrap_or(line_color),
                    st.area.bottom_color.as_deref().unwrap_or("rgba(0,0,0,0)"),
                );
                self.draw_price_line(ctx, bars, i0, i1, coords, &plane, line_color, st.area.width);
            }
            "baseline" => {
                self.draw_baseline(ctx, bars, i0, i1, coords, &plane, scene, theme, data_w);
            }
            "columns" => {
                let up = st
                    .columns
                    .up_color
                    .clone()
                    .unwrap_or_else(|| with_alpha(up_color, COLUMNS_FILL_ALPHA));
                let down = st
                    .columns
                    .down_color
                    .clone()
                    .unwrap_or_else(|| with_alpha(down_color, COLUMNS_FILL_ALPHA));
                self.draw_columns(ctx, bars, i0, i1, coords, &plane, &up, &down, bar_colors);
            }
            // 'candles' + candle-drawn styles (a plugin style may fade candles to reveal
            // its order-flow layer) and 'heikinashi' (the bars themselves arrive already
            // transformed)
            _ => self.draw_candles(ctx, scene, bars, i0, i1, coords, &plane, theme, bar_colors),
        }
    }

    /// OHLC bars: high-low stick + left open tick + right close tick (no body).
    fn draw_bars(
        &self,
        ctx: &dyn Canvas2d,
        bars: &[Ohlcv],
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        up: &str,
        down: &str,
        bar_colors: &HashMap<i64, String>,
    ) {
        let spacing = coords.body_spacing();
        let tier = candle_tier(spacing);
        let color_of = |b: &Ohlcv| -> String {
            bar_colors
                .get(&b.time)
                .cloned()
                .unwrap_or_else(|| if b.close >= b.open { up } else { down }.to_string())
        };
        if tier == CandleTier::Aggregate {
            self.draw_candles_aggregated(ctx, bars, i0, i1, coords, plane, &color_of);
            return;
        }
        let tick_w = js_max(1.0, js_round(spacing * 0.35));
        let draw_ticks = tier == CandleTier::Full;
        ctx.set_line_width(1.0);
        for i in i0..=i1 {
            let Some(b) = bars.get(i) else { continue };
            if b.high <= b.low {
                continue;
            }
            let x = js_round(coords.logical_to_x(i as f64)) + 0.5;
            let color = color_of(b);
            let h_y = y_of(coords, plane, b.high);
            let l_y = y_of(coords, plane, b.low);
            ctx.set_stroke_style(&color.into());
            ctx.begin_path();
            ctx.move_to(x, h_y);
            ctx.line_to(x, l_y);
            if draw_ticks {
                let o_y = y_of(coords, plane, b.open);
                let c_y = y_of(coords, plane, b.close);
                ctx.move_to(x - tick_w, o_y);
                ctx.line_to(x, o_y);
                ctx.move_to(x, c_y);
                ctx.line_to(x + tick_w, c_y);
            }
            ctx.stroke();
        }
    }

    /// Close-price line (also the top stroke of the area style).
    fn draw_price_line(
        &self,
        ctx: &dyn Canvas2d,
        bars: &[Ohlcv],
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        color: &str,
        width: f64,
    ) {
        ctx.set_line_width(width);
        ctx.set_line_join(LineJoin::Round);
        ctx.set_line_cap(LineCap::Round);
        ctx.set_stroke_style(&color.into());
        ctx.begin_path();
        let mut started = false;
        for i in i0.saturating_sub(1)..=i1 {
            let Some(b) = bars.get(i) else {
                started = false;
                continue;
            };
            let x = coords.logical_to_x(i as f64);
            let y = y_of(coords, plane, b.close);
            if !started {
                ctx.move_to(x, y);
                started = true;
            } else {
                ctx.line_to(x, y);
            }
        }
        ctx.stroke();
        ctx.set_line_cap(LineCap::Butt);
    }

    /// Line with markers: a filled dot on each bar's close, once bars are spaced enough
    /// to tell the dots apart (see `line_markers_visible`). One path, so dots that touch
    /// never darken a translucent color where they overlap.
    fn draw_line_markers(
        &self,
        ctx: &dyn Canvas2d,
        bars: &[Ohlcv],
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        color: &str,
        width: f64,
    ) {
        let r = line_marker_radius(width);
        if !line_markers_visible(coords.px_per_bar(), r) {
            return;
        }
        ctx.set_fill_style(&color.into());
        ctx.begin_path();
        for i in i0.saturating_sub(1)..=i1 {
            let Some(b) = bars.get(i) else { continue };
            let x = coords.logical_to_x(i as f64);
            let y = y_of(coords, plane, b.close);
            ctx.move_to(x + r, y);
            ctx.arc(x, y, r, 0.0, std::f64::consts::PI * 2.0, false);
        }
        ctx.fill();
    }

    /// Step line: each close held flat to the next bar, then a vertical step to its close.
    /// Vertices snap so the runs cover whole device pixels; square caps and miter joins
    /// give the corners and ends the same square shape the WebGL2 backend builds.
    fn draw_step_line(
        &self,
        ctx: &dyn Canvas2d,
        bars: &[Ohlcv],
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        color: &str,
        width: f64,
    ) {
        let dpr = coords.dpr();
        ctx.set_line_width(width);
        ctx.set_line_join(LineJoin::Miter);
        ctx.set_line_cap(LineCap::Square);
        ctx.set_stroke_style(&color.into());
        ctx.begin_path();
        let runs = close_runs(i0.saturating_sub(1), i1, &|i| {
            bars.get(i).map(|b| ClosePoint {
                x: snap_stroke(coords.logical_to_x(i as f64), width, dpr),
                y: snap_stroke(y_of(coords, plane, b.close), width, dpr),
            })
        });
        for run in &runs {
            let vs = step_vertices(run);
            let Some(first) = vs.first() else { continue };
            ctx.move_to(first.x, first.y);
            for v in &vs[1..] {
                ctx.line_to(v.x, v.y);
            }
        }
        ctx.stroke();
        ctx.set_line_cap(LineCap::Butt);
    }

    /// Columns: one per bar from the pane's bottom edge up to its close, colored by the
    /// close against the previous close (`column_is_up`); barcolor() tints a column as it
    /// tints a candle body.
    fn draw_columns(
        &self,
        ctx: &dyn Canvas2d,
        bars: &[Ohlcv],
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        up: &str,
        down: &str,
        bar_colors: &HashMap<i64, String>,
    ) {
        let spacing = coords.body_spacing();
        let dpr = coords.dpr();
        let bottom = snap_y(plane.bounds.top + plane.bounds.height, dpr);
        for i in i0..=i1 {
            let Some(b) = bars.get(i) else { continue };
            let top = snap_y(y_of(coords, plane, b.close), dpr);
            if !(top < bottom) {
                continue;
            }
            let g = column_geometry(coords.logical_to_x(i as f64), spacing, dpr);
            let color = bar_colors
                .get(&b.time)
                .map(String::as_str)
                .unwrap_or(if column_is_up(bars, i) { up } else { down });
            ctx.set_fill_style(&color.into());
            ctx.fill_rect(g.left, top, g.width, bottom - top);
        }
    }

    /// Filled area under the close line: a vertical gradient from `top_color` (at the
    /// line) to `bottom_color` (at the pane floor). Defaults fade the line color out.
    fn draw_price_area(
        &self,
        ctx: &dyn Canvas2d,
        bars: &[Ohlcv],
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        top_color: &str,
        bottom_color: &str,
    ) {
        let base_y = plane.bounds.top + plane.bounds.height;
        let mut pts: Vec<(f64, f64)> = Vec::new();
        let mut min_y = f64::INFINITY;
        for i in i0.saturating_sub(1)..=i1 {
            let Some(b) = bars.get(i) else { continue };
            let x = coords.logical_to_x(i as f64);
            let y = y_of(coords, plane, b.close);
            pts.push((x, y));
            if y < min_y {
                min_y = y;
            }
        }
        if pts.len() < 2 {
            return;
        }
        let g = LinearGradient::new(0.0, min_y, 0.0, base_y)
            .stop(0.0, top_color)
            .stop(1.0, bottom_color);
        ctx.set_fill_style(&g.into());
        ctx.begin_path();
        ctx.move_to(pts[0].0, base_y);
        for p in &pts {
            ctx.line_to(p.0, p.1);
        }
        ctx.line_to(pts[pts.len() - 1].0, base_y);
        ctx.close_path();
        ctx.fill();
    }

    /// Baseline style: the close line split at a baseline price, filled and stroked with
    /// the up color above the baseline and the down color below it, each fading to
    /// transparent at the baseline. Clipping (not geometry-splitting) does the split.
    fn draw_baseline(
        &self,
        ctx: &dyn Canvas2d,
        bars: &[Ohlcv],
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        scene: &SceneGraph,
        theme: &VelaTheme,
        data_w: f64,
    ) {
        let baseline = scene.baseline_price_for(&plane.scale);
        let base_y = y_of(coords, plane, baseline);
        let top = plane.bounds.top;
        let bottom = plane.bounds.top + plane.bounds.height;
        let bs = &scene.style.baseline;
        let top_line = bs.top_line_color.as_deref().unwrap_or(BASELINE_TOP_LINE);
        let bottom_line = bs
            .bottom_line_color
            .as_deref()
            .unwrap_or(BASELINE_BOTTOM_LINE);
        let top_fill = bs
            .top_fill_color
            .clone()
            .unwrap_or_else(|| with_alpha(top_line, BASELINE_FILL_ALPHA));
        let top_fill2 = bs
            .top_fill_color2
            .clone()
            .unwrap_or_else(|| with_alpha(top_line, BASELINE_FILL_ALPHA_FAR));
        let bottom_fill = bs
            .bottom_fill_color
            .clone()
            .unwrap_or_else(|| with_alpha(bottom_line, BASELINE_FILL_ALPHA));
        let bottom_fill2 = bs
            .bottom_fill_color2
            .clone()
            .unwrap_or_else(|| with_alpha(bottom_line, BASELINE_FILL_ALPHA_FAR));
        let mut pts: Vec<(f64, f64)> = Vec::new();
        for i in i0.saturating_sub(1)..=i1 {
            let Some(b) = bars.get(i) else { continue };
            pts.push((coords.logical_to_x(i as f64), y_of(coords, plane, b.close)));
        }
        if pts.len() < 2 {
            return;
        }
        let poly = || {
            ctx.begin_path();
            ctx.move_to(pts[0].0, base_y);
            for p in &pts {
                ctx.line_to(p.0, p.1);
            }
            ctx.line_to(pts[pts.len() - 1].0, base_y);
            ctx.close_path();
        };
        // Above the baseline: top fill near the line, then the fainter top fill at the
        // baseline.
        if base_y > top {
            ctx.save();
            ctx.begin_path();
            ctx.rect(0.0, top, data_w, base_y - top);
            ctx.clip();
            let g_up = LinearGradient::new(0.0, top, 0.0, base_y)
                .stop(0.0, top_fill)
                .stop(1.0, top_fill2);
            ctx.set_fill_style(&g_up.into());
            poly();
            ctx.fill();
            ctx.restore();
        }
        // Below the baseline: fainter bottom fill at the baseline, then the bottom fill
        // near the line.
        if base_y < bottom {
            ctx.save();
            ctx.begin_path();
            ctx.rect(0.0, base_y, data_w, bottom - base_y);
            ctx.clip();
            let g_dn = LinearGradient::new(0.0, base_y, 0.0, bottom)
                .stop(0.0, bottom_fill2)
                .stop(1.0, bottom_fill);
            ctx.set_fill_style(&g_dn.into());
            poly();
            ctx.fill();
            ctx.restore();
        }
        // The baseline marker, then the close line colored per segment by its side.
        let border = scene
            .style
            .border_color
            .as_deref()
            .unwrap_or_else(|| theme_str(theme, "borderColor"));
        ctx.set_stroke_style(&border.into());
        ctx.set_line_width(1.0);
        set_line_dash(ctx, LineStyle::Dashed);
        ctx.begin_path();
        ctx.move_to(0.0, js_round(base_y) + 0.5);
        ctx.line_to(data_w, js_round(base_y) + 0.5);
        ctx.stroke();
        set_line_dash(ctx, LineStyle::Solid);
        ctx.set_line_width(bs.width);
        ctx.set_line_join(LineJoin::Round);
        ctx.set_line_cap(LineCap::Round);
        let seg = |x0: f64, y0: f64, x1: f64, y1: f64, color: &str| {
            ctx.set_stroke_style(&color.into());
            ctx.begin_path();
            ctx.move_to(x0, y0);
            ctx.line_to(x1, y1);
            ctx.stroke();
        };
        for k in 1..pts.len() {
            let a = pts[k - 1];
            let c = pts[k];
            let a_up = a.1 <= base_y;
            let c_up = c.1 <= base_y;
            // Split the segment AT the baseline so each side is colored correctly: a
            // segment must never be the up color below the baseline (or vice versa).
            if a_up != c_up && c.1 != a.1 {
                let t = (base_y - a.1) / (c.1 - a.1);
                let x_cross = a.0 + (c.0 - a.0) * t;
                seg(
                    a.0,
                    a.1,
                    x_cross,
                    base_y,
                    if a_up { top_line } else { bottom_line },
                );
                seg(
                    x_cross,
                    base_y,
                    c.0,
                    c.1,
                    if c_up { top_line } else { bottom_line },
                );
            } else {
                seg(
                    a.0,
                    a.1,
                    c.0,
                    c.1,
                    if a_up { top_line } else { bottom_line },
                );
            }
        }
        ctx.set_line_cap(LineCap::Butt);
    }

    // ── data ──

    fn draw_candles(
        &self,
        ctx: &dyn Canvas2d,
        scene: &SceneGraph,
        bars: &[Ohlcv],
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        theme: &VelaTheme,
        bar_colors: &HashMap<i64, String>,
    ) {
        let spacing = coords.body_spacing();
        let tier = candle_tier(spacing);
        // A candle-based plugin style paints with its OWN cosmetics (unset keys inherit
        // the shared candles block); built-ins pass through untouched.
        let paint = effective_candle_paint(
            &scene.style.candle,
            scene.candle_override.as_ref(),
            theme_str(theme, "upColor"),
            theme_str(theme, "downColor"),
        );
        let cs = &paint.candle;
        if tier == CandleTier::Aggregate {
            // The stick IS the candle, so a bar's paint resolves as in the stick-only
            // tier below: the wick color setting wins, then barcolor(), then the
            // direction color.
            self.draw_candles_aggregated(ctx, bars, i0, i1, coords, plane, &|b: &Ohlcv| {
                let up = b.close >= b.open;
                let wick = if up {
                    &cs.wick_up_color
                } else {
                    &cs.wick_down_color
                };
                wick.clone()
                    .or_else(|| bar_colors.get(&b.time).cloned())
                    .unwrap_or_else(|| if up { &paint.up } else { &paint.down }.clone())
            });
            return;
        }
        let draw_body = tier == CandleTier::Full;
        // When a fading style drops the body below the structure, draw a body outline
        // even if no border is configured, so the candle keeps a visible (hollow)
        // skeleton.
        let fading = self.candle_structure_alpha > self.candle_body_alpha + 0.001;
        let dpr = coords.dpr();
        for i in i0..=i1 {
            let Some(b) = bars.get(i) else { continue };
            if b.high <= b.low {
                continue; // skip zero-range candles (e.g. the intro reveal's un-started state)
            }
            // Wick + body snapped to the device grid as one unit, so the wick is always
            // dead-center in the body and the gap between candles stays uniform.
            let g = candle_geometry(
                coords.logical_to_x(i as f64),
                spacing,
                dpr,
                self.candle_body_scale,
            );
            let x = g.center;
            let up = b.close >= b.open;
            let dir = if up { &paint.up } else { &paint.down };
            let color = bar_colors.get(&b.time).unwrap_or(dir);
            // Body geometry up front so the wick can be clipped to it when the body is
            // hollow.
            let mut top = 0.0;
            let mut body_h = 0.0;
            if draw_body {
                let o_y = y_of(coords, plane, b.open);
                let c_y = y_of(coords, plane, b.close);
                // Both edges snapped to the device grid so the horizontal edges rasterize
                // crisp (no blended rim); a doji keeps a visible 1-device-px body.
                top = snap_y(js_min(o_y, c_y), dpr);
                body_h = js_max(1.0 / dpr, snap_y(js_max(o_y, c_y), dpr) - top);
            }
            if cs.wick_visible {
                // barcolor() recolors only the BODY (TV semantics): the wick keeps the
                // direction color while a body is drawn. At stick-only zoom the stick IS
                // the candle, so it keeps the barcolor tint.
                let wick_setting = if up {
                    &cs.wick_up_color
                } else {
                    &cs.wick_down_color
                };
                let wick = wick_setting
                    .as_ref()
                    .unwrap_or(if draw_body { dir } else { color });
                let h_y = snap_y(y_of(coords, plane, b.high), dpr);
                let l_y = snap_y(y_of(coords, plane, b.low), dpr);
                ctx.set_global_alpha(self.candle_structure_alpha);
                ctx.set_stroke_style(&wick.into());
                ctx.set_line_width(g.wick_w);
                ctx.begin_path();
                if draw_body {
                    // Draw the wick only outside the body, so it never shows through it,
                    // including hollow bodies and semi-transparent fills.
                    ctx.move_to(x, h_y);
                    ctx.line_to(x, top);
                    ctx.move_to(x, top + body_h);
                    ctx.line_to(x, l_y);
                } else {
                    ctx.move_to(x, h_y);
                    ctx.line_to(x, l_y);
                }
                ctx.stroke();
            }
            if draw_body {
                if cs.body_visible {
                    ctx.set_global_alpha(self.candle_body_alpha);
                    ctx.set_fill_style(&color.into());
                    ctx.fill_rect(g.body_x, top, g.body_w, body_h);
                }
                // The border strictly follows its visibility setting: barcolor() never
                // forces one. An unconfigured border color inherits the body color, so a
                // barcolored body gets a matching tinted border, not a direction-colored
                // outline.
                if cs.border_visible || (fading && cs.body_visible) {
                    ctx.set_global_alpha(self.candle_structure_alpha);
                    let border = if cs.border_visible {
                        let configured = if up {
                            &cs.border_up_color
                        } else {
                            &cs.border_down_color
                        };
                        configured.as_ref().unwrap_or(color)
                    } else {
                        color
                    };
                    ctx.set_stroke_style(&border.into());
                    ctx.set_line_width(1.0);
                    // Inset by half the stroke so the border lands on whole pixels
                    // (crisp) and stays inside the body's snapped footprint.
                    let bw = js_max(0.0, g.body_w - 1.0);
                    let bh = js_max(0.0, body_h - 1.0);
                    ctx.stroke_rect(g.body_x + 0.5, top + 0.5, bw, bh);
                }
            }
        }
    }

    /// Sub-pixel LOD: bars sharing a rounded pixel column collapse into high-low sticks
    /// (one per contiguous SAME-COLOR coverage run, see `aggregate_candle_columns`), so
    /// draw cost is bounded by screen width (not bar count) when zoomed far out, a price
    /// gap inside the column stays a void, and every bar's extent keeps its own color.
    /// `color_of` is the calling style's per-bar paint rule (candles and OHLC bars
    /// resolve theirs differently).
    fn draw_candles_aggregated(
        &self,
        ctx: &dyn Canvas2d,
        bars: &[Ohlcv],
        i0: usize,
        i1: usize,
        coords: &CoordinateSystem,
        plane: &Plane,
        color_of: &dyn Fn(&Ohlcv) -> String,
    ) {
        let price_y = |price: f64| y_of(coords, plane, price);
        ctx.set_line_width(1.0);
        for s in aggregate_candle_columns(
            bars,
            i0,
            i1,
            &|i| coords.logical_to_x(i as f64),
            &price_y,
            color_of,
        ) {
            let x = s.x + 0.5;
            ctx.set_stroke_style(&s.color.into());
            ctx.begin_path();
            ctx.move_to(x, price_y(s.hi));
            ctx.line_to(x, price_y(s.lo));
            ctx.stroke();
        }
    }

    fn draw_series(
        &self,
        ctx: &dyn Canvas2d,
        spec: &SeriesSpec,
        plane: &Plane,
        coords: &CoordinateSystem,
        i0: f64,
        i1: f64,
        theme: &VelaTheme,
        off: f64,
    ) {
        // Off-pane series (legend/data-window-only readouts, hidden fill anchors) are kept
        // in the model for fills and readouts but never painted here.
        if !spec.shown_on(SeriesSurface::Pane) {
            return;
        }
        match &spec.data {
            SeriesData::Candle {
                kind,
                bars,
                style,
                bar_colors,
            } => self.draw_plot_candles(
                ctx,
                *kind,
                bars,
                style.as_ref(),
                bar_colors.as_deref(),
                plane,
                coords,
                i0,
                i1,
                theme,
                off,
            ),
            SeriesData::Markers { .. } => {}
            SeriesData::LineLike {
                kind,
                points,
                style,
            } => match kind {
                LineLikeKind::Histogram | LineLikeKind::Columns => {
                    self.draw_histogram(ctx, *kind, points, style, plane, coords, i0, i1, off);
                }
                LineLikeKind::Area => {
                    self.draw_area(ctx, points, style, plane, coords, i0, i1, off);
                    self.draw_polyline(ctx, points, style, plane, coords, i0, i1, false, off);
                }
                LineLikeKind::Circles | LineLikeKind::Cross => {
                    self.draw_point_markers(ctx, *kind, points, style, plane, coords, i0, i1, off);
                }
                LineLikeKind::Step => {
                    self.draw_polyline(ctx, points, style, plane, coords, i0, i1, true, off);
                }
                LineLikeKind::Line => {
                    self.draw_polyline(ctx, points, style, plane, coords, i0, i1, false, off);
                }
            },
        }
    }

    fn draw_polyline(
        &self,
        ctx: &dyn Canvas2d,
        points: &[SeriesPoint],
        style: &LineLikeStyle,
        plane: &Plane,
        coords: &CoordinateSystem,
        i0: f64,
        i1: f64,
        step: bool,
        off: f64,
    ) {
        ctx.set_line_width(js_max(1.0, style.width));
        ctx.set_line_join(LineJoin::Round);
        set_line_dash(ctx, style.line_style);
        // Per-segment stroke so a varying per-point color renders natively (no series
        // split). The segment entering point i takes point i's color.
        let mut prev_x = 0.0;
        let mut prev_y = 0.0;
        let mut have = false;
        let mut i = js_max(1.0, i0 - 1.0);
        while i <= i1 {
            let p = at(points, i - off);
            let pv = at(points, i - 1.0 - off);
            let (Some(p), Some(pv)) = (p, pv) else {
                have = false;
                i += 1.0;
                continue;
            };
            let (Some(value), Some(prev_value)) = (p.value, pv.value) else {
                have = false;
                i += 1.0;
                continue;
            };
            let x = coords.logical_to_x(i);
            let y = y_of(coords, plane, value);
            if !have {
                prev_x = coords.logical_to_x(i - 1.0);
                prev_y = y_of(coords, plane, prev_value);
            }
            let color = p.color.as_deref().unwrap_or(&style.color);
            ctx.set_stroke_style(&color.into());
            ctx.begin_path();
            ctx.move_to(prev_x, prev_y);
            if step {
                ctx.line_to(x, prev_y);
                ctx.line_to(x, y);
            } else {
                ctx.line_to(x, y);
            }
            ctx.stroke();
            prev_x = x;
            prev_y = y;
            have = true;
            i += 1.0;
        }
        set_line_dash(ctx, LineStyle::Solid);
    }

    fn draw_area(
        &self,
        ctx: &dyn Canvas2d,
        points: &[SeriesPoint],
        style: &LineLikeStyle,
        plane: &Plane,
        coords: &CoordinateSystem,
        i0: f64,
        i1: f64,
        off: f64,
    ) {
        let base_y = plane.bounds.top + plane.bounds.height;
        // One filled polygon per contiguous non-null run (breaks on gaps, like the line
        // overlay + AreaSeries), with a top-color to transparent vertical fade.
        let mut pts: Vec<(f64, f64)> = Vec::new();
        let mut min_y = f64::INFINITY;
        let flush = |pts: &mut Vec<(f64, f64)>, min_y: &mut f64| {
            if pts.is_empty() {
                return;
            }
            let g = LinearGradient::new(0.0, *min_y, 0.0, base_y)
                .stop(0.0, style.color.clone())
                .stop(1.0, "rgba(0,0,0,0)");
            ctx.set_fill_style(&g.into());
            ctx.begin_path();
            ctx.move_to(pts[0].0, base_y);
            for p in pts.iter() {
                ctx.line_to(p.0, p.1);
            }
            ctx.line_to(pts[pts.len() - 1].0, base_y);
            ctx.close_path();
            ctx.fill();
            pts.clear();
            *min_y = f64::INFINITY;
        };
        let mut i = i0;
        while i <= i1 {
            let Some(value) = at(points, i - off).and_then(|p| p.value) else {
                flush(&mut pts, &mut min_y);
                i += 1.0;
                continue;
            };
            let x = coords.logical_to_x(i);
            let y = y_of(coords, plane, value);
            pts.push((x, y));
            if y < min_y {
                min_y = y;
            }
            i += 1.0;
        }
        flush(&mut pts, &mut min_y);
    }

    fn draw_histogram(
        &self,
        ctx: &dyn Canvas2d,
        kind: LineLikeKind,
        points: &[SeriesPoint],
        style: &LineLikeStyle,
        plane: &Plane,
        coords: &CoordinateSystem,
        i0: f64,
        i1: f64,
        off: f64,
    ) {
        let spacing = coords.body_spacing();
        let w = js_max(
            1.0,
            (if kind == LineLikeKind::Columns {
                0.7
            } else {
                0.8
            }) * spacing,
        );
        let half = w / 2.0;
        let base = style.base.unwrap_or(0.0);
        let base_y = y_of(coords, plane, base);
        let mut i = i0;
        while i <= i1 {
            let Some(p) = at(points, i - off) else {
                i += 1.0;
                continue;
            };
            let Some(value) = p.value else {
                i += 1.0;
                continue;
            };
            let x = coords.logical_to_x(i);
            let y = y_of(coords, plane, value);
            let color = p.color.as_deref().unwrap_or(&style.color);
            ctx.set_fill_style(&color.into());
            ctx.fill_rect(
                x - half,
                js_min(base_y, y),
                w,
                js_max(1.0, (y - base_y).abs()),
            );
            i += 1.0;
        }
    }

    fn draw_point_markers(
        &self,
        ctx: &dyn Canvas2d,
        kind: LineLikeKind,
        points: &[SeriesPoint],
        style: &LineLikeStyle,
        plane: &Plane,
        coords: &CoordinateSystem,
        i0: f64,
        i1: f64,
        off: f64,
    ) {
        let r = js_max(1.5, style.width);
        let mut i = i0;
        while i <= i1 {
            let Some(p) = at(points, i - off) else {
                i += 1.0;
                continue;
            };
            let Some(value) = p.value else {
                i += 1.0;
                continue;
            };
            let x = coords.logical_to_x(i);
            let y = y_of(coords, plane, value);
            let color = p.color.as_deref().unwrap_or(&style.color);
            ctx.set_stroke_style(&color.into());
            ctx.set_fill_style(&color.into());
            if kind == LineLikeKind::Cross {
                ctx.set_line_width(1.0);
                ctx.begin_path();
                ctx.move_to(x - r, y);
                ctx.line_to(x + r, y);
                ctx.move_to(x, y - r);
                ctx.line_to(x, y + r);
                ctx.stroke();
            } else {
                ctx.begin_path();
                ctx.arc(x, y, r, 0.0, std::f64::consts::PI * 2.0, false);
                ctx.fill();
            }
            i += 1.0;
        }
    }

    fn draw_plot_candles(
        &self,
        ctx: &dyn Canvas2d,
        kind: CandleKind,
        bars: &[Ohlcv],
        style: Option<&CandleStyle>,
        bar_colors: Option<&[Option<CandleBarColor>]>,
        plane: &Plane,
        coords: &CoordinateSystem,
        i0: f64,
        i1: f64,
        theme: &VelaTheme,
        off: f64,
    ) {
        let spacing = coords.body_spacing();
        let up = style
            .and_then(|s| s.up.as_deref())
            .unwrap_or_else(|| theme_str(theme, "upColor"));
        let down = style
            .and_then(|s| s.down.as_deref())
            .unwrap_or_else(|| theme_str(theme, "downColor"));
        let is_bar = kind == CandleKind::Bar;
        let half = js_max(0.5, f64::floor(spacing * 0.7) / 2.0);
        let tick_w = js_max(1.0, js_round(spacing * 0.35));
        let mut i = i0;
        while i <= i1 {
            let slot = i - off;
            i += 1.0;
            let Some(b) = at(bars, slot) else { continue };
            if !(b.open + b.high + b.low + b.close).is_finite() {
                continue;
            }
            let bc: Option<&CandleBarColor> = bar_colors
                .and_then(|colors| at(colors, slot))
                .and_then(Option::as_ref);
            let body = bc
                .and_then(|c| c.color.as_deref())
                .unwrap_or(if b.close >= b.open { up } else { down });
            let x = js_round(coords.logical_to_x(slot + off)) + 0.5;
            let h_y = y_of(coords, plane, b.high);
            let l_y = y_of(coords, plane, b.low);
            let o_y = y_of(coords, plane, b.open);
            let c_y = y_of(coords, plane, b.close);
            if is_bar {
                // OHLC bar: high-low stick + left open tick + right close tick.
                ctx.set_stroke_style(&body.into());
                ctx.set_line_width(1.0);
                ctx.begin_path();
                ctx.move_to(x, h_y);
                ctx.line_to(x, l_y);
                ctx.move_to(x - tick_w, o_y);
                ctx.line_to(x, o_y);
                ctx.move_to(x, c_y);
                ctx.line_to(x + tick_w, c_y);
                ctx.stroke();
                continue;
            }
            let wick = bc.and_then(|c| c.wick_color.as_deref()).unwrap_or(body);
            ctx.set_stroke_style(&wick.into());
            ctx.set_line_width(wick_width(spacing));
            ctx.begin_path();
            ctx.move_to(x, h_y);
            ctx.line_to(x, l_y);
            ctx.stroke();
            ctx.set_fill_style(&body.into());
            ctx.fill_rect(
                x - half,
                js_min(o_y, c_y),
                half * 2.0,
                js_max(1.0, (c_y - o_y).abs()),
            );
            if let Some(border) = bc
                .and_then(|c| c.border_color.as_deref())
                .filter(|c| !c.is_empty())
            {
                ctx.set_stroke_style(&border.into());
                ctx.stroke_rect(
                    x - half,
                    js_min(o_y, c_y),
                    half * 2.0,
                    js_max(1.0, (c_y - o_y).abs()),
                );
            }
        }
    }

    fn draw_fill(
        &self,
        ctx: &dyn Canvas2d,
        model: &IndicatorModel,
        fill: &Fill,
        plane: &Plane,
        coords: &CoordinateSystem,
        i0: f64,
        i1: f64,
        off: f64,
    ) {
        let (Some(from), Some(to)) = (
            find_points(model, &fill.from_series_id),
            find_points(model, &fill.to_series_id),
        ) else {
            return;
        };
        // Gradient pool: reuse one gradient across a run of bars with identical top/bottom
        // y + colors (a constant gradient makes 1 object for the whole fill, instead of
        // one per bar per frame).
        let mut g_top = f64::NAN;
        let mut g_bot = f64::NAN;
        let mut g_tc = String::new();
        let mut g_bc = String::new();
        let mut g_obj: Option<LinearGradient> = None;
        // Per-bar quads (flat / conditional / vertical gradient), culled to view.
        let mut i = js_max(1.0, i0);
        while i <= i1 {
            let cur = i;
            i += 1.0;
            let value_at = |points: &[SeriesPoint], idx: f64| at(points, idx).and_then(|p| p.value);
            let (Some(a0), Some(b0), Some(a1), Some(b1)) = (
                value_at(from, cur - 1.0 - off),
                value_at(to, cur - 1.0 - off),
                value_at(from, cur - off),
                value_at(to, cur - off),
            ) else {
                continue;
            };
            let x_prev = coords.logical_to_x(cur - 1.0);
            let x_cur = coords.logical_to_x(cur);
            let t_prev = y_of(coords, plane, a0);
            let b_prev = y_of(coords, plane, b0);
            let t_cur = y_of(coords, plane, a1);
            let b_cur = y_of(coords, plane, b1);

            // Style a span [i-1, i] by its LEFT/head column (matches FillPrimitive's
            // run-head convention: keying off i would shift colours/gradient by 1 bar).
            let grad = fill
                .gradient
                .as_deref()
                .and_then(|g| at(g, cur - 1.0 - off))
                .and_then(Option::as_ref);
            if let Some(grad) = grad {
                let y_top = y_of(coords, plane, grad.top_value);
                let y_bot0 = y_of(coords, plane, grad.bottom_value);
                let y_bot = if y_bot0 == y_top { y_top + 1.0 } else { y_bot0 };
                if g_obj.is_none()
                    || y_top != g_top
                    || y_bot != g_bot
                    || grad.top_color != g_tc
                    || grad.bottom_color != g_bc
                {
                    g_obj = Some(
                        LinearGradient::new(0.0, y_top, 0.0, y_bot)
                            .stop(0.0, grad.top_color.clone())
                            .stop(1.0, grad.bottom_color.clone()),
                    );
                    g_top = y_top;
                    g_bot = y_bot;
                    g_tc = grad.top_color.clone();
                    g_bc = grad.bottom_color.clone();
                }
                if let Some(g) = &g_obj {
                    ctx.set_fill_style(&g.clone().into());
                }
            } else {
                let color = match &fill.colors {
                    Some(colors) => at(colors, cur - 1.0 - off).and_then(Option::as_ref),
                    None => fill.color.as_ref(),
                };
                let Some(color) = color.filter(|c| !c.is_empty()) else {
                    continue;
                };
                ctx.set_fill_style(&color.into());
            }
            ctx.begin_path();
            ctx.move_to(x_prev, t_prev);
            ctx.line_to(x_cur, t_cur);
            ctx.line_to(x_cur, b_cur);
            ctx.line_to(x_prev, b_prev);
            ctx.close_path();
            ctx.fill();
        }
    }

    fn draw_background(
        &self,
        ctx: &dyn Canvas2d,
        bg: &Background,
        plane: &Plane,
        coords: &CoordinateSystem,
    ) {
        let x1 = coords.time_to_x(bg.from as f64);
        let x2 = coords.time_to_x(bg.to as f64);
        if x2 < 0.0 || x1 > coords.width() || x2 <= x1 {
            return;
        }
        ctx.set_fill_style(&(&bg.color).into());
        ctx.fill_rect(x1, plane.bounds.top, x2 - x1, plane.bounds.height);
    }

    fn draw_hline(
        &self,
        ctx: &dyn Canvas2d,
        pl: &PriceLine,
        plane: &Plane,
        coords: &CoordinateSystem,
        data_w: f64,
        theme: &VelaTheme,
    ) {
        let y = js_round(y_of(coords, plane, pl.price)) + 0.5;
        if y < plane.bounds.top || y > plane.bounds.top + plane.bounds.height {
            return;
        }
        let color = pl
            .color
            .as_deref()
            .unwrap_or_else(|| theme_str(theme, "textColor"));
        ctx.set_stroke_style(&color.into());
        ctx.set_line_width(pl.width.unwrap_or(1.0));
        set_line_dash(ctx, pl.line_style.unwrap_or(LineStyle::Solid));
        ctx.begin_path();
        ctx.move_to(0.0, y);
        ctx.line_to(data_w, y);
        ctx.stroke();
        set_line_dash(ctx, LineStyle::Solid);
    }
}

fn find_points<'a>(model: &'a IndicatorModel, series_id: &str) -> Option<&'a [SeriesPoint]> {
    model
        .series
        .iter()
        .find(|s| s.id == series_id)
        .and_then(SeriesSpec::points)
}

/// Every `barcolor()` entry of every indicator, by bar time (a later indicator overrides
/// an earlier one). Empty in the common case of no barcolor() anywhere.
fn merge_bar_colors<'a>(models: impl Iterator<Item = &'a IndicatorModel>) -> HashMap<i64, String> {
    let mut map = HashMap::new();
    for m in models {
        for bc in &m.bar_colors {
            map.insert(bc.time, bc.color.clone());
        }
    }
    map
}

impl RenderBackend for Canvas2dBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Canvas2d
    }

    fn model_alpha(&self) -> f64 {
        self.model_alpha
    }

    fn set_model_alpha(&mut self, alpha: f64) {
        self.model_alpha = alpha;
    }

    fn candle_body_alpha(&self) -> f64 {
        self.candle_body_alpha
    }

    fn set_candle_body_alpha(&mut self, alpha: f64) {
        self.candle_body_alpha = alpha;
    }

    fn candle_structure_alpha(&self) -> f64 {
        self.candle_structure_alpha
    }

    fn set_candle_structure_alpha(&mut self, alpha: f64) {
        self.candle_structure_alpha = alpha;
    }

    fn candle_body_scale(&self) -> f64 {
        self.candle_body_scale
    }

    fn set_candle_body_scale(&mut self, scale: f64) {
        self.candle_body_scale = scale;
    }

    fn render(&mut self, scene: &SceneGraph, coords: &CoordinateSystem, theme: &VelaTheme) {
        let Some(surface) = &self.surface else {
            return;
        };
        let ctx: &dyn Canvas2d = &*surface.ctx;
        let dpr = coords.dpr();
        let full_w = surface.width / dpr;
        let full_h = surface.height / dpr;
        let data_w = coords.width(); // excludes the right price-axis strip

        begin_plot_frame(ctx, dpr);
        ctx.clear_rect(0.0, 0.0, full_w, full_h);

        let n = coords.bar_count();
        if n == 0 {
            return;
        }
        let vr = coords.visible_logical_range();
        let i0 = js_max(0.0, f64::floor(vr.from));
        let i1 = js_min((n - 1) as f64, f64::ceil(vr.to));
        if !(i1 >= i0) {
            return;
        }
        // Indicator series and fills may carry points past the last bar (a plot shifted
        // forward), so they paint through the visible right whitespace too.
        let s1 = js_max(i1, f64::ceil(vr.to));
        let (i0_idx, i1_idx) = (i0 as usize, i1 as usize);

        let bar_color_map = merge_bar_colors(scene.indicators.values());
        let panes = scene.ordered_panes();

        // Session highlights + the grid paint on the renderer's BACKDROP canvas, below
        // every layer canvas: nothing to do here.

        // ── per-pane data (pane.scale was set by the renderer's autoscale) ──
        for pane in panes {
            // A collapsed pane is a legend-only strip: draw no plots (its separator and
            // legend remain).
            if pane.collapsed {
                continue;
            }
            // Indicators sorted by foreground z; the candles are one orderable layer at
            // `scene.candle_z`, so an overlay with z below it draws BEHIND the candles.
            let models = scene.ordered_indicators_for_pane(&pane.id);
            let is_price = pane.kind == PaneKind::Price;
            let pane_plane = Plane::of(pane);
            ctx.save();
            ctx.begin_path();
            ctx.rect(0.0, pane.bounds.top, data_w, pane.bounds.height);
            ctx.clip();

            // A merged (own-scale) indicator draws against its own price window inside
            // the pane; everything else against the pane's master scale.
            let eff_plane = |m: &IndicatorModel| Plane {
                scale: scene.scale_for(m, pane),
                bounds: pane.bounds,
            };

            // Behind everything: bgcolor spans, since Pine keeps the background under
            // every layer regardless of stacking. Own (non-force_overlay) spans on each
            // model's pane; force_overlay spans on the price pane, whatever pane the
            // indicator owns (Pine semantics, mirroring the chrome's overlay-drawing
            // routing). Fills are NOT here: they paint inside the z loop at their
            // model's slot.
            ctx.set_global_alpha(self.model_alpha); // indicator models fade in after the intro; candles stay opaque
            for m in &models {
                for bg in &m.backgrounds {
                    if !bg.overlay {
                        self.draw_background(ctx, bg, &pane_plane, coords);
                    }
                }
            }
            if is_price {
                for m in scene.indicators.values() {
                    for bg in &m.backgrounds {
                        if bg.overlay {
                            self.draw_background(ctx, bg, &pane_plane, coords);
                        }
                    }
                }
            }

            // User-drawing interleave layers, prepainted by the renderer: each composites
            // just before the series carrying its `before_z`, so a drawing can sit under
            // the candles or between two indicators. Full-opacity: drawings don't fade
            // with the models.
            let slices = scene
                .drawing_slices
                .get(&pane.id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let mut si = 0;
            let mut draw_slices_up_to = |z: f64| {
                while si < slices.len() && slices[si].before_z <= z {
                    ctx.set_global_alpha(1.0);
                    ctx.draw_image(&slices[si].canvas, 0.0, 0.0, full_w, full_h);
                    si += 1;
                }
            };

            // Foreground: candles + each indicator's series, interleaved by z-order. When
            // the price is hidden, the candle layer is skipped entirely (overlays still
            // draw).
            let draw_candles = is_price && !scene.candles_hidden;
            let mut candle_drawn = false;
            for m in &models {
                if draw_candles && !candle_drawn && scene.z_of(&m.id) >= scene.candle_z {
                    draw_slices_up_to(scene.candle_z);
                    ctx.set_global_alpha(self.candle_structure_alpha); // baseline; draw_candles sets body vs structure per-element
                    self.draw_price_series(
                        ctx,
                        scene,
                        i0_idx,
                        i1_idx,
                        coords,
                        pane,
                        theme,
                        &bar_color_map,
                        data_w,
                    );
                    candle_drawn = true;
                }
                draw_slices_up_to(scene.z_of(&m.id));
                ctx.set_global_alpha(self.model_alpha);
                // Model data is index-aligned from the model's ANCHOR bar (offset 0 =
                // whole-chart).
                let off = scene.offset_of(&m.id) as f64;
                let mp = eff_plane(m);
                // Fills paint at the model's z slot, under its own series: a band between
                // two plots sits behind the plot lines, and the whole model moves as one
                // unit when its object-tree row is reordered.
                for f in &m.fills {
                    if !f.overlay {
                        self.draw_fill(ctx, m, f, &mp, coords, i0, s1, off);
                    }
                }
                for s in &m.series {
                    if !s.overlay {
                        self.draw_series(ctx, s, &mp, coords, i0, s1, theme, off);
                    }
                }
            }
            if draw_candles && !candle_drawn {
                draw_slices_up_to(scene.candle_z);
                ctx.set_global_alpha(self.candle_structure_alpha);
                self.draw_price_series(
                    ctx,
                    scene,
                    i0_idx,
                    i1_idx,
                    coords,
                    pane,
                    theme,
                    &bar_color_map,
                    data_w,
                );
            }
            // force_overlay content from EVERY indicator paints on the price pane, at the
            // top of its series stack, against the master price scale: fills first so a
            // forced band still sits under the forced plot lines.
            if is_price {
                ctx.set_global_alpha(self.model_alpha);
                for m in scene.indicators.values() {
                    let off = scene.offset_of(&m.id) as f64;
                    for f in &m.fills {
                        if f.overlay {
                            self.draw_fill(ctx, m, f, &pane_plane, coords, i0, s1, off);
                        }
                    }
                }
                for m in scene.indicators.values() {
                    let off = scene.offset_of(&m.id) as f64;
                    for s in &m.series {
                        if s.overlay {
                            self.draw_series(ctx, s, &pane_plane, coords, i0, s1, theme, off);
                        }
                    }
                }
            }
            // Stack-top slices: the topmost indicator's drawings, force_overlay drawings,
            // and layers bound to a hidden/removed series: above the forced series too,
            // since drawings always paint over their own plots.
            draw_slices_up_to(f64::INFINITY);

            // On top: price lines.
            ctx.set_global_alpha(self.model_alpha);
            for m in &models {
                let mp = eff_plane(m);
                for pl in &m.price_lines {
                    self.draw_hline(ctx, pl, &mp, coords, data_w, theme);
                }
            }

            ctx.restore();
        }
    }

    fn destroy(&mut self) {
        self.surface = None;
    }
}

/// Fixtures shared by the painters' tests.
#[cfg(test)]
pub(crate) mod test_support {
    use std::rc::Rc;

    use serde_json::{Value, json};

    use crate::model::{
        IndicatorModel, LineLikeKind, LineLikeStyle, LineStyle, Ohlcv, PaneHint, PaneKind,
        SeriesData, SeriesPoint, SeriesSpec,
    };
    use crate::render::canvas::RecordingCanvas;
    use crate::render::core::{CoordinateSystem, SceneGraph};

    pub fn theme() -> Value {
        json!({
            "background": "#000000",
            "textColor": "#cccccc",
            "gridColor": "#222222",
            "borderColor": "#333333",
            "upColor": "#00AA00",
            "downColor": "#AA0000",
            "fontFamily": "sans-serif",
        })
    }

    pub fn model(id: &str, pane_id: &str) -> IndicatorModel {
        IndicatorModel {
            id: id.into(),
            title: id.into(),
            shorttitle: None,
            display_title: None,
            display_shorttitle: None,
            overlay: true,
            pane_hint: PaneHint::Price,
            native: None,
            legend: None,
            pane_axis: None,
            pane_id: Some(pane_id.into()),
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
        }
    }

    pub fn line_like(
        id: &str,
        kind: LineLikeKind,
        values: &[Option<f64>],
        color: &str,
        width: f64,
    ) -> SeriesSpec {
        SeriesSpec {
            id: id.into(),
            title: id.into(),
            display_title: None,
            pane_id: "price".into(),
            z_order: None,
            visible: None,
            display: None,
            overlay: false,
            data: SeriesData::LineLike {
                kind,
                points: values
                    .iter()
                    .enumerate()
                    .map(|(i, v)| SeriesPoint {
                        time: i as i64 * 60_000,
                        value: *v,
                        color: None,
                    })
                    .collect(),
                style: LineLikeStyle {
                    color: color.into(),
                    width,
                    line_style: LineStyle::Solid,
                    base: None,
                },
            },
        }
    }

    /// Bars one minute apart from time 0.
    pub fn bars(ohlc: &[(f64, f64, f64, f64)]) -> Vec<Ohlcv> {
        ohlc.iter()
            .enumerate()
            .map(|(i, (o, h, l, c))| Ohlcv::new(i as i64 * 60_000, *o, *h, *l, *c, Some(1.0)))
            .collect()
    }

    /// A scene with a price pane over `min..max` (pixel extent `top..top + height`), grid off.
    pub fn scene_with(bars: Vec<Ohlcv>, top: f64, height: f64, min: f64, max: f64) -> SceneGraph {
        let mut scene = SceneGraph::new();
        scene.bars = bars;
        scene.show_grid = false;
        let pane = scene.ensure_pane("price", PaneKind::Price, 0, Some(3.0));
        pane.bounds.top = top;
        pane.bounds.height = height;
        pane.scale.min = min;
        pane.scale.max = max;
        scene
    }

    /// Coordinates over the scene's bars: `width` x `height` CSS px, `dpr`, bar pitch and
    /// right margin (in bars).
    pub fn coords_for(
        scene: &SceneGraph,
        width: f64,
        height: f64,
        dpr: f64,
        spacing: f64,
        right_offset: f64,
    ) -> CoordinateSystem {
        let mut coords = CoordinateSystem::new();
        coords.set_size(width, height, dpr);
        let times: Vec<i64> = scene.bars.iter().map(|b| b.time).collect();
        coords.set_bars(&times);
        coords.set_viewport(crate::render::core::ViewportState {
            bar_spacing: spacing,
            right_offset,
        });
        coords
    }

    pub fn recording() -> Rc<RecordingCanvas> {
        Rc::new(RecordingCanvas::new())
    }

    /// The first three calls of every frame on a plot canvas: the device transform, the
    /// pinned direction, and the clear.
    pub fn frame_start(dpr: f64, full_w: f64, full_h: f64) -> Vec<Value> {
        vec![
            json!(["setTransform", dpr, 0.0, 0.0, dpr, 0.0, 0.0]),
            json!(["direction", "ltr"]),
            json!(["clearRect", 0.0, 0.0, full_w, full_h]),
        ]
    }

    /// Every fill and stroke the recording shows, with the style it painted with:
    /// `("fill" | "stroke", style)`, as the original's tests record them.
    pub fn paint_ops(canvas: &RecordingCanvas) -> Vec<(String, String)> {
        let mut fill = String::new();
        let mut stroke = String::new();
        let mut out = Vec::new();
        for op in canvas.ops() {
            let name = op[0].as_str().unwrap_or("");
            match name {
                "fillStyle" => fill = op[1].as_str().unwrap_or("<gradient>").to_string(),
                "strokeStyle" => stroke = op[1].as_str().unwrap_or("<gradient>").to_string(),
                "fill" | "fillRect" => out.push(("fill".to_string(), fill.clone())),
                "stroke" | "strokeRect" => out.push(("stroke".to_string(), stroke.clone())),
                _ => {}
            }
        }
        out
    }

    pub fn pair(op: &str, style: &str) -> (String, String) {
        (op.to_string(), style.to_string())
    }

    /// A stream from concatenated pieces.
    pub fn stream(parts: Vec<Vec<Value>>) -> Value {
        Value::Array(parts.into_iter().flatten().collect())
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use serde_json::{Value, json};

    use super::test_support::*;
    use super::*;
    use crate::model::{
        CandleBarColor, CandleKind, CandleStyle as SeriesCandleStyle, FillGradientStop, PriceLine,
        SeriesDisplay,
    };
    use crate::render::canvas::{CanvasImage, RecordingCanvas};
    use crate::render::core::scene_graph::DrawingSlice;

    /// Mount, render one frame, and hand back the recording.
    fn paint(scene: &SceneGraph, coords: &CoordinateSystem) -> Rc<RecordingCanvas> {
        let canvas = recording();
        let mut backend = Canvas2dBackend::new();
        backend.mount(
            canvas.clone(),
            coords.width() * coords.dpr(),
            coords.height() * coords.dpr(),
        );
        backend.render(scene, coords, &theme());
        canvas
    }

    /// The three-bar scene of the stream tests: bars at x = 236, 244, 252 (pitch 8 on a
    /// 300 px plot with a 6-bar margin), prices mapped as `y = 100 - price`.
    fn three_bars() -> SceneGraph {
        scene_with(
            bars(&[
                (40.0, 60.0, 30.0, 50.0), // up
                (50.0, 70.0, 40.0, 45.0), // down
                (45.0, 55.0, 35.0, 52.0), // up
            ]),
            0.0,
            100.0,
            0.0,
            100.0,
        )
    }

    fn three_coords(scene: &SceneGraph) -> CoordinateSystem {
        coords_for(scene, 300.0, 100.0, 1.0, 8.0, 6.0)
    }

    /// The pixel y of a price on the stream tests' pane (0..100 over 100 px), by the
    /// original's arithmetic (`top + height * (1 - t)`), whose rounding noise a literal
    /// would not carry (`y(45)` is 55.00000000000001).
    fn y(price: f64) -> f64 {
        0.0 + 100.0 * (1.0 - (price - 0.0) / (100.0 - 0.0))
    }

    /// The clip of the price pane every pane starts with, and the alpha it sets for the
    /// models' backgrounds.
    fn pane_open() -> Vec<Value> {
        vec![
            json!(["save"]),
            json!(["beginPath"]),
            json!(["rect", 0.0, 0.0, 300.0, 100.0]),
            json!(["clip"]),
            json!(["globalAlpha", 1.0]),
        ]
    }

    /// The model alpha set before the price lines (also after the candles), the price
    /// lines' own, and the pane restore.
    fn pane_close() -> Vec<Value> {
        vec![
            json!(["globalAlpha", 1.0]),
            json!(["globalAlpha", 1.0]),
            json!(["restore"]),
        ]
    }

    // ── the trait surface ──

    #[test]
    fn defaults_and_setters_follow_the_backend_contract() {
        let mut b = Canvas2dBackend::default();
        assert_eq!(b.kind(), BackendKind::Canvas2d);
        assert_eq!(b.model_alpha(), 1.0);
        assert_eq!(b.candle_body_alpha(), 1.0);
        assert_eq!(b.candle_structure_alpha(), 1.0);
        assert_eq!(b.candle_body_scale(), 1.0);
        b.set_model_alpha(0.25);
        b.set_candle_body_alpha(0.5);
        b.set_candle_structure_alpha(0.75);
        b.set_candle_body_scale(0.6);
        assert_eq!(
            (
                b.model_alpha(),
                b.candle_body_alpha(),
                b.candle_structure_alpha(),
                b.candle_body_scale()
            ),
            (0.25, 0.5, 0.75, 0.6)
        );
    }

    #[test]
    fn an_unmounted_or_destroyed_backend_paints_nothing() {
        let scene = three_bars();
        let coords = three_coords(&scene);
        let mut backend = Canvas2dBackend::new();
        backend.render(&scene, &coords, &theme()); // no surface: must not panic
        let canvas = recording();
        backend.mount(canvas.clone(), 300.0, 100.0);
        backend.destroy();
        backend.render(&scene, &coords, &theme());
        assert!(canvas.is_empty());
    }

    #[test]
    fn a_frame_without_bars_only_clears_and_a_resize_changes_the_cleared_area() {
        let scene = scene_with(vec![], 0.0, 100.0, 0.0, 1.0);
        let coords = coords_for(&scene, 300.0, 100.0, 2.0, 8.0, 6.0);
        let canvas = recording();
        let mut backend = Canvas2dBackend::new();
        backend.mount(canvas.clone(), 600.0, 200.0);
        backend.render(&scene, &coords, &theme());
        assert_eq!(
            canvas.to_json(),
            Value::Array(frame_start(2.0, 300.0, 100.0))
        );
        canvas.clear();
        backend.set_backing_size(800.0, 400.0);
        backend.render(&scene, &coords, &theme());
        assert_eq!(
            canvas.to_json()[2],
            json!(["clearRect", 0.0, 0.0, 400.0, 200.0])
        );
    }

    #[test]
    fn a_collapsed_pane_draws_no_plots() {
        let mut scene = three_bars();
        scene.panes.get_mut("price").unwrap().collapsed = true;
        let coords = three_coords(&scene);
        assert_eq!(
            paint(&scene, &coords).to_json(),
            Value::Array(frame_start(1.0, 300.0, 100.0))
        );
    }

    // ── candles: the complete stream ──

    #[test]
    fn three_candles_paint_this_exact_stream() {
        let scene = three_bars();
        let coords = three_coords(&scene);
        // candle_geometry at pitch 8, dpr 1: wick 2 wide centered on x, body 6 wide
        // (x - 3 .. x + 3). Y is 100 - price, snapped to whole pixels.
        let candle = |x: f64, color: &str, h: f64, top: f64, body_h: f64, l: f64| {
            vec![
                json!(["globalAlpha", 1.0]),
                json!(["strokeStyle", color]),
                json!(["lineWidth", 2.0]),
                json!(["beginPath"]),
                json!(["moveTo", x, h]),
                json!(["lineTo", x, top]),
                json!(["moveTo", x, top + body_h]),
                json!(["lineTo", x, l]),
                json!(["stroke"]),
                json!(["globalAlpha", 1.0]),
                json!(["fillStyle", color]),
                json!(["fillRect", x - 3.0, top, 6.0, body_h]),
            ]
        };
        let expected = stream(vec![
            frame_start(1.0, 300.0, 100.0),
            pane_open(),
            vec![json!(["globalAlpha", 1.0])], // candle structure alpha
            candle(236.0, "#00AA00", 40.0, 50.0, 10.0, 70.0),
            candle(244.0, "#AA0000", 30.0, 50.0, 5.0, 60.0),
            candle(252.0, "#00AA00", 45.0, 48.0, 7.0, 65.0),
            pane_close(),
        ]);
        assert_eq!(paint(&scene, &coords).to_json(), expected);
    }

    #[test]
    fn flat_bars_are_skipped_and_nan_bars_paint_with_nan_coordinates() {
        // A flat bar (high == low) is skipped; a NaN bar is not (NaN comparisons are
        // false), so it paints with NaN coordinates, which a context ignores.
        let scene = scene_with(
            bars(&[
                (50.0, 50.0, 50.0, 50.0),
                (f64::NAN, f64::NAN, f64::NAN, f64::NAN),
            ]),
            0.0,
            100.0,
            0.0,
            100.0,
        );
        let coords = three_coords(&scene);
        let canvas = paint(&scene, &coords);
        assert_eq!(canvas.calls("fillRect").len(), 1);
        assert_eq!(canvas.calls("stroke").len(), 1);
    }

    #[test]
    fn a_candle_on_a_two_x_screen_snaps_to_device_pixels() {
        let scene = scene_with(
            bars(&[(100.0, 106.0, 99.0, 105.0)]),
            0.0,
            100.0,
            95.0,
            110.0,
        );
        let coords = coords_for(&scene, 300.0, 100.0, 2.0, 8.0, 6.0);
        let canvas = paint(&scene, &coords);
        // dpr 2: wick = max(1, round(1.5 * 2)) = 3 device px; body = round(floor(5.6) * 2)
        // = 10, raised to 11 to share the wick's parity; wick left = round(504 - 1.5) = 503
        // (a half rounds up), so body left = 503 - 4 = 499 device px = 249.5 css px, 5.5 wide.
        // Y: close 33.33 css -> 66.67 device -> 67 (33.5 css); open 66.67 css -> 133.33
        // device -> 133 (66.5 css): a body 33 css px tall.
        let rect = &canvas.calls("fillRect")[0];
        assert_eq!(rect, &json!(["fillRect", 249.5, 33.5, 5.5, 33.0]));
    }

    // ── barcolor() and the candle cosmetics ──

    /// One candle over 95..110 on a 300 x 100 plot, optionally barcolored, bordered.
    fn one_candle(
        bar: (f64, f64, f64, f64),
        bar_color: Option<&str>,
        border_visible: Option<bool>,
        border_up: Option<&str>,
    ) -> Vec<(String, String)> {
        let mut scene = scene_with(bars(&[bar]), 0.0, 100.0, 95.0, 110.0);
        if let Some(visible) = border_visible {
            scene.style.candle.border_visible = visible;
        }
        if let Some(color) = border_up {
            scene.style.candle.border_up_color = Some(color.to_string());
        }
        if let Some(color) = bar_color {
            let mut m = model("ind", "price");
            m.bar_colors = vec![crate::model::BarColor {
                time: 0,
                color: color.to_string(),
            }];
            scene.indicators.insert("ind", m);
            scene.assign_indicator_z("ind");
        }
        let coords = coords_for(&scene, 300.0, 100.0, 1.0, 8.0, 6.0);
        paint_ops(&paint(&scene, &coords))
    }

    const UP_BAR: (f64, f64, f64, f64) = (100.0, 106.0, 99.0, 105.0);
    const DOWN_BAR: (f64, f64, f64, f64) = (105.0, 106.0, 99.0, 100.0);

    #[test]
    fn plain_candle_body_and_wick_in_the_direction_color_no_border() {
        assert_eq!(
            one_candle(UP_BAR, None, None, None),
            vec![pair("stroke", "#00AA00"), pair("fill", "#00AA00")]
        );
    }

    #[test]
    fn a_barcolored_candle_tints_the_body_and_keeps_the_direction_wick() {
        assert_eq!(
            one_candle(UP_BAR, Some("#123456"), None, None),
            vec![pair("stroke", "#00AA00"), pair("fill", "#123456")]
        );
        assert_eq!(
            one_candle(DOWN_BAR, Some("#123456"), None, None),
            vec![pair("stroke", "#AA0000"), pair("fill", "#123456")]
        );
    }

    #[test]
    fn the_border_follows_its_setting_and_inherits_the_body_color() {
        assert_eq!(
            one_candle(UP_BAR, None, Some(true), None),
            vec![
                pair("stroke", "#00AA00"),
                pair("fill", "#00AA00"),
                pair("stroke", "#00AA00")
            ]
        );
        assert_eq!(
            one_candle(UP_BAR, Some("#123456"), Some(true), None),
            vec![
                pair("stroke", "#00AA00"),
                pair("fill", "#123456"),
                pair("stroke", "#123456")
            ]
        );
        let ops = one_candle(UP_BAR, Some("#123456"), Some(true), Some("#ABCDEF"));
        assert_eq!(ops.last(), Some(&pair("stroke", "#ABCDEF")));
    }

    #[test]
    fn a_fading_style_keeps_a_hollow_outline_and_the_alphas_follow_the_setters() {
        let scene = scene_with(bars(&[UP_BAR]), 0.0, 100.0, 95.0, 110.0);
        let coords = coords_for(&scene, 300.0, 100.0, 1.0, 8.0, 6.0);
        let canvas = recording();
        let mut backend = Canvas2dBackend::new();
        backend.mount(canvas.clone(), 300.0, 100.0);
        backend.set_candle_body_alpha(0.2);
        backend.set_candle_structure_alpha(0.6);
        backend.set_candle_body_scale(0.5);
        backend.render(&scene, &coords, &theme());
        // Body scale 0.5: floor(8 * 0.7 * 0.5) = 2 css px = 2 device px, equal to the wick's
        // (2 device px): the body is as wide as the wick.
        assert_eq!(
            canvas.to_json(),
            stream(vec![
                frame_start(1.0, 300.0, 100.0),
                vec![
                    json!(["save"]),
                    json!(["beginPath"]),
                    json!(["rect", 0.0, 0.0, 300.0, 100.0]),
                    json!(["clip"]),
                    json!(["globalAlpha", 1.0]), // model alpha for the backgrounds
                    json!(["globalAlpha", 0.6]), // structure alpha before the series
                    // wick (outside the body: 27..33 and 67..73)
                    json!(["globalAlpha", 0.6]),
                    json!(["strokeStyle", "#00AA00"]),
                    json!(["lineWidth", 2.0]),
                    json!(["beginPath"]),
                    json!(["moveTo", 252.0, 27.0]),
                    json!(["lineTo", 252.0, 33.0]),
                    json!(["moveTo", 252.0, 67.0]),
                    json!(["lineTo", 252.0, 73.0]),
                    json!(["stroke"]),
                    // translucent body
                    json!(["globalAlpha", 0.2]),
                    json!(["fillStyle", "#00AA00"]),
                    json!(["fillRect", 251.0, 33.0, 2.0, 34.0]),
                    // no border configured, but the body fades below the structure
                    json!(["globalAlpha", 0.6]),
                    json!(["strokeStyle", "#00AA00"]),
                    json!(["lineWidth", 1.0]),
                    json!(["strokeRect", 251.5, 33.5, 1.0, 33.0]),
                    json!(["globalAlpha", 1.0]), // model alpha before the price lines
                    json!(["globalAlpha", 1.0]),
                    json!(["restore"]),
                ],
            ])
        );
    }

    #[test]
    fn wick_and_body_visibility_and_wick_colors_follow_the_candle_style() {
        let mut scene = scene_with(bars(&[UP_BAR]), 0.0, 100.0, 95.0, 110.0);
        scene.style.candle.wick_visible = false;
        scene.style.candle.body_visible = false;
        let coords = coords_for(&scene, 300.0, 100.0, 1.0, 8.0, 6.0);
        assert!(paint_ops(&paint(&scene, &coords)).is_empty());

        scene.style.candle.wick_visible = true;
        scene.style.candle.wick_up_color = Some("#AAAAAA".into());
        scene.style.candle.body_visible = true;
        let ops = paint_ops(&paint(&scene, &coords));
        assert_eq!(
            ops,
            vec![pair("stroke", "#AAAAAA"), pair("fill", "#00AA00")]
        );
    }

    #[test]
    fn a_plugin_style_paints_with_its_own_candle_cosmetics() {
        let mut scene = scene_with(bars(&[UP_BAR]), 0.0, 100.0, 95.0, 110.0);
        scene.candle_override = Some(crate::render::core::chart_config::CandlePaintOverride {
            up_color: Some("#112233".into()),
            down_color: None,
            body_visible: None,
            border_visible: Some(true),
            border_up_color: None,
            border_down_color: None,
            wick_visible: None,
            wick_up_color: None,
            wick_down_color: None,
        });
        let coords = coords_for(&scene, 300.0, 100.0, 1.0, 8.0, 6.0);
        assert_eq!(
            paint_ops(&paint(&scene, &coords)),
            vec![
                pair("stroke", "#112233"),
                pair("fill", "#112233"),
                pair("stroke", "#112233")
            ]
        );
    }

    #[test]
    fn a_none_base_painting_or_hidden_candles_draw_no_price_series() {
        let mut scene = three_bars();
        scene.base_painting = BasePainting::None;
        let coords = three_coords(&scene);
        assert_eq!(
            paint(&scene, &coords).to_json(),
            stream(vec![
                frame_start(1.0, 300.0, 100.0),
                pane_open(),
                vec![json!(["globalAlpha", 1.0])],
                pane_close()
            ])
        );
        let mut hidden = three_bars();
        hidden.candles_hidden = true;
        assert_eq!(
            paint(&hidden, &coords).to_json(),
            stream(vec![
                frame_start(1.0, 300.0, 100.0),
                pane_open(),
                pane_close()
            ])
        );
    }

    // ── candle series with gaps ──

    #[test]
    fn a_candle_series_draws_nothing_for_a_gap() {
        // The slots the indicator has no candle for are entries with NaN prices: neither
        // paints, the others stay on their bars.
        let series_color = "#abcdef";
        let chart_bars = bars(&[
            (101.0, 102.0, 99.0, 100.0),
            (102.0, 103.0, 100.0, 101.0),
            (103.0, 104.0, 101.0, 102.0),
            (104.0, 105.0, 102.0, 103.0),
        ]);
        let mut scene = scene_with(chart_bars.clone(), 0.0, 100.0, 95.0, 110.0);
        let gap = |time: i64| Ohlcv::new(time, f64::NAN, f64::NAN, f64::NAN, f64::NAN, None);
        let mut series_bars = chart_bars;
        series_bars[1] = gap(60_000);
        series_bars[2] = gap(120_000);
        let mut m = model("m", "price");
        m.series.push(SeriesSpec {
            id: "m:candle".into(),
            title: "M".into(),
            display_title: None,
            pane_id: "price".into(),
            z_order: None,
            visible: None,
            display: None,
            overlay: false,
            data: SeriesData::Candle {
                kind: CandleKind::Candle,
                bars: series_bars,
                style: Some(SeriesCandleStyle {
                    up: Some(series_color.into()),
                    down: Some(series_color.into()),
                    ..Default::default()
                }),
                bar_colors: None,
            },
        });
        scene.indicators.insert("m", m);
        scene.assign_indicator_z("m");
        scene.base_painting = BasePainting::None; // isolate the series
        let coords = coords_for(&scene, 400.0, 100.0, 1.0, 8.0, 6.0);
        let ops = paint_ops(&paint(&scene, &coords));
        // Bars 0 and 3 only: a wick stroke and a body fill each.
        assert_eq!(ops.iter().filter(|(_, s)| s == series_color).count(), 4);
        assert_eq!(ops.len(), 4);
    }

    // ── price styles ──

    #[test]
    fn the_line_style_strokes_one_round_path_through_the_closes() {
        let mut scene = three_bars();
        scene.price_style = "line".into();
        scene.style.line.color = Some("#123456".into());
        let coords = three_coords(&scene);
        assert_eq!(
            paint(&scene, &coords).to_json(),
            stream(vec![
                frame_start(1.0, 300.0, 100.0),
                pane_open(),
                vec![
                    json!(["globalAlpha", 1.0]),
                    json!(["lineWidth", 2.0]),
                    json!(["lineJoin", "round"]),
                    json!(["lineCap", "round"]),
                    json!(["strokeStyle", "#123456"]),
                    json!(["beginPath"]),
                    json!(["moveTo", 236.0, 50.0]),
                    json!(["lineTo", 244.0, y(45.0)]),
                    json!(["lineTo", 252.0, y(52.0)]),
                    json!(["stroke"]),
                    json!(["lineCap", "butt"]),
                ],
                pane_close(),
            ])
        );
    }

    #[test]
    fn the_bars_style_draws_the_stick_with_open_and_close_ticks() {
        let mut scene = three_bars();
        scene.price_style = "bars".into();
        let coords = three_coords(&scene);
        // Pitch 8 is the full tier: ticks are round(8 * 0.35) = 3 px; the stick sits on a
        // pixel middle (round(x) + 0.5); colors by direction.
        let bar = |x: f64, color: &str, h: f64, l: f64, o: f64, c: f64| {
            vec![
                json!(["strokeStyle", color]),
                json!(["beginPath"]),
                json!(["moveTo", x, h]),
                json!(["lineTo", x, l]),
                json!(["moveTo", x - 3.0, o]),
                json!(["lineTo", x, o]),
                json!(["moveTo", x, c]),
                json!(["lineTo", x + 3.0, c]),
                json!(["stroke"]),
            ]
        };
        assert_eq!(
            paint(&scene, &coords).to_json(),
            stream(vec![
                frame_start(1.0, 300.0, 100.0),
                pane_open(),
                vec![json!(["globalAlpha", 1.0]), json!(["lineWidth", 1.0])],
                bar(236.5, "#00AA00", y(60.0), y(30.0), y(40.0), y(50.0)),
                bar(244.5, "#AA0000", y(70.0), y(40.0), y(50.0), y(45.0)),
                bar(252.5, "#00AA00", y(55.0), y(35.0), y(45.0), y(52.0)),
                pane_close(),
            ])
        );
    }

    #[test]
    fn the_area_style_fills_a_gradient_under_the_line_then_strokes_it() {
        let mut scene = three_bars();
        scene.price_style = "area".into();
        scene.style.area.line_color = Some("#123456".into());
        scene.style.area.top_color = Some("#aa000040".into());
        scene.style.area.bottom_color = None; // transparent at the floor
        let coords = three_coords(&scene);
        assert_eq!(
            paint(&scene, &coords).to_json(),
            stream(vec![
                frame_start(1.0, 300.0, 100.0),
                pane_open(),
                vec![
                    json!(["globalAlpha", 1.0]),
                    json!(["fillStyle", { "linearGradient": [0.0, y(52.0), 0.0, 100.0], "stops": [[0.0, "#aa000040"], [1.0, "rgba(0,0,0,0)"]] }]),
                    json!(["beginPath"]),
                    json!(["moveTo", 236.0, 100.0]),
                    json!(["lineTo", 236.0, 50.0]),
                    json!(["lineTo", 244.0, y(45.0)]),
                    json!(["lineTo", 252.0, y(52.0)]),
                    json!(["lineTo", 252.0, 100.0]),
                    json!(["closePath"]),
                    json!(["fill"]),
                    json!(["lineWidth", 2.0]),
                    json!(["lineJoin", "round"]),
                    json!(["lineCap", "round"]),
                    json!(["strokeStyle", "#123456"]),
                    json!(["beginPath"]),
                    json!(["moveTo", 236.0, 50.0]),
                    json!(["lineTo", 244.0, y(45.0)]),
                    json!(["lineTo", 252.0, y(52.0)]),
                    json!(["stroke"]),
                    json!(["lineCap", "butt"]),
                ],
                pane_close(),
            ])
        );
    }

    #[test]
    fn a_single_bar_has_no_area_to_fill() {
        let mut scene = scene_with(bars(&[UP_BAR]), 0.0, 100.0, 95.0, 110.0);
        scene.price_style = "area".into();
        let coords = coords_for(&scene, 300.0, 100.0, 1.0, 8.0, 6.0);
        let canvas = paint(&scene, &coords);
        assert!(canvas.calls("fill").is_empty());
        // The stroke of one point is a bare moveTo.
        assert_eq!(canvas.calls("lineTo").len(), 0);
    }

    #[test]
    fn the_baseline_style_clips_two_gradient_fills_at_the_baseline_and_splits_the_stroke() {
        let mut scene = three_bars();
        scene.price_style = "baseline".into();
        scene.baseline_value = Some(50.0); // y = 50
        let b = &mut scene.style.baseline;
        b.top_line_color = Some("#0f0".into());
        b.bottom_line_color = Some("#f00".into());
        b.top_fill_color = Some("#t1".into());
        b.top_fill_color2 = Some("#t2".into());
        b.bottom_fill_color = Some("#b1".into());
        b.bottom_fill_color2 = Some("#b2".into());
        let coords = three_coords(&scene);
        let poly = vec![
            json!(["beginPath"]),
            json!(["moveTo", 236.0, 50.0]),
            json!(["lineTo", 236.0, 50.0]),
            json!(["lineTo", 244.0, y(45.0)]),
            json!(["lineTo", 252.0, y(52.0)]),
            json!(["lineTo", 252.0, 50.0]),
            json!(["closePath"]),
            json!(["fill"]),
        ];
        let seg = |x0: f64, y0: f64, x1: f64, y1: f64, color: &str| {
            vec![
                json!(["strokeStyle", color]),
                json!(["beginPath"]),
                json!(["moveTo", x0, y0]),
                json!(["lineTo", x1, y1]),
                json!(["stroke"]),
            ]
        };
        // The segment 244 -> 252 crosses y = 50 at t = (50 - 55) / (48 - 55).
        let x_cross = 244.0 + 8.0 * ((50.0 - y(45.0)) / (y(52.0) - y(45.0)));
        let expected = stream(vec![
            frame_start(1.0, 300.0, 100.0),
            pane_open(),
            vec![
                json!(["globalAlpha", 1.0]),
                // above the baseline (rows 0..50)
                json!(["save"]),
                json!(["beginPath"]),
                json!(["rect", 0.0, 0.0, 300.0, 50.0]),
                json!(["clip"]),
                json!(["fillStyle", { "linearGradient": [0.0, 0.0, 0.0, 50.0], "stops": [[0.0, "#t1"], [1.0, "#t2"]] }]),
            ],
            poly.clone(),
            vec![
                json!(["restore"]),
                // below the baseline (rows 50..100), the far color first
                json!(["save"]),
                json!(["beginPath"]),
                json!(["rect", 0.0, 50.0, 300.0, 50.0]),
                json!(["clip"]),
                json!(["fillStyle", { "linearGradient": [0.0, 50.0, 0.0, 100.0], "stops": [[0.0, "#b2"], [1.0, "#b1"]] }]),
            ],
            poly,
            vec![
                json!(["restore"]),
                // the dashed baseline marker on a pixel middle
                json!(["strokeStyle", "#333333"]),
                json!(["lineWidth", 1.0]),
                json!(["setLineDash", [6.0, 4.0]]),
                json!(["beginPath"]),
                json!(["moveTo", 0.0, 50.5]),
                json!(["lineTo", 300.0, 50.5]),
                json!(["stroke"]),
                json!(["setLineDash", []]),
                json!(["lineWidth", 2.0]),
                json!(["lineJoin", "round"]),
                json!(["lineCap", "round"]),
            ],
            // 236 -> 244: starts on the baseline (up side), ends below it
            seg(236.0, 50.0, 236.0, 50.0, "#0f0"),
            seg(236.0, 50.0, 244.0, y(45.0), "#f00"),
            // 244 -> 252: below, then up through the crossing
            seg(244.0, y(45.0), x_cross, 50.0, "#f00"),
            seg(x_cross, 50.0, 252.0, y(52.0), "#0f0"),
            vec![json!(["lineCap", "butt"])],
            pane_close(),
        ]);
        assert_eq!(paint(&scene, &coords).to_json(), expected);
    }

    #[test]
    fn the_baseline_level_places_the_marker_at_that_share_of_the_pane_height() {
        // `baseline_price_for` resolves the level (see the scene tests); here only the
        // painted y: level 25 on 0..100 is the price 25, drawn at y = 75.
        let mut scene = three_bars();
        scene.price_style = "baseline".into();
        scene.style.baseline.baseline_level = 25.0;
        let coords = three_coords(&scene);
        let canvas = paint(&scene, &coords);
        assert!(
            canvas
                .calls("moveTo")
                .contains(&json!(["moveTo", 0.0, 75.5]))
        );
    }

    #[test]
    fn a_baseline_off_the_pane_skips_the_fill_it_cannot_show_and_keeps_clips_balanced() {
        let mut scene = three_bars();
        scene.price_style = "baseline".into();
        scene.baseline_value = Some(500.0); // above the window: base_y < top
        let coords = three_coords(&scene);
        let canvas = paint(&scene, &coords);
        let names = canvas.names();
        let saves = names.iter().filter(|n| *n == "save").count();
        let restores = names.iter().filter(|n| *n == "restore").count();
        assert_eq!(saves, restores);
        assert_eq!(canvas.calls("clip").len(), 2); // the pane's, and the lower half's
    }

    // ── close-only styles ──

    const CLOSES: [f64; 5] = [10.0, 12.0, 11.0, 11.0, 13.0];

    /// Bars at x = 210 ... 290 (pitch 20, margin 0.5 bar) over 0..20: `y = 100 - 5 * close`.
    fn closes_scene(
        style: &str,
        spacing: f64,
        bar_color: Option<(i64, &str)>,
    ) -> (SceneGraph, CoordinateSystem) {
        let bars: Vec<Ohlcv> = CLOSES
            .iter()
            .enumerate()
            .map(|(i, c)| Ohlcv::new(i as i64 * 60_000, *c, *c, *c, *c, None))
            .collect();
        let mut scene = scene_with(bars, 0.0, 100.0, 0.0, 20.0);
        scene.price_style = style.into();
        scene.style.line.color = Some("#123456".into());
        if let Some((time, color)) = bar_color {
            let mut m = model("ind", "price");
            m.bar_colors = vec![crate::model::BarColor {
                time,
                color: color.to_string(),
            }];
            scene.indicators.insert("ind", m);
            scene.assign_indicator_z("ind");
        }
        let coords = coords_for(&scene, 300.0, 100.0, 1.0, spacing, 0.5);
        (scene, coords)
    }

    fn up() -> String {
        with_alpha("#00AA00", COLUMNS_FILL_ALPHA)
    }

    fn down() -> String {
        with_alpha("#AA0000", COLUMNS_FILL_ALPHA)
    }

    #[test]
    fn columns_paint_one_column_per_bar_from_the_pane_bottom_colored_against_the_previous_close() {
        let (scene, coords) = closes_scene("columns", 20.0, None);
        let canvas = paint(&scene, &coords);
        let cols = canvas.calls("fillRect");
        let styles: Vec<String> = paint_ops(&canvas).into_iter().map(|(_, s)| s).collect();
        assert_eq!(styles, vec![up(), up(), down(), up(), up()]);
        let ys: Vec<f64> = cols.iter().map(|c| c[2].as_f64().unwrap()).collect();
        assert_eq!(
            ys,
            CLOSES.iter().map(|c| 100.0 - 5.0 * c).collect::<Vec<_>>()
        );
        for c in &cols {
            assert_eq!(c[2].as_f64().unwrap() + c[4].as_f64().unwrap(), 100.0);
        }
        // 20 px slots less a 1 px gap.
        let slots: Vec<(f64, f64)> = cols
            .iter()
            .map(|c| (c[1].as_f64().unwrap(), c[3].as_f64().unwrap()))
            .collect();
        assert_eq!(
            slots,
            vec![
                (200.0, 19.0),
                (220.0, 19.0),
                (240.0, 19.0),
                (260.0, 19.0),
                (280.0, 19.0)
            ]
        );
    }

    #[test]
    fn a_barcolor_tints_its_column() {
        let (scene, coords) = closes_scene("columns", 20.0, Some((2 * 60_000, "#abcdef")));
        let styles: Vec<String> = paint_ops(&paint(&scene, &coords))
            .into_iter()
            .map(|(_, s)| s)
            .collect();
        assert_eq!(styles, vec![up(), up(), "#abcdef".to_string(), up(), up()]);
    }

    #[test]
    fn a_column_with_no_height_is_skipped() {
        // The close sits on the pane floor: top == bottom, `!(top < bottom)`.
        let mut scene = scene_with(bars(&[(0.0, 0.0, 0.0, 0.0)]), 0.0, 100.0, 0.0, 20.0);
        scene.price_style = "columns".into();
        let coords = coords_for(&scene, 300.0, 100.0, 1.0, 20.0, 0.5);
        assert!(paint(&scene, &coords).calls("fillRect").is_empty());
    }

    #[test]
    fn line_markers_draw_the_line_plus_a_dot_per_close() {
        let (scene, coords) = closes_scene("linemarkers", 20.0, None);
        let canvas = paint(&scene, &coords);
        let arcs = canvas.calls("arc");
        let centers: Vec<(f64, f64)> = arcs
            .iter()
            .map(|a| {
                (
                    a[1].as_f64().unwrap(),
                    (a[2].as_f64().unwrap() * 1e6).round() / 1e6,
                )
            })
            .collect();
        let expected: Vec<(f64, f64)> = CLOSES
            .iter()
            .enumerate()
            .map(|(i, c)| (210.0 + 20.0 * i as f64, 100.0 - 5.0 * c))
            .collect();
        assert_eq!(centers, expected);
        assert!(arcs.iter().all(|a| a[3] == json!(4.0)));
        // One moveTo for the line, one per dot (each dot starts its own subpath).
        assert_eq!(canvas.calls("moveTo").len(), 1 + 5);
        let ops = paint_ops(&canvas);
        assert_eq!(
            ops,
            vec![pair("stroke", "#123456"), pair("fill", "#123456")]
        );
    }

    #[test]
    fn dense_bars_leave_the_plain_line_and_the_plain_style_never_draws_markers() {
        let (scene, coords) = closes_scene("linemarkers", 6.0, None);
        let canvas = paint(&scene, &coords);
        assert!(canvas.calls("arc").is_empty());
        assert_eq!(canvas.calls("stroke").len(), 1);
        let (scene, coords) = closes_scene("line", 20.0, None);
        assert!(paint(&scene, &coords).calls("arc").is_empty());
    }

    #[test]
    fn the_step_line_strokes_horizontal_then_vertical_runs_with_square_corners() {
        let (scene, coords) = closes_scene("stepline", 20.0, None);
        let canvas = paint(&scene, &coords);
        let path: Vec<(f64, f64)> = canvas
            .ops()
            .iter()
            .filter(|op| op[0] == "moveTo" || op[0] == "lineTo")
            .map(|op| (op[1].as_f64().unwrap(), op[2].as_f64().unwrap()))
            .collect();
        // The unchanged close (bar 3) adds no step.
        assert_eq!(
            path,
            vec![
                (210.0, 50.0),
                (230.0, 50.0),
                (230.0, 40.0),
                (250.0, 40.0),
                (250.0, 45.0),
                (270.0, 45.0),
                (290.0, 45.0),
                (290.0, 35.0),
            ]
        );
        assert_eq!(
            canvas.calls("lineCap"),
            vec![json!(["lineCap", "square"]), json!(["lineCap", "butt"])]
        );
        assert_eq!(canvas.calls("lineJoin"), vec![json!(["lineJoin", "miter"])]);
        assert_eq!(canvas.calls("lineWidth"), vec![json!(["lineWidth", 2.0])]);
    }

    // ── sub-pixel LOD ──

    #[test]
    fn below_one_pixel_per_bar_candles_collapse_into_per_column_sticks_per_color() {
        let scene = three_bars();
        let coords = coords_for(&scene, 300.0, 100.0, 1.0, 0.5, 6.0);
        // x = 296, 296.5, 297: the first bar owns column 296, the second (a down bar) and
        // the third (up) share column 297 as two same-color runs, ordered by their latest
        // bar.
        let stick = |x: f64, color: &str, hi: f64, lo: f64| {
            vec![
                json!(["strokeStyle", color]),
                json!(["beginPath"]),
                json!(["moveTo", x, hi]),
                json!(["lineTo", x, lo]),
                json!(["stroke"]),
            ]
        };
        assert_eq!(
            paint(&scene, &coords).to_json(),
            stream(vec![
                frame_start(1.0, 300.0, 100.0),
                pane_open(),
                vec![json!(["globalAlpha", 1.0]), json!(["lineWidth", 1.0])],
                stick(296.5, "#00AA00", y(60.0), y(30.0)),
                stick(297.5, "#AA0000", y(70.0), y(40.0)),
                stick(297.5, "#00AA00", y(55.0), y(35.0)),
                pane_close(),
            ])
        );
    }

    // ── indicator series ──

    fn with_model(scene: &mut SceneGraph, m: crate::model::IndicatorModel) {
        let id = m.id.clone();
        scene.indicators.insert(id.clone(), m);
        scene.assign_indicator_z(&id);
    }

    /// The three bars with base painting off, so only the indicator paints.
    fn series_scene(series: Vec<SeriesSpec>) -> (SceneGraph, CoordinateSystem) {
        let mut scene = three_bars();
        scene.base_painting = BasePainting::None;
        let mut m = model("ind", "price");
        m.series = series;
        with_model(&mut scene, m);
        let coords = three_coords(&scene);
        (scene, coords)
    }

    #[test]
    fn an_indicator_line_strokes_one_segment_per_pair_and_breaks_at_gaps() {
        let (scene, coords) = series_scene(vec![line_like(
            "l",
            LineLikeKind::Line,
            &[Some(20.0), Some(30.0), None],
            "#ff0000",
            2.0,
        )]);
        assert_eq!(
            paint(&scene, &coords).to_json(),
            stream(vec![
                frame_start(1.0, 300.0, 100.0),
                pane_open(),
                vec![
                    json!(["globalAlpha", 1.0]), // the model's slot
                    json!(["lineWidth", 2.0]),
                    json!(["lineJoin", "round"]),
                    json!(["setLineDash", []]),
                    json!(["strokeStyle", "#ff0000"]),
                    json!(["beginPath"]),
                    json!(["moveTo", 236.0, 80.0]),
                    json!(["lineTo", 244.0, 70.0]),
                    json!(["stroke"]),
                    json!(["setLineDash", []]),
                    json!(["globalAlpha", 1.0]), // the candle layer's baseline alpha
                ],
                pane_close(),
            ])
        );
    }

    #[test]
    fn a_series_takes_its_per_point_color_width_and_dash() {
        let mut spec = line_like(
            "l",
            LineLikeKind::Line,
            &[Some(20.0), Some(30.0), Some(40.0)],
            "#ff0000",
            0.5,
        );
        if let SeriesData::LineLike { points, style, .. } = &mut spec.data {
            points[2].color = Some("#00ff00".into());
            style.line_style = LineStyle::Dashed;
        }
        let (scene, coords) = series_scene(vec![spec]);
        let canvas = paint(&scene, &coords);
        // Width floors at 1; a segment takes the color of the point it enters; the dash
        // is set first and reset after the series.
        assert_eq!(canvas.calls("lineWidth"), vec![json!(["lineWidth", 1.0])]);
        assert_eq!(
            canvas.calls("setLineDash"),
            vec![
                json!(["setLineDash", [6.0, 4.0]]),
                json!(["setLineDash", []])
            ]
        );
        assert_eq!(
            canvas.calls("strokeStyle"),
            vec![
                json!(["strokeStyle", "#ff0000"]),
                json!(["strokeStyle", "#00ff00"])
            ]
        );
        // The second segment continues from the first one's end.
        assert_eq!(
            canvas.calls("moveTo"),
            vec![
                json!(["moveTo", 236.0, 80.0]),
                json!(["moveTo", 244.0, 70.0])
            ]
        );
    }

    #[test]
    fn a_step_series_goes_flat_then_vertical() {
        let (scene, coords) = series_scene(vec![line_like(
            "s",
            LineLikeKind::Step,
            &[Some(20.0), Some(30.0), Some(30.0)],
            "#ff0000",
            1.0,
        )]);
        let canvas = paint(&scene, &coords);
        assert_eq!(
            canvas.calls("lineTo"),
            vec![
                json!(["lineTo", 244.0, 80.0]),
                json!(["lineTo", 244.0, 70.0]),
                json!(["lineTo", 252.0, 70.0]),
                json!(["lineTo", 252.0, 70.0]),
            ]
        );
    }

    #[test]
    fn histogram_and_columns_fill_from_the_base_with_a_one_pixel_minimum() {
        for (kind, w) in [
            (LineLikeKind::Histogram, 0.8 * 8.0),
            (LineLikeKind::Columns, 0.7 * 8.0),
        ] {
            let (scene, coords) = series_scene(vec![line_like(
                "h",
                kind,
                &[Some(20.0), Some(0.0), None],
                "#ff0000",
                1.0,
            )]);
            let canvas = paint(&scene, &coords);
            assert_eq!(
                canvas.calls("fillRect"),
                vec![
                    json!(["fillRect", 236.0 - w / 2.0, 80.0, w, 20.0]),
                    // value == base: a one-pixel bar
                    json!(["fillRect", 244.0 - w / 2.0, 100.0, w, 1.0]),
                ]
            );
        }
    }

    #[test]
    fn circles_and_crosses_mark_each_point() {
        let (scene, coords) = series_scene(vec![line_like(
            "c",
            LineLikeKind::Circles,
            &[Some(20.0), None, Some(40.0)],
            "#ff0000",
            2.0,
        )]);
        let canvas = paint(&scene, &coords);
        let tau = std::f64::consts::PI * 2.0;
        assert_eq!(
            canvas.calls("arc"),
            vec![
                json!(["arc", 236.0, 80.0, 2.0, 0.0, tau, false]),
                json!(["arc", 252.0, 60.0, 2.0, 0.0, tau, false]),
            ]
        );
        let (scene, coords) = series_scene(vec![line_like(
            "x",
            LineLikeKind::Cross,
            &[Some(20.0)],
            "#ff0000",
            1.0, // radius floors at 1.5
        )]);
        let canvas = paint(&scene, &coords);
        assert_eq!(
            canvas.calls("moveTo"),
            vec![
                json!(["moveTo", 234.5, 80.0]),
                json!(["moveTo", 236.0, 78.5])
            ]
        );
        assert_eq!(
            canvas.calls("lineTo"),
            vec![
                json!(["lineTo", 237.5, 80.0]),
                json!(["lineTo", 236.0, 81.5])
            ]
        );
    }

    #[test]
    fn an_area_series_fills_each_run_to_the_pane_floor_then_strokes_its_line() {
        let (scene, coords) = series_scene(vec![line_like(
            "a",
            LineLikeKind::Area,
            &[Some(20.0), Some(30.0), None],
            "#ff0000",
            2.0,
        )]);
        let canvas = paint(&scene, &coords);
        assert_eq!(
            canvas.calls("fillStyle"),
            vec![
                json!(["fillStyle", { "linearGradient": [0.0, 70.0, 0.0, 100.0], "stops": [[0.0, "#ff0000"], [1.0, "rgba(0,0,0,0)"]] }])
            ]
        );
        let names = canvas.names();
        let fill_at = names.iter().position(|n| n == "fill").unwrap();
        let stroke_at = names.iter().position(|n| n == "stroke").unwrap();
        assert!(fill_at < stroke_at, "the fill sits under the line");
    }

    #[test]
    fn a_series_hidden_from_the_pane_and_a_markers_series_paint_nothing() {
        let mut hidden = line_like(
            "l",
            LineLikeKind::Line,
            &[Some(1.0), Some(2.0)],
            "#f00",
            1.0,
        );
        hidden.display = Some(SeriesDisplay {
            pane: Some(false),
            ..Default::default()
        });
        let markers = SeriesSpec {
            id: "mk".into(),
            title: "mk".into(),
            display_title: None,
            pane_id: "price".into(),
            z_order: None,
            visible: None,
            display: None,
            overlay: false,
            data: SeriesData::Markers { markers: vec![] },
        };
        let (scene, coords) = series_scene(vec![hidden, markers]);
        assert!(paint_ops(&paint(&scene, &coords)).is_empty());
    }

    #[test]
    fn plot_candles_and_plot_bars_take_series_colors_and_the_per_bar_override() {
        let spec = |kind: CandleKind| SeriesSpec {
            id: "c".into(),
            title: "c".into(),
            display_title: None,
            pane_id: "price".into(),
            z_order: None,
            visible: None,
            display: None,
            overlay: false,
            data: SeriesData::Candle {
                kind,
                bars: bars(&[(40.0, 60.0, 30.0, 50.0), (50.0, 70.0, 40.0, 45.0)]),
                style: Some(SeriesCandleStyle {
                    up: Some("#u".into()),
                    down: Some("#d".into()),
                    ..Default::default()
                }),
                bar_colors: Some(vec![
                    Some(CandleBarColor {
                        color: Some("#body".into()),
                        wick_color: Some("#wick".into()),
                        border_color: Some("#border".into()),
                    }),
                    None,
                ]),
            },
        };
        let (scene, coords) = series_scene(vec![spec(CandleKind::Candle)]);
        let canvas = paint(&scene, &coords);
        // half-width floor(8 * 0.7) / 2 = 2.5, wick width 1.5; bar 0 is overridden (wick,
        // body, border), bar 1 falls back to the series down color.
        let series_ops: Vec<Value> = canvas
            .ops()
            .into_iter()
            .skip(frame_start(1.0, 300.0, 100.0).len() + pane_open().len() + 1)
            .collect();
        assert_eq!(
            series_ops[..16],
            [
                json!(["strokeStyle", "#wick"]),
                json!(["lineWidth", 1.5]),
                json!(["beginPath"]),
                json!(["moveTo", 236.5, 40.0]),
                json!(["lineTo", 236.5, 70.0]),
                json!(["stroke"]),
                json!(["fillStyle", "#body"]),
                json!(["fillRect", 234.0, 50.0, 5.0, 10.0]),
                json!(["strokeStyle", "#border"]),
                json!(["strokeRect", 234.0, 50.0, 5.0, 10.0]),
                json!(["strokeStyle", "#d"]),
                json!(["lineWidth", 1.5]),
                json!(["beginPath"]),
                json!(["moveTo", 244.5, y(70.0)]),
                json!(["lineTo", 244.5, 60.0]),
                json!(["stroke"]),
            ]
        );
        let (scene, coords) = series_scene(vec![spec(CandleKind::Bar)]);
        let canvas = paint(&scene, &coords);
        assert_eq!(canvas.calls("fillRect").len(), 0);
        assert_eq!(
            canvas.calls("strokeStyle"),
            vec![
                json!(["strokeStyle", "#body"]),
                json!(["strokeStyle", "#d"])
            ]
        );
        assert_eq!(canvas.calls("lineTo").len(), 6); // stick + two ticks per bar
    }

    // ── fills, backgrounds, price lines, z-order ──

    fn off_pane(mut spec: SeriesSpec) -> SeriesSpec {
        spec.display = Some(SeriesDisplay {
            pane: Some(false),
            ..Default::default()
        });
        spec
    }

    fn fill_model(fill: crate::model::Fill) -> crate::model::IndicatorModel {
        let mut m = model("ind", "price");
        m.series = vec![
            off_pane(line_like(
                "a",
                LineLikeKind::Line,
                &[Some(60.0), Some(60.0), Some(60.0)],
                "#f00",
                1.0,
            )),
            off_pane(line_like(
                "b",
                LineLikeKind::Line,
                &[Some(40.0), Some(40.0), Some(40.0)],
                "#0f0",
                1.0,
            )),
        ];
        m.fills = vec![fill];
        m
    }

    fn base_fill() -> crate::model::Fill {
        crate::model::Fill {
            id: "f".into(),
            pane_id: "price".into(),
            from_series_id: "a".into(),
            to_series_id: "b".into(),
            color: Some("#cccccc40".into()),
            colors: None,
            gradient: None,
            overlay: false,
        }
    }

    #[test]
    fn a_flat_fill_paints_a_quad_per_bar_span_between_its_two_series() {
        let mut scene = three_bars();
        scene.base_painting = BasePainting::None;
        with_model(&mut scene, fill_model(base_fill()));
        let coords = three_coords(&scene);
        let quad = |x0: f64, x1: f64| {
            vec![
                json!(["fillStyle", "#cccccc40"]),
                json!(["beginPath"]),
                json!(["moveTo", x0, y(60.0)]),
                json!(["lineTo", x1, y(60.0)]),
                json!(["lineTo", x1, y(40.0)]),
                json!(["lineTo", x0, y(40.0)]),
                json!(["closePath"]),
                json!(["fill"]),
            ]
        };
        assert_eq!(
            paint(&scene, &coords).to_json(),
            stream(vec![
                frame_start(1.0, 300.0, 100.0),
                pane_open(),
                vec![json!(["globalAlpha", 1.0])],
                quad(236.0, 244.0),
                quad(244.0, 252.0),
                vec![json!(["globalAlpha", 1.0])], // the candle layer's baseline alpha
                pane_close(),
            ])
        );
    }

    #[test]
    fn per_bar_fill_colors_apply_to_the_span_head_and_a_missing_color_skips_the_span() {
        let mut fill = base_fill();
        fill.color = None;
        fill.colors = Some(vec![Some("#111".into()), None, Some("#333".into())]);
        let mut scene = three_bars();
        scene.base_painting = BasePainting::None;
        with_model(&mut scene, fill_model(fill));
        let coords = three_coords(&scene);
        let canvas = paint(&scene, &coords);
        // Span [0, 1] is styled by column 0, span [1, 2] by column 1 (no color): skipped.
        assert_eq!(
            canvas.calls("fillStyle"),
            vec![json!(["fillStyle", "#111"])]
        );
        assert_eq!(canvas.calls("fill").len(), 1);
    }

    #[test]
    fn gradient_fills_reuse_one_gradient_across_identical_spans() {
        let stop = |top: f64, bottom: f64| {
            Some(FillGradientStop {
                top_value: top,
                bottom_value: bottom,
                top_color: "#top".into(),
                bottom_color: "#bottom".into(),
            })
        };
        let mut fill = base_fill();
        fill.color = None;
        fill.gradient = Some(vec![stop(60.0, 40.0), stop(60.0, 40.0), None]);
        let mut scene = three_bars();
        scene.base_painting = BasePainting::None;
        with_model(&mut scene, fill_model(fill));
        let coords = three_coords(&scene);
        let canvas = paint(&scene, &coords);
        let g = json!(["fillStyle", { "linearGradient": [0.0, 40.0, 0.0, 60.0], "stops": [[0.0, "#top"], [1.0, "#bottom"]] }]);
        assert_eq!(canvas.calls("fillStyle"), vec![g.clone(), g]);

        // A gradient whose two ends coincide opens one pixel.
        let mut flat = base_fill();
        flat.color = None;
        flat.gradient = Some(vec![stop(60.0, 60.0), None, None]);
        let mut scene = three_bars();
        scene.base_painting = BasePainting::None;
        with_model(&mut scene, fill_model(flat));
        let canvas = paint(&scene, &coords);
        assert_eq!(
            canvas.calls("fillStyle")[0],
            json!(["fillStyle", { "linearGradient": [0.0, 40.0, 0.0, 41.0], "stops": [[0.0, "#top"], [1.0, "#bottom"]] }])
        );
    }

    #[test]
    fn a_fill_without_both_series_paints_nothing() {
        let mut fill = base_fill();
        fill.to_series_id = "missing".into();
        let mut scene = three_bars();
        scene.base_painting = BasePainting::None;
        with_model(&mut scene, fill_model(fill));
        let coords = three_coords(&scene);
        assert!(paint(&scene, &coords).calls("fill").is_empty());
    }

    #[test]
    fn model_data_is_read_from_its_anchor_bar() {
        // The model ran over a suffix: its first point belongs to chart bar 1.
        let mut scene = three_bars();
        scene.base_painting = BasePainting::None;
        let mut m = model("ind", "price");
        m.series = vec![line_like(
            "l",
            LineLikeKind::Line,
            &[Some(20.0), Some(30.0)],
            "#f00",
            1.0,
        )];
        with_model(&mut scene, m);
        scene.set_anchor_offset("ind", 1);
        let coords = three_coords(&scene);
        assert_eq!(
            paint(&scene, &coords).calls("moveTo"),
            vec![json!(["moveTo", 244.0, 80.0])]
        );
    }

    #[test]
    fn backgrounds_span_their_time_range_and_force_overlay_ones_paint_on_the_price_pane() {
        let mut scene = three_bars();
        scene.base_painting = BasePainting::None;
        let study = scene.ensure_pane("study", PaneKind::Study, 1, Some(1.0));
        study.bounds.top = 100.0;
        study.bounds.height = 50.0;
        let bg = |id: &str, overlay: bool, color: &str| crate::model::Background {
            id: id.into(),
            pane_id: "price".into(),
            from: 0,
            to: 60_000,
            color: color.into(),
            overlay,
        };
        let mut own = model("own", "price");
        own.backgrounds = vec![bg("b1", false, "#own")];
        with_model(&mut scene, own);
        let mut forced = model("forced", "study");
        forced.backgrounds = vec![bg("b2", true, "#forced"), bg("b3", false, "#study")];
        with_model(&mut scene, forced);
        let coords = coords_for(&scene, 300.0, 150.0, 1.0, 8.0, 6.0);
        let canvas = paint(&scene, &coords);
        // Price pane: its own span, then the forced one; the study pane: its own span only.
        assert_eq!(
            canvas.calls("fillRect"),
            vec![
                json!(["fillRect", 236.0, 0.0, 8.0, 100.0]),
                json!(["fillRect", 236.0, 0.0, 8.0, 100.0]),
                json!(["fillRect", 236.0, 100.0, 8.0, 50.0]),
            ]
        );
        assert_eq!(
            canvas.calls("fillStyle"),
            vec![
                json!(["fillStyle", "#own"]),
                json!(["fillStyle", "#forced"]),
                json!(["fillStyle", "#study"])
            ]
        );
    }

    #[test]
    fn a_background_outside_the_data_width_or_with_no_extent_is_skipped() {
        let mut scene = three_bars();
        scene.base_painting = BasePainting::None;
        let mut m = model("m", "price");
        let bg = |from: i64, to: i64| crate::model::Background {
            id: "b".into(),
            pane_id: "price".into(),
            from,
            to,
            color: "#bg".into(),
            overlay: false,
        };
        m.backgrounds = vec![
            bg(60_000, 60_000),
            bg(-10_000_000, -9_000_000),
            bg(100_000_000, 200_000_000),
        ];
        with_model(&mut scene, m);
        let coords = three_coords(&scene);
        assert!(paint(&scene, &coords).calls("fillRect").is_empty());
    }

    #[test]
    fn price_lines_paint_last_with_their_style_and_skip_what_falls_outside_the_pane() {
        let mut scene = three_bars();
        scene.base_painting = BasePainting::None;
        let mut m = model("m", "price");
        let line =
            |price: f64, color: Option<&str>, style: Option<LineStyle>, width: Option<f64>| {
                PriceLine {
                    id: "h".into(),
                    pane_id: "price".into(),
                    price,
                    color: color.map(str::to_string),
                    line_style: style,
                    width,
                    title: None,
                    display_title: None,
                }
            };
        m.price_lines = vec![
            line(50.0, None, Some(LineStyle::Dashed), None),
            line(500.0, None, None, None), // above the window
            line(25.2, Some("#abc"), Some(LineStyle::Dotted), Some(3.0)),
        ];
        with_model(&mut scene, m);
        let coords = three_coords(&scene);
        assert_eq!(
            paint(&scene, &coords).to_json(),
            stream(vec![
                frame_start(1.0, 300.0, 100.0),
                pane_open(),
                vec![
                    json!(["globalAlpha", 1.0]), // the model's slot
                    json!(["globalAlpha", 1.0]), // the candle layer's baseline alpha
                    json!(["globalAlpha", 1.0]), // models on the price pane, after the candles
                    json!(["globalAlpha", 1.0]), // the price lines
                    json!(["strokeStyle", "#cccccc"]),
                    json!(["lineWidth", 1.0]),
                    json!(["setLineDash", [6.0, 4.0]]),
                    json!(["beginPath"]),
                    json!(["moveTo", 0.0, 50.5]),
                    json!(["lineTo", 300.0, 50.5]),
                    json!(["stroke"]),
                    json!(["setLineDash", []]),
                    json!(["strokeStyle", "#abc"]),
                    json!(["lineWidth", 3.0]),
                    json!(["setLineDash", [2.0, 3.0]]),
                    json!(["beginPath"]),
                    json!(["moveTo", 0.0, 75.5]),
                    json!(["lineTo", 300.0, 75.5]),
                    json!(["stroke"]),
                    json!(["setLineDash", []]),
                    json!(["restore"]),
                ],
            ])
        );
    }

    #[test]
    fn indicators_below_the_candle_z_paint_behind_them_and_above_it_in_front() {
        let line = |color: &str| {
            line_like(
                "l",
                LineLikeKind::Line,
                &[Some(20.0), Some(30.0)],
                color,
                1.0,
            )
        };
        let mut scene = three_bars();
        let mut back = model("back", "price");
        back.series = vec![line("#back")];
        with_model(&mut scene, back); // mounts at z = -1: behind the candles
        let mut front = model("front", "price");
        front.series = vec![line("#front")];
        with_model(&mut scene, front);
        scene.set_indicator_z("front", 5.0);
        let coords = three_coords(&scene);
        let strokes: Vec<String> = paint_ops(&paint(&scene, &coords))
            .into_iter()
            .filter(|(op, _)| op == "stroke")
            .map(|(_, s)| s)
            .collect();
        // back line, then the three candle wicks, then the front line.
        assert_eq!(
            strokes,
            vec!["#back", "#00AA00", "#AA0000", "#00AA00", "#front"]
        );
    }

    #[test]
    fn force_overlay_content_paints_on_the_price_pane_and_stack_top_slices_come_last() {
        let mut scene = three_bars();
        scene.base_painting = BasePainting::None;
        let study = scene.ensure_pane("study", PaneKind::Study, 1, Some(1.0));
        study.bounds.top = 100.0;
        study.bounds.height = 50.0;
        study.scale.min = 0.0;
        study.scale.max = 100.0;
        let mut m = model("m", "study");
        let mut forced = line_like(
            "f",
            LineLikeKind::Line,
            &[Some(20.0), Some(30.0)],
            "#forced",
            1.0,
        );
        forced.overlay = true;
        m.series = vec![
            forced,
            line_like(
                "own",
                LineLikeKind::Line,
                &[Some(20.0), Some(30.0)],
                "#own",
                1.0,
            ),
        ];
        with_model(&mut scene, m);
        scene.drawing_slices.insert(
            "price".into(),
            vec![DrawingSlice {
                before_z: f64::INFINITY,
                canvas: Rc::new(()) as CanvasImage,
            }],
        );
        let coords = coords_for(&scene, 300.0, 150.0, 1.0, 8.0, 6.0);
        let canvas = paint(&scene, &coords);
        let strokes: Vec<String> = paint_ops(&canvas)
            .into_iter()
            .map(|(_, style)| style)
            .collect();
        // The price pane paints first: the forced series, then (stack top) the slice; the
        // study pane then paints its own series.
        assert_eq!(strokes, vec!["#forced".to_string(), "#own".to_string()]);
        let names = canvas.names();
        let image = names.iter().position(|n| n == "drawImage").unwrap();
        let forced_stroke = names.iter().position(|n| n == "stroke").unwrap();
        let own_stroke = names.iter().rposition(|n| n == "stroke").unwrap();
        assert!(forced_stroke < image && image < own_stroke);
        assert_eq!(
            canvas.calls("drawImage"),
            vec![json!(["drawImage", 0.0, 0.0, 300.0, 150.0])]
        );
    }

    #[test]
    fn slices_below_the_candle_z_composite_before_the_candles_at_full_alpha() {
        let mut scene = three_bars();
        scene.drawing_slices.insert(
            "price".into(),
            vec![DrawingSlice {
                before_z: -0.5,
                canvas: Rc::new(()) as CanvasImage,
            }],
        );
        let coords = three_coords(&scene);
        let canvas = paint(&scene, &coords);
        let names = canvas.names();
        let image = names.iter().position(|n| n == "drawImage").unwrap();
        let first_candle = names.iter().position(|n| n == "fillRect").unwrap();
        assert!(image < first_candle);
        assert_eq!(canvas.to_json()[image - 1], json!(["globalAlpha", 1.0]));
    }

    #[test]
    fn the_model_alpha_scales_indicators_and_leaves_candles_opaque() {
        let mut scene = three_bars();
        let mut m = model("m", "price");
        m.series = vec![line_like(
            "l",
            LineLikeKind::Line,
            &[Some(20.0), Some(30.0)],
            "#f00",
            1.0,
        )];
        with_model(&mut scene, m);
        let coords = three_coords(&scene);
        let canvas = recording();
        let mut backend = Canvas2dBackend::new();
        backend.mount(canvas.clone(), 300.0, 100.0);
        backend.set_model_alpha(0.3);
        backend.render(&scene, &coords, &theme());
        let alphas: Vec<f64> = canvas
            .calls("globalAlpha")
            .iter()
            .map(|op| op[1].as_f64().unwrap())
            .collect();
        // backgrounds, the model's slot, the candle structure baseline (opaque), the
        // candles, then the model alpha again on top for the price lines.
        assert_eq!(alphas[0], 0.3);
        assert_eq!(alphas[1], 0.3);
        assert_eq!(alphas[2], 1.0);
        assert_eq!(alphas[alphas.len() - 2], 0.3);
        assert_eq!(alphas[alphas.len() - 1], 0.3);
    }

    // ── helpers ──

    #[test]
    fn at_reads_like_a_javascript_index() {
        let items = [1, 2, 3];
        assert_eq!(at(&items, 0.0), Some(&1));
        assert_eq!(at(&items, 2.0), Some(&3));
        assert_eq!(at(&items, 3.0), None);
        assert_eq!(at(&items, -1.0), None);
        assert_eq!(at(&items, 1.5), None);
        assert_eq!(at(&items, f64::NAN), None);
    }

    #[test]
    fn theme_strings_default_to_empty() {
        let t = theme();
        assert_eq!(theme_str(&t, "upColor"), "#00AA00");
        assert_eq!(theme_str(&t, "missing"), "");
        assert_eq!(theme_str(&json!({ "upColor": 3 }), "upColor"), "");
    }
}
