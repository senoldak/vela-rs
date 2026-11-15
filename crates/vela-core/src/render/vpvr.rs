//! The VPVR layer: its own canvas2d surface mounted ABOVE the data canvas (the profile
//! reads over the candles), painting the visible-range volume-by-price rows against the
//! right edge. It re-buckets from the chart's bars per frame, memoized on the visible
//! window, so the profile tracks pan and zoom smoothly with no orchestrator round-trip.
//! Row widths are screen fractions (its own horizontal scale); only the y-axis uses the
//! shared price scale, so autoscale is untouched.
//!
//! [`paint_vpvr`] is the pure painter (`paintVpvr.ts`); [`VpvrRenderer`] is the layer
//! (`VpvrRenderer.ts`), called by the renderer each data frame with a [`VpvrRenderArgs`].
//! The renderer-layer seam ([`RendererLayerInstance`]) is implemented too: it reads the
//! layer config from the channel payload (`args.data`, the JSON of `VpvrLayerData`) and
//! treats the layer as visible whenever a payload is present.
//!
//! Deviation: with a NaN visible range (a zero bar spacing) the original indexes its
//! bars with NaN and throws a `TypeError` mid-frame; the layer here returns after
//! clearing its canvas.
//!
//! Left for the browser crate: the original's `mount(canvas)` reads the context and the
//! backing-store size off the element; here they are values ([`PlotSurface`]). Through
//! the registry's `mount(canvas)` only the context arrives, so the shell must follow with
//! [`VpvrRenderer::set_backing_size`] (until it does the clear covers nothing).

use std::rc::Rc;

use crate::indicators::classics::math::{js_max, js_min};
use crate::model::Ohlcv;
use crate::model::volume_layers::VpvrLayerData;
use crate::ports::chart_renderer::VelaTheme;
use crate::render::backend::canvas2d_backend::{PlotSurface, theme_str};
use crate::render::backend::layers::{RendererLayerArgs, RendererLayerInstance};
use crate::render::backend::vpvr_profile::{VpvrProfile, build_vpvr_profile};
use crate::render::canvas::Canvas2d;
use crate::render::core::plot_direction::begin_plot_frame;
use crate::render::core::{CoordinateSystem, PaneBounds, PriceScale};
use crate::util::js_number;

/// Row fill opacity inside / outside the value area (the profile overlays the candles).
pub const VPVR_VA_ALPHA: f64 = 0.62;
pub const VPVR_OUTSIDE_ALPHA: f64 = 0.28;
/// Opacity of the point-of-control row outline.
pub const VPVR_POC_ALPHA: f64 = 0.9;

/// Geometry the layer hands the painter (pane-resolved price to y, and the profile's
/// anchor edge).
pub struct VpvrGeom<'a> {
    /// Right pixel edge of the data area (rows hug this, extending left).
    pub right_x: f64,
    /// Pixel width of the largest row (the profile's horizontal scale).
    pub max_w: f64,
    /// Price to pixel y.
    pub y_of: &'a dyn Fn(f64) -> f64,
}

pub struct VpvrPaintStyle<'a> {
    pub up_color: &'a str,
    pub down_color: &'a str,
    pub show_poc: bool,
    /// Outline color for the POC row (the theme's text color).
    pub poc_color: &'a str,
}

