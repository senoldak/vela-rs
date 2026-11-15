//! Lifecycle and data: mount, size sync, destroy, bars, indicators, the countdown
//! timer, loading, localization and the other port writes.

use std::rc::Rc;

use serde_json::Value;

use crate::i18n::Translator;
use crate::indicators::classics::math::{js_max, js_round};
use crate::marks::{MarkGroup, TimelineMark};
use crate::model::volume_layers::{VolumeLayerData, VpvrLayerData};
use crate::model::{AnchorUpdate, IndicatorModel, Ohlcv, PaneKind, ScenePatch};
use crate::options::NativeBackend;
use crate::ports::chart_renderer::{
    IndicatorStatus, LayoutMode, RendererRootAttributes, VelaTheme, WallClock,
};
use crate::render::backend::canvas2d_backend::Canvas2dBackend;
use crate::render::backend::render_backend::{BackendKind, RenderBackend};
use crate::render::backend::webgl2_backend::WebGL2Backend;
use crate::render::core::scheduler::InvalidateLevel;
use crate::render::core::{ViewportState, default_viewport};

use super::host::{CanvasSlot, LegendRow};
use super::state::{
    Canvases, DataCanvas, ExtLayer, GeometryBackend, PRICE_PANE_ID, State, TIME_AXIS_H,
    derive_theme,
};
use super::util::{apply_patch, leading_points_before, normalize_bars, theme_string};

impl State {
    // ── mount / size / destroy ──

    /// Create and mount the L0 geometry backend and return its canvas. Picks WebGL2 when
    /// the mode allows it and the host offers a GL canvas (the original's probe), else the
    /// canvas2d backend (the permanent fallback). If the program cannot be built the
    /// (tainted) GL canvas is discarded and a fresh 2D one is made.
    fn create_geometry_backend(&mut self) -> DataCanvas {
        if matches!(
            self.backend_mode,
            NativeBackend::Auto | NativeBackend::Webgl2
        ) && let Some(canvas) = self.host.create_gl_canvas()
        {
            let mut gl = WebGL2Backend::new();
            gl.glow = self.glow_amount;
            gl.mount(canvas.gl(), 0.0, 0.0);
            if gl.ok() {
                self.backend = GeometryBackend::WebGl2(Box::new(gl));
                return DataCanvas::Gl(canvas);
            }
            gl.destroy(); // context setup failed: discard the tainted canvas
            self.host.remove_canvas(&CanvasSlot::Data);
        }
        self.make_2d_data_canvas()
    }

    fn make_2d_data_canvas(&mut self) -> DataCanvas {
        let canvas = self.host.create_canvas(&CanvasSlot::Data);
        let mut c2 = Canvas2dBackend::new();
        c2.mount(canvas.context(), 0.0, 0.0);
        self.backend = GeometryBackend::Canvas2d(c2);
        DataCanvas::Canvas2d(canvas)
    }

    /// `webglcontextlost`: the backend stops painting until the context is restored.
    pub(crate) fn gl_context_lost(&mut self) {
        if let GeometryBackend::WebGl2(b) = &mut self.backend {
            b.on_context_lost();
        }
    }

    /// `webglcontextrestored`: rebuild the GL objects and repaint, or, when that fails,
    /// replace the backend with canvas2d on a fresh data canvas.
    pub(crate) fn gl_context_restored(&mut self) {
        let GeometryBackend::WebGl2(b) = &mut self.backend else {
            return;
        };
        if b.on_context_restored() {
            self.invalidate(InvalidateLevel::Full);
        } else {
            self.swap_to_canvas2d_fallback();
        }
    }

    /// The WebGL2 context became permanently unusable: replace the L0 backend with the
    /// canvas2d fallback on a fresh data canvas. The chrome and cursor layers and the
    /// viewport state are untouched; the host re-attaches its pointer input to the new
    /// `Data` canvas.
    fn swap_to_canvas2d_fallback(&mut self) {
        if self.backend.kind() == BackendKind::Canvas2d {
            return;
        }
        self.backend.destroy();
        self.host.remove_canvas(&CanvasSlot::Data);
        let canvas = self.make_2d_data_canvas();
        if let Some(c) = &mut self.canvases {
            c.data = canvas;
        }
        self.sync_size(); // size the new canvas and repaint
    }

