//! Renderer-owned crosshair layer (chrome). Lives on its OWN transparent canvas stacked
//! above the data canvas so a pointer move repaints ONLY the crosshair: the data layer
//! (series, fills, drawings, axes, grid) is left untouched. This is the scheduler's
//! "Cursor" tier: hovering no longer clears, re-autoscales and redraws the whole scene
//! every frame.
//!
//! It is always canvas2d and independent of the data backend (canvas2d now, the WebGL2
//! backend later): the GPU path never touches the crosshair.
//!
//! Left for the browser crate: the original's `mount(canvas)` reads
//! `canvas.getContext('2d')` and the canvas' backing-store size; here
//! [`CrosshairRenderer::mount`] takes the context and that size as values
//! ([`PlotSurface`]).

#![allow(clippy::too_many_arguments)]

use std::rc::Rc;

use crate::format::{FormatSurface, ResolvedFormatters, english_format};
use crate::indicators::classics::math::{js_max, js_round};
use crate::ports::chart_renderer::VelaTheme;
use crate::render::backend::canvas2d_backend::{PlotSurface, set_line_dash, theme_str};
use crate::render::backend::gl::color::readable_text;
use crate::render::canvas::{Canvas2d, TextAlign, TextBaseline};
use crate::render::core::chart_config::ShadeRight;
use crate::render::core::plot_direction::begin_plot_frame;
use crate::render::core::scene_graph::{PaneAxisFormat, percent_scale_for};
use crate::render::core::{CoordinateSystem, PaneNode, SceneGraph};

use super::chrome_renderer::font_spec;
use super::ticks::{AxisScale, format_axis_value, format_time_stamp};

/// A SYNCED ghost crosshair (another chart's pointer), pixel-resolved by the renderer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExternalCrosshair {
    pub x: f64,
    pub y: Option<f64>,
    pub time: f64,
    pub price: Option<f64>,
    /// `Some(false)` draws no ghost line (only the veil, if any).
    pub line: Option<bool>,
}

/// The plot area a frame veiled for `crosshairOverride.shadeRight`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadeArea {
    pub x: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Default)]
pub struct CrosshairRenderer {
    surface: Option<PlotSurface>,
    shade: Option<ShadeArea>,
}

fn formatters(scene: &SceneGraph) -> std::borrow::Cow<'_, ResolvedFormatters> {
    match &scene.format {
        Some(f) => std::borrow::Cow::Borrowed(f),
        None => std::borrow::Cow::Owned(english_format()),
    }
}

