//! The volume layer: its own canvas2d surface mounted BEHIND the data canvas, painting
//! bottom-anchored per-bar volume columns on the price pane. The columns carry their OWN
//! scale (the tallest VISIBLE bar spans `data.height_frac` of the pane), computed here
//! each frame, so volume units never reach the price autoscale. Renderer-owned and
//! ephemeral (not part of the indicator model), like a plugin renderer layer.
//!
//! [`paint_volume`] is the pure painter (`paintVolume.ts`); [`VolumeRenderer`] is the
//! layer (`VolumeRenderer.ts`). The renderer calls [`VolumeRenderer::render`] with a
//! [`VolumeRenderArgs`] each data frame, exactly as the original's `NativeRenderer` does.
//! The renderer-layer seam ([`RendererLayerInstance`]) is implemented too, for a shell
//! that drives every layer through the registry: it reads the layer config from the
//! channel payload (`args.data`, the JSON of `VolumeLayerData`), treats the layer as
//! visible whenever a payload is present, and paints in a price pane unless
//! [`VolumeRenderer::set_fill_pane`] says the indicator moved into its own pane.
//!
//! Left for the browser crate: the original's `mount(canvas)` reads the context and the
//! backing-store size off the element; here they are values ([`PlotSurface`]). Through
//! the registry's `mount(canvas)` only the context arrives, so the shell must follow with
//! [`VolumeRenderer::set_backing_size`] (until it does the clear covers nothing).

// A NaN operand must fail these checks, as the original's `!(a > b)` does.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

use std::rc::Rc;

use crate::indicators::classics::math::{js_max, js_min};
use crate::model::Ohlcv;
use crate::model::volume_layers::VolumeLayerData;
use crate::render::backend::canvas2d_backend::PlotSurface;
use crate::render::backend::layers::{RendererLayerArgs, RendererLayerInstance};
use crate::render::canvas::Canvas2d;
use crate::render::core::plot_direction::begin_plot_frame;
use crate::render::core::{CoordinateSystem, PaneBounds};

/// Fraction of its own (study) pane the tallest visible volume column fills: leaves a
/// sliver of headroom on top. Shared with the renderer's autoscale so the axis labels line
/// up with the columns exactly (a value `v` sits at `(v / max_vol) *
/// VOLUME_PANE_FILL_FRAC` of the pane).
pub const VOLUME_PANE_FILL_FRAC: f64 = 0.96;

/// Fill opacity of the volume columns: the candles behind/above must stay readable.
pub const VOLUME_FILL_ALPHA: f64 = 0.5;

/// Geometry the layer precomputes once per frame (keeps the painter free of the
/// coordinate system, and so unit-testable).
pub struct VolumeGeom<'a> {
    /// Inclusive visible bar-index range into `bars`.
    pub i0: usize,
    pub i1: usize,
    /// Bar index to the pixel x of the bar's center.
    pub x_of: &'a dyn Fn(usize) -> f64,
    /// Half-width (px) of one column (matched to the candle body).
    pub half_w: f64,
    /// Pixel y of the pane's bottom edge (columns grow upward from here).
    pub bottom_y: f64,
    /// Pixel height of the tallest visible bar (the layer's own scale).
    pub max_h: f64,
    /// Largest visible volume (normalization; the caller guarantees > 0).
    pub max_vol: f64,
}

/// Up and down column colors.
pub struct VolumeColors<'a> {
    pub up: &'a str,
    pub down: &'a str,
}

/// Paint the bottom-anchored volume columns: one per visible bar, height proportional to
/// volume relative to the VISIBLE max, colored by bar direction. Pure: every coordinate
/// comes from `geom`.
pub fn paint_volume(ctx: &dyn Canvas2d, bars: &[Ohlcv], geom: &VolumeGeom, colors: &VolumeColors) {
    let VolumeGeom {
        i0,
        i1,
        x_of,
        half_w,
        bottom_y,
        max_h,
        max_vol,
    } = *geom;
    if max_vol <= 0.0 || max_h <= 0.0 {
        return;
    }
    ctx.set_global_alpha(VOLUME_FILL_ALPHA);
    for i in i0..=i1 {
        let Some(b) = bars.get(i) else { continue };
        let Some(v) = b.volume else { continue }; // volume is optional on OHLCV
        if !(v > 0.0) {
            continue;
        }
        let h = js_max(1.0, (v / max_vol) * max_h);
        ctx.set_fill_style(
            &(if b.close >= b.open {
                colors.up
            } else {
                colors.down
            })
            .into(),
        );
        ctx.fill_rect(x_of(i) - half_w, bottom_y - h, half_w * 2.0, h);
    }
    ctx.set_global_alpha(1.0);
}