    pub(crate) fn mount(&mut self, theme: &VelaTheme) {
        self.theme = derive_theme(theme, &self.candle_up, &self.candle_down);
        self.app_theme = theme.clone();
        // Provisional chrome surface (refined by the first applyConfig).
        self.surface_background = theme_string(theme, "background");
        self.surface_text_color = theme_string(theme, "textColor");
        self.mounted = true;
        self.publish_gutters();

        // L-2 backdrop: session highlights + gridlines, the very BOTTOM of the pile.
        let backdrop = self.host.create_canvas(&CanvasSlot::Backdrop);
        // L0.25 volume columns: bottom-anchored per-bar volume, ABOVE the geometry canvas.
        let volume = self.host.create_canvas(&CanvasSlot::Volume);
        // L0 geometry: the swappable backend (WebGL2 if allowed and offered, else
        // canvas2d) owns the data canvas.
        let data = self.create_geometry_backend();
        // L0.6 VPVR: the visible-range volume profile, above the candles.
        let vpvr = self.host.create_canvas(&CanvasSlot::Vpvr);
        // L1 chrome: axes + current-price line, above the geometry layer.
        let chrome = self.host.create_canvas(&CanvasSlot::Chrome);
        // L1.5 user drawings.
        let drawings = self.host.create_canvas(&CanvasSlot::Drawings);
        // L2 cursor: the crosshair, repainted alone on the cheap cursor tier.
        let cursor = self.host.create_canvas(&CanvasSlot::Cursor);
        // SDK renderer layers: one transparent canvas each, stacked by placement.
        let defs: Vec<_> = self.layers.layers().to_vec();
        for def in defs {
            let canvas = self.host.create_canvas(&CanvasSlot::Layer(def.id.clone()));
            let mut instance = (def.create)();
            instance.mount(canvas.context());
            let channel = def.id.clone();
            self.ext_layers.push(ExtLayer {
                def,
                instance,
                canvas,
                channel,
                owner: None,
            });
        }
        self.layer_order_sig.clear(); // recomputed on the first data frame
        self.factory_config = Some(self.get_config());
        self.factory_theme = theme.clone();
        self.push_theme_to_host();
        self.host.apply_root_attributes(&self.root_attrs);

        self.backdrop.mount(backdrop.context(), 0.0, 0.0);
        self.volume.mount(volume.context(), 0.0, 0.0);
        self.vpvr.mount(vpvr.context(), 0.0, 0.0);
        self.chrome.mount(chrome.context(), 0.0, 0.0);
        self.crosshair_layer.mount(cursor.context(), 0.0, 0.0);
        self.canvases = Some(Canvases {
            backdrop,
            data,
            volume,
            vpvr,
            chrome,
            drawings,
            cursor,
        });
        // Indicators mounted before the renderer (a re-mount) bring their dedicated layers back.
        let ids: Vec<String> = self.scene.indicators.keys().map(str::to_string).collect();
        for id in ids {
            self.ensure_instance_layer(&id);
        }
        self.coords.set_viewport(default_viewport());
        self.scene
            .ensure_pane(PRICE_PANE_ID, PaneKind::Price, 0, None);
        self.target_bar_spacing = self.viewport().bar_spacing;
        self.sync_countdown_timer(); // start the countdown pump if the feature is on
        self.host.set_keyboard_enabled(self.keyboard_enabled);
        self.sync_size();
    }

    /// The plot's size changed (or the device-pixel ratio): re-size every canvas of
    /// the pile and repaint synchronously.
    pub(crate) fn sync_size(&mut self) {
        if self.canvases.is_none() {
            return;
        }
        let m = self.host.metrics();
        if m.client_width <= 0.0 || m.client_height <= 0.0 {
            return;
        }
        let dpr = if m.dpr > 0.0 { m.dpr } else { 1.0 };
        // Size the pile from the plot's REAL box: client sizes are integer-rounded, and
        // on fractional-dpr displays a backing store rounded from them covers a
        // different number of device pixels than the box it is displayed in.
        let mut bw = js_max(1.0, js_round(m.plot_width * dpr));
        let mut bh = js_max(1.0, js_round(m.plot_height * dpr));
        // Prefer the observer-reported device-pixel size when it describes the CURRENT
        // box (within 1 device px of the rect): the browser's snapping can differ from
        // plain rounding by up to a pixel, which is exactly why its report wins.
        if let Some((dw, dh)) = m.device_size
            && (dw - m.plot_width * dpr).abs() <= 1.0
            && (dh - m.plot_height * dpr).abs() <= 1.0
        {
            bw = js_max(1.0, dw);
            bh = js_max(1.0, dh);
        }
        // Bitmap px == device px: each canvas gets an explicit CSS size derived from its
        // backing store, so nothing is rescaled at composite time.
        let (pw, ph) = (bw / dpr, bh / dpr);
        self.backing = (bw, bh);
        self.css_size = (pw, ph);
        if let Some(c) = &self.canvases {
            c.data.set_size(bw, bh, pw, ph);
            for canvas in [
                &c.backdrop,
                &c.volume,
                &c.vpvr,
                &c.chrome,
                &c.drawings,
                &c.cursor,
            ] {
                canvas.set_size(bw, bh, pw, ph);
            }
        }
        for l in &self.ext_layers {
            l.canvas.set_size(bw, bh, pw, ph);
        }
        self.backend.set_backing_size(bw, bh);
        self.backdrop.set_backing_size(bw, bh);
        self.volume.set_backing_size(bw, bh);
        self.vpvr.set_backing_size(bw, bh);
        self.chrome.set_backing_size(bw, bh);
        self.crosshair_layer.set_backing_size(bw, bh);
        // Reserve strips for the price axis (right) + time axis (bottom); the data area
        // drives the coordinate transform so series sit clear of them.
        self.coords.set_size(
            js_max(1.0, pw - self.right_axis_w),
            js_max(1.0, ph - TIME_AXIS_H),
            dpr,
        );
        // Geometry changed: the retained crosshair {x,y} now points at the wrong
        // bar/price, so drop it (LWC hides the crosshair until the next move).
        self.scene.crosshair = None;
        self.layout_panes();
        if let Some(h) = &self.user_drawings {
            h.on_resize(); // dismiss the settings popup on a resize
        }
        if !self.did_initial_fit && self.coords.bar_count() > 0 {
            self.fit_content();
            self.did_initial_fit = true;
        }
        if self.anim.active() {
            // The size assignments above just CLEARED every canvas, and render_frame
            // yields to the animator while it runs: paint synchronously instead.
            self.compute_scales();
            self.paint_data();
            self.render_crosshair_layer();
        } else {
            self.flush = Some(InvalidateLevel::Full);
        }
    }