/// Paint the visible-range volume profile: one horizontal row per price bucket, anchored
/// to the right edge and extending left, width proportional to the row's total volume
/// relative to the largest row. Each row splits into an up segment (left part) and a down
/// segment (right part, against the axis). Rows inside the value area draw brighter; the
/// POC row is outlined. Pure: every coordinate comes from `geom`.
pub fn paint_vpvr(
    ctx: &dyn Canvas2d,
    profile: &VpvrProfile,
    geom: &VpvrGeom,
    style: &VpvrPaintStyle,
) {
    let VpvrProfile {
        rows,
        row_h,
        max_total,
        poc,
        va_from,
        va_to,
        ..
    } = profile;
    let (row_h, max_total, poc, va_from, va_to) = (*row_h, *max_total, *poc, *va_from, *va_to);
    let VpvrGeom {
        right_x,
        max_w,
        y_of,
    } = *geom;
    if max_total <= 0.0 || max_w <= 0.0 {
        return;
    }

    for (k, row) in rows.iter().enumerate() {
        let total = row.up + row.down;
        if total <= 0.0 {
            continue;
        }
        let y_top = y_of(row.price + row_h);
        let h = js_max(1.0, y_of(row.price) - y_top - 1.0); // 1px inter-row gap
        let w = (total / max_total) * max_w;
        let up_w = (row.up / total) * w;
        ctx.set_global_alpha(if k >= va_from && k <= va_to {
            VPVR_VA_ALPHA
        } else {
            VPVR_OUTSIDE_ALPHA
        });
        if up_w > 0.0 {
            ctx.set_fill_style(&style.up_color.into());
            ctx.fill_rect(right_x - w, y_top, up_w, h);
        }
        if w - up_w > 0.0 {
            ctx.set_fill_style(&style.down_color.into());
            ctx.fill_rect(right_x - w + up_w, y_top, w - up_w, h);
        }
    }

    if style.show_poc
        && let Some(row) = rows.get(poc)
    {
        let y_top = y_of(row.price + row_h);
        let h = js_max(1.0, y_of(row.price) - y_top - 1.0);
        ctx.set_global_alpha(VPVR_POC_ALPHA);
        ctx.set_stroke_style(&style.poc_color.into());
        ctx.set_line_width(1.0);
        ctx.stroke_rect(right_x - max_w + 0.5, y_top + 0.5, max_w - 1.0, h);
    }
    ctx.set_global_alpha(1.0);
}

/// One frame's inputs.
pub struct VpvrRenderArgs<'a> {
    pub bars: &'a [Ohlcv],
    /// Layer config pushed by the VPVR native indicator (`None`: layer off).
    pub data: Option<&'a VpvrLayerData>,
    /// The indicator is mounted and not hidden.
    pub visible: bool,
    pub coords: &'a CoordinateSystem,
    /// The price pane's scale (row y-positions are real prices) and pixel extent.
    pub scale: PriceScale,
    pub bounds: PaneBounds,
    pub theme: &'a VelaTheme,
}

#[derive(Default)]
pub struct VpvrRenderer {
    surface: Option<PlotSurface>,
    memo_key: String,
    memo: Option<VpvrProfile>,
}

impl VpvrRenderer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind to the VPVR canvas: its 2d context and backing-store size (device pixels).
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
        self.memo_key.clear();
        self.memo = None;
    }

    pub fn render(&mut self, args: &VpvrRenderArgs) {
        let Some(surface) = &self.surface else {
            return;
        };
        let ctx = surface.ctx.clone();
        let (width, height) = (surface.width, surface.height);
        let VpvrRenderArgs {
            bars,
            data,
            visible,
            coords,
            scale,
            bounds,
            theme,
        } = *args;
        let ctx: &dyn Canvas2d = &*ctx;

        let dpr = coords.dpr();
        begin_plot_frame(ctx, dpr);
        ctx.clear_rect(0.0, 0.0, width / dpr, height / dpr); // always clear: a hide/remove must wipe the last frame
        let Some(data) = data else { return };
        if !visible || bars.is_empty() || bounds.height <= 0.0 {
            return;
        }

        let r = coords.visible_logical_range();
        let i0 = js_max(0.0, f64::floor(r.from));
        let i1 = js_min((bars.len() - 1) as f64, f64::ceil(r.to));
        if i0 > i1 {
            return;
        }
        if i0.is_nan() || i1.is_nan() {
            return;
        }
        let (i0, i1) = (i0 as usize, i1 as usize);

        // Memoize the bucketing on the visible window + the forming bar (its OHLCV mutates
        // in place on live ticks) + the shape inputs; pans/zooms hit the cache mid-gesture
        // only when the window is unchanged, which is exactly when recompute is waste.
        let Some(last) = bars.get(i1) else { return };
        let key = format!(
            "{}:{}:{}:{}:{}:{}:{}:{}:{}",
            i0,
            i1,
            bars.len(),
            js_number(last.high),
            js_number(last.low),
            js_number(last.close),
            js_number(last.volume.unwrap_or(0.0)),
            data.rows,
            js_number(data.value_area_frac),
        );
        if key != self.memo_key {
            self.memo_key = key;
            self.memo = build_vpvr_profile(
                bars,
                i0 as i64,
                i1 as i64,
                f64::from(data.rows),
                data.value_area_frac,
            );
        }
        let Some(profile) = &self.memo else { return };

        ctx.save();
        ctx.begin_path();
        ctx.rect(0.0, bounds.top, coords.width(), bounds.height); // clip to the price pane's data area
        ctx.clip();
        paint_vpvr(
            ctx,
            profile,
            &VpvrGeom {
                right_x: coords.width(),
                max_w: coords.width() * data.width_frac,
                y_of: &|p| coords.price_to_y(p, &scale, &bounds),
            },
            &VpvrPaintStyle {
                up_color: &data.up_color,
                down_color: &data.down_color,
                show_poc: data.show_poc,
                poc_color: theme_str(theme, "textColor"),
            },
        );
        ctx.restore();
    }
}

