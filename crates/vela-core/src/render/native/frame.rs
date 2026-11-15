//! The frame: `renderFrame`, `paintData` and the layers it drives (SDK layers,
//! volume, VPVR, backdrop, geometry backend, chrome, drawings, crosshair), plus the
//! intro reveal and the forming-bar glide.

use std::collections::HashMap;
use std::rc::Rc;

use serde_json::Map;

use crate::i18n::{Translator, english_translator};
use crate::indicators::classics::math::{js_max, js_min};
use crate::model::{IndicatorModel, Ohlcv, PaneKind};
use crate::options::IntroStyle;
use crate::ports::chart_renderer::VelaTheme;
use crate::render::backend::layers::{
    BasePaintingModulation, CursorPoint, LayerPlacement as BackendPlacement, RendererLayerArgs,
    fold_base_modulation,
};
use crate::render::backend::render_backend::RenderBackend;
use crate::render::chrome::chrome_renderer::FrameClock;
use crate::render::chrome::crosshair_renderer::ExternalCrosshair;
use crate::render::chrome::marks_paint::{MarkIcons, NoMarkIcons};
use crate::render::core::layer_stacking::{
    LayerPlacement as StackPlacement, LayerStackEntry, stack_layers,
};
use crate::render::core::scene_graph::PaneNode;
use crate::render::core::scheduler::{InvalidateLevel, repaints_chrome, repaints_data};
use crate::render::core::{CoordinateSystem, PaneBounds, PriceScale, SceneGraph, ease_toward};
use crate::render::drawings::indicator_slices::{SliceSurface, SliceSurfaceFactory, merge_slices};
use crate::render::volume::VolumeRenderArgs;
use crate::render::vpvr::VpvrRenderArgs;

use super::SeriesLook;
use super::host::{HostCanvas, MarkPopoverAction, NativeHost};
use super::state::{ExtLayer, INTRO_MODEL_FADE_MS, PRICE_PANE_ID, State};
use super::user_drawings::FrameView;
use super::util::{clamp01, theme_string};

/// The slice canvases come from the host.
pub(crate) struct HostSlices(pub Rc<dyn NativeHost>);

impl SliceSurfaceFactory for HostSlices {
    fn create_surface(&self) -> Rc<dyn SliceSurface> {
        self.0.create_slice_surface()
    }
}

/// The mounted native indicator that OWNS an SDK layer entry. A dedicated entry is
/// owned by the instance it was mounted for; the base entry by the mounted
/// indicator whose type equals the layer id and that has no channel of its own.
fn layer_owner<'a>(scene: &'a SceneGraph, l: &ExtLayer) -> Option<&'a IndicatorModel> {
    if let Some(owner) = &l.owner {
        return scene.indicators.get(owner);
    }
    scene.indicators.values().find(|m| {
        m.native
            .as_ref()
            .is_some_and(|n| n.indicator_type == l.def.id && n.channel.is_none())
    })
}

/// The pane an SDK layer paints on: its owner's pane, else the price pane.
fn layer_pane<'a>(scene: &'a SceneGraph, l: &ExtLayer) -> Option<&'a PaneNode> {
    let pane_id = layer_owner(scene, l)
        .and_then(|o| o.pane_id.as_deref())
        .unwrap_or(PRICE_PANE_ID);
    scene.panes.get(pane_id)
}

/// A chart type IS the price series, so "hide chart" blanks its layer along with the
/// bars; an indicator's layer is independent content and keeps painting.
fn layer_hidden_with_candles(scene: &SceneGraph, l: &ExtLayer) -> bool {
    scene.candles_hidden && l.owner.is_none() && l.def.id == scene.price_style
}

fn placement_of(p: BackendPlacement) -> StackPlacement {
    match p {
        BackendPlacement::BelowData => StackPlacement::BelowData,
        BackendPlacement::AboveData => StackPlacement::AboveData,
    }
}