    pub(crate) fn destroy(&mut self) {
        if let Some(h) = self.intro_raf.take() {
            self.host.cancel_frame(h);
        }
        if let Some(h) = self.mark_pulse_raf.take() {
            self.host.cancel_frame(h);
        }
        if self.loading {
            self.loading = false;
            self.host.set_loading_visible(false);
        }
        if let Some(unsub) = self.countdown_unsub.take() {
            unsub();
        }
        self.sched.destroy();
        self.anim.stop();
        if let Some(h) = self.user_drawings.take() {
            h.destroy();
        }
        for mut l in std::mem::take(&mut self.ext_layers) {
            l.instance.destroy();
            self.host.remove_canvas(&CanvasSlot::Layer(l.channel));
        }
        self.backdrop.destroy();
        self.volume.destroy();
        self.vpvr.destroy();
        self.backend.destroy();
        self.chrome.destroy();
        self.crosshair_layer.destroy();
        if self.canvases.take().is_some() {
            for slot in [
                CanvasSlot::Backdrop,
                CanvasSlot::Volume,
                CanvasSlot::Data,
                CanvasSlot::Vpvr,
                CanvasSlot::Chrome,
                CanvasSlot::Drawings,
                CanvasSlot::Cursor,
            ] {
                self.host.remove_canvas(&slot);
            }
        }
        self.mounted = false;
        self.painted_data = false;
        self.layout.bottom_gutter = None;
        self.layout.price_pane_top = None;
        self.layout.price_pane_bottom = None;
    }

    // ── countdown ──

    /// Subscribe to the second pulse while the countdown chip is on (so it ticks);
    /// unsubscribe otherwise. Chrome tier: only the chip's text moves, so an idle chart
    /// never recomputes scales or repaints the geometry layers once a second.
    pub(crate) fn sync_countdown_timer(&mut self) {
        if self.scene.show_countdown {
            if self.countdown_unsub.is_none() {
                let clock = self.host_clock.clone().or_else(|| self.host.second_clock());
                if let Some(clock) = clock {
                    let me = self.me.clone();
                    self.countdown_unsub = Some(clock.on_tick(Box::new(move |_| {
                        if let Some(r) = me.upgrade() {
                            r.countdown_tick();
                        }
                    })));
                }
            }
        } else if let Some(unsub) = self.countdown_unsub.take() {
            unsub();
        }
    }

    pub(crate) fn set_wall_clock(&mut self, clock: Option<Rc<dyn WallClock>>) {
        let same = match (&clock, &self.host_clock) {
            (None, None) => true,
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            _ => false,
        };
        if same {
            return;
        }
        self.host_clock = clock;
        if let Some(unsub) = self.countdown_unsub.take() {
            unsub(); // re-subscribe on the new source
        }
        self.sync_countdown_timer();
    }

    // ── bars ──