impl RendererLayerInstance for VpvrRenderer {
    fn mount(&mut self, canvas: Rc<dyn Canvas2d>) {
        self.surface = Some(PlotSurface::new(canvas, 0.0, 0.0));
    }

    fn render(&mut self, args: &RendererLayerArgs) {
        let data: Option<VpvrLayerData> = args
            .data
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        VpvrRenderer::render(
            self,
            &VpvrRenderArgs {
                bars: args.bars,
                data: data.as_ref(),
                visible: true,
                coords: args.coords,
                scale: args.scale,
                bounds: args.bounds,
                theme: args.theme,
            },
        );
    }

    fn destroy(&mut self) {
        VpvrRenderer::destroy(self);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::i18n::english_translator;
    use crate::render::backend::canvas2d_backend::test_support::{
        frame_start, recording, stream, theme,
    };
    use crate::render::backend::vpvr_profile::VpvrRow;
    use crate::render::canvas::RecordingCanvas;

    const W: f64 = 300.0;
    const H: f64 = 200.0;
    const BOUNDS: PaneBounds = PaneBounds {
        top: 0.0,
        height: H,
    };
    const SCALE: PriceScale = PriceScale {
        min: 95.0,
        max: 110.0,
        log: None,
        invert: None,
    };

    fn data() -> VpvrLayerData {
        VpvrLayerData {
            rows: 10,
            width_frac: 0.3,
            up_color: "#2962FF".into(),
            down_color: "#F7525F".into(),
            show_poc: true,
            value_area_frac: 0.7,
        }
    }

    fn coords(n: usize) -> CoordinateSystem {
        let mut c = CoordinateSystem::new();
        c.set_size(W, H, 1.0);
        let times: Vec<i64> = (0..n as i64).map(|i| i * 60_000).collect();
        c.set_bars(&times);
        c.set_viewport(crate::render::core::ViewportState {
            bar_spacing: 20.0,
            right_offset: 2.0,
        });
        c
    }

    fn up_bar(i: i64, volume: Option<f64>) -> Ohlcv {
        Ohlcv::new(i * 60_000, 100.0, 106.0, 99.0, 105.0, volume)
    }

    fn down_bar(i: i64, volume: f64) -> Ohlcv {
        Ohlcv::new(i * 60_000, 105.0, 106.0, 99.0, 100.0, Some(volume))
    }

    /// A bar that never moves: every volumed bar at one price profiles as a single row.
    fn flat_bar(i: i64, volume: f64) -> Ohlcv {
        Ohlcv::new(i * 60_000, 100.0, 100.0, 100.0, 100.0, Some(volume))
    }

    fn render_with(
        layer: &mut VpvrRenderer,
        canvas: &RecordingCanvas,
        bars: &[Ohlcv],
        data: Option<&VpvrLayerData>,
        visible: bool,
    ) {
        canvas.clear();
        let c = coords(bars.len());
        layer.render(&VpvrRenderArgs {
            bars,
            data,
            visible,
            coords: &c,
            scale: SCALE,
            bounds: BOUNDS,
            theme: &theme(),
        });
    }

    fn render(
        bars: &[Ohlcv],
        data: Option<&VpvrLayerData>,
        visible: bool,
    ) -> std::rc::Rc<RecordingCanvas> {
        let canvas = recording();
        let mut layer = VpvrRenderer::new();
        layer.mount(canvas.clone(), W, H);
        render_with(&mut layer, &canvas, bars, data, visible);
        canvas
    }

    /// The pixel y of a price on the 95..110 scale over the 200 px pane.
    fn price_y(p: f64) -> f64 {
        0.0 + 200.0 * (1.0 - (p - 95.0) / (110.0 - 95.0))
    }

    fn rects(canvas: &RecordingCanvas, name: &str) -> Vec<(f64, f64, f64, f64)> {
        canvas
            .calls(name)
            .iter()
            .map(|r| {
                (
                    r[1].as_f64().unwrap(),
                    r[2].as_f64().unwrap(),
                    r[3].as_f64().unwrap(),
                    r[4].as_f64().unwrap(),
                )
            })
            .collect()
    }

    // ── the pure painter ──

    #[test]
    fn the_painter_draws_rows_from_its_geometry_in_this_exact_stream() {
        let profile = VpvrProfile {
            rows: vec![
                VpvrRow {
                    price: 100.0,
                    up: 30.0,
                    down: 10.0,
                },
                VpvrRow {
                    price: 101.0,
                    up: 0.0,
                    down: 0.0,
                }, // empty: skipped
                VpvrRow {
                    price: 102.0,
                    up: 5.0,
                    down: 15.0,
                },
            ],
            row_h: 1.0,
            min: 100.0,
            max_total: 40.0,
            poc: 0,
            va_from: 0,
            va_to: 1,
        };
        let canvas = recording();
        paint_vpvr(
            &*canvas,
            &profile,
            &VpvrGeom {
                right_x: 300.0,
                max_w: 90.0,
                y_of: &|p| 200.0 - p,
            },
            &VpvrPaintStyle {
                up_color: "#up",
                down_color: "#down",
                show_poc: true,
                poc_color: "#poc",
            },
        );
        // Row 0 (total 40 = the largest): 90 px wide, 30/40 of it up: 67.5. One pixel tall
        // (y(101) = 99, y(100) = 100: 100 - 99 - 1 = 0, raised to 1). Inside the value area:
        // 0.62. Row 2 (total 20): half the width, outside the value area: 0.28.
        assert_eq!(
            canvas.to_json(),
            json!([
                ["globalAlpha", 0.62],
                ["fillStyle", "#up"],
                ["fillRect", 210.0, 99.0, 67.5, 1.0],
                ["fillStyle", "#down"],
                ["fillRect", 277.5, 99.0, 22.5, 1.0],
                ["globalAlpha", 0.28],
                ["fillStyle", "#up"],
                ["fillRect", 255.0, 97.0, 11.25, 1.0],
                ["fillStyle", "#down"],
                ["fillRect", 266.25, 97.0, 33.75, 1.0],
                ["globalAlpha", 0.9],
                ["strokeStyle", "#poc"],
                ["lineWidth", 1.0],
                ["strokeRect", 210.5, 99.5, 89.0, 1.0],
                ["globalAlpha", 1.0],
            ])
        );
    }

    #[test]
    fn the_painter_skips_the_poc_outline_when_off_and_draws_a_pure_up_row_without_a_down_part() {
        let profile = VpvrProfile {
            rows: vec![VpvrRow {
                price: 100.0,
                up: 8.0,
                down: 0.0,
            }],
            row_h: 1.0,
            min: 100.0,
            max_total: 8.0,
            poc: 0,
            va_from: 0,
            va_to: 0,
        };
        let canvas = recording();
        paint_vpvr(
            &*canvas,
            &profile,
            &VpvrGeom {
                right_x: 100.0,
                max_w: 40.0,
                y_of: &|p| 50.0 - p,
            },
            &VpvrPaintStyle {
                up_color: "#up",
                down_color: "#down",
                show_poc: false,
                poc_color: "#poc",
            },
        );
        assert_eq!(
            canvas.to_json(),
            json!([
                ["globalAlpha", 0.62],
                ["fillStyle", "#up"],
                ["fillRect", 60.0, -51.0, 40.0, 1.0],
                ["globalAlpha", 1.0],
            ])
        );
    }

    #[test]
    fn the_painter_draws_nothing_without_a_scale() {
        let profile = VpvrProfile {
            rows: vec![VpvrRow {
                price: 1.0,
                up: 1.0,
                down: 0.0,
            }],
            row_h: 1.0,
            min: 1.0,
            max_total: 0.0,
            poc: 0,
            va_from: 0,
            va_to: 0,
        };
        let canvas = recording();
        let geom = |max_w| VpvrGeom {
            right_x: 10.0,
            max_w,
            y_of: &|p| p,
        };
        let style = VpvrPaintStyle {
            up_color: "#u",
            down_color: "#d",
            show_poc: true,
            poc_color: "#p",
        };
        paint_vpvr(&*canvas, &profile, &geom(10.0), &style); // no volume scale
        let with_total = VpvrProfile {
            max_total: 1.0,
            ..profile.clone()
        };
        paint_vpvr(&*canvas, &with_total, &geom(0.0), &style); // no width
        assert!(canvas.is_empty());
        // A POC index off the rows (a corrupt profile) draws no outline instead of panicking.
        let off = VpvrProfile {
            poc: 5,
            ..with_total
        };
        paint_vpvr(&*canvas, &off, &geom(10.0), &style);
        assert_eq!(canvas.calls("strokeRect").len(), 0);
    }

    // ── the layer ──

    #[test]
    fn a_flat_window_paints_a_one_row_profile_in_this_exact_stream() {
        // Both volumed bars sit at 100: one row [100, 101), 100 + 100 up volume, the widest
        // possible row (30% of the 300 px plot = 90 px), which is also the POC.
        let bars = [flat_bar(0, 100.0), flat_bar(1, 100.0)];
        let canvas = render(&bars, Some(&data()), true);
        let y_top = price_y(101.0);
        let h = js_max(1.0, price_y(100.0) - y_top - 1.0);
        assert_eq!(
            canvas.to_json(),
            stream(vec![
                frame_start(1.0, W, H),
                vec![
                    json!(["save"]),
                    json!(["beginPath"]),
                    json!(["rect", 0.0, 0.0, W, H]),
                    json!(["clip"]),
                    json!(["globalAlpha", VPVR_VA_ALPHA]),
                    json!(["fillStyle", "#2962FF"]),
                    json!(["fillRect", 210.0, y_top, 90.0, h]),
                    json!(["globalAlpha", VPVR_POC_ALPHA]),
                    json!(["strokeStyle", "#cccccc"]),
                    json!(["lineWidth", 1.0]),
                    json!(["strokeRect", 210.5, y_top + 0.5, 89.0, h]),
                    json!(["globalAlpha", 1.0]),
                    json!(["restore"]),
                ],
            ])
        );
    }

    #[test]
    fn rows_hug_the_right_edge_and_stay_within_the_width_fraction() {
        let canvas = render(
            &[up_bar(0, Some(100.0)), up_bar(1, Some(100.0))],
            Some(&data()),
            true,
        );
        let fills = rects(&canvas, "fillRect");
        assert!(!fills.is_empty());
        let max_w = W * 0.3;
        for (x, _, w, _) in &fills {
            assert!(x + w <= W + 1e-6);
            assert!(*w <= max_w + 1e-6);
        }
        let right = fills.iter().map(|f| f.0 + f.2).fold(f64::MIN, f64::max);
        assert!((right - W).abs() < 1e-6);
    }

    #[test]
    fn the_poc_row_is_outlined_across_the_full_profile_width_and_omitted_when_off() {
        let bars = [up_bar(0, Some(100.0)), up_bar(1, Some(100.0))];
        let strokes = rects(&render(&bars, Some(&data()), true), "strokeRect");
        assert_eq!(strokes.len(), 1);
        assert!((strokes[0].2 - (W * 0.3 - 1.0)).abs() < 1e-6);
        let canvas = render(&bars, Some(&data()), true);
        assert_eq!(
            canvas.calls("strokeStyle"),
            vec![json!(["strokeStyle", "#cccccc"])]
        );
        let off = VpvrLayerData {
            show_poc: false,
            ..data()
        };
        assert!(rects(&render(&bars, Some(&off), true), "strokeRect").is_empty());
    }

    #[test]
    fn each_row_splits_into_an_up_part_left_of_a_down_part_against_the_axis() {
        // Equal up/down volume over one range: each row = two segments of equal width.
        let canvas = render(
            &[up_bar(0, Some(100.0)), down_bar(1, 100.0)],
            Some(&data()),
            true,
        );
        let mut ups = Vec::new();
        let mut downs = Vec::new();
        let mut style = String::new();
        for op in canvas.ops() {
            match op[0].as_str().unwrap() {
                "fillStyle" => style = op[1].as_str().unwrap().to_string(),
                "fillRect" => {
                    let r = (
                        op[1].as_f64().unwrap(),
                        op[2].as_f64().unwrap(),
                        op[3].as_f64().unwrap(),
                    );
                    if style == "#2962FF" {
                        ups.push(r);
                    } else {
                        downs.push(r);
                    }
                }
                _ => {}
            }
        }
        assert!(!ups.is_empty());
        assert_eq!(ups.len(), downs.len());
        for d in &downs {
            assert!((d.0 + d.2 - W).abs() < 1e-6); // down hugs the axis
        }
        for u in &ups {
            let mate = downs.iter().find(|d| (d.1 - u.1).abs() < 1e-6).unwrap();
            assert!((u.0 + u.2 - mate.0).abs() < 1e-6);
            assert!((u.2 - mate.2).abs() < 1e-6);
        }
    }

    #[test]
    fn nothing_paints_when_hidden_unconfigured_volumeless_empty_or_the_pane_has_no_height() {
        let cleared = Value::Array(frame_start(1.0, W, H));
        assert_eq!(
            render(&[up_bar(0, Some(80.0))], Some(&data()), false).to_json(),
            cleared
        );
        assert_eq!(
            render(&[up_bar(0, Some(80.0))], None, true).to_json(),
            cleared
        );
        assert_eq!(
            render(&[up_bar(0, None)], Some(&data()), true).to_json(),
            cleared
        );
        assert_eq!(render(&[], Some(&data()), true).to_json(), cleared);
        let canvas = recording();
        let mut layer = VpvrRenderer::new();
        layer.mount(canvas.clone(), W, H);
        let c = coords(1);
        layer.render(&VpvrRenderArgs {
            bars: &[up_bar(0, Some(80.0))],
            data: Some(&data()),
            visible: true,
            coords: &c,
            scale: SCALE,
            bounds: PaneBounds {
                top: 0.0,
                height: 0.0,
            },
            theme: &theme(),
        });
        assert_eq!(canvas.to_json(), cleared);
    }

    #[test]
    fn the_profile_is_memoized_on_the_visible_window_and_the_forming_bar() {
        let canvas = recording();
        let mut layer = VpvrRenderer::new();
        layer.mount(canvas.clone(), W, H);
        let mut bars = vec![
            up_bar(0, Some(100.0)),
            Ohlcv::new(60_000, 100.0, 102.0, 100.0, 101.0, Some(100.0)),
        ];
        render_with(&mut layer, &canvas, &bars, Some(&data()), true);
        let first = canvas.to_json();
        render_with(&mut layer, &canvas, &bars, Some(&data()), true);
        assert_eq!(
            canvas.to_json(),
            first,
            "an unchanged window repaints the same profile"
        );
        // A live tick mutates the forming bar's volume in place: the key changes, the profile
        // is rebuilt and the painted widths follow.
        bars[1].volume = Some(300.0);
        render_with(&mut layer, &canvas, &bars, Some(&data()), true);
        assert_ne!(canvas.to_json(), first);
        // A different row count rebuilds too.
        let coarse = VpvrLayerData { rows: 3, ..data() };
        render_with(&mut layer, &canvas, &bars, Some(&coarse), true);
        assert!(rects(&canvas, "fillRect").len() <= 6);
    }

    #[test]
    fn destroying_the_layer_stops_painting_and_forgets_the_memo() {
        let canvas = recording();
        let mut layer = VpvrRenderer::new();
        layer.mount(canvas.clone(), W, H);
        let bars = [up_bar(0, Some(100.0))];
        render_with(&mut layer, &canvas, &bars, Some(&data()), true);
        layer.destroy();
        render_with(&mut layer, &canvas, &bars, Some(&data()), true);
        assert!(canvas.is_empty());
    }

    #[test]
    fn the_registry_seam_reads_its_config_from_the_channel_payload() {
        let canvas = recording();
        let mut layer = VpvrRenderer::new();
        RendererLayerInstance::mount(&mut layer, canvas.clone());
        layer.set_backing_size(W, H);
        let bars = [flat_bar(0, 100.0), flat_bar(1, 100.0)];
        let c = coords(2);
        let payload = json!({
            "rows": 10, "widthFrac": 0.3, "upColor": "#2962FF", "downColor": "#F7525F",
            "showPoc": true, "valueAreaFrac": 0.7
        });
        let t = theme();
        let settings = serde_json::Map::new();
        let args = RendererLayerArgs {
            bars: &bars,
            data: Some(&payload),
            settings: &settings,
            pending: &[],
            coords: &c,
            scale: SCALE,
            bounds: BOUNDS,
            theme: &t,
            price_style: "candles",
            now_ms: 0.0,
            cursor: None,
            i18n: english_translator(),
        };
        RendererLayerInstance::render(&mut layer, &args);
        assert_eq!(canvas.calls("strokeRect").len(), 1);
        canvas.clear();
        RendererLayerInstance::render(&mut layer, &RendererLayerArgs { data: None, ..args });
        assert_eq!(canvas.to_json(), Value::Array(frame_start(1.0, W, H)));
        RendererLayerInstance::destroy(&mut layer);
    }
}