impl State {
    pub(crate) fn render_frame(&mut self, level: InvalidateLevel) {
        if !self.mounted || self.canvases.is_none() {
            return;
        }
        // While a gesture animates, the animator's loop owns the frame (it paints the
        // data layers and the crosshair every tick and picks up live data and scale
        // changes), so a concurrent scheduler invalidation must NOT also paint.
        if self.anim.active() {
            return;
        }
        if repaints_data(level) {
            self.compute_scales();
            self.paint_data();
        } else if repaints_chrome(level) && self.painted_data {
            // Chrome-only tier (the countdown's wall-clock tick): neither data nor
            // viewport changed, so repaint just the chrome canvas over the frame's
            // existing scales.
            self.render_chrome();
            self.track_mark_popover();
        }
        self.render_crosshair_layer();
        // Hover-testing SDK layers follow pointer moves too.
        if !repaints_data(level) && self.painted_data {
            self.repaint_cursor_layers();
        }
        self.update_legend_values();
    }

    fn render_chrome(&mut self) {
        let clock = FrameClock {
            now_ms: self.host.frame_now_ms(),
            wall_ms: self.host.wall_ms(),
        };
        let icons: Rc<dyn MarkIcons> = self
            .host
            .mark_icons()
            .unwrap_or_else(|| Rc::new(NoMarkIcons));
        let surface = self.axis_surface();
        self.chrome.render(
            &self.scene,
            &self.coords,
            &self.theme,
            Some(&surface),
            &clock,
            &*icons,
        );
    }

    /// Repaint the L2 crosshair canvas and tell the host where the veil went.
    pub(crate) fn render_crosshair_layer(&mut self) {
        let external = self.external_cross_px();
        self.crosshair_layer.render(
            &self.scene,
            &self.coords,
            &self.theme,
            self.hover_separator_y,
            external.as_ref(),
        );
        let area = self
            .crosshair_layer
            .shade_right_area()
            .map(|a| (a.x, a.width, a.height));
        if area != self.shade_sent {
            self.shade_sent = area;
            self.host.shade_right(area);
        }
    }

    /// Repaint the SDK layers that opted into cursor tracking (their own canvas only).
    fn repaint_cursor_layers(&mut self) {
        let Some(pane) = self.scene.panes.get(PRICE_PANE_ID) else {
            return;
        };
        let now_ms = self.host.frame_now_ms();
        let empty = Map::new();
        let animate = self.anim_zoom.on();
        let mut start_anim = false;
        for l in self.ext_layers.iter_mut() {
            if l.def.repaint_on_cursor != Some(true) {
                continue;
            }
            let lp = layer_pane(&self.scene, l).unwrap_or(pane);
            // Blanked by the data frame: nothing to hover.
            if lp.collapsed || layer_hidden_with_candles(&self.scene, l) {
                continue;
            }
            let args = ext_layer_args(
                &self.scene,
                &self.coords,
                &self.theme,
                self.i18n.as_deref(),
                self.last_pointer,
                &l.channel,
                &l.def.id,
                lp.scale,
                lp.bounds,
                now_ms,
                &empty,
            );
            l.instance.render(&args);
            if animate && l.instance.animating() {
                start_anim = true;
            }
        }
        if start_anim {
            self.anim.start();
        }
    }

    /// Blank one SDK layer canvas (a collapsed host pane suppresses the layer's painting).
    fn clear_layer_canvas(&self, canvas: &Rc<dyn HostCanvas>) {
        let (bw, bh) = self.backing;
        if bw == 0.0 || bh == 0.0 {
            return;
        }
        canvas.context().clear_rect(0.0, 0.0, bw, bh);
    }