    pub(crate) fn set_bars(&mut self, bars: &[Ohlcv], preserve_view: bool) {
        if let Some(h) = self.intro_raf.take() {
            // A re-set interrupts a running reveal.
            self.host.cancel_frame(h);
            self.model_alpha = 1.0;
        }
        // A series replacement invalidates the forming-bar glide: after an in-place
        // market switch the NEW market's forming bar shares the same bucket open-time,
        // so a stale eased close/high/low would paint a full-height candle until the
        // next tick re-seeds. The bars given here carry real values: drop the ease.
        self.live_ease_time = 0;
        let prev_bars = std::mem::take(&mut self.bars);
        let prev_head_time = prev_bars.first().map(|b| b.time);
        self.bars = normalize_bars(bars);
        self.scene.bars = self.bars.clone();
        let times: Vec<i64> = self.bars.iter().map(|b| b.time).collect();
        self.coords.set_bars(&times);
        // The bar array's head may have moved (a history-backfill prepend): re-derive
        // every mounted model's anchor offset against the new indices.
        let ids: Vec<String> = self.scene.indicators.keys().map(str::to_string).collect();
        for id in &ids {
            self.refresh_anchor_offset(id);
        }
        // Logical-space INTERACTION anchors need the same prepend shift: a wheel-zoom
        // glide recomputes rightOffset from `zoom_anchor_logical` EVERY frame, so a
        // backfill landing mid-zoom would otherwise re-anchor thousands of bars into
        // the past and teleport the viewport. Same series only: on a symbol/timeframe
        // switch the head time won't match and the view re-fits anyway.
        if preserve_view
            && let Some(prev_head) = prev_head_time
            && let Some(first) = self.bars.first()
        {
            // The head can move BOTH ways on the same series: a backfill PREPENDS older
            // bars (indices shift up), a shallower depth TRIMS from the front (indices
            // shift down). The newest end is fixed either way.
            let new_head = first.time;
            let mut shift: i64 = 0;
            if new_head != prev_head {
                match self.bars.iter().position(|b| b.time == prev_head) {
                    Some(p) if p > 0 => shift = p as i64,
                    _ => {
                        if let Some(t) = prev_bars.iter().position(|b| b.time == new_head)
                            && t > 0
                        {
                            shift = -(t as i64);
                        }
                    }
                }
            }
            if shift != 0 {
                self.zoom_anchor_logical += shift as f64;
                if let Some(h) = self.hover_logical.as_mut() {
                    *h += shift;
                }
            }
        }
        // Full bar replacement re-frames the view: a symbol/timeframe switch must not
        // keep a PAN aimed at another market's time range. The ZOOM is a user
        // preference rather than market state, so the re-frame keeps the current bar
        // spacing and only re-anchors the newest bars; the very first frame has no zoom
        // to keep and fits instead. `preserve_view` keeps the whole viewport: the series
        // is right-anchored, so extending it with older bars in place leaves the visible
        // bars unchanged. Still fit the first time, so there is always an initial frame.
        let skip_fit = preserve_view && self.did_initial_fit;
        if self.coords.width() > 0.0 && !skip_fit {
            if self.did_initial_fit {
                self.reframe_keep_zoom();
            } else {
                self.fit_content();
            }
            self.did_initial_fit = true;
        }
        if !self.intro_played && !self.bars.is_empty() {
            self.intro_played = true; // play the reveal once, when candles first appear
            if self.intro.style.is_some() && self.mounted {
                self.play_intro(); // the reveal owns the paint: no full-frame flash first
                return;
            }
        }
        self.invalidate(InvalidateLevel::Full);
    }

    pub(crate) fn update_bar(&mut self, bar: &Ohlcv) {
        let n = self.bars.len();
        let last_time = self.bars.last().map(|b| b.time);
        match last_time {
            Some(t) if bar.time == t => {
                self.bars[n - 1] = *bar; // the actual forming bar: the ease target
                if !self.anim_live_bar.on() || self.live_ease_time != bar.time {
                    self.sync_live_ease(bar); // glide off, or the first tick of this bar: snap
                } else {
                    self.anim.start(); // glide the displayed high/low/close toward this tick
                }
            }
            Some(t) if bar.time <= t => return,
            _ => {
                // The newest bar is scrolled off the right edge (and no glide is
                // bringing it back): the user is reading history, so hold the view on
                // the same bars instead of sliding it along with every new one.
                let vp = self.viewport();
                let hold_view = n > 0 && vp.right_offset < 0.0 && self.scroll_target_ro.is_none();
                self.bars.push(*bar);
                self.sync_live_ease(bar); // a fresh bar: snap (never ease across bars)
                // Warm path: O(1) append. Cold start (interval not yet established)
                // takes the robust full-median set_bars path.
                if self.coords.bar_interval() > 0.0 {
                    self.coords.append_bar(bar.time);
                } else {
                    let times: Vec<i64> = self.bars.iter().map(|b| b.time).collect();
                    self.coords.set_bars(&times);
                }
                if hold_view {
                    self.coords.set_viewport(ViewportState {
                        right_offset: vp.right_offset - 1.0,
                        ..vp
                    });
                }
            }
        }
        // `scene.bars` is the painted copy (a running intro reveal replaces it): keep
        // it in step without re-cloning the whole series on every tick.
        if self.intro_raf.is_none() && self.scene.bars.len() + 1 >= self.bars.len() {
            match self.scene.bars.len().cmp(&self.bars.len()) {
                std::cmp::Ordering::Equal => {
                    if let (Some(slot), Some(src)) = (self.scene.bars.last_mut(), self.bars.last())
                    {
                        *slot = *src;
                    }
                }
                _ => {
                    if let Some(src) = self.bars.last() {
                        self.scene.bars.push(*src);
                    }
                }
            }
        } else {
            self.scene.bars = self.bars.clone();
        }
        self.invalidate(InvalidateLevel::Full);
    }

