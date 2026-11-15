//! The backdrop layer (L-2): session highlights and the axis gridlines, on their own
//! canvas at the very BOTTOM of the canvas pile. The grid used to be painted inside the
//! data canvas, but SDK layer canvases can slot BELOW that canvas (an indicator
//! restacked behind the candles takes its layer canvas along), and nothing may ever paint
//! under the grid. Keeping the grid on the bottom-most canvas makes that invariant
//! structural. Repainted on data frames only, from the same scene and coordinates the
//! geometry backend reads.
//!
//! The tick source is shared with the chrome layer, so gridlines and price labels agree:
//! it is an `Rc<PriceAxisTickSource>` (a handle with interior state).
//!
//! Left for the browser crate: the original's `mount(canvas)` reads
//! `canvas.getContext('2d')` and the canvas' backing-store size; here
//! [`BackdropRenderer::mount`] takes the context and that size as values
//! ([`PlotSurface`]).

use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use crate::format::{ResolvedFormatters, english_format};
use crate::indicators::classics::math::{js_max, js_min, js_round};
use crate::intl::{Intl, default_intl};
use crate::ports::chart_renderer::VelaTheme;
use crate::render::backend::canvas2d_backend::{PlotSurface, theme_str};
use crate::render::canvas::Canvas2d;
use crate::render::chrome::price_axis_ticks::PriceAxisTickSource;
use crate::render::chrome::ticks::time_ticks;
use crate::render::chrome::tz::{display_zone, tz_offset_ms};
use crate::render::core::plot_direction::begin_plot_frame;
use crate::render::core::scene_graph::HighlightArea;
use crate::render::core::{CoordinateSystem, SceneGraph};

/// The scene's `Intl` backend (the chart's formatters carry it), the built-in one when the
/// renderer runs without a chart. The original reads the platform's global `Intl`.
pub fn scene_intl(scene: &SceneGraph) -> Arc<dyn Intl> {
    match &scene.format {
        Some(format) => format.settings().intl.clone(),
        None => default_intl(),
    }
}

fn builtin_formatters() -> &'static ResolvedFormatters {
    static ENGLISH: OnceLock<ResolvedFormatters> = OnceLock::new();
    ENGLISH.get_or_init(english_format)
}

/// A time band clipped to its permitted horizontal paint interval.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HighlightRect {
    pub x: f64,
    pub width: f64,
}

/// Clip one time-band rectangle to its permitted horizontal paint interval.
pub fn clip_highlight_rect(x1: f64, x2: f64, left: f64, right: f64) -> Option<HighlightRect> {
    let x = js_max(left, x1);
    let end = js_min(right, x2);
    (end > x).then_some(HighlightRect { x, width: end - x })
}

/// The logical SLOT boundary a session-band edge lands on. Bars belong to a band by their
/// open time (`[from, to)` semantics), so an edge must sit BETWEEN the last bar inside and
/// the first bar outside, never through a candle body. `logical` is the edge timestamp's
/// fractional bar index: an exact bar open (integer) yields that bar's own slot start; a
/// timestamp between opens (a gap, or mid-bar) yields the boundary before the next bar,
/// which is the same pixel from either side, so adjacent bands meet seamlessly across
/// barless stretches like a maintenance halt.
pub fn band_edge_slot(logical: f64) -> f64 {
    logical.ceil() - 0.5
}

pub struct BackdropRenderer {
    axis_ticks: Rc<PriceAxisTickSource>,
    surface: Option<PlotSurface>,
}

impl Default for BackdropRenderer {
    fn default() -> Self {
        Self::new(Rc::new(PriceAxisTickSource::new()))
    }
}

impl BackdropRenderer {
    /// `axis_ticks` is shared with the chrome layer, so gridlines and price labels agree.
    pub fn new(axis_ticks: Rc<PriceAxisTickSource>) -> Self {
        Self {
            axis_ticks,
            surface: None,
        }
    }

    /// Bind to the backdrop canvas: its 2d context and backing-store size (device pixels).
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

    pub fn destroy(&mut self) {
        self.surface = None;
    }