    /// Paint the below-data (L-1) + geometry (L0) + chrome (L1) layers from the
    /// current scene and coordinates.
    pub(crate) fn paint_data(&mut self) {
        if self.canvases.is_none() {
            return;
        }
        // Layer-canvas stacking follows the indicators' z keys: recomputed lazily here
        // so every path that can move them is covered without its own call site.
        self.sync_layer_canvas_order();
        self.sync_legend_order(); // catches any path that moved a key without its own call
        self.stamp_scale_invert(); // flip axes (if inverted) before any layer reads pane.scale
        self.backend.set_model_alpha(self.model_alpha);
        self.backend.set_candle_body_alpha(self.candle_body_alpha);
        self.backend
            .set_candle_structure_alpha(self.candle_structure_alpha);
        let mut grid_alpha = 1.0; // the backdrop's gridline opacity (layers may fade it)
        self.backend.set_candle_body_scale(1.0);
        if let Some(pane) = self.scene.panes.get(PRICE_PANE_ID) {
            let (pane_scale, pane_bounds, pane_collapsed) =
                (pane.scale, pane.bounds, pane.collapsed);
            // The volume layer follows its indicator's pane; a collapsed host pane shows
            // its legend strip only, so the columns are suppressed.
            let (vol_bounds, vol_collapsed, vol_kind) = match self.native_layer_pane("volume") {
                Some(p) => (p.bounds, p.collapsed, p.kind),
                None => (pane_bounds, pane_collapsed, PaneKind::Price),
            };
            self.volume.render(&VolumeRenderArgs {
                bars: &self.scene.bars,
                data: self.scene.volume_layer.as_ref(),
                visible: self.volume_active && !self.volume_hidden && !vol_collapsed,
                coords: &self.coords,
                bounds: vol_bounds,
                fill_pane: vol_kind != PaneKind::Price,
            });
            self.vpvr.render(&VpvrRenderArgs {
                bars: &self.scene.bars,
                data: self.scene.vpvr_layer.as_ref(),
                visible: self.vpvr_active && !self.vpvr_hidden,
                coords: &self.coords,
                scale: pane_scale,
                bounds: pane_bounds,
                theme: &self.theme,
            });
            // SDK renderer layers: shared paint cycle, own channel data, own canvas. Each
            // paints on its OWNING indicator's pane.
            let now_ms = self.host.frame_now_ms();
            let empty = Map::new();
            let animate = self.anim_zoom.on();
            let mut folded: Option<BasePaintingModulation> = None;
            let mut start_anim = false;
            let mut to_clear: Vec<Rc<dyn HostCanvas>> = Vec::new();
            let price = self.scene.panes.get(PRICE_PANE_ID);
            for l in self.ext_layers.iter_mut() {
                let Some(price) = price else { break };
                let lp = layer_pane(&self.scene, l).unwrap_or(price);
                // A collapsed host pane shows its legend strip only, and a hidden price
                // series takes its chart type's layer with it: blank the layer for the
                // duration, and skip the modulation fold too.
                if lp.collapsed || layer_hidden_with_candles(&self.scene, l) {
                    to_clear.push(l.canvas.clone());
                    continue;
                }
                let args = ext_layer_args(
                    &self.scene,
                    &self.coords,
                    &self.theme,
                    self.i18n.as_deref(),
                    self.last_pointer,
                    &l.channel,
                    &l.def.id,
                    lp.scale,
                    lp.bounds,
                    now_ms,
                    &empty,
                );
                l.instance.render(&args);
                // Any mounted layer may dim or slim the base painting: folded this same
                // frame. Only layers ON the price pane get a say.
                if lp.id == price.id {
                    folded = fold_base_modulation(folded, l.instance.modulate_base(&args));
                }
                // A pulsing/fading layer keeps the animator alive; it stops itself when done.
                if animate && l.instance.animating() {
                    start_anim = true;
                }
            }
            for canvas in &to_clear {
                self.clear_layer_canvas(canvas);
            }
            if start_anim {
                self.anim.start();
            }
            if let Some(folded) = folded {
                if let Some(s) = folded.candle_body_scale {
                    let v = clamp01(s);
                    self.backend
                        .set_candle_body_scale(if v == 0.0 || v.is_nan() { 0.01 } else { v });
                }
                if let Some(a) = folded.candle_body_alpha {
                    self.backend
                        .set_candle_body_alpha(clamp01(a) * self.candle_body_alpha);
                }
                if let Some(g) = folded.grid_alpha {
                    grid_alpha = clamp01(g);
                }
            }
        }
        // Live-bar easing: render the eased (gliding) OHLC for the forming bar, then
        // restore the true values immediately so every reader sees real data.
        let live = self.live_ease_override();
        if let Some((li, eased)) = &live
            && let Some(slot) = self.scene.bars.get_mut(*li)
        {
            *slot = *eased;
        }

        // Interleave layers: each indicator's Pine drawings at that indicator's z slot,
        // plus the user drawings whose z sits inside a pane's series stack.
        let ref_size = (self.backing.0 as u32, self.backing.1 as u32);
        let factory = HostSlices(self.host.clone());
        let indicator = self.indicator_slices.prepare(
            &self.scene,
            &self.coords,
            &self.theme,
            ref_size,
            &factory,
        );
        let user = match self.user_drawings.clone() {
            Some(hook) => {
                let view = self.frame_view();
                hook.prepare_slices(&self.scene.ordered_pane_ids(), &view)
            }
            None => HashMap::new(),
        };
        self.scene.drawing_slices = merge_slices(&indicator, &user);
        self.backdrop
            .render(&self.scene, &self.coords, &self.theme, grid_alpha);
        self.backend.render(&self.scene, &self.coords, &self.theme);
        self.render_chrome();
        self.track_mark_popover();
        self.render_user_drawings(); // L1.5: above the chrome, below the crosshair

        if let Some((li, _)) = live
            && let (Some(slot), Some(actual)) = (self.scene.bars.get_mut(li), self.bars.get(li))
        {
            *slot = *actual; // restore the true forming bar
        }
        self.painted_data = true; // scales are real from here on: the chrome-only tier may run
    }