    /// Re-derive one model's index offset: the chart bar index of its `anchor_time`.
    /// An absent anchor, or one matching the first bar, means whole-chart alignment
    /// (offset 0, today's norm). An anchor that doesn't land exactly on a chart bar
    /// renders unanchored (defensive; engines stamp a real bar time).
    pub(crate) fn refresh_anchor_offset(&mut self, id: &str) {
        let Some(model) = self.scene.indicators.get(id) else {
            return;
        };
        let anchor = model.anchor_time;
        let Some(anchor) = anchor.filter(|_| !self.bars.is_empty()) else {
            self.scene.set_anchor_offset(id, 0);
            return;
        };
        let head = self.bars[0].time;
        if anchor == head {
            self.scene.set_anchor_offset(id, 0);
            return;
        }
        if anchor < head {
            // The model starts BEFORE the chart's first bar: its own leading values fall
            // off the left edge. A NEGATIVE offset skips exactly those points; treating
            // this as "unanchored" would pin a stale model's first value onto the
            // chart's first bar and draw the whole plot shifted.
            match leading_points_before(model, head) {
                None => {
                    // Nothing index-aligned to measure against (a drawings-only model).
                    self.host.warn(&format!(
                        "[vela] indicator \"{id}\" starts before the first bar with no series to align on, rendering unanchored"
                    ));
                    self.scene.set_anchor_offset(id, 0);
                }
                Some(skip) => self.scene.set_anchor_offset(id, -(skip as i64)),
            }
            return;
        }
        // Lower-bound binary search over the sorted bar times.
        let at = self.bars.partition_point(|b| b.time < anchor);
        if at >= self.bars.len() || self.bars[at].time != anchor {
            self.host.warn(&format!(
                "[vela] indicator \"{id}\" anchorTime matches no chart bar, rendering unanchored"
            ));
            self.scene.set_anchor_offset(id, 0);
            return;
        }
        self.scene.set_anchor_offset(id, at as i64);
    }

    // ── indicators ──

    pub(crate) fn mount_indicator(&mut self, model: &IndicatorModel) {
        let id = model.id.clone();
        self.scene.indicators.insert(id.clone(), model.clone());
        self.refresh_anchor_offset(&id);
        // Default z = mount order (later: in front), except a native that paints through
        // an SDK layer: its canvas stacks ABOVE the data canvas by default, so its key
        // must say so or the recorded order starts out a lie.
        if self.native_names_a_layer(model) {
            self.scene.assign_indicator_z_top(&id);
        } else {
            self.scene.assign_indicator_z(&id);
        }
        self.ensure_instance_layer(&id);
        // The legend chip, the settings dialog and the object tree's rows all show the
        // compact shorttitle when declared. A `legend: false` native gets none of it:
        // the scene model still mounts and paints.
        if model.legend != Some(false) {
            self.host.legend_upsert(&LegendRow {
                id: id.clone(),
                title: self.legend_title(model),
                pane_id: model.pane_id.clone(),
                native: model.native.is_some(),
            });
        }
        match model.native.as_ref().map(|n| n.indicator_type.as_str()) {
            Some("volume") => {
                self.volume_active = true; // the volume layer follows the indicator's presence
                self.volume_hidden = false;
            }
            Some("vpvr") => {
                self.vpvr_active = true;
                self.vpvr_hidden = false;
            }
            _ => {}
        }
        if model.own_scale {
            self.refresh_axis_width();
        }
        self.sync_legend_order();
        self.host.layout_changed();
        self.invalidate(InvalidateLevel::Full);
    }

    pub(crate) fn update_indicator(&mut self, id: &str, patch: &ScenePatch) {
        let Some(model) = self.scene.indicators.get_mut(id) else {
            return;
        };
        // A value patch from a re-run over a DIFFERENT bar window carries its anchor;
        // `Clear` says the run spanned the whole chart and CLEARS the previous one.
        let mut refresh = false;
        if let ScenePatch::Value(vp) = patch {
            let next = match vp.anchor_time {
                AnchorUpdate::Keep => None,
                AnchorUpdate::Clear => Some(None),
                AnchorUpdate::Set(t) => Some(Some(t)),
            };
            if let Some(next) = next
                && next != model.anchor_time
            {
                model.anchor_time = next;
                refresh = true;
            }
        }
        if refresh {
            self.refresh_anchor_offset(id);
        }
        if let Some(model) = self.scene.indicators.get_mut(id) {
            apply_patch(model, patch);
        }
        self.invalidate(InvalidateLevel::Light);
    }

    pub(crate) fn remove_indicator(&mut self, id: &str) {
        let native_type = self
            .scene
            .indicators
            .get(id)
            .and_then(|m| m.native.as_ref())
            .map(|n| n.indicator_type.clone());
        match native_type.as_deref() {
            Some("volume") => {
                self.volume_active = false; // tear down the volume layer with the indicator
                self.volume_hidden = false;
                self.scene.volume_layer = None;
            }
            Some("vpvr") => {
                self.vpvr_active = false;
                self.vpvr_hidden = false;
                self.scene.vpvr_layer = None;
            }
            _ => {}
        }
        self.scene.indicators.remove(id);
        self.drop_instance_layer(id);
        self.scene.forget_indicator_z(id);
        self.scene.forget_anchor_offset(id);
        self.scene.drop_indicator_scale(id);
        self.host.legend_remove(id);
        self.refresh_axis_width();
        self.host.layout_changed();
        self.invalidate(InvalidateLevel::Full);
    }