impl CrosshairRenderer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind to the cursor canvas: its 2d context and backing-store size (device pixels).
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

    /// The plot area the last frame veiled for `crosshairOverride.shadeRight`: from the
    /// picked bar's right edge to the price scale, full plot height (the union when both
    /// the local and a synced crosshair veil), or `None` when no veil was painted.
    pub fn shade_right_area(&self) -> Option<ShadeArea> {
        self.shade
    }

    /// Clear the cursor canvas and (re)draw the crosshair lines and axis chips. The
    /// optional `separator_hover_y` highlights the draggable pane separator under the
    /// cursor; `external` is a SYNCED ghost crosshair.
    pub fn render(
        &mut self,
        scene: &SceneGraph,
        coords: &CoordinateSystem,
        theme: &VelaTheme,
        separator_hover_y: Option<f64>,
        external: Option<&ExternalCrosshair>,
    ) {
        self.shade = None;
        let Some(surface) = self.surface.clone() else {
            return;
        };
        let ctx: &dyn Canvas2d = &*surface.ctx;

        let dpr = coords.dpr();
        begin_plot_frame(ctx, dpr);
        ctx.clear_rect(0.0, 0.0, surface.width / dpr, surface.height / dpr);

        // Pane-separator hover affordance: drawn before the crosshair-presence guard so it
        // shows even while a resize drag has the crosshair sitting right on the boundary.
        if let Some(y) = separator_hover_y {
            self.draw_separator_hover(ctx, surface.width / dpr, theme, y);
        }

        // External (synced) ghost: drawn dimmer and FIRST, so a real local pointer paints
        // over it, and drawn regardless of the local-crosshair presence guard.
        if let Some(external) = external {
            self.draw_external(ctx, external, scene, coords, theme);
        }

        let Some(ch) = scene.crosshair else { return };
        let data_w = coords.width();
        let data_h = coords.height();
        if ch.x < 0.0 || ch.x > data_w || ch.y < 0.0 || ch.y > data_h {
            return;
        }

        let cs = &scene.style.crosshair;
        let ov = scene.crosshair_override.as_ref();
        let vertical = ov.and_then(|o| o.vertical) != Some(false);
        let horizontal = ov.and_then(|o| o.horizontal) != Some(false);
        ctx.set_font(&font_spec(
            scene.style.font_size,
            theme_str(theme, "fontFamily"),
        ));
        ctx.set_text_baseline(TextBaseline::Middle);

        // snap the vertical line to the nearest bar center
        let logical = js_round(coords.x_to_logical(ch.x));
        let line_width = ov.and_then(|o| o.width).unwrap_or(cs.width);
        let x = crisp(coords.logical_to_x(logical), line_width);
        if vertical && let Some(shade) = ov.and_then(|o| o.shade_right.as_ref()) {
            // From the bar's right edge: the bar under the line stays in the clear.
            let from = js_max(0.0, js_round(coords.logical_to_x(logical + 0.5)));
            if from < data_w {
                self.paint_shade(ctx, shade, from, data_w, data_h);
            }
        }
        let color = ov
            .and_then(|o| o.color.as_deref())
            .or(cs.color.as_deref())
            .unwrap_or_else(|| theme_str(theme, "textColor"));
        ctx.set_stroke_style(&color.into());
        ctx.set_line_width(line_width);
        ctx.set_global_alpha(ov.and_then(|o| o.opacity).unwrap_or(cs.opacity));
        set_line_dash(ctx, ov.and_then(|o| o.style).unwrap_or(cs.style));
        ctx.begin_path();
        if vertical {
            ctx.move_to(x, 0.0);
            ctx.line_to(x, data_h);
        }
        if horizontal {
            ctx.move_to(0.0, js_round(ch.y) + 0.5);
            ctx.line_to(data_w, js_round(ch.y) + 0.5);
        }
        ctx.stroke();
        ctx.set_line_dash(&[]);
        ctx.set_line_width(1.0);
        ctx.set_global_alpha(1.0);

        // price chip on the right axis for the pane under the cursor. Linear scan over the
        // pane map (no sort/alloc): panes don't overlap in Y, and this runs on the hot
        // Cursor-tier path on every pointer move.
        let pane = pane_at(scene, ch.y);
        let chip_bg = cs
            .label_background
            .as_deref()
            .unwrap_or_else(|| theme_str(theme, "borderColor"));
        let background = theme_str(theme, "background");
        let fmt = formatters(scene);
        // An unscaled pane (axis format 'none') has no value axis, so no value chip either.
        if horizontal
            && let Some(pane) = pane
            && pane.axis_format != Some(PaneAxisFormat::NoAxis)
        {
            let price = coords.y_to_price(ch.y, &pane.scale, &pane.bounds);
            let text = format_axis_value(
                &AxisScale::from(&pane.scale),
                pane.bounds.height,
                price,
                percent_scale_for(scene, pane),
                scene.price_mintick,
                pane.axis_format,
                &fmt,
                FormatSurface::Crosshair,
            );
            self.chip(
                ctx,
                data_w + 1.0,
                ch.y,
                &text,
                chip_bg,
                ChipAlign::Left,
                false,
                background,
            );
        }
        // time chip on the bottom axis
        if vertical {
            let text = format_time_stamp(
                coords.logical_to_time(logical),
                &scene.timezone,
                coords.bar_interval(),
                &fmt,
                FormatSurface::Crosshair,
            );
            self.chip(
                ctx,
                x,
                data_h + 1.0,
                &text,
                chip_bg,
                ChipAlign::Center,
                true,
                background,
            );
        }
    }

    pub fn destroy(&mut self) {
        self.surface = None;
        self.shade = None;
    }

    /// The synced ghost: a dimmed vertical line at the bar the renderer resolved as
    /// CONTAINING the foreign time (+ horizontal line when a comparable price came along),
    /// with that bar's time chip in this chart's own timezone and, when the level
    /// resolved, the price chip on the right axis. The snap happened upstream
    /// (`externalCrossPx`, floor-to-containing-bar): this method only draws. Chips render
    /// slightly dimmed so the ghost still reads as foreign. A `crosshairOverride` applies
    /// to the ghost as well (its line style, a dropped horizontal line, and the
    /// `shadeRight` veil from the ghost bar's right edge), so a pick that spans several
    /// charts reads the same on every one of them.
    fn draw_external(
        &mut self,
        ctx: &dyn Canvas2d,
        ext: &ExternalCrosshair,
        scene: &SceneGraph,
        coords: &CoordinateSystem,
        theme: &VelaTheme,
    ) {
        let cs = &scene.style.crosshair;
        let ov = scene.crosshair_override.as_ref();
        let data_w = coords.width();
        let data_h = coords.height();
        let line_width = ov.and_then(|o| o.width).unwrap_or(cs.width);
        let x = crisp(ext.x, line_width);
        if ov.and_then(|o| o.vertical) == Some(false) {
            return;
        }
        if let Some(shade) = ov.and_then(|o| o.shade_right.as_ref()) {
            // Even with the ghost off the window: left of it, every bar in view is veiled.
            let from = js_max(
                0.0,
                js_round(coords.logical_to_x(js_round(coords.x_to_logical(ext.x)) + 0.5)),
            );
            if from < data_w {
                self.paint_shade(ctx, shade, from, data_w, data_h);
            }
        }
        if ext.line == Some(false) || x < 0.0 || x > data_w {
            return;
        }
        ctx.set_font(&font_spec(
            scene.style.font_size,
            theme_str(theme, "fontFamily"),
        ));
        ctx.set_text_baseline(TextBaseline::Middle);
        let color = ov
            .and_then(|o| o.color.as_deref())
            .or(cs.color.as_deref())
            .unwrap_or_else(|| theme_str(theme, "textColor"));
        ctx.set_stroke_style(&color.into());
        ctx.set_line_width(line_width);
        ctx.set_global_alpha(ov.and_then(|o| o.opacity).unwrap_or(cs.opacity) * 0.55); // a ghost: dimmer than the local crosshair
        set_line_dash(ctx, ov.and_then(|o| o.style).unwrap_or(cs.style));
        let horizontal = ov.and_then(|o| o.horizontal) != Some(false);
        ctx.begin_path();
        ctx.move_to(x, 0.0);
        ctx.line_to(x, data_h);
        if let Some(y) = ext.y
            && horizontal
        {
            ctx.move_to(0.0, js_round(y) + 0.5);
            ctx.line_to(data_w, js_round(y) + 0.5);
        }
        ctx.stroke();
        ctx.set_line_dash(&[]);
        ctx.set_line_width(1.0);
        ctx.set_global_alpha(0.8); // chips stay readable but still read as foreign
        let chip_bg = cs
            .label_background
            .as_deref()
            .unwrap_or_else(|| theme_str(theme, "borderColor"));
        let background = theme_str(theme, "background");
        let fmt = formatters(scene);
        if let (Some(y), Some(price)) = (ext.y, ext.price)
            && horizontal
        {
            // Same chip the local crosshair puts on the axis, at the ghost's level,
            // formatted on the pane under the line (the price pane, by construction).
            if let Some(pane) = pane_at(scene, y)
                && pane.axis_format != Some(PaneAxisFormat::NoAxis)
            {
                let text = format_axis_value(
                    &AxisScale::from(&pane.scale),
                    pane.bounds.height,
                    price,
                    percent_scale_for(scene, pane),
                    scene.price_mintick,
                    pane.axis_format,
                    &fmt,
                    FormatSurface::Crosshair,
                );
                self.chip(
                    ctx,
                    data_w + 1.0,
                    y,
                    &text,
                    chip_bg,
                    ChipAlign::Left,
                    false,
                    background,
                );
            }
        }
        let text = format_time_stamp(
            ext.time,
            &scene.timezone,
            coords.bar_interval(),
            &fmt,
            FormatSurface::Crosshair,
        );
        self.chip(
            ctx,
            x,
            data_h + 1.0,
            &text,
            chip_bg,
            ChipAlign::Center,
            true,
            background,
        );
        ctx.set_global_alpha(1.0);
    }

    /// Paint the `shadeRight` veil from `from` to the price scale and record the veiled
    /// area.
    fn paint_shade(
        &mut self,
        ctx: &dyn Canvas2d,
        shade: &ShadeRight,
        from: f64,
        data_w: f64,
        data_h: f64,
    ) {
        ctx.set_fill_style(&(&shade.color).into());
        ctx.set_global_alpha(shade.opacity.unwrap_or(1.0));
        ctx.fill_rect(from, 0.0, data_w - from, data_h);
        ctx.set_global_alpha(1.0);
        let x = match self.shade {
            Some(existing) => existing.x.min(from),
            None => from,
        };
        self.shade = Some(ShadeArea {
            x,
            width: data_w - x,
            height: data_h,
        });
    }

    /// A soft band + a brighter crisp center line over the hovered separator, so it reads
    /// as a draggable handle (the cursor is already `row-resize`). Spans the full width
    /// (data + scale gutter) to match the separator itself. Theme-derived (textColor).
    fn draw_separator_hover(&self, ctx: &dyn Canvas2d, full_w: f64, theme: &VelaTheme, y: f64) {
        let yy = js_round(y);
        ctx.set_fill_style(&theme_str(theme, "textColor").into());
        ctx.set_global_alpha(0.1);
        ctx.fill_rect(0.0, yy - 4.0, full_w, 8.0);
        ctx.set_global_alpha(0.55);
        ctx.fill_rect(0.0, yy - 1.0, full_w, 2.0);
        ctx.set_global_alpha(1.0);
    }

    fn chip(
        &self,
        ctx: &dyn Canvas2d,
        x: f64,
        y: f64,
        text: &str,
        bg: &str,
        align: ChipAlign,
        below: bool,
        over: &str,
    ) {
        let w = ctx.measure_text(text).width + 8.0;
        let h = 16.0;
        let rx = match align {
            ChipAlign::Left => x,
            ChipAlign::Center => x - w / 2.0,
        };
        let ry = if below { y } else { y - h / 2.0 };
        ctx.set_fill_style(&bg.into());
        ctx.fill_rect(rx, ry, w, h);
        ctx.set_fill_style(&readable_text(bg, over).into()); // legible whatever the chip color is (light or dark)
        ctx.set_text_align(TextAlign::Center);
        ctx.fill_text(
            text,
            rx + w / 2.0,
            ry + h / 2.0 + if below { 2.0 } else { 0.0 },
        );
        ctx.set_text_align(TextAlign::Start);
    }
}