    /// What a frame hands the user-drawings hook.
    pub(crate) fn frame_view(&self) -> FrameView<'_> {
        FrameView {
            scene: &self.scene,
            coords: &self.coords,
            theme: &self.theme,
            drawings: self.canvases.as_ref().map(|c| c.drawings.context()),
            backing: self.backing,
            series_look: self.series_look(),
            i18n: self.i18n.clone(),
        }
    }

    /// Repaint the user-drawings layer alone (hover, ghost, drag, selection: nothing the
    /// data layers depend on). A no-op until the first data frame has run.
    pub(crate) fn render_user_drawings(&self) {
        if !self.painted_data {
            return;
        }
        if let Some(hook) = self.user_drawings.clone() {
            hook.render(&self.frame_view());
        }
    }

    /// The look the price series actually paints with.
    pub(crate) fn series_look(&self) -> SeriesLook {
        let s = &self.scene.style;
        let up = theme_string(&self.theme, "upColor");
        let down = theme_string(&self.theme, "downColor");
        let paint = crate::render::core::chart_config::effective_candle_paint(
            &s.candle,
            self.scene.candle_override.as_ref(),
            &up,
            &down,
        );
        let style = self.scene.price_style.clone();
        let bars_up = s.bars.up_color.clone().unwrap_or_else(|| up.clone());
        let bars_down = s.bars.down_color.clone().unwrap_or_else(|| down.clone());
        let line_color = match style.as_str() {
            "area" => s.area.line_color.clone().unwrap_or_else(|| up.clone()),
            "columns" => crate::render::core::color::with_alpha(
                s.columns.up_color.as_deref().unwrap_or(&up),
                1.0,
            ),
            _ => s.line.color.clone().unwrap_or_else(|| up.clone()),
        };
        SeriesLook {
            up_color: if style == "bars" { bars_up } else { paint.up },
            down_color: if style == "bars" {
                bars_down
            } else {
                paint.down
            },
            line_color,
            style,
        }
    }

    /// The forming bar with its eased high/low/close, when the glide is displaying a
    /// value other than the actual one.
    fn live_ease_override(&self) -> Option<(usize, Ohlcv)> {
        let li = self.bars.len().checked_sub(1)?;
        let actual = self.bars.get(li)?;
        let ease = self.live_ease_time == actual.time
            && (actual.high != self.live_ease_high
                || actual.low != self.live_ease_low
                || actual.close != self.live_ease_close);
        if !ease || self.scene.bars.len() != self.bars.len() {
            return None;
        }
        Some((
            li,
            Ohlcv {
                high: self.live_ease_high,
                low: self.live_ease_low,
                close: self.live_ease_close,
                ..*actual
            },
        ))
    }

    /// After a chrome frame: keep the open mark popup on its glyph, or close it once
    /// the glyph is gone.
    pub(crate) fn track_mark_popover(&mut self) {
        let Some(key) = self.mark_popover_key.clone() else {
            return;
        };
        let anchor = self
            .chrome
            .mark_glyph_by_key(&key)
            .filter(|g| !(g.decked && g.depth != 0))
            .map(|g| (g.x, g.y, g.size));
        match anchor {
            Some(a) => self.host.mark_popover(&MarkPopoverAction::Track(Some(a))),
            None => self.close_mark_popover(),
        }
    }

    pub(crate) fn close_mark_popover(&mut self) {
        if self.mark_popover_key.take().is_some() {
            self.host.mark_popover(&MarkPopoverAction::Close);
            self.scene.marks_active_key = None;
            self.invalidate(InvalidateLevel::Chrome);
        }
    }

    // ── SDK layers ──

    /// The SDK layer canvases split around the data canvas, each side back to front:
    /// owned layers by their owner's z key against the candles', unowned by declared
    /// placement.
    fn ordered_layer_channels(&self) -> (Vec<String>, Vec<String>) {
        let entries: Vec<LayerStackEntry> = self
            .ext_layers
            .iter()
            .map(|l| LayerStackEntry {
                id: l.channel.clone(),
                placement: placement_of(l.def.placement()),
                owner_z: layer_owner(&self.scene, l).map(|o| self.scene.z_of(&o.id)),
            })
            .collect();
        let stacked = stack_layers(&entries, self.scene.candle_z);
        (stacked.below, stacked.above)
    }

    /// The SDK layer images split around the data canvas, for the screenshot compositor.
    pub(crate) fn ordered_layer_canvases_for_pile(
        &self,
    ) -> (
        Vec<crate::render::canvas::CanvasImage>,
        Vec<crate::render::canvas::CanvasImage>,
    ) {
        let (below, above) = self.ordered_layer_channels();
        let image = |channel: &String| {
            self.ext_layers
                .iter()
                .find(|l| &l.channel == channel)
                .map(|l| l.canvas.image())
        };
        (
            below.iter().filter_map(image).collect(),
            above.iter().filter_map(image).collect(),
        )
    }

    /// Re-slot the SDK layer canvases when the computed order changed. Runs at the
    /// top of every data frame; a no-op when the signature is unchanged.
    fn sync_layer_canvas_order(&mut self) {
        if self.ext_layers.is_empty() {
            return;
        }
        let (below, above) = self.ordered_layer_channels();
        let sig = format!("{}|{}", below.join(","), above.join(","));
        if sig == self.layer_order_sig {
            return;
        }
        self.layer_order_sig = sig;
        self.host.order_layer_canvases(&below, &above);
    }

    /// Mount the DEDICATED layer of a native indicator that names its own channel (an
    /// instance of a multi-instance type painting through a registered layer).
    /// Idempotent: a show after a hide re-mounts the model and the layer is reused.
    pub(crate) fn ensure_instance_layer(&mut self, id: &str) {
        let Some(model) = self.scene.indicators.get(id) else {
            return;
        };
        let Some(native) = model.native.as_ref() else {
            return;
        };
        let Some(channel) = native.channel.clone() else {
            return;
        };
        if !self.mounted
            || self
                .ext_layers
                .iter()
                .any(|l| l.owner.as_deref() == Some(id))
        {
            return;
        }
        let kind = native.indicator_type.clone();
        let Some(base_def) = self
            .ext_layers
            .iter()
            .find(|l| l.owner.is_none() && l.def.id == kind)
            .map(|l| l.def.clone())
        else {
            return; // no layer registered for the type: the model paints through its series alone
        };
        let slot = super::host::CanvasSlot::Layer(channel.clone());
        let canvas = self.host.create_canvas(&slot);
        // Match the pile's current backing size; the next resize re-sizes every layer anyway.
        let (bw, bh) = self.backing;
        let (pw, ph) = self.css_size;
        canvas.set_size(bw, bh, pw, ph);
        let mut instance = (base_def.create)();
        instance.mount(canvas.context());
        self.ext_layers.push(ExtLayer {
            def: base_def,
            instance,
            canvas,
            channel,
            owner: Some(id.to_string()),
        });
        self.layer_order_sig.clear(); // re-slotted by the owner's z on the next data frame
    }

    /// Tear down the dedicated layer of a removed indicator (a hide keeps it).
    pub(crate) fn drop_instance_layer(&mut self, indicator_id: &str) {
        let Some(i) = self
            .ext_layers
            .iter()
            .position(|l| l.owner.as_deref() == Some(indicator_id))
        else {
            return;
        };
        let mut l = self.ext_layers.remove(i);
        l.instance.destroy();
        self.host
            .remove_canvas(&super::host::CanvasSlot::Layer(l.channel));
        self.layer_order_sig.clear();
    }

    // ── external (synced) crosshair ──

    /// Resolve the ghost to pixels for THIS frame (`None` when off-window or
    /// dataless). Snaps by FLOOR to the bar CONTAINING the foreign time, never by
    /// rounding: a 1h pointer at 14:00 must light THIS day's daily candle.
    pub(crate) fn external_cross_px(&self) -> Option<ExternalCrosshair> {
        let (time, price) = self.external_cross?;
        if self.coords.bar_count() == 0 {
            return None;
        }
        let logical = self.coords.time_to_logical(time as f64).floor();
        #[allow(clippy::neg_cmp_op_on_partial_ord)] // a NaN logical must fail this check
        if !(logical < self.coords.bar_count() as f64) {
            return None; // past the forming bar: nothing after it
        }
        // Before the first open, or off the window: no line to draw, but a
        // `crosshairOverride` veil still covers every bar after it.
        let x = self.coords.logical_to_x(js_max(logical, -1.0));
        if !x.is_finite() {
            return None;
        }
        let line = logical >= 0.0 && x >= 0.0 && x <= self.coords.width();
        let mut y: Option<f64> = None;
        if let Some(price) = price
            && let Some(pane) = self
                .scene
                .ordered_panes()
                .iter()
                .find(|p| p.kind == PaneKind::Price)
        {
            let py = self.coords.price_to_y(price, &pane.scale, &pane.bounds);
            if py.is_finite() && py >= pane.bounds.top && py <= pane.bounds.top + pane.bounds.height
            {
                y = Some(py);
            }
        }
        // The raw price rides along for the axis chip (only meaningful with a resolved y).
        Some(ExternalCrosshair {
            x,
            y,
            time: self.coords.logical_to_time(js_max(logical, 0.0)),
            price: if y.is_some() { price } else { None },
            line: Some(line),
        })
    }

    // ── intro reveal ──

    /// The candles-first-appear reveal: animates the candles themselves. Each
    /// candle's OHLC is interpolated from a flat tick at its open up to full size,
    /// eased, with a left-to-right stagger; `settle` adds an ease-out-back overshoot.
    /// Autoscale stays on the real bars so the frame never moves.
    pub(crate) fn play_intro(&mut self) {
        if let Some(h) = self.intro_raf.take() {
            self.host.cancel_frame(h);
        }
        let n = self.bars.len();
        if n == 0 || self.intro.style.is_none() {
            return;
        }
        // Pin the price scale to the real-data autoscale for the whole reveal.
        self.compute_scales();
        for pane in self.scene.panes.values_mut() {
            pane.scale = pane.scale_target;
        }
        self.model_alpha = 0.0; // indicators stay hidden until the candles finish
        let start = self.host.frame_now_ms();
        self.schedule_intro_reveal(start);
    }

    fn schedule_intro_reveal(&mut self, start: f64) {
        let handle = self.request_task(move |r| r.intro_reveal_frame(start));
        self.intro_raf = Some(handle);
    }

    fn schedule_intro_fade(&mut self, start: f64) {
        let handle = self.request_task(move |r| r.intro_fade_frame(start));
        self.intro_raf = Some(handle);
    }

    /// One frame of the candle reveal.
    pub(crate) fn intro_reveal_step(&mut self, start: f64) {
        self.intro_raf = None;
        let now = self.host.frame_now_ms();
        let duration = self.intro.duration;
        let Some(style) = self.intro.style else {
            return;
        };
        let p = js_min(1.0, (now - start) / duration);
        let n = self.bars.len();
        self.scene.bars = if p >= 1.0 {
            self.bars.clone()
        } else {
            self.bars
                .iter()
                .enumerate()
                .map(|(i, b)| reveal_candle(b, i, p, n, style))
                .collect()
        };
        self.paint_data();
        if p < 1.0 {
            self.schedule_intro_reveal(start);
        } else {
            self.scene.bars = self.bars.clone();
            self.fade_in_models(); // candles done: fade the indicators in
        }
    }

    /// After the candle reveal, fade the indicator models from hidden to full.
    fn fade_in_models(&mut self) {
        let start = self.host.frame_now_ms();
        self.schedule_intro_fade(start);
    }

    /// One frame of the model fade-in.
    pub(crate) fn intro_fade_step(&mut self, start: f64) {
        self.intro_raf = None;
        let now = self.host.frame_now_ms();
        let duration = if self.intro.duration == 0.0 {
            INTRO_MODEL_FADE_MS
        } else {
            self.intro.duration
        };
        let fade = js_min(INTRO_MODEL_FADE_MS, duration);
        self.model_alpha = js_min(1.0, (now - start) / fade);
        self.paint_data();
        if self.model_alpha < 1.0 {
            self.schedule_intro_fade(start);
        } else {
            self.model_alpha = 1.0;
        }
    }

    // ── live (forming) bar easing ──

    /// Snap the eased forming-bar state to `bar`: no glide (a fresh bar or the first
    /// tick of one).
    pub(crate) fn sync_live_ease(&mut self, bar: &Ohlcv) {
        self.live_ease_time = bar.time;
        self.live_ease_high = bar.high;
        self.live_ease_low = bar.low;
        self.live_ease_close = bar.close;
    }

    /// Glide the forming bar's displayed high/low/close toward the actual latest.
    /// Returns true while easing.
    pub(crate) fn ease_live_bar(&mut self, dt_ms: f64) -> bool {
        let Some(target) = self.bars.last().cloned() else {
            return false;
        };
        if self.live_ease_time != target.time {
            return false;
        }
        let eps = js_max(1e-9, target.close.abs() * 1e-6);
        let tau = self.anim_live_bar.tau(); // 0: ease_toward returns the target (a mid-glide switch-off snaps)
        let nh = ease_toward(self.live_ease_high, target.high, dt_ms, tau);
        let nl = ease_toward(self.live_ease_low, target.low, dt_ms, tau);
        let nc = ease_toward(self.live_ease_close, target.close, dt_ms, tau);
        let active = (nh - target.high).abs() > eps
            || (nl - target.low).abs() > eps
            || (nc - target.close).abs() > eps;
        self.live_ease_high = if active { nh } else { target.high };
        self.live_ease_low = if active { nl } else { target.low };
        self.live_ease_close = if active { nc } else { target.close };
        active
    }

    /// Frame task helper: run `f` on the renderer on the next host frame.
    pub(crate) fn request_task(&self, f: impl FnOnce(&super::NativeRenderer) + 'static) -> u32 {
        let me = self.me.clone();
        self.host.request_frame(Box::new(move || {
            if let Some(r) = me.upgrade() {
                f(&r);
            }
        }))
    }
}