    /// Move/merge a mounted indicator to another pane (its scale column follows via
    /// `own_scale`).
    pub(crate) fn set_indicator_pane(&mut self, id: &str, pane_id: &str, own_scale: bool) {
        let Some(model) = self.scene.indicators.get_mut(id) else {
            return;
        };
        model.pane_id = Some(pane_id.to_string());
        model.own_scale = own_scale;
        if !own_scale {
            self.scene.drop_indicator_scale(id);
        }
        self.host.legend_set_pane(id, pane_id);
        self.sync_legend_order();
        self.refresh_anchor_offset(id);
        self.refresh_axis_width();
        self.layout_panes();
        self.invalidate(InvalidateLevel::Full);
    }

    /// Hide/show a mounted indicator. Hiding drops its model from the scene (so every
    /// paint path skips it) while keeping its z key and its legend row (marked hidden).
    /// The core re-mounts it on show.
    pub(crate) fn set_indicator_visible(&mut self, id: &str, visible: bool) {
        // Layer-drawn natives have no series in the scene: suppress/restore their layer
        // instead of dropping the model (kept so the layer stays identifiable).
        let native_type = self
            .scene
            .indicators
            .get(id)
            .and_then(|m| m.native.as_ref())
            .map(|n| n.indicator_type.clone());
        if let Some(kind) = native_type.as_deref()
            && (kind == "volume" || kind == "vpvr")
        {
            if kind == "volume" {
                self.volume_hidden = !visible;
            } else {
                self.vpvr_hidden = !visible;
            }
            self.host.legend_set_visible(id, visible);
            self.invalidate(InvalidateLevel::Full);
            return;
        }
        if !visible {
            // Keep the z key (forget_indicator_z NOT called) so show preserves order.
            self.scene.indicators.remove(id);
        }
        self.host.legend_set_visible(id, visible);
        self.invalidate(InvalidateLevel::Full);
    }

    pub(crate) fn set_indicator_status(&mut self, id: &str, status: IndicatorStatus) {
        self.host.legend_set_status(id, status);
    }

    // ── port writes ──

    /// Native-layer data push, keyed by native type. The volume and VPVR indicators
    /// push their layer CONFIG only (both layers read the chart's bars each frame); a
    /// chart-type indicator pushes its per-bar order-flow on its own channel.
    pub(crate) fn set_native_data(&mut self, kind: &str, data: &Value) {
        if kind == "volume" {
            self.scene.volume_layer = serde_json::from_value::<VolumeLayerData>(data.clone()).ok();
        } else if kind == "vpvr" {
            self.scene.vpvr_layer = serde_json::from_value::<VpvrLayerData>(data.clone()).ok();
        } else if let Some(channel) = kind.strip_suffix("-pending") {
            // The loading protocol of an SDK layer channel: `<id>-pending` = ranges
            // still on their way.
            let ranges: Vec<(f64, f64)> = data
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|r| {
                            let pair = r.as_array()?;
                            Some((pair.first()?.as_f64()?, pair.get(1)?.as_f64()?))
                        })
                        .collect()
                })
                .unwrap_or_default();
            self.scene.native_pending.insert(channel, ranges);
        } else {
            // A generic SDK layer channel (chart-type data engines push here).
            self.scene.native_data.insert(kind, data.clone());
        }
        self.invalidate(InvalidateLevel::Light);
    }

    pub(crate) fn set_loading(&mut self, loading: bool) {
        // Tables need no special handling here: they paint through the interleave
        // slices, and the backend paints nothing on an emptied (bar-less) chart.
        if !loading || !self.mounted {
            if self.loading {
                self.loading = false;
                self.host.set_loading_visible(false);
            }
            return;
        }
        if self.loading {
            return;
        }
        self.loading = true;
        self.host.set_loading_visible(true);
    }

    /// Set the active symbol's tick size so the price axis shows the instrument's true
    /// precision. Undefined or non-positive falls back to the zoom formula.
    pub(crate) fn set_price_precision(&mut self, mintick: Option<f64>) {
        let m = mintick.filter(|m| *m > 0.0);
        if m == self.scene.price_mintick {
            return;
        }
        self.scene.price_mintick = m;
        self.invalidate(InvalidateLevel::Full);
    }

    pub(crate) fn set_timeline_marks(&mut self, marks: &[TimelineMark], groups: &[MarkGroup]) {
        self.scene.timeline_marks = marks.to_vec();
        self.scene.mark_groups = groups.to_vec();
        self.scene.marks_expanded_stack = None;
        // The open cluster may no longer exist (a market switch replaced the set).
        self.close_mark_popover();
        self.invalidate(InvalidateLevel::Chrome);
    }

    /// Adopt the chart's localization: the formatters feed the axes, chips and
    /// readouts, the root carries `lang`/`dir`, and everything repaints.
    pub(crate) fn set_localization(
        &mut self,
        i18n: Rc<dyn Translator>,
        root: RendererRootAttributes,
    ) {
        self.scene.format = Some(i18n.format().clone());
        self.i18n = Some(i18n);
        self.root_attrs = root;
        if self.mounted {
            self.host.apply_root_attributes(&self.root_attrs);
        }
        self.relabel();
        if let Some(h) = &self.user_drawings {
            h.relocalize();
        }
        self.invalidate(InvalidateLevel::Full);
    }

    /// Re-read the legend titles in the current language.
    fn relabel(&self) {
        if !self.mounted {
            return;
        }
        let titles: Vec<(String, String)> = self
            .scene
            .indicators
            .values()
            .filter(|m| m.legend != Some(false))
            .map(|m| (m.id.clone(), self.legend_title(m)))
            .collect();
        self.host.legend_relabel(&titles);
    }

    pub(crate) fn set_layout_mode(&mut self, mode: LayoutMode) {
        if mode == self.layout_mode {
            return;
        }
        self.layout_mode = mode;
        if let Some(h) = &self.user_drawings {
            h.set_layout_mode(mode);
        }
        self.host.set_layout_mode(mode);
        // A finger needs a larger target than a cursor; touch has no cursor proximity.
        self.scroll_btn_sent = None;
        self.update_scroll_to_realtime_button();
    }
}