#[derive(Clone, Copy)]
enum ChipAlign {
    Left,
    Center,
}

/// The first pane (insertion order) whose pixel band holds `y`.
fn pane_at(scene: &SceneGraph, y: f64) -> Option<&PaneNode> {
    scene
        .panes
        .values()
        .find(|p| y >= p.bounds.top && y <= p.bounds.top + p.bounds.height)
}

/// A vertical line's x on the pixel grid: odd widths centre on a half pixel, even ones on
/// a pixel edge; either way the stroke fills whole pixels instead of blurring across two.
fn crisp(px: f64, width: f64) -> f64 {
    js_round(px)
        + if js_round(width) % 2.0 == 1.0 {
            0.5
        } else {
            0.0
        }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::model::LineStyle;
    use crate::model::PaneKind;
    use crate::palette::{CHIP_PLATE, CROSSHAIR};
    use crate::render::backend::canvas2d_backend::test_support::{
        frame_start, recording, stream, theme,
    };
    use crate::render::canvas::RecordingCanvas;
    use crate::render::core::ViewportState;
    use crate::render::core::chart_config::CrosshairOverride;
    use crate::render::core::scene_graph::CrosshairPoint;

    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 24 * HOUR;

    /// A 380 x 280 plot over 39 bars at pitch 10 with the newest bar on the right edge:
    /// `logical_to_x(l) = 10 l`, `x_to_logical(x) = x / 10`. Bar 10's time is `bar10`.
    fn coords(interval: i64, bar10: i64) -> CoordinateSystem {
        let mut c = CoordinateSystem::new();
        c.set_size(380.0, 280.0, 1.0);
        let times: Vec<i64> = (0..39).map(|i| bar10 + (i - 10) * interval).collect();
        c.set_bars(&times);
        c.set_viewport(ViewportState {
            bar_spacing: 10.0,
            right_offset: 0.0,
        });
        c
    }

    fn scene() -> SceneGraph {
        let mut s = SceneGraph::new();
        let pane = s.ensure_pane("price", PaneKind::Price, 0, Some(3.0));
        pane.bounds.top = 0.0;
        pane.bounds.height = 280.0;
        pane.scale.min = 0.0;
        pane.scale.max = 100.0;
        s.crosshair = Some(CrosshairPoint { x: 100.0, y: 120.0 });
        s
    }

    const BAR10: i64 = 1_700_000_000_000;

    fn mounted() -> (CrosshairRenderer, std::rc::Rc<RecordingCanvas>) {
        let canvas = recording();
        let mut cr = CrosshairRenderer::new();
        cr.mount(canvas.clone(), 400.0, 300.0);
        (cr, canvas)
    }

    fn paint(
        scene: &SceneGraph,
        external: Option<&ExternalCrosshair>,
    ) -> (CrosshairRenderer, std::rc::Rc<RecordingCanvas>) {
        let (mut cr, canvas) = mounted();
        cr.render(scene, &coords(HOUR, BAR10), &theme(), None, external);
        (cr, canvas)
    }

    /// What the original's recorder captures.
    struct Seen {
        segments: Vec<([f64; 2], [f64; 2])>,
        texts: Vec<String>,
        strokes: Vec<(String, Vec<f64>, f64)>,
        fills: Vec<(String, f64, [f64; 4])>,
    }

    fn seen(canvas: &RecordingCanvas) -> Seen {
        let mut out = Seen {
            segments: vec![],
            texts: vec![],
            strokes: vec![],
            fills: vec![],
        };
        let mut at = [0.0, 0.0];
        let mut dash: Vec<f64> = vec![];
        let (mut stroke_style, mut fill_style, mut alpha) = (String::new(), String::new(), 1.0);
        for op in canvas.ops() {
            let num = |i: usize| op[i].as_f64().unwrap();
            match op[0].as_str().unwrap() {
                "moveTo" => at = [num(1), num(2)],
                "lineTo" => out.segments.push((at, [num(1), num(2)])),
                "setLineDash" => {
                    dash = op[1]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_f64().unwrap())
                        .collect();
                }
                "strokeStyle" => stroke_style = op[1].as_str().unwrap().to_string(),
                "fillStyle" => fill_style = op[1].as_str().unwrap().to_string(),
                "globalAlpha" => alpha = num(1),
                "stroke" => out
                    .strokes
                    .push((stroke_style.clone(), dash.clone(), alpha)),
                "fillRect" => {
                    out.fills
                        .push((fill_style.clone(), alpha, [num(1), num(2), num(3), num(4)]))
                }
                "fillText" => out.texts.push(op[1].as_str().unwrap().to_string()),
                _ => {}
            }
        }
        out
    }

    fn horizontal(s: &([f64; 2], [f64; 2])) -> bool {
        s.0[1] == s.1[1]
    }

    fn vertical(s: &([f64; 2], [f64; 2])) -> bool {
        s.0[0] == s.1[0]
    }

    fn with_override(o: CrosshairOverride) -> SceneGraph {
        let mut s = scene();
        s.crosshair_override = Some(o);
        s
    }

    fn fmt_text(scene: &SceneGraph, price: f64) -> String {
        let pane = scene.panes.get("price").unwrap();
        format_axis_value(
            &AxisScale::from(&pane.scale),
            pane.bounds.height,
            price,
            None,
            None,
            None,
            &english_format(),
            FormatSurface::Crosshair,
        )
    }

    // ── the complete streams ──

    #[test]
    fn the_default_crosshair_paints_this_exact_stream() {
        let s = scene();
        let (_, canvas) = paint(&s, None);
        // Pitch 10, newest bar (38) on the right edge: x(10) = 100, a 1 px line on a pixel
        // middle. Dashed [6, 4] at opacity 0.4 in the configured color; the level line at
        // round(120) + 0.5. The price chip on the axis at the cursor's level, the time
        // chip under the plot at the snapped bar's time, in the chip plate with the ink
        // `readable_text` picks.
        let price = 100.0 * (1.0 - 120.0 / 280.0);
        let price_text = fmt_text(&s, price);
        let time_text = format_time_stamp(
            BAR10 as f64,
            "UTC",
            HOUR as f64,
            &english_format(),
            FormatSurface::Crosshair,
        );
        let ink = readable_text(CHIP_PLATE, "#000000");
        let (pw, tw) = (
            price_text.chars().count() as f64 * 6.6 + 8.0,
            time_text.chars().count() as f64 * 6.6 + 8.0,
        );
        let (rx_time, _) = (100.5 - tw / 2.0, 0.0);
        assert_eq!(
            canvas.to_json(),
            stream(vec![
                frame_start(1.0, 400.0, 300.0),
                vec![
                    json!(["font", "11px sans-serif"]),
                    json!(["textBaseline", "middle"]),
                    json!(["strokeStyle", CROSSHAIR]),
                    json!(["lineWidth", 1.0]),
                    json!(["globalAlpha", 0.4]),
                    json!(["setLineDash", [6.0, 4.0]]),
                    json!(["beginPath"]),
                    json!(["moveTo", 100.5, 0.0]),
                    json!(["lineTo", 100.5, 280.0]),
                    json!(["moveTo", 0.0, 120.5]),
                    json!(["lineTo", 380.0, 120.5]),
                    json!(["stroke"]),
                    json!(["setLineDash", []]),
                    json!(["lineWidth", 1.0]),
                    json!(["globalAlpha", 1.0]),
                    // price chip: left edge on the axis, centered on the level
                    json!(["fillStyle", CHIP_PLATE]),
                    json!(["fillRect", 381.0, 112.0, pw, 16.0]),
                    json!(["fillStyle", ink]),
                    json!(["textAlign", "center"]),
                    json!(["fillText", price_text, 381.0 + pw / 2.0, 112.0 + 8.0]),
                    json!(["textAlign", "start"]),
                    // time chip: centered on the line, hanging under the plot
                    json!(["fillStyle", CHIP_PLATE]),
                    json!(["fillRect", rx_time, 281.0, tw, 16.0]),
                    json!(["fillStyle", ink]),
                    json!(["textAlign", "center"]),
                    json!(["fillText", time_text, rx_time + tw / 2.0, 281.0 + 8.0 + 2.0]),
                    json!(["textAlign", "start"]),
                ],
            ])
        );
    }

    #[test]
    fn the_separator_hover_paints_a_soft_band_and_a_crisp_center_line_over_the_full_width() {
        let mut s = scene();
        s.crosshair = None;
        let (mut cr, canvas) = mounted();
        cr.render(&s, &coords(HOUR, BAR10), &theme(), Some(150.4), None);
        assert_eq!(
            canvas.to_json(),
            stream(vec![
                frame_start(1.0, 400.0, 300.0),
                vec![
                    json!(["fillStyle", "#cccccc"]),
                    json!(["globalAlpha", 0.1]),
                    json!(["fillRect", 0.0, 146.0, 400.0, 8.0]),
                    json!(["globalAlpha", 0.55]),
                    json!(["fillRect", 0.0, 149.0, 400.0, 2.0]),
                    json!(["globalAlpha", 1.0]),
                ],
            ])
        );
    }

    #[test]
    fn nothing_but_the_clear_paints_without_a_crosshair_or_outside_the_plot() {
        let mut s = scene();
        s.crosshair = None;
        assert_eq!(
            paint(&s, None).1.to_json(),
            Value::Array(frame_start(1.0, 400.0, 300.0))
        );
        for (x, y) in [(-1.0, 10.0), (381.0, 10.0), (10.0, -1.0), (10.0, 281.0)] {
            s.crosshair = Some(CrosshairPoint { x, y });
            assert_eq!(
                paint(&s, None).1.to_json(),
                Value::Array(frame_start(1.0, 400.0, 300.0))
            );
        }
        // The edges themselves are inside.
        s.crosshair = Some(CrosshairPoint { x: 380.0, y: 280.0 });
        assert!(paint(&s, None).1.calls("stroke").len() == 1);
    }

    #[test]
    fn an_unmounted_or_destroyed_crosshair_paints_nothing_and_a_resize_changes_the_clear() {
        let s = scene();
        let mut cr = CrosshairRenderer::new();
        cr.render(&s, &coords(HOUR, BAR10), &theme(), None, None);
        let (mut cr, canvas) = mounted();
        cr.set_backing_size(800.0, 600.0);
        cr.render(&s, &coords(HOUR, BAR10), &theme(), None, None);
        assert_eq!(
            canvas.to_json()[2],
            json!(["clearRect", 0.0, 0.0, 800.0, 600.0])
        );
        cr.destroy();
        canvas.clear();
        cr.render(&s, &coords(HOUR, BAR10), &theme(), None, None);
        assert!(canvas.is_empty());
    }

    // ── under an override (crosshair-override.test.ts) ──

    #[test]
    fn it_draws_both_lines_and_both_chips_by_default() {
        let rec = seen(&paint(&scene(), None).1);
        assert!(rec.segments.iter().any(vertical));
        assert!(rec.segments.iter().any(horizontal));
        assert_eq!(rec.texts.len(), 2); // price chip + time chip
        assert_eq!(rec.strokes[0].1, vec![6.0, 4.0]); // the configured dashed style
    }

    #[test]
    fn horizontal_false_drops_the_level_line_and_its_price_chip_and_the_style_fields_apply() {
        let s = with_override(CrosshairOverride {
            horizontal: Some(false),
            color: Some("#2962ff".into()),
            style: Some(LineStyle::Solid),
            opacity: Some(1.0),
            ..Default::default()
        });
        let rec = seen(&paint(&s, None).1);
        assert!(!rec.segments.iter().any(horizontal));
        assert!(rec.segments.iter().any(vertical));
        assert_eq!(rec.texts.len(), 1); // the time chip only
        assert_eq!(rec.strokes[0], ("#2962ff".to_string(), vec![], 1.0));
    }

    #[test]
    fn vertical_false_drops_the_vertical_line_and_the_time_chip() {
        let s = with_override(CrosshairOverride {
            vertical: Some(false),
            ..Default::default()
        });
        let rec = seen(&paint(&s, None).1);
        assert!(!rec.segments.iter().any(vertical));
        assert_eq!(rec.texts.len(), 1);
    }

    #[test]
    fn the_vertical_line_lands_on_the_pixel_grid_for_its_width() {
        let x_of = |width: Option<f64>| {
            let s = with_override(CrosshairOverride {
                horizontal: Some(false),
                width,
                ..Default::default()
            });
            seen(&paint(&s, None).1)
                .segments
                .iter()
                .find(|s| vertical(s))
                .unwrap()
                .0[0]
        };
        assert_eq!(x_of(None), 100.5); // the configured 1 px crosshair
        assert_eq!(x_of(Some(2.0)), 100.0);
        assert_eq!(x_of(Some(3.0)), 100.5);
    }

    #[test]
    fn shade_right_veils_the_plot_from_the_bar_right_edge_to_the_price_scale_under_the_line() {
        let s = with_override(CrosshairOverride {
            horizontal: Some(false),
            shade_right: Some(ShadeRight {
                color: "#101010".into(),
                opacity: Some(0.6),
            }),
            ..Default::default()
        });
        let (cr, canvas) = paint(&s, None);
        let rec = seen(&canvas);
        // x = 100 -> bar 10 at x = 100; its right edge is logical 10.5 -> x = 105; the plot
        // is 380 x 280.
        assert_eq!(
            rec.fills[0],
            ("#101010".to_string(), 0.6, [105.0, 0.0, 275.0, 280.0])
        );
        assert_ne!(rec.strokes[0].2, 0.6); // the line keeps its own opacity
        assert_eq!(
            cr.shade_right_area(),
            Some(ShadeArea {
                x: 105.0,
                width: 275.0,
                height: 280.0
            })
        );
        // The veil is painted before the line.
        let names = canvas.names();
        assert!(
            names.iter().position(|n| n == "fillRect") < names.iter().position(|n| n == "stroke")
        );
        let plain = seen(
            &paint(
                &with_override(CrosshairOverride {
                    horizontal: Some(false),
                    ..Default::default()
                }),
                None,
            )
            .1,
        );
        assert!(!plain.fills.iter().any(|f| f.0 == "#101010"));
    }

    #[test]
    fn a_veil_with_no_opacity_is_opaque_and_one_off_the_plot_is_not_painted() {
        let veil = |x: f64| {
            let mut s = with_override(CrosshairOverride {
                shade_right: Some(ShadeRight {
                    color: "#000".into(),
                    opacity: None,
                }),
                ..Default::default()
            });
            s.crosshair = Some(CrosshairPoint { x, y: 10.0 });
            let (cr, canvas) = paint(&s, None);
            (seen(&canvas).fills.first().cloned(), cr.shade_right_area())
        };
        let (fill, area) = veil(100.0);
        assert_eq!(fill.unwrap().1, 1.0);
        assert!(area.is_some());
        // The last bar's right edge (38.5 -> 385) lies beyond the data width: no veil.
        let (fill, area) = veil(380.0);
        assert!(fill.iter().all(|f| f.0 != "#000"));
        assert_eq!(area, None);
    }

    #[test]
    fn a_synced_ghost_wears_the_override_too() {
        let ghost = ExternalCrosshair {
            x: 200.0,
            y: Some(90.0),
            time: BAR10 as f64,
            price: Some(42.0),
            line: None,
        };
        let mut s = with_override(CrosshairOverride {
            horizontal: Some(false),
            color: Some("#2962ff".into()),
            style: Some(LineStyle::Solid),
            opacity: Some(1.0),
            shade_right: Some(ShadeRight {
                color: "#101010".into(),
                opacity: Some(0.6),
            }),
            ..Default::default()
        });
        s.crosshair = None;
        let (cr, canvas) = paint(&s, Some(&ghost));
        let rec = seen(&canvas);
        // ghost x = 200 -> bar 20; its right edge is logical 20.5 -> x = 205
        assert_eq!(
            rec.fills[0],
            ("#101010".to_string(), 0.6, [205.0, 0.0, 175.0, 280.0])
        );
        assert!(!rec.segments.iter().any(horizontal));
        assert_eq!(rec.strokes[0], ("#2962ff".to_string(), vec![], 0.55)); // dimmed: still a ghost
        assert_eq!(rec.texts.len(), 1); // the time chip only
        assert_eq!(cr.shade_right_area().map(|a| a.x), Some(205.0));
    }

    #[test]
    fn a_ghost_left_of_the_window_still_veils_every_bar_in_view_with_no_line() {
        let off_left = ExternalCrosshair {
            x: -50.0,
            y: None,
            time: BAR10 as f64,
            price: None,
            line: Some(false),
        };
        let mut s = with_override(CrosshairOverride {
            horizontal: Some(false),
            shade_right: Some(ShadeRight {
                color: "#101010".into(),
                opacity: Some(0.6),
            }),
            ..Default::default()
        });
        s.crosshair = None;
        let rec = seen(&paint(&s, Some(&off_left)).1);
        assert_eq!(
            rec.fills[0],
            ("#101010".to_string(), 0.6, [0.0, 0.0, 380.0, 280.0])
        );
        assert!(rec.strokes.is_empty());
        assert!(rec.texts.is_empty());
        let mut plain = scene();
        plain.crosshair = None;
        let rec = seen(&paint(&plain, Some(&off_left)).1);
        assert!(!rec.fills.iter().any(|f| f.0 == "#101010"));
        assert!(rec.strokes.is_empty());
    }

    #[test]
    fn without_an_override_the_ghost_keeps_the_configured_style_the_level_line_and_both_chips() {
        let ghost = ExternalCrosshair {
            x: 200.0,
            y: Some(90.0),
            time: BAR10 as f64,
            price: Some(42.0),
            line: None,
        };
        let mut s = scene();
        s.crosshair = None;
        let (cr, canvas) = paint(&s, Some(&ghost));
        let rec = seen(&canvas);
        assert!(rec.segments.iter().any(horizontal));
        assert_eq!(rec.strokes[0].1, vec![6.0, 4.0]);
        assert_eq!(rec.strokes[0].2, 0.4 * 0.55);
        assert_eq!(rec.texts.len(), 2);
        assert_eq!(cr.shade_right_area(), None);
        // The chips are dimmed to 0.8 and the frame ends opaque.
        let alphas: Vec<f64> = canvas
            .calls("globalAlpha")
            .iter()
            .map(|o| o[1].as_f64().unwrap())
            .collect();
        assert!(alphas.contains(&0.8));
        assert_eq!(*alphas.last().unwrap(), 1.0);
    }

    #[test]
    fn a_ghost_and_the_local_crosshair_both_paint_and_the_local_one_goes_on_top() {
        let ghost = ExternalCrosshair {
            x: 300.0,
            y: None,
            time: BAR10 as f64,
            price: None,
            line: None,
        };
        let (_, canvas) = paint(&scene(), Some(&ghost));
        let rec = seen(&canvas);
        assert_eq!(rec.strokes.len(), 2);
        // The ghost's (dimmer) stroke comes first.
        assert!(rec.strokes[0].2 < rec.strokes[1].2);
    }

    #[test]
    fn an_unscaled_pane_has_no_value_chip_and_an_off_pane_cursor_none_either() {
        let mut s = scene();
        s.panes.get_mut("price").unwrap().axis_format = Some(PaneAxisFormat::NoAxis);
        assert_eq!(seen(&paint(&s, None).1).texts.len(), 1); // the time chip only
        let mut s = scene();
        s.panes.get_mut("price").unwrap().bounds.height = 100.0; // the cursor (y = 120) is under the pane
        assert_eq!(seen(&paint(&s, None).1).texts.len(), 1);
    }

    // ── the time chip on coarse bars (coarse-bar-dates.test.ts) ──

    /// The first chip text of a frame at `interval` whose hovered bar opens at `time`.
    fn chip(zone: &str, interval: i64, time: i64) -> Option<String> {
        let mut s = scene();
        s.timezone = zone.into();
        s.crosshair_override = Some(CrosshairOverride {
            horizontal: Some(false),
            ..Default::default()
        });
        let (mut cr, canvas) = mounted();
        cr.render(&s, &coords(interval, time), &theme(), None, None);
        seen(&canvas).texts.first().cloned()
    }

    #[test]
    fn a_daily_bar_reads_its_utc_date_in_every_zone_and_an_intraday_bar_the_display_zone() {
        let friday_23_oct = 1_792_713_600_000_i64; // 2026-10-23 00:00 UTC
        for zone in ["Etc/GMT+5", "+09:00", "UTC", "Etc/UTC"] {
            assert_eq!(
                chip(zone, DAY, friday_23_oct).as_deref(),
                Some("Fri 23 Oct '26"),
                "{zone}"
            );
        }
        // 03:00 UTC on the 23rd is 23:00 on the 22nd at UTC-4.
        assert_eq!(
            chip("Etc/GMT+4", HOUR, friday_23_oct + 3 * HOUR).as_deref(),
            Some("Thu 22 Oct '26 23:00")
        );
    }
}