/// Half-width (px) of one volume column: the candle body's half-width formula, so columns
/// line up under their candles.
fn column_half_width(bar_spacing: f64) -> f64 {
    js_max(0.5, f64::floor(bar_spacing * 0.7) / 2.0)
}

/// One frame's inputs.
pub struct VolumeRenderArgs<'a> {
    pub bars: &'a [Ohlcv],
    /// Layer config pushed by the volume native indicator (`None`: layer off).
    pub data: Option<&'a VolumeLayerData>,
    /// The indicator is mounted and not hidden.
    pub visible: bool,
    pub coords: &'a CoordinateSystem,
    /// The pixel extent of the pane the volume indicator lives in (columns clip to it).
    pub bounds: PaneBounds,
    /// True when the indicator was moved into its own (study) pane: columns fill the pane
    /// instead of hugging `height_frac` of it (the price-pane overlay proportion).
    pub fill_pane: bool,
}

#[derive(Default)]
pub struct VolumeRenderer {
    surface: Option<PlotSurface>,
    fill_pane: bool,
}

impl VolumeRenderer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind to the volume canvas: its 2d context and backing-store size (device pixels).
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

    /// Only for the registry seam: the volume indicator lives in its own study pane.
    pub fn set_fill_pane(&mut self, fill_pane: bool) {
        self.fill_pane = fill_pane;
    }

    pub fn destroy(&mut self) {
        self.surface = None;
    }

    pub fn render(&self, args: &VolumeRenderArgs) {
        let Some(surface) = &self.surface else {
            return;
        };
        let ctx: &dyn Canvas2d = &*surface.ctx;
        let VolumeRenderArgs {
            bars,
            data,
            visible,
            coords,
            bounds,
            fill_pane,
        } = *args;

        let dpr = coords.dpr();
        begin_plot_frame(ctx, dpr);
        ctx.clear_rect(0.0, 0.0, surface.width / dpr, surface.height / dpr); // always clear: a hide/remove must wipe the last frame
        let Some(data) = data else { return };
        if !visible || bars.is_empty() || bounds.height <= 0.0 {
            return;
        }

        // Visible bar-index window (bar index = logical index), padded nowhere: a
        // half-off column at either edge is included via the floor/ceil.
        let r = coords.visible_logical_range();
        let i0 = js_max(0.0, f64::floor(r.from));
        let i1 = js_min((bars.len() - 1) as f64, f64::ceil(r.to));
        if !(i0 <= i1) {
            return;
        }
        let (i0, i1) = (i0 as usize, i1 as usize);

        let mut max_vol = 0.0;
        for b in bars.iter().take(i1 + 1).skip(i0) {
            if let Some(v) = b.volume
                && v > max_vol
            {
                max_vol = v;
            }
        }
        if max_vol <= 0.0 {
            return;
        }

        ctx.save();
        ctx.begin_path();
        ctx.rect(0.0, bounds.top, coords.width(), bounds.height); // clip to the host pane's data area
        ctx.clip();
        paint_volume(
            ctx,
            bars,
            &VolumeGeom {
                i0,
                i1,
                x_of: &|i| coords.logical_to_x(i as f64),
                half_w: column_half_width(coords.body_spacing()),
                bottom_y: bounds.top + bounds.height,
                // In its own pane the tallest visible bar nearly fills the pane; as a
                // price-pane overlay it occupies the configured bottom fraction.
                max_h: bounds.height
                    * if fill_pane {
                        VOLUME_PANE_FILL_FRAC
                    } else {
                        data.height_frac
                    },
                max_vol,
            },
            &VolumeColors {
                up: &data.up_color,
                down: &data.down_color,
            },
        );
        ctx.restore();
    }
}