#[cfg(test)]
mod gl_tests {
    use std::rc::Rc;

    use serde_json::json;

    use super::super::testing::{TestHost, sample_bars};
    use super::super::tests::display;
    use super::super::{CanvasSlot, NativeRenderer};
    use crate::options::NativeBackend;
    use crate::ports::chart_renderer::{ChartRenderer, SetBarsOptions};
    use crate::render::backend::render_backend::BackendKind;
    use crate::theme::dark_theme;

    fn unmounted(
        mode: NativeBackend,
        offered: bool,
        glow: f64,
    ) -> (Rc<TestHost>, Rc<NativeRenderer>) {
        let host = TestHost::new(800.0, 420.0, 2.0);
        host.provide_gl(offered);
        let mut opts = display(false);
        opts.native_backend = mode;
        opts.glow = glow;
        let r = NativeRenderer::new(host.clone(), Some(opts));
        (host, r)
    }

    fn mounted(mode: NativeBackend, offered: bool) -> (Rc<TestHost>, Rc<NativeRenderer>) {
        let (host, r) = unmounted(mode, offered, 0.0);
        r.mount(&dark_theme());
        (host, r)
    }

    fn feed(host: &TestHost, r: &NativeRenderer) {
        r.set_bars(
            &sample_bars(40, 1_700_000_000_000, 60_000),
            SetBarsOptions::default(),
        );
        host.run_until_idle();
    }

    fn data_painted(host: &TestHost) -> bool {
        host.canvas(&CanvasSlot::Data)
            .is_some_and(|c| !c.is_empty())
    }

    #[test]
    fn a_host_that_offers_no_gl_keeps_the_canvas2d_backend() {
        let (host, r) = mounted(NativeBackend::Auto, false);
        feed(&host, &r);
        assert_eq!(r.geometry_backend(), BackendKind::Canvas2d);
        assert!(host.gl_canvas().is_none());
        assert!(host.gl().is_empty());
        assert!(data_painted(&host));
    }

    #[test]
    fn gl_is_used_when_offered_and_the_mode_allows_it() {
        for mode in [NativeBackend::Auto, NativeBackend::Webgl2] {
            let (host, r) = mounted(mode, true);
            assert_eq!(r.geometry_backend(), BackendKind::WebGl2);
            assert!(
                host.canvas(&CanvasSlot::Data).is_none(),
                "the data canvas is the GL one"
            );
            assert!(host.gl_canvas().is_some());
            feed(&host, &r);
            let gl = host.gl();
            assert_eq!(gl.calls("createProgram").len(), 1);
            assert!(!gl.calls("drawArrays").is_empty(), "candles were drawn");
            // The other canvases of the pile are still 2D.
            assert!(
                host.canvas(&CanvasSlot::Chrome)
                    .is_some_and(|c| !c.is_empty())
            );
        }
    }

    #[test]
    fn the_canvas2d_mode_ignores_an_offered_gl_canvas() {
        let (host, r) = mounted(NativeBackend::Canvas2d, true);
        feed(&host, &r);
        assert_eq!(r.geometry_backend(), BackendKind::Canvas2d);
        assert!(host.gl_canvas().is_none());
        assert!(host.gl().is_empty());
        assert!(data_painted(&host));
    }

    #[test]
    fn the_webgl2_mode_without_gl_available_falls_back_to_canvas2d() {
        let (host, r) = mounted(NativeBackend::Webgl2, false);
        feed(&host, &r);
        assert_eq!(r.geometry_backend(), BackendKind::Canvas2d);
        assert!(data_painted(&host));
    }

    #[test]
    fn a_program_that_does_not_link_discards_the_gl_canvas_for_a_2d_one() {
        let (host, r) = unmounted(NativeBackend::Auto, true, 0.0);
        host.gl().set_link_ok(false);
        r.mount(&dark_theme());
        assert_eq!(r.geometry_backend(), BackendKind::Canvas2d);
        assert!(host.gl_canvas().is_none());
        assert!(host.removed_slots().contains(&CanvasSlot::Data));
        feed(&host, &r);
        assert!(data_painted(&host));
    }