    /// Paint one frame: highlight bands first, gridlines on top (the order they had inside
    /// the data canvas). `grid_alpha` fades the gridlines as a reveal-under layer opens.
    pub fn render(
        &self,
        scene: &SceneGraph,
        coords: &CoordinateSystem,
        theme: &VelaTheme,
        grid_alpha: f64,
    ) {
        let Some(surface) = &self.surface else {
            return;
        };
        let ctx: &dyn Canvas2d = &*surface.ctx;

        let dpr = coords.dpr();
        begin_plot_frame(ctx, dpr);
        ctx.clear_rect(0.0, 0.0, surface.width / dpr, surface.height / dpr);

        // Same gate as the geometry backend: no grid before data reaches the view.
        let n = coords.bar_count();
        if n == 0 {
            return;
        }
        let vr = coords.visible_logical_range();
        if js_min((n - 1) as f64, vr.to.ceil()) < js_max(0.0, vr.from.floor()) {
            return;
        }

        self.draw_highlights(ctx, scene, coords);
        self.draw_grid(ctx, scene, coords, theme, coords.width(), grid_alpha);
    }

    /// Renderer-owned session highlight bands: full-height (all panes), behind the grid.
    /// Session-zone washes paint first (edges snapped to bar-slot boundaries so a band
    /// never cuts a candle in two), host highlights on top (continuous time mapping: hosts
    /// may paint sub-bar spans).
    fn draw_highlights(&self, ctx: &dyn Canvas2d, scene: &SceneGraph, coords: &CoordinateSystem) {
        let sessions = scene.session_highlight_bands();
        if !sessions.is_empty() {
            // Calendar windows may extend beyond loaded candles (the tracker fetches ahead
            // for live charts). Paint only the occupied bar slots: no wash before history,
            // and none in right-side whitespace beyond the current bar.
            let left = js_max(0.0, coords.logical_to_x(-0.5));
            let right = js_min(
                coords.width(),
                coords.logical_to_x(coords.bar_count() as f64 - 0.5),
            );
            self.draw_highlight_set(ctx, &sessions, coords, left, right, true);
        }
        self.draw_highlight_set(ctx, &scene.highlights, coords, 0.0, coords.width(), false);
    }

    fn draw_highlight_set(
        &self,
        ctx: &dyn Canvas2d,
        bands: &[HighlightArea],
        coords: &CoordinateSystem,
        left: f64,
        right: f64,
        snap_to_slots: bool,
    ) {
        let edge_x = |ms: f64| {
            if snap_to_slots {
                coords.logical_to_x(band_edge_slot(coords.time_to_logical(ms)))
            } else {
                coords.time_to_x(ms)
            }
        };
        for band in bands {
            let Some(rect) = clip_highlight_rect(
                edge_x(band.from as f64),
                edge_x(band.to as f64),
                left,
                right,
            ) else {
                continue;
            };
            ctx.set_fill_style(&(&band.color).into());
            ctx.fill_rect(rect.x, 0.0, rect.width, coords.height());
        }
    }