impl RendererLayerInstance for VolumeRenderer {
    fn mount(&mut self, canvas: Rc<dyn Canvas2d>) {
        self.surface = Some(PlotSurface::new(canvas, 0.0, 0.0));
    }

    fn render(&mut self, args: &RendererLayerArgs) {
        let data: Option<VolumeLayerData> = args
            .data
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        VolumeRenderer::render(
            self,
            &VolumeRenderArgs {
                bars: args.bars,
                data: data.as_ref(),
                visible: true,
                coords: args.coords,
                bounds: args.bounds,
                fill_pane: self.fill_pane,
            },
        );
    }

    fn destroy(&mut self) {
        self.surface = None;
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
    use crate::render::canvas::RecordingCanvas;

    const W: f64 = 300.0;
    const H: f64 = 200.0;
    const BOUNDS: PaneBounds = PaneBounds {
        top: 0.0,
        height: H,
    };

    fn data() -> VolumeLayerData {
        VolumeLayerData {
            up_color: "#11AA11".into(),
            down_color: "#AA1111".into(),
            height_frac: 0.2,
        }
    }

    /// `n` bars at x = 300 - (n + 1 - i) * 20 (pitch 20, 2-bar margin).
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

    fn render(
        bars: &[Ohlcv],
        data: Option<&VolumeLayerData>,
        visible: bool,
        fill_pane: bool,
    ) -> std::rc::Rc<RecordingCanvas> {
        let canvas = recording();
        let mut layer = VolumeRenderer::new();
        layer.mount(canvas.clone(), W, H);
        let c = coords(bars.len());
        layer.render(&VolumeRenderArgs {
            bars,
            data,
            visible,
            coords: &c,
            bounds: BOUNDS,
            fill_pane,
        });
        canvas
    }

    fn fills(canvas: &RecordingCanvas) -> Vec<(f64, f64, f64, f64)> {
        canvas
            .calls("fillRect")
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

    #[test]
    fn the_columns_paint_this_exact_stream() {
        let bars = [
            up_bar(0, Some(50.0)),
            up_bar(1, Some(100.0)),
            down_bar(2, 25.0),
        ];
        let canvas = render(&bars, Some(&data()), true, false);
        // Pitch 20, 2-bar margin: x = 220, 240, 260; the half width is floor(20 * 0.7) / 2
        // = 7. The tallest visible volume (100) spans heightFrac of the pane (40 px); the
        // others scale from it.
        assert_eq!(
            canvas.to_json(),
            stream(vec![
                frame_start(1.0, W, H),
                vec![
                    json!(["save"]),
                    json!(["beginPath"]),
                    json!(["rect", 0.0, 0.0, W, H]),
                    json!(["clip"]),
                    json!(["globalAlpha", 0.5]),
                    json!(["fillStyle", "#11AA11"]),
                    json!(["fillRect", 213.0, 180.0, 14.0, 20.0]),
                    json!(["fillStyle", "#11AA11"]),
                    json!(["fillRect", 233.0, 160.0, 14.0, 40.0]),
                    json!(["fillStyle", "#AA1111"]),
                    json!(["fillRect", 253.0, 190.0, 14.0, 10.0]),
                    json!(["globalAlpha", 1.0]),
                    json!(["restore"]),
                ],
            ])
        );
    }

    #[test]
    fn every_column_is_bottom_anchored_and_proportional_to_volume() {
        let canvas = render(
            &[
                up_bar(0, Some(50.0)),
                up_bar(1, Some(100.0)),
                up_bar(2, Some(25.0)),
            ],
            Some(&data()),
            true,
            false,
        );
        let rects = fills(&canvas);
        assert_eq!(rects.len(), 3);
        for (_, y, _, h) in &rects {
            assert!((y + h - H).abs() < 1e-6);
        }
        let mut heights: Vec<f64> = rects.iter().map(|r| r.3).collect();
        heights.sort_by(f64::total_cmp);
        let tallest = heights[2];
        assert!((tallest - H * 0.2).abs() < 1e-6);
        assert!((heights[0] - tallest / 4.0).abs() < 1e-6);
        assert!((heights[1] - tallest / 2.0).abs() < 1e-6);
    }

    #[test]
    fn columns_are_colored_by_direction_and_painted_translucent() {
        let canvas = render(
            &[up_bar(0, Some(50.0)), down_bar(1, 50.0)],
            Some(&data()),
            true,
            false,
        );
        assert_eq!(
            canvas.calls("fillStyle"),
            vec![
                json!(["fillStyle", "#11AA11"]),
                json!(["fillStyle", "#AA1111"])
            ]
        );
        assert_eq!(
            canvas.calls("globalAlpha")[0],
            json!(["globalAlpha", VOLUME_FILL_ALPHA])
        );
    }

    #[test]
    fn bars_without_volume_are_skipped_and_a_tiny_volume_keeps_one_pixel() {
        let canvas = render(
            &[up_bar(0, None), up_bar(1, Some(80.0))],
            Some(&data()),
            true,
            false,
        );
        assert_eq!(fills(&canvas).len(), 1);
        // 1 of 1_000_000: far under a pixel, floored at 1.
        let canvas = render(
            &[up_bar(0, Some(1.0)), up_bar(1, Some(1_000_000.0))],
            Some(&data()),
            true,
            false,
        );
        assert_eq!(fills(&canvas)[0].3, 1.0);
        // A zero volume is skipped like a missing one.
        let canvas = render(
            &[up_bar(0, Some(0.0)), up_bar(1, Some(8.0))],
            Some(&data()),
            true,
            false,
        );
        assert_eq!(fills(&canvas).len(), 1);
    }

    #[test]
    fn nothing_paints_when_hidden_unconfigured_volumeless_empty_or_the_pane_has_no_height() {
        let cleared = Value::Array(frame_start(1.0, W, H));
        let bars = [up_bar(0, Some(80.0))];
        assert_eq!(
            render(&bars, Some(&data()), false, false).to_json(),
            cleared
        ); // the legend eye
        assert_eq!(render(&bars, None, true, false).to_json(), cleared); // layer off
        assert_eq!(
            render(&[up_bar(0, None)], Some(&data()), true, false).to_json(),
            cleared
        );
        assert_eq!(render(&[], Some(&data()), true, false).to_json(), cleared);
        let canvas = recording();
        let mut layer = VolumeRenderer::new();
        layer.mount(canvas.clone(), W, H);
        let c = coords(1);
        layer.render(&VolumeRenderArgs {
            bars: &bars,
            data: Some(&data()),
            visible: true,
            coords: &c,
            bounds: PaneBounds {
                top: 0.0,
                height: 0.0,
            },
            fill_pane: false,
        });
        assert_eq!(canvas.to_json(), cleared);
    }

    #[test]
    fn in_its_own_pane_the_tallest_column_nearly_fills_the_pane() {
        let canvas = render(
            &[up_bar(0, Some(50.0)), up_bar(1, Some(100.0))],
            Some(&data()),
            true,
            true,
        );
        let tallest = fills(&canvas).iter().map(|r| r.3).fold(0.0, f64::max);
        assert!((tallest - H * 0.96).abs() < 1e-6);
    }

    #[test]
    fn the_clip_follows_the_host_pane_and_columns_anchor_to_its_bottom() {
        let canvas = recording();
        let mut layer = VolumeRenderer::new();
        layer.mount(canvas.clone(), W, H);
        let bars = [up_bar(0, Some(10.0))];
        let c = coords(1);
        layer.render(&VolumeRenderArgs {
            bars: &bars,
            data: Some(&data()),
            visible: true,
            coords: &c,
            bounds: PaneBounds {
                top: 100.0,
                height: 100.0,
            },
            fill_pane: true,
        });
        assert_eq!(
            canvas.calls("rect"),
            vec![json!(["rect", 0.0, 100.0, W, 100.0])]
        );
        // x = 300 - 2 * 20 = 260; the lone bar is the tallest: 96 px from the pane bottom.
        assert_eq!(
            canvas.calls("fillRect"),
            vec![json!(["fillRect", 253.0, 200.0 - 96.0, 14.0, 96.0])]
        );
    }

    #[test]
    fn a_zero_visible_volume_paints_nothing_and_the_painter_guards_its_own_scale() {
        let canvas = recording();
        let x_of = |_: usize| 0.0;
        paint_volume(
            &*canvas,
            &[up_bar(0, Some(5.0))],
            &VolumeGeom {
                i0: 0,
                i1: 0,
                x_of: &x_of,
                half_w: 1.0,
                bottom_y: 10.0,
                max_h: 0.0,
                max_vol: 5.0,
            },
            &VolumeColors {
                up: "#u",
                down: "#d",
            },
        );
        paint_volume(
            &*canvas,
            &[up_bar(0, Some(5.0))],
            &VolumeGeom {
                i0: 0,
                i1: 0,
                x_of: &x_of,
                half_w: 1.0,
                bottom_y: 10.0,
                max_h: 4.0,
                max_vol: 0.0,
            },
            &VolumeColors {
                up: "#u",
                down: "#d",
            },
        );
        assert!(canvas.is_empty());
    }

    #[test]
    fn the_pure_painter_draws_from_its_geometry_alone() {
        let canvas = recording();
        let bars = [up_bar(0, Some(10.0)), down_bar(1, 5.0), up_bar(2, None)];
        paint_volume(
            &*canvas,
            &bars,
            &VolumeGeom {
                i0: 0,
                i1: 3, // past the slice: a hole
                x_of: &|i| 10.0 * i as f64,
                half_w: 2.0,
                bottom_y: 50.0,
                max_h: 20.0,
                max_vol: 10.0,
            },
            &VolumeColors {
                up: "#u",
                down: "#d",
            },
        );
        assert_eq!(
            canvas.to_json(),
            json!([
                ["globalAlpha", 0.5],
                ["fillStyle", "#u"],
                ["fillRect", -2.0, 30.0, 4.0, 20.0],
                ["fillStyle", "#d"],
                ["fillRect", 8.0, 40.0, 4.0, 10.0],
                ["globalAlpha", 1.0],
            ])
        );
    }

    #[test]
    fn the_registry_seam_reads_its_config_from_the_channel_payload() {
        let canvas = recording();
        let mut layer = VolumeRenderer::new();
        RendererLayerInstance::mount(&mut layer, canvas.clone());
        layer.set_backing_size(W, H);
        let bars = [up_bar(0, Some(50.0)), down_bar(1, 100.0)];
        let c = coords(2);
        let payload = json!({ "upColor": "#11AA11", "downColor": "#AA1111", "heightFrac": 0.2 });
        let t = theme();
        let settings = serde_json::Map::new();
        let args = RendererLayerArgs {
            bars: &bars,
            data: Some(&payload),
            settings: &settings,
            pending: &[],
            coords: &c,
            scale: crate::render::core::PriceScale::new(95.0, 110.0),
            bounds: BOUNDS,
            theme: &t,
            price_style: "candles",
            now_ms: 0.0,
            cursor: None,
            i18n: english_translator(),
        };
        RendererLayerInstance::render(&mut layer, &args);
        assert_eq!(canvas.calls("fillRect").len(), 2);
        // No payload yet (before the first push): the layer clears and paints nothing.
        canvas.clear();
        RendererLayerInstance::render(&mut layer, &RendererLayerArgs { data: None, ..args });
        assert_eq!(canvas.to_json(), Value::Array(frame_start(1.0, W, H)));
        assert!(!RendererLayerInstance::animating(&layer));
        RendererLayerInstance::destroy(&mut layer);
        canvas.clear();
        layer.render(&VolumeRenderArgs {
            bars: &bars,
            data: Some(&data()),
            visible: true,
            coords: &c,
            bounds: BOUNDS,
            fill_pane: false,
        });
        assert!(canvas.is_empty());
    }
}