    #[test]
    fn the_gl_canvas_is_sized_with_the_pile() {
        let (host, r) = mounted(NativeBackend::Auto, true);
        feed(&host, &r);
        let gl = host.gl_canvas().unwrap();
        let chrome = host.test_canvas(&CanvasSlot::Chrome).unwrap();
        assert_eq!(gl.backing_size(), chrome.backing_size());
        assert_eq!(gl.css_size(), chrome.css_size());
        assert!(gl.backing_size().0 > 0.0);
        // The viewport of the last frame is the backing size.
        let viewports = host.gl().calls("viewport");
        let (bw, bh) = gl.backing_size();
        assert_eq!(
            viewports.last().unwrap(),
            &json!(["viewport", 0, 0, bw as i64, bh as i64])
        );
        // A resize follows through.
        host.set_size(600.0, 300.0);
        r.resize();
        host.run_until_idle();
        let after = host
            .test_canvas(&CanvasSlot::Chrome)
            .unwrap()
            .backing_size();
        assert_ne!(after, (bw, bh));
        assert_eq!(gl.backing_size(), after);
    }

    #[test]
    fn a_lost_context_pauses_painting_and_a_restore_rebuilds_and_repaints() {
        let (host, r) = mounted(NativeBackend::Auto, true);
        feed(&host, &r);
        let gl = host.gl();
        gl.clear();
        r.gl_context_lost();
        r.flush();
        host.run_until_idle();
        assert!(gl.is_empty(), "a lost context takes no calls");
        r.gl_context_restored();
        host.run_until_idle();
        assert_eq!(
            gl.calls("createProgram").len(),
            1,
            "the program was rebuilt"
        );
        assert!(
            !gl.calls("drawArrays").is_empty(),
            "and the chart repainted"
        );
        assert_eq!(r.geometry_backend(), BackendKind::WebGl2);
    }

    #[test]
    fn a_restore_that_cannot_rebuild_swaps_to_canvas2d_on_a_fresh_data_canvas() {
        let (host, r) = mounted(NativeBackend::Auto, true);
        feed(&host, &r);
        r.gl_context_lost();
        host.gl().set_link_ok(false);
        r.gl_context_restored();
        host.run_until_idle();
        assert_eq!(r.geometry_backend(), BackendKind::Canvas2d);
        assert!(host.gl_canvas().is_none());
        assert!(host.removed_slots().contains(&CanvasSlot::Data));
        let data = host
            .test_canvas(&CanvasSlot::Data)
            .expect("a 2D data canvas replaced it");
        assert!(data.resize_count() >= 1, "sized");
        assert!(data_painted(&host), "and painted");
        assert_eq!(
            data.backing_size(),
            host.test_canvas(&CanvasSlot::Chrome)
                .unwrap()
                .backing_size()
        );
        // The events are no-ops once on canvas2d.
        r.gl_context_lost();
        r.gl_context_restored();
        assert_eq!(r.geometry_backend(), BackendKind::Canvas2d);
    }

    #[test]
    fn the_glow_feature_drives_the_bloom_pass() {
        let (host, r) = mounted(NativeBackend::Auto, true);
        feed(&host, &r);
        assert!(host.gl().calls("createFramebuffer").is_empty());
        r.apply_feature("glow", &json!(1.5));
        host.run_until_idle();
        assert_eq!(r.read_feature("glow"), Some(json!(1.5)));
        let gl = host.gl();
        assert_eq!(gl.calls("createFramebuffer").len(), 2);
        assert!(
            gl.calls("uniform1f")
                .contains(&json!(["uniform1f", "uniform#4", 1.5]))
        );
        // Off again: no more composite passes.
        gl.clear();
        r.apply_feature("glow", &json!(0));
        host.run_until_idle();
        assert!(gl.calls("blendFunc").is_empty());
    }

    #[test]
    fn a_glow_option_at_construction_builds_the_screen_pipeline_at_mount() {
        let (host, r) = unmounted(NativeBackend::Auto, true, 2.0);
        r.mount(&dark_theme());
        assert_eq!(host.gl().calls("createProgram").len(), 2);
    }

    #[test]
    fn the_glow_feature_is_stored_under_canvas2d_too() {
        let (host, r) = mounted(NativeBackend::Canvas2d, false);
        r.apply_feature("glow", &json!(0.75));
        host.run_until_idle();
        assert_eq!(r.read_feature("glow"), Some(json!(0.75)));
    }

    #[test]
    fn destroying_frees_the_gl_objects_and_removes_the_canvas() {
        let (host, r) = mounted(NativeBackend::Auto, true);
        feed(&host, &r);
        r.destroy();
        let ops = host.gl().ops();
        assert_eq!(
            ops[ops.len() - 2..],
            [
                json!(["getExtension", "WEBGL_lose_context"]),
                json!(["loseContext"])
            ]
        );
        assert!(host.removed_slots().contains(&CanvasSlot::Data));
        assert!(host.gl_canvas().is_none());
    }

    #[test]
    fn a_screenshot_composites_the_gl_data_canvas() {
        let (host, r) = mounted(NativeBackend::Auto, true);
        feed(&host, &r);
        let pile = r.screenshot_pile().unwrap();
        let labels: Vec<String> = pile
            .images
            .iter()
            .filter_map(|i| i.downcast_ref::<String>().cloned())
            .collect();
        assert!(labels.contains(&"Data(gl)".to_string()), "{labels:?}");
    }
}