    // ── grid ── vert/horz gate on `scene.show_grid` AND their own per-axis visibility
    // (style); each uses its own color. Pane separators are drawn on the chrome layer
    // (full-width, above the data) so series never overpaint them.
    fn draw_grid(
        &self,
        ctx: &dyn Canvas2d,
        scene: &SceneGraph,
        coords: &CoordinateSystem,
        theme: &VelaTheme,
        data_w: f64,
        grid_alpha: f64,
    ) {
        let panes = scene.ordered_panes();
        let grid_vert = &scene.style.grid_vert;
        let grid_horz = &scene.style.grid_horz;
        let vert_color = grid_vert
            .color
            .as_deref()
            .unwrap_or_else(|| theme_str(theme, "gridColor"));
        let horz_color = grid_horz
            .color
            .as_deref()
            .unwrap_or_else(|| theme_str(theme, "gridColor"));
        ctx.set_line_width(1.0);
        if scene.show_grid && grid_vert.visible {
            ctx.set_global_alpha(grid_alpha); // fade the grid out as a reveal-under style opens up
            ctx.set_stroke_style(&vert_color.into());
            let tr = coords.visible_time_range();
            let intl = scene_intl(scene);
            let offset = tz_offset_ms(
                &*intl,
                (tr.from + tr.to) / 2.0,
                display_zone(&scene.timezone, coords.bar_interval()),
            );
            ctx.begin_path();
            for tick in time_ticks(tr.from, tr.to, 8.0, offset, builtin_formatters()) {
                let x = js_round(coords.time_to_x(tick.time)) + 0.5;
                if x < 0.0 || x > data_w {
                    continue;
                }
                ctx.move_to(x, 0.0);
                ctx.line_to(x, coords.height());
            }
            ctx.stroke();
        }
        for pane in panes {
            if scene.show_grid && grid_horz.visible && !pane.collapsed {
                ctx.set_global_alpha(grid_alpha); // fade horizontal gridlines so they don't show through fading candles
                ctx.set_stroke_style(&horz_color.into());
                ctx.begin_path();
                for t in self.axis_ticks.ticks_for(scene, pane).iter() {
                    let y = js_round(coords.price_to_y(t.price, &pane.scale, &pane.bounds)) + 0.5;
                    if y < pane.bounds.top || y > pane.bounds.top + pane.bounds.height {
                        continue;
                    }
                    ctx.move_to(0.0, y);
                    ctx.line_to(data_w, y);
                }
                ctx.stroke();
            }
        }
        ctx.set_global_alpha(1.0);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::model::{Ohlcv, PaneKind};
    use crate::options::{PriceAxisTick, PriceAxisTickContext, PriceAxisTicksFn};
    use crate::render::backend::canvas2d_backend::test_support::{
        frame_start, recording, stream, theme,
    };
    use crate::render::canvas::RecordingCanvas;
    use crate::render::chrome::chrome_renderer::{ChromeRenderer, FrameClock};
    use crate::render::chrome::marks_paint::NoMarkIcons;
    use crate::render::core::scene_graph::{PaneAxisFormat, SessionZones};
    use crate::render::core::{PaneNode, ViewportState};

    const W: f64 = 500.0; // data width (the price-axis column starts here)
    const H: f64 = 400.0; // data height
    const T0: i64 = 1_700_000_000_000;
    const HOUR: i64 = 3_600_000;

    /// 60 hourly bars on a W x H plot; default viewport (pitch 8, 6-bar margin), so bar `i`
    /// sits at `x = 500 - (65 - i) * 8`.
    fn coords() -> CoordinateSystem {
        let mut c = CoordinateSystem::new();
        c.set_size(W, H, 1.0);
        let times: Vec<i64> = (0..60).map(|i| T0 + i * HOUR).collect();
        c.set_bars(&times);
        c
    }

    /// A price pane (100-200 over the top 300 px) and a study pane below it.
    fn scene() -> SceneGraph {
        let mut s = SceneGraph::new();
        s.bars = (0..60)
            .map(|i| Ohlcv::new(T0 + i * HOUR, 100.0, 101.0, 99.0, 100.0, None))
            .collect();
        let price = s.ensure_pane("price", PaneKind::Price, 0, Some(3.0));
        price.bounds.top = 0.0;
        price.bounds.height = 300.0;
        price.scale.min = 100.0;
        price.scale.max = 200.0;
        let study = s.ensure_pane("study-1", PaneKind::Study, 1, Some(1.0));
        study.bounds.top = 300.0;
        study.bounds.height = 100.0;
        study.scale.min = 0.0;
        study.scale.max = 100.0;
        s
    }

    fn tick(price: f64, label: &str) -> PriceAxisTick {
        PriceAxisTick {
            price,
            label: label.to_string(),
            major: None,
        }
    }

    fn mounted(source: &Rc<PriceAxisTickSource>) -> (BackdropRenderer, Rc<RecordingCanvas>) {
        let canvas = recording();
        let mut backdrop = BackdropRenderer::new(source.clone());
        backdrop.mount(canvas.clone(), W + 70.0, H + 22.0);
        (backdrop, canvas)
    }

    /// The price pane's pixel y of a price (0..300 px over 100..200).
    fn price_y(p: f64) -> f64 {
        0.0 + 300.0 * (1.0 - (p - 100.0) / (200.0 - 100.0))
    }

    // ── the pure helpers (session-shading.test.ts) ──

    #[test]
    fn shading_is_clipped_before_the_first_loaded_bar_and_after_the_current_bar() {
        let r = |x, width| Some(HighlightRect { x, width });
        assert_eq!(
            clip_highlight_rect(-100.0, 80.0, 20.0, 200.0),
            r(20.0, 60.0)
        );
        assert_eq!(
            clip_highlight_rect(150.0, 300.0, 20.0, 200.0),
            r(150.0, 50.0)
        );
        assert_eq!(clip_highlight_rect(-100.0, 10.0, 20.0, 200.0), None);
        assert_eq!(clip_highlight_rect(210.0, 300.0, 20.0, 200.0), None);
        assert_eq!(clip_highlight_rect(f64::NAN, 80.0, 20.0, 200.0), None);
    }

    #[test]
    fn session_band_edges_land_between_candles_never_through_one() {
        // Five bars at a 15-unit pitch with a gap (a maintenance halt): 0, 15, 30, 105, 120.
        use crate::render::core::bar_time::bar_time_to_logical;
        let times = [0, 15, 30, 105, 120];
        let edge = |ms: f64| band_edge_slot(bar_time_to_logical(ms, &times, 15.0));
        // An edge on an exact bar open sits at that bar slot start: a band starting at 15
        // owns bar 1 wholly; one ENDING at 105 excludes bar 3, the same boundary.
        assert_eq!(edge(15.0), 0.5);
        assert_eq!(edge(105.0), 2.5);
        // An edge inside a bar gap resolves to the boundary after the last bar inside.
        assert_eq!(edge(60.0), 2.5);
        // An edge between bar opens owns the NEXT bar (bars belong by open time).
        assert_eq!(edge(7.0), 0.5);
    }

    // ── the frame ──

    #[test]
    fn an_unmounted_or_destroyed_backdrop_paints_nothing_and_an_empty_chart_only_clears() {
        let s = scene();
        let source = Rc::new(PriceAxisTickSource::new());
        let backdrop = BackdropRenderer::new(source.clone());
        backdrop.render(&s, &coords(), &theme(), 1.0);
        let (mut backdrop, canvas) = mounted(&source);
        let mut empty = CoordinateSystem::new();
        empty.set_size(W, H, 2.0);
        backdrop.render(&s, &empty, &theme(), 1.0);
        assert_eq!(
            canvas.to_json(),
            Value::Array(frame_start(2.0, (W + 70.0) / 2.0, (H + 22.0) / 2.0))
        );
        canvas.clear();
        backdrop.destroy();
        backdrop.render(&s, &coords(), &theme(), 1.0);
        assert!(canvas.is_empty());
    }

    #[test]
    fn a_viewport_with_no_bar_in_view_paints_the_clear_only() {
        let s = scene();
        let source = Rc::new(PriceAxisTickSource::new());
        let (backdrop, canvas) = mounted(&source);
        let mut c = coords();
        // The last bar scrolled far off to the left: nothing of the data is in view.
        c.set_viewport(ViewportState {
            bar_spacing: 8.0,
            right_offset: -200.0,
        });
        backdrop.render(&s, &c, &theme(), 1.0);
        assert_eq!(
            canvas.to_json(),
            Value::Array(frame_start(1.0, W + 70.0, H + 22.0))
        );
    }

    #[test]
    fn a_highlight_and_host_gridlines_paint_this_exact_stream() {
        let mut s = scene();
        s.style.grid_vert.visible = false; // the vertical lines are checked on their own
        s.panes.get_mut("study-1").unwrap().axis_format = Some(PaneAxisFormat::NoAxis);
        // A host highlight spans bar 10 to bar 20 on the continuous time mapping:
        // x(10) = 60, x(20) = 140.
        s.highlights.push(HighlightArea {
            from: T0 + 10 * HOUR,
            to: T0 + 20 * HOUR,
            color: "rgba(255,0,0,0.1)".into(),
        });
        let source = Rc::new(PriceAxisTickSource::new());
        let hook: PriceAxisTicksFn = Rc::new(|ctx: &PriceAxisTickContext| {
            Some(if ctx.pane.kind == PaneKind::Price {
                vec![
                    tick(120.0, "a"),
                    tick(150.0, "b"),
                    tick(180.0, "c"),
                    tick(250.0, "off"),
                ]
            } else {
                vec![]
            })
        });
        source.set_hook(Some(hook));
        let (backdrop, canvas) = mounted(&source);
        backdrop.render(&s, &coords(), &theme(), 0.5);
        let line = |p: f64| {
            let y = price_y(p).round() + 0.5;
            vec![json!(["moveTo", 0.0, y]), json!(["lineTo", W, y])]
        };
        assert_eq!(
            canvas.to_json(),
            stream(vec![
                frame_start(1.0, W + 70.0, H + 22.0),
                vec![
                    json!(["fillStyle", "rgba(255,0,0,0.1)"]),
                    json!(["fillRect", 60.0, 0.0, 80.0, H]),
                    json!(["lineWidth", 1.0]),
                    // price pane: the fixed ticks inside the pane (250 is off the window)
                    json!(["globalAlpha", 0.5]),
                    json!(["strokeStyle", "#222222"]),
                    json!(["beginPath"]),
                ],
                line(120.0),
                line(150.0),
                line(180.0),
                vec![
                    json!(["stroke"]),
                    // study pane: an unscaled pane draws no gridlines, but still opens and
                    // strokes its (empty) path
                    json!(["globalAlpha", 0.5]),
                    json!(["strokeStyle", "#222222"]),
                    json!(["beginPath"]),
                    json!(["stroke"]),
                    json!(["globalAlpha", 1.0]),
                ],
            ])
        );
    }

    #[test]
    fn vertical_gridlines_sit_on_the_time_ticks_on_pixel_middles_inside_the_data_width() {
        let s = scene();
        let source = Rc::new(PriceAxisTickSource::new());
        source.set_hook(Some(Rc::new(|_: &PriceAxisTickContext| Some(vec![]))));
        let (backdrop, canvas) = mounted(&source);
        let c = coords();
        backdrop.render(&s, &c, &theme(), 1.0);
        // The ladder the frame used: UTC (no zone set), 8 ticks asked.
        let tr = c.visible_time_range();
        let fmt = english_format();
        let expected: Vec<f64> = time_ticks(tr.from, tr.to, 8.0, 0.0, &fmt)
            .iter()
            .map(|t| js_round(c.time_to_x(t.time)) + 0.5)
            .filter(|x| *x >= 0.0 && *x <= W)
            .collect();
        assert!(expected.len() >= 2);
        let ops = canvas.ops();
        let begin = ops.iter().position(|o| o[0] == "globalAlpha").unwrap();
        assert_eq!(ops[begin + 1], json!(["strokeStyle", "#222222"]));
        let verticals: Vec<f64> = ops
            .iter()
            .filter(|o| o[0] == "moveTo" && o[2] == json!(0.0) && o[1].as_f64().unwrap() > 0.0)
            .map(|o| o[1].as_f64().unwrap())
            .collect();
        assert_eq!(verticals, expected);
        assert!(
            canvas
                .calls("lineTo")
                .contains(&json!(["lineTo", expected[0], H]))
        );
    }

    #[test]
    fn the_grid_follows_the_display_zone_offset() {
        // A zone east of UTC shifts the tick ladder: the lines land on that zone midnights
        // and hours, not UTC's.
        let mut s = scene();
        s.timezone = "+09:00".into(); // the built-in backend knows offset ids, not IANA regions
        let source = Rc::new(PriceAxisTickSource::new());
        source.set_hook(Some(Rc::new(|_: &PriceAxisTickContext| Some(vec![]))));
        let c = coords();
        let (backdrop, canvas) = mounted(&source);
        backdrop.render(&s, &c, &theme(), 1.0);
        let tokyo: Vec<f64> = canvas
            .calls("moveTo")
            .iter()
            .map(|o| o[1].as_f64().unwrap())
            .collect();
        let (backdrop, canvas) = mounted(&source);
        s.timezone = "UTC".into();
        backdrop.render(&s, &c, &theme(), 1.0);
        let utc: Vec<f64> = canvas
            .calls("moveTo")
            .iter()
            .map(|o| o[1].as_f64().unwrap())
            .collect();
        assert_ne!(tokyo, utc);
    }

    #[test]
    fn session_bands_snap_to_slot_boundaries_and_stay_inside_the_occupied_bars() {
        let mut s = scene();
        s.style.grid_vert.visible = false;
        s.style.grid_horz.visible = false;
        s.session_zones = Some(SessionZones {
            pre: vec![
                (T0 + 10 * HOUR, T0 + 20 * HOUR),
                // from before the history to far past the newest bar
                (T0 - 100 * HOUR, T0 + 1000 * HOUR),
            ],
            post: vec![],
            extended: vec![],
        });
        s.style.sessions.premarket_color = "#aa0000".into();
        let source = Rc::new(PriceAxisTickSource::new());
        let (backdrop, canvas) = mounted(&source);
        backdrop.render(&s, &coords(), &theme(), 1.0);
        // Edges are bar-slot boundaries: slot start of bar 10 is x(9.5) = 56, of bar 20
        // x(19.5) = 136. The wide band clips to the occupied slots: x(-0.5) = -24 -> 0 and
        // x(59.5) = 456.
        assert_eq!(
            canvas.calls("fillRect"),
            vec![
                json!(["fillRect", 56.0, 0.0, 80.0, H]),
                json!(["fillRect", 0.0, 0.0, 456.0, H]),
            ]
        );
    }

    #[test]
    fn the_session_wash_paints_before_host_highlights() {
        let mut s = scene();
        s.style.grid_vert.visible = false;
        s.style.grid_horz.visible = false;
        s.session_zones = Some(SessionZones {
            pre: vec![(T0 + 10 * HOUR, T0 + 20 * HOUR)],
            post: vec![],
            extended: vec![],
        });
        s.style.sessions.premarket_color = "#session".into();
        s.highlights.push(HighlightArea {
            from: T0 + 10 * HOUR,
            to: T0 + 20 * HOUR,
            color: "#host".into(),
        });
        let source = Rc::new(PriceAxisTickSource::new());
        let (backdrop, canvas) = mounted(&source);
        backdrop.render(&s, &coords(), &theme(), 1.0);
        assert_eq!(
            canvas.calls("fillStyle"),
            vec![
                json!(["fillStyle", "#session"]),
                json!(["fillStyle", "#host"])
            ]
        );
    }

    #[test]
    fn the_master_toggle_and_per_axis_visibility_gate_the_lines() {
        let source = Rc::new(PriceAxisTickSource::new());
        let mut off = scene();
        off.show_grid = false;
        let (backdrop, canvas) = mounted(&source);
        backdrop.render(&off, &coords(), &theme(), 1.0);
        assert_eq!(
            canvas.to_json(),
            stream(vec![
                frame_start(1.0, W + 70.0, H + 22.0),
                vec![json!(["lineWidth", 1.0]), json!(["globalAlpha", 1.0])],
            ])
        );

        let mut custom = scene();
        custom.style.grid_horz.visible = false;
        custom.style.grid_vert.color = Some("#abcdef".into());
        custom.panes.get_mut("study-1").unwrap().collapsed = true;
        let (backdrop, canvas) = mounted(&source);
        backdrop.render(&custom, &coords(), &theme(), 1.0);
        // Only the vertical set, in its own color; no horizontal path at all.
        assert_eq!(
            canvas.calls("strokeStyle"),
            vec![json!(["strokeStyle", "#abcdef"])]
        );
        assert_eq!(canvas.calls("stroke").len(), 1);

        let mut collapsed = scene();
        collapsed.style.grid_vert.visible = false;
        collapsed.panes.get_mut("study-1").unwrap().collapsed = true;
        let (backdrop, canvas) = mounted(&source);
        backdrop.render(&collapsed, &coords(), &theme(), 1.0);
        assert_eq!(canvas.calls("stroke").len(), 1); // the price pane only
    }

    // ── gridlines and price labels share one tick source (price-axis-ticks.test.ts) ──

    /// A price label as painted: its text, y, the font and the alpha in force.
    type Label = (String, f64, String, f64);

    /// Paint one data frame (backdrop + chrome) through a shared tick source: the y of every
    /// full-width horizontal gridline, and the labels in the master price column.
    fn paint_frame(
        source: &Rc<PriceAxisTickSource>,
        s: &SceneGraph,
        c: &CoordinateSystem,
    ) -> (Vec<f64>, Vec<Label>) {
        let (backdrop, back) = mounted(source);
        let front = recording();
        let mut chrome = ChromeRenderer::new(source.clone());
        chrome.mount(front.clone(), W + 70.0, H + 22.0);
        backdrop.render(s, c, &theme(), 1.0);
        chrome.render(
            s,
            c,
            &theme(),
            None,
            &FrameClock {
                now_ms: 0.0,
                wall_ms: 0.0,
            },
            &NoMarkIcons,
        );
        // Horizontal gridlines span the data width; price labels sit in the master column.
        let ops = back.ops();
        let mut grid_ys = Vec::new();
        for pair in ops.windows(2) {
            if pair[0][0] == "moveTo"
                && pair[1][0] == "lineTo"
                && pair[0][1] == json!(0.0)
                && pair[1][1] == json!(W)
                && pair[0][2] == pair[1][2]
            {
                grid_ys.push(pair[0][2].as_f64().unwrap());
            }
        }
        // Each label with the font and alpha in force when it was painted.
        let mut font = String::new();
        let mut alpha = 1.0;
        let mut labels = Vec::new();
        for op in front.ops() {
            match op[0].as_str().unwrap() {
                "font" => font = op[1].as_str().unwrap().to_string(),
                "globalAlpha" => alpha = op[1].as_f64().unwrap(),
                "fillText" if op[2] == json!(W + 6.0) => {
                    labels.push((
                        op[1].as_str().unwrap().to_string(),
                        op[3].as_f64().unwrap(),
                        font.clone(),
                        alpha,
                    ));
                }
                _ => {}
            }
        }
        (grid_ys, labels)
    }

    fn pane_y(c: &CoordinateSystem, p: f64, pane: &PaneNode) -> f64 {
        c.price_to_y(p, &pane.scale, &pane.bounds)
    }

    #[test]
    fn host_ticks_replace_both_the_gridlines_and_the_labels_with_one_host_call_per_pane() {
        use std::cell::Cell;
        let s = scene();
        let c = coords();
        let calls = Rc::new(Cell::new(0));
        let counter = calls.clone();
        let hook: PriceAxisTicksFn = Rc::new(move |ctx: &PriceAxisTickContext| {
            counter.set(counter.get() + 1);
            Some(if ctx.pane.kind == PaneKind::Price {
                vec![
                    tick(120.0, "P120"),
                    tick(150.0, "P150"),
                    tick(180.0, "P180"),
                ]
            } else {
                vec![tick(25.0, "S25"), tick(75.0, "S75")]
            })
        });
        let source = Rc::new(PriceAxisTickSource::new());
        source.set_hook(Some(hook));
        let (grid_ys, labels) = paint_frame(&source, &s, &c);
        let price = s.panes.get("price").unwrap();
        let study = s.panes.get("study-1").unwrap();
        assert_eq!(
            labels.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(),
            vec!["P120", "P150", "P180", "S25", "S75"]
        );
        let expected_y: Vec<f64> = [120.0, 150.0, 180.0]
            .iter()
            .map(|p| pane_y(&c, *p, price))
            .chain([25.0, 75.0].iter().map(|p| pane_y(&c, *p, study)))
            .collect();
        assert_eq!(labels.iter().map(|l| l.1).collect::<Vec<_>>(), expected_y);
        assert_eq!(
            grid_ys,
            expected_y
                .iter()
                .map(|y| js_round(*y) + 0.5)
                .collect::<Vec<_>>()
        );
        assert_eq!(calls.get(), 2); // one per pane: the backdrop and the chrome shared it

        // A chrome-only repaint (the countdown's second pulse) asks the host nothing new.
        let mut chrome = ChromeRenderer::new(source.clone());
        chrome.mount(recording(), W + 70.0, H + 22.0);
        chrome.render(
            &s,
            &c,
            &theme(),
            None,
            &FrameClock {
                now_ms: 0.0,
                wall_ms: 0.0,
            },
            &NoMarkIcons,
        );
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn without_a_host_function_both_layers_keep_the_built_in_ladder() {
        use crate::render::chrome::ticks::{AxisScale, pane_axis_ticks};
        let mut s = scene();
        s.panes.get_mut("study-1").unwrap().axis_format = Some(PaneAxisFormat::NoAxis);
        let c = coords();
        let (grid_ys, labels) = paint_frame(&Rc::new(PriceAxisTickSource::new()), &s, &c);
        let price = s.panes.get("price").unwrap();
        let built_in = pane_axis_ticks(
            &AxisScale::from(&price.scale),
            300.0,
            None,
            None,
            None,
            &english_format(),
        );
        let ys: Vec<f64> = built_in
            .iter()
            .map(|t| pane_y(&c, t.price, price))
            .collect();
        assert!(built_in.len() > 2);
        assert_eq!(
            grid_ys,
            ys.iter()
                .map(|y| js_round(*y) + 0.5)
                .filter(|y| *y >= 0.0 && *y <= 300.0)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            labels.iter().map(|l| l.0.clone()).collect::<Vec<_>>(),
            built_in
                .iter()
                .zip(&ys)
                .filter(|(_, y)| **y >= 6.0 && **y <= 296.0)
                .map(|(t, _)| t.label.clone())
                .collect::<Vec<_>>()
        );
        assert!(
            labels
                .iter()
                .all(|l| l.2 == "11px sans-serif" && l.3 == 1.0)
        );
    }

    #[test]
    fn major_draws_semibold_minor_draws_muted_and_omitted_draws_the_regular_label() {
        let mut s = scene();
        s.panes.get_mut("study-1").unwrap().axis_format = Some(PaneAxisFormat::NoAxis);
        let source = Rc::new(PriceAxisTickSource::new());
        source.set_hook(Some(Rc::new(|_: &PriceAxisTickContext| {
            Some(vec![
                PriceAxisTick {
                    price: 120.0,
                    label: "major".into(),
                    major: Some(true),
                },
                tick(150.0, "plain"),
                PriceAxisTick {
                    price: 180.0,
                    label: "minor".into(),
                    major: Some(false),
                },
            ])
        })));
        let (_, labels) = paint_frame(&source, &s, &coords());
        let regular = "11px sans-serif";
        assert_eq!(
            labels
                .iter()
                .map(|l| (l.0.as_str(), l.2.clone(), l.3))
                .collect::<Vec<_>>(),
            vec![
                ("major", format!("600 {regular}"), 1.0),
                ("plain", regular.to_string(), 1.0),
                ("minor", regular.to_string(), 0.5),
            ]
        );
    }

    #[test]
    fn labels_too_close_to_the_pane_edges_stay_clipped_but_their_gridlines_still_draw() {
        let mut s = scene();
        s.panes.get_mut("study-1").unwrap().axis_format = Some(PaneAxisFormat::NoAxis);
        let source = Rc::new(PriceAxisTickSource::new());
        // y ~ 1.5 and 298.5 on a 300 px pane
        source.set_hook(Some(Rc::new(|_: &PriceAxisTickContext| {
            Some(vec![
                tick(199.5, "L199.5"),
                tick(150.0, "L150"),
                tick(100.5, "L100.5"),
            ])
        })));
        let (grid_ys, labels) = paint_frame(&source, &s, &coords());
        assert_eq!(
            labels.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(),
            vec!["L150"]
        );
        assert_eq!(grid_ys.len(), 3);
    }

    // ── the vertical grid on coarse bars (coarse-bar-dates.test.ts) ──

    fn utc_ms(y: i32, m0: u32, d: u32, h: u32) -> i64 {
        crate::intl::days_from_civil(i64::from(y), m0 + 1, d) * 24 * HOUR + i64::from(h) * HOUR
    }

    /// The x of every vertical line `draw_grid` strokes over bars at `times`.
    fn grid_lines(
        times: &[i64],
        width: f64,
        spacing: f64,
        zone: &str,
    ) -> (CoordinateSystem, Vec<f64>) {
        let mut c = CoordinateSystem::new();
        c.set_size(width, 300.0, 1.0);
        c.set_bars(times);
        c.set_viewport(ViewportState {
            bar_spacing: spacing,
            right_offset: 0.0,
        });
        let mut s = SceneGraph::new();
        s.timezone = zone.into();
        let canvas = recording();
        BackdropRenderer::default().draw_grid(&*canvas, &s, &c, &theme(), width, 1.0);
        let xs = canvas
            .calls("moveTo")
            .iter()
            .filter(|o| o[2] == json!(0.0))
            .map(|o| o[1].as_f64().unwrap())
            .collect();
        (c, xs)
    }

    #[test]
    fn the_grid_stands_on_the_same_utc_midnights_in_every_zone_on_a_daily_chart() {
        let daily: Vec<i64> = [19, 20, 21, 22, 23, 26, 27]
            .iter()
            .map(|d| utc_ms(2026, 9, *d, 0))
            .collect();
        let (c, utc) = grid_lines(&daily, 600.0, 100.0, "UTC");
        assert!(utc.contains(&(js_round(c.logical_to_x(1.0)) + 0.5)));
        for zone in ["Etc/GMT+5", "+09:00", "UTC", "Etc/UTC"] {
            assert_eq!(grid_lines(&daily, 600.0, 100.0, zone).1, utc, "{zone}");
        }
    }

    #[test]
    fn an_intraday_grid_shifts_into_the_display_zone() {
        let hourly: Vec<i64> = (0..48).map(|i| utc_ms(2026, 9, 22, 0) + i * HOUR).collect();
        assert_ne!(
            grid_lines(&hourly, 480.0, 10.0, "Etc/GMT+4").1,
            grid_lines(&hourly, 480.0, 10.0, "UTC").1
        );
    }
}