/// One candle of the reveal: interpolate its body and wick from a flat tick at the
/// open up to full size, staggered left to right. `settle` overshoots past full then
/// eases back.
fn reveal_candle(b: &Ohlcv, i: usize, p: f64, n: usize, style: IntroStyle) -> Ohlcv {
    const SPAN: f64 = 0.55; // fraction of the timeline the wave takes to cross left to right
    let denom = js_max(1.0, n as f64 - 1.0);
    let local = js_max(
        0.0,
        js_min(1.0, (p - (i as f64 / denom) * SPAN) / (1.0 - SPAN)),
    );
    let t = match style {
        IntroStyle::Settle => {
            let c1 = 1.70158;
            let u = local - 1.0;
            1.0 + (c1 + 1.0) * u * u * u + c1 * u * u // ease-out-back (overshoot)
        }
        IntroStyle::Grow => {
            let u = 1.0 - local;
            1.0 - u * u * u // ease-out cubic (grow)
        }
    };
    let o = b.open;
    Ohlcv {
        high: o + (b.high - o) * t,
        low: o + (b.low - o) * t,
        close: o + (b.close - o) * t,
        ..*b
    }
}

/// One frame's args for an SDK renderer layer (shared by the data and cursor paint
/// paths). Data and pending come from the entry's channel; the settings bag is the
/// TYPE's: a chart type's dialog values, one bag per type.
#[allow(clippy::too_many_arguments)]
fn ext_layer_args<'a>(
    scene: &'a SceneGraph,
    coords: &'a CoordinateSystem,
    theme: &'a VelaTheme,
    i18n: Option<&'a dyn Translator>,
    pointer: Option<(f64, f64)>,
    channel: &str,
    def_id: &str,
    scale: PriceScale,
    bounds: PaneBounds,
    now_ms: f64,
    empty: &'a Map<String, serde_json::Value>,
) -> RendererLayerArgs<'a> {
    RendererLayerArgs {
        bars: &scene.bars,
        data: scene.native_data.get(channel),
        settings: scene
            .native_data
            .get(&format!("{def_id}-settings"))
            .and_then(|v| v.as_object())
            .unwrap_or(empty),
        pending: scene
            .native_pending
            .get(channel)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        coords,
        scale,
        bounds,
        theme,
        price_style: &scene.price_style,
        now_ms,
        cursor: pointer.map(|(x, y)| CursorPoint { x, y }),
        i18n: i18n.unwrap_or(english_translator()),
    }
}
