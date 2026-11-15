//! Scale computation: the per-pane autoscale pass (`computeScales`), the manual
//! price scale (axis drag and vertical pan), and the per-pane log / percent /
//! invert settings.

use crate::indicators::classics::math::{js_max, js_min, js_round};
use crate::model::{IndicatorModel, Ohlcv, PaneAxis, PaneKind, SeriesData};
use crate::ports::chart_renderer::VelaTheme;
use crate::render::chrome::axis_layout::{AXIS_MASTER_W, AXIS_MERGED_W};
use crate::render::core::autoscale::{
    ValueExtent, compute_pane_scale, expand_scale_by_pixels, overlay_series_range,
};
use crate::render::core::manual_scale::{rescale_around, shift_scale};
use crate::render::core::scene_graph::{
    PaneAxisFormat, PaneNode, ScaleMode, pane_invert, pane_log_scale, pane_scale_mode,
};
use crate::render::core::scheduler::InvalidateLevel;
use crate::render::core::{CoordinateSystem, PriceScale};
use crate::render::shared::drawing_scene_renderer::{
    BarHighLow, DrawingSceneRenderer, LayerDeps, model_drawing_set,
};
use crate::render::shared::trade_markers::{
    BarExtent, TradeMarkerDeps, TradeMarkerHints, trades_price_hints,
};
use crate::render::volume::VOLUME_PANE_FILL_FRAC;

use super::state::{PRICE_PANE_ID, PRICE_SCALE_K, ScaleHolder, State};

fn union_range(a: Option<ValueExtent>, b: Option<ValueExtent>) -> Option<ValueExtent> {
    match (a, b) {
        (None, b) => b,
        (a, None) => a,
        (Some(a), Some(b)) => Some(ValueExtent {
            min: js_min(a.min, b.min),
            max: js_max(a.max, b.max),
        }),
    }
}

/// `array[Math.round(logical)]`.
fn bar_at_logical(bars: &[Ohlcv], logical: f64) -> Option<&Ohlcv> {
    let i = js_round(logical);
    if i >= 0.0 && i.fract() == 0.0 {
        bars.get(i as usize)
    } else {
        None
    }
}

/// The visible Pine-drawing price range of one set of models (`drawingsRange`).
#[allow(clippy::too_many_arguments)]
fn drawings_range(
    draw_scene: &mut DrawingSceneRenderer,
    coords: &CoordinateSystem,
    bars: &[Ohlcv],
    theme: &VelaTheme,
    model: &IndicatorModel,
    overlay: bool,
    vr_from: f64,
    vr_to: f64,
    offset: i64,
) -> Option<ValueExtent> {
    draw_scene.set_set(model_drawing_set(model, overlay), offset as f64);
    if draw_scene.is_empty() {
        return None;
    }
    let time_to_logical = |ms: f64| coords.time_to_logical(ms);
    let bar_at = |logical: f64| {
        bar_at_logical(bars, logical).map(|b| BarHighLow {
            high: b.high,
            low: b.low,
        })
    };
    let deps = LayerDeps {
        time_to_logical: &time_to_logical,
        bar_at: &bar_at,
        theme,
    };
    draw_scene
        .price_range(&deps, vr_from, vr_to)
        .map(|r| ValueExtent {
            min: r.min,
            max: r.max,
        })
}

struct HintDeps<'a> {
    coords: &'a CoordinateSystem,
    bars: &'a [Ohlcv],
}

impl TradeMarkerDeps for HintDeps<'_> {
    fn time_to_logical(&self, ms: f64) -> f64 {
        self.coords.time_to_logical(ms)
    }
    fn bar_at(&self, logical: f64) -> Option<BarExtent> {
        bar_at_logical(self.bars, logical).map(|b| BarExtent {
            high: b.high,
            low: b.low,
        })
    }
}

/// What one pane's autoscale pass produced.
struct PaneScaleResult {
    manual: Option<PriceScale>,
    scale_target: PriceScale,
    percent_baseline: f64,
    axis_format: Option<PaneAxisFormat>,
    axis_bands: Option<Vec<crate::model::PaneAxisBand>>,
}

impl State {
    /// Per-pane autoscale: the single price-window pass both layers consume. Folds
    /// the visible series with the visible Pine-drawing range (own drawings per pane
    /// plus `force_overlay` drawings on the price pane), so a box, line or label
    /// outside the series range still expands the scale and never clips. Runs only on
    /// Light and Full frames (the cursor tier reuses the retained `pane.scale`).
    pub(crate) fn compute_scales(&mut self) {
        let n = self.coords.bar_count();
        if n == 0 {
            return;
        }
        let vr = self.coords.visible_logical_range();
        let i0 = js_max(0.0, vr.from.floor());
        let i1 = js_min((n - 1) as f64, vr.to.ceil());
        if i1 < i0 {
            return;
        }
        let (i0, i1) = (i0 as i64, i1 as i64);
        // Series points past the last bar paint in the right whitespace, so they scale there too.
        let s1 = js_max(i1 as f64, vr.to.ceil()) as i64;
        // Percent mode: the reference is the first visible bar's close.
        self.scene.percent_baseline = self.bars.get(i0 as usize).map_or(0.0, |b| b.close);
        let pane_ids = self.scene.ordered_pane_ids();
        let price_id = self
            .scene
            .ordered_panes()
            .iter()
            .find(|p| p.kind == PaneKind::Price)
            .map(|p| p.id.clone());
        let animating = self.anim.active();
        let margins = self.scene.style.margins;

        for pane_id in &pane_ids {
            let is_price = price_id.as_deref() == Some(pane_id.as_str());
            let result = self.pane_scale_pass(pane_id, is_price, vr.from, vr.to, i0, i1, s1);
            let Some(result) = result else { continue };
            {
                let Some(pane) = self.scene.panes.get_mut(pane_id) else {
                    continue;
                };
                if let Some(manual) = result.manual {
                    // Manual mode (price-axis drag / vertical pan): the user's window
                    // verbatim, no autoscale.
                    pane.scale_target = manual;
                    pane.scale = manual;
                    pane.initialized = true;
                    continue;
                }
                pane.scale_target = result.scale_target;
                pane.percent_baseline = result.percent_baseline;
                pane.axis_format = result.axis_format;
                pane.axis_bands = result.axis_bands;
                // Idle: snap. While zooming or flinging the animator eases pane.scale
                // toward the target, EXCEPT an uninitialized pane (added mid-gesture),
                // which snaps once so it does not flash-ease from the {0,1} placeholder.
                if !animating || !pane.initialized {
                    pane.scale = pane.scale_target;
                    pane.initialized = true;
                }
            }
            // Merged indicators: each gets its own window, rescaled with identical
            // margins so its visible extent lines up pixel for pixel with the pane's.
            let own_ids: Vec<String> = self
                .scene
                .own_scale_indicators_for_pane(pane_id)
                .iter()
                .map(|m| m.id.clone())
                .collect();
            let seed = self.scene.panes.get(pane_id).map(|p| p.scale_target);
            for id in own_ids {
                let manual = self.scene.ensure_indicator_scale(&id, seed).manual_scale;
                if let Some(manual) = manual {
                    let sl = self.scene.ensure_indicator_scale(&id, seed);
                    sl.scale_target = manual;
                    sl.scale = manual;
                    sl.initialized = true;
                    continue;
                }
                let target = {
                    let Some(model) = self.scene.indicators.get(&id) else {
                        continue;
                    };
                    let mdr = drawings_range(
                        &mut self.draw_scene,
                        &self.coords,
                        &self.bars,
                        &self.theme,
                        model,
                        false,
                        vr.from,
                        vr.to,
                        self.scene.offset_of(&id),
                    );
                    let scene = &self.scene;
                    compute_pane_scale(
                        [model],
                        &self.bars,
                        false,
                        i0,
                        s1,
                        mdr,
                        false,
                        &|mid| scene.offset_of(mid),
                        margins,
                    )
                };
                let sl = self.scene.ensure_indicator_scale(&id, seed);
                sl.scale_target = target;
                if !animating || !sl.initialized {
                    sl.scale = sl.scale_target;
                    sl.initialized = true;
                }
            }
        }
    }

    /// The immutable half of one pane's autoscale: everything computed from the
    /// scene as it stands. `None` when the pane vanished.
    #[allow(clippy::too_many_arguments)]
    fn pane_scale_pass(
        &mut self,
        pane_id: &str,
        is_price: bool,
        vr_from: f64,
        vr_to: f64,
        i0: i64,
        i1: i64,
        s1: i64,
    ) -> Option<PaneScaleResult> {
        let pane = self.scene.panes.get(pane_id)?.clone();
        if let Some(manual) = pane.manual_scale {
            return Some(PaneScaleResult {
                manual: Some(manual),
                scale_target: manual,
                percent_baseline: pane.percent_baseline,
                axis_format: pane.axis_format,
                axis_bands: pane.axis_bands.clone(),
            });
        }
        let margins = self.scene.style.margins;
        let scene = &self.scene;
        let bars = &self.bars;
        let offset_of = |id: &str| scene.offset_of(id);
        let models = scene.indicators_for_pane(pane_id);
        // A merged (own-scale) indicator does NOT contribute to the pane master scale.
        let master_models: Vec<&IndicatorModel> =
            models.iter().copied().filter(|m| !m.own_scale).collect();
        // User drawings do not expand the scale (placing one in the empty margin must
        // not yank the window to follow the cursor). Pine drawings still fold in.
        let mut dr: Option<ValueExtent> = None;
        for m in &master_models {
            dr = union_range(
                dr,
                drawings_range(
                    &mut self.draw_scene,
                    &self.coords,
                    bars,
                    &self.theme,
                    m,
                    false,
                    vr_from,
                    vr_to,
                    scene.offset_of(&m.id),
                ),
            );
        }
        if is_price {
            for m in scene.indicators.values() {
                dr = union_range(
                    dr,
                    drawings_range(
                        &mut self.draw_scene,
                        &self.coords,
                        bars,
                        &self.theme,
                        m,
                        true,
                        vr_from,
                        vr_to,
                        scene.offset_of(&m.id),
                    ),
                );
            }
        }
        // force_overlay series render on the price pane whatever pane their indicator
        // owns: fold their visible range in here (their own pane excludes them).
        if is_price {
            let or = overlay_series_range(scene.indicators.values(), i0, s1, &offset_of);
            dr = union_range(dr, or);
        }
        // Hidden candles drop out of the price pane's autoscale, so overlay indicators
        // fill the pane, unless layer content still paints at bar prices there, or
        // nothing else on the pane can be measured.
        let include_candles = pane.kind == PaneKind::Price
            && (!scene.candles_hidden
                || self.price_layers_anchored_to_bars(&master_models)
                || !pane_has_measurable_content(&master_models, dr));
        // Each pane logs on its OWN flag. Scale price chart only: the bars alone set the
        // price window; with the bars out of the scale there is nothing else to fit.
        let log = pane_log_scale(scene, &pane);
        let price_only = is_price && scene.scale_price_only && include_candles;
        let mut scale_target = if price_only {
            compute_pane_scale([], bars, true, i0, s1, None, log, &offset_of, margins)
        } else {
            compute_pane_scale(
                master_models.iter().copied(),
                bars,
                include_candles,
                i0,
                s1,
                dr,
                log,
                &offset_of,
                margins,
            )
        };
        // Percent baseline for THIS pane: the first visible bar close on the price pane,
        // else the master series' first visible value. 0 means no reference yet.
        let mut percent_baseline = if pane.kind == PaneKind::Price {
            bars.get(i0 as usize).map_or(0.0, |b| b.close)
        } else {
            self.first_visible_value(&master_models, i0)
        };
        let mut axis_format = None;
        let mut axis_bands = None;
        // The volume layer is a bespoke renderer layer with no model series, so on its
        // own pane the master scale would be the empty {0,1} placeholder: drive it from
        // the visible volume instead, matching the columns' bottom-anchored mapping.
        if self.volume_owns_pane(&pane, &master_models) {
            let max_vol = self.max_visible_volume(i0, i1);
            if max_vol > 0.0 {
                scale_target = PriceScale::new(0.0, max_vol / VOLUME_PANE_FILL_FRAC);
                axis_format = Some(PaneAxisFormat::Volume);
            }
        } else if self.layer_natives_own_pane(&pane, &master_models) {
            // An SDK-layer native moved to its own pane hits the same placeholder: its
            // layer paints at BAR PRICES, so scale the pane from the visible bars.
            scale_target = compute_pane_scale([], bars, true, i0, i1, dr, log, &offset_of, margins);
            percent_baseline = bars.get(i0 as usize).map_or(0.0, |b| b.close);
            // Content declaring a paneAxis override is not value-mapped: no price ticks,
            // no horizontal gridlines, no crosshair chip.
            if master_models.iter().all(|m| m.pane_axis.is_some()) {
                axis_format = Some(PaneAxisFormat::NoAxis);
                axis_bands = master_models.iter().find_map(|m| match &m.pane_axis {
                    Some(PaneAxis::Bands(bands)) => Some(bands.clone()),
                    _ => None,
                });
            }
        }
        // Strategy trade markers reserve their PIXEL headroom on the price pane, so a
        // marker stack under the lows (or above the highs) never clips at the pane edge.
        if is_price
            && scene.trade_markers.visible
            && pane.bounds.height > 0.0
            && let Some(th) = self.trades_scale_hints(vr_from, vr_to)
        {
            let st = scale_target;
            let folded = PriceScale {
                min: js_min(st.min, th.min),
                max: js_max(st.max, th.max),
                log: st.log,
                invert: st.invert,
            };
            scale_target =
                expand_scale_by_pixels(&folded, pane.bounds.height, th.above_px, th.below_px);
        }
        Some(PaneScaleResult {
            manual: None,
            scale_target,
            percent_baseline,
            axis_format,
            axis_bands,
        })
    }

    /// Union of the visible trade-marker autoscale hints across every mounted
    /// indicator (trade markers always target the price pane).
    fn trades_scale_hints(&self, from: f64, to: f64) -> Option<TradeMarkerHints> {
        let deps = HintDeps {
            coords: &self.coords,
            bars: &self.bars,
        };
        let mut out: Option<TradeMarkerHints> = None;
        for m in self.scene.indicators.values() {
            if m.trades.is_empty() {
                continue;
            }
            let Some(h) = trades_price_hints(
                &m.trades,
                &self.scene.trade_markers,
                &deps,
                from,
                to,
                self.scene.style.font_size,
            ) else {
                continue;
            };
            out = Some(match out {
                Some(o) => TradeMarkerHints {
                    min: js_min(o.min, h.min),
                    max: js_max(o.max, h.max),
                    above_px: js_max(o.above_px, h.above_px),
                    below_px: js_max(o.below_px, h.below_px),
                },
                None => h,
            });
        }
        out
    }

    /// True when an overlay layer native (a series-less model whose type names a
    /// mounted layer) sits on the price pane: it paints at BAR PRICES without
    /// contributing a series, so hiding the candles must keep the bars in the scale.
    fn price_layers_anchored_to_bars(&self, master_models: &[&IndicatorModel]) -> bool {
        master_models
            .iter()
            .any(|m| m.series.is_empty() && self.native_names_a_layer(m))
    }

    pub(crate) fn native_names_a_layer(&self, m: &IndicatorModel) -> bool {
        m.native
            .as_ref()
            .is_some_and(|n| self.ext_layers.iter().any(|l| l.def.id == n.indicator_type))
    }

    /// True when an active volume layer is this study pane's ONLY content: its scale
    /// should come from volume, not the empty placeholder.
    fn volume_owns_pane(&self, pane: &PaneNode, master_models: &[&IndicatorModel]) -> bool {
        if pane.kind == PaneKind::Price {
            return false;
        }
        if !self.volume_active || self.volume_hidden || self.scene.volume_layer.is_none() {
            return false;
        }
        if self.native_layer_pane("volume").map(|p| p.id.as_str()) != Some(pane.id.as_str()) {
            return false;
        }
        // The volume indicator's own (series-less) model doesn't count as real content.
        master_models.iter().all(|m| {
            m.native
                .as_ref()
                .is_some_and(|n| n.indicator_type == "volume")
        })
    }

    /// True when this study pane's master content is only SDK-layer natives.
    fn layer_natives_own_pane(&self, pane: &PaneNode, master_models: &[&IndicatorModel]) -> bool {
        if pane.kind == PaneKind::Price || master_models.is_empty() {
            return false;
        }
        master_models
            .iter()
            .all(|m| m.series.is_empty() && self.native_names_a_layer(m))
    }

    /// The pane a bespoke-layer native indicator (volume / vpvr) lives in.
    pub(crate) fn native_layer_pane(&self, kind: &str) -> Option<&PaneNode> {
        for m in self.scene.indicators.values() {
            if m.native.as_ref().is_some_and(|n| n.indicator_type == kind) {
                return self
                    .scene
                    .panes
                    .get(m.pane_id.as_deref().unwrap_or(PRICE_PANE_ID));
            }
        }
        None
    }

    /// The first visible value a study pane measures percent-change from.
    fn first_visible_value(&self, models: &[&IndicatorModel], i0: i64) -> f64 {
        for m in models {
            let off = self.scene.offset_of(&m.id);
            let idx = i0 - off;
            for s in &m.series {
                if s.overlay {
                    continue; // renders on the price pane, not this one
                }
                match &s.data {
                    SeriesData::LineLike { points, .. } => {
                        if let Some(v) = slot(points, idx).and_then(|p| p.value)
                            && v.is_finite()
                        {
                            return v;
                        }
                    }
                    SeriesData::Candle { bars, .. } => {
                        if let Some(b) = slot(bars, idx)
                            && b.close.is_finite()
                        {
                            return b.close;
                        }
                    }
                    SeriesData::Markers { .. } => {}
                }
            }
        }
        0.0
    }

    /// Largest volume across the visible bar-index window (0 when none).
    fn max_visible_volume(&self, i0: i64, i1: i64) -> f64 {
        let mut max_vol = 0.0;
        let mut i = i0;
        while i <= i1 {
            if let Some(v) = slot(&self.bars, i).and_then(|b| b.volume)
                && v > max_vol
            {
                max_vol = v;
            }
            i += 1;
        }
        max_vol
    }

    // ── per-pane scale settings ──

    /// Stamp each pane's current inversion onto the live scale objects the coordinate
    /// system reads (the pane master scale, its target and manual windows, and any
    /// merged own-scale columns), so price/y conversions flip consistently across
    /// every layer. Run once per paint.
    pub(crate) fn stamp_scale_invert(&mut self) {
        let invs: Vec<(String, bool)> = self
            .scene
            .panes
            .values()
            .map(|p| (p.id.clone(), pane_invert(&self.scene, p)))
            .collect();
        for (id, inv) in invs {
            if let Some(pane) = self.scene.panes.get_mut(&id) {
                pane.scale.invert = Some(inv);
                pane.scale_target.invert = Some(inv);
                if let Some(m) = pane.manual_scale.as_mut() {
                    m.invert = Some(inv);
                }
            }
            let own: Vec<String> = self
                .scene
                .own_scale_indicators_for_pane(&id)
                .iter()
                .map(|m| m.id.clone())
                .collect();
            for model_id in own {
                if let Some(sl) = self.scene.indicator_scales.get_mut(&model_id) {
                    sl.scale.invert = Some(inv);
                    sl.scale_target.invert = Some(inv);
                    if let Some(m) = sl.manual_scale.as_mut() {
                        m.invert = Some(inv);
                    }
                }
            }
        }
    }

    pub(crate) fn pane_scale_infos(
        &self,
    ) -> Vec<(String, PaneKind, f64, f64, ScaleMode, bool, bool)> {
        self.scene
            .ordered_panes()
            .iter()
            .map(|p| {
                (
                    p.id.clone(),
                    p.kind,
                    p.bounds.top,
                    p.bounds.height,
                    pane_scale_mode(&self.scene, p),
                    pane_log_scale(&self.scene, p),
                    pane_invert(&self.scene, p),
                )
            })
            .collect()
    }

    /// Toggle one pane's autoscale (the A axis button): off freezes the current window
    /// into manual mode, on drops it.
    pub(crate) fn toggle_pane_auto(&mut self, pane_id: &str) {
        let Some(pane) = self.scene.panes.get_mut(pane_id) else {
            return;
        };
        pane.manual_scale = if pane.manual_scale.is_none() {
            Some(pane.scale)
        } else {
            None
        };
        self.invalidate(InvalidateLevel::Full);
    }

    /// Set one pane's axis mode. The price pane routes to the scene-level setting; a
    /// study pane keeps its own, so panes never affect each other.
    pub(crate) fn set_pane_scale_mode(&mut self, pane_id: &str, mode: ScaleMode) {
        if pane_id == PRICE_PANE_ID {
            self.scene.scale_mode = mode;
        } else {
            let Some(p) = self.scene.panes.get_mut(pane_id) else {
                return;
            };
            p.scale_mode = Some(mode);
        }
        self.invalidate(InvalidateLevel::Full);
    }

    /// Set one pane's logarithmic flag; the price pane routes to the scene-level flag.
    /// Log and auto are INDEPENDENT: a frozen (manual) window is kept, only its `log`
    /// tag flips, so the same range re-renders in the new space.
    pub(crate) fn set_pane_log(&mut self, pane_id: &str, log: bool) {
        let target = if pane_id == PRICE_PANE_ID {
            self.scene
                .ordered_panes()
                .iter()
                .find(|p| p.kind == PaneKind::Price)
                .map(|p| p.id.clone())
        } else {
            self.scene.panes.get(pane_id).map(|p| p.id.clone())
        };
        if pane_id == PRICE_PANE_ID {
            self.scene.log_scale = log;
        } else {
            let Some(p) = self.scene.panes.get_mut(pane_id) else {
                return;
            };
            p.log_scale = Some(log);
        }
        if let Some(id) = target
            && let Some(m) = self
                .scene
                .panes
                .get_mut(&id)
                .and_then(|p| p.manual_scale.as_mut())
        {
            m.log = Some(log);
        }
        self.invalidate(InvalidateLevel::Full);
    }

    /// Flip one pane's axis (high at the bottom).
    pub(crate) fn set_pane_invert(&mut self, pane_id: &str, invert: bool) {
        if pane_id == PRICE_PANE_ID {
            self.scene.invert_scale = invert;
        } else {
            let Some(p) = self.scene.panes.get_mut(pane_id) else {
                return;
            };
            p.invert = Some(invert);
        }
        self.invalidate(InvalidateLevel::Full);
    }

    // ── manual vertical scaling (price-axis drag and vertical price pan) ──

    /// The scale window a gesture targets: a pane's master scale, or a merged
    /// indicator's own scale column (resolved from the grabbed x).
    fn resolve_scale_holder(&mut self, x: f64, y: f64) -> Option<(ScaleHolder, f64)> {
        let (pane_id, height, scale_target) = {
            let pane = self.pane_node_at_y(y)?;
            (pane.id.clone(), pane.bounds.height, pane.scale_target)
        };
        let data_w = self.coords.width();
        let merged: Vec<String> = self
            .scene
            .own_scale_indicators_for_pane(&pane_id)
            .iter()
            .map(|m| m.id.clone())
            .collect();
        if x >= data_w + AXIS_MASTER_W && !merged.is_empty() {
            let k = ((x - data_w - AXIS_MASTER_W) / AXIS_MERGED_W).floor();
            if k >= 0.0
                && let Some(id) = merged.get(k as usize)
            {
                self.scene.ensure_indicator_scale(id, Some(scale_target));
                return Some((ScaleHolder::Indicator(id.clone()), height));
            }
        }
        Some((ScaleHolder::Pane(pane_id), height))
    }

    /// Read a holder's `(scale, scale_target, manual, initialized)`.
    fn holder_scale(&self, h: &ScaleHolder) -> Option<PriceScale> {
        match h {
            ScaleHolder::Pane(id) => self.scene.panes.get(id).map(|p| p.scale),
            ScaleHolder::Indicator(id) => self.scene.indicator_scales.get(id).map(|s| s.scale),
        }
    }

    fn set_holder_manual(&mut self, h: &ScaleHolder, manual: Option<PriceScale>) {
        match h {
            ScaleHolder::Pane(id) => {
                if let Some(p) = self.scene.panes.get_mut(id) {
                    p.manual_scale = manual;
                }
            }
            ScaleHolder::Indicator(id) => {
                if let Some(s) = self.scene.indicator_scales.get_mut(id) {
                    s.manual_scale = manual;
                }
            }
        }
    }

    fn set_manual_scale(&mut self, h: &ScaleHolder, scale: PriceScale) {
        match h {
            ScaleHolder::Pane(id) => {
                if let Some(p) = self.scene.panes.get_mut(id) {
                    p.manual_scale = Some(scale);
                    p.scale = scale;
                    p.scale_target = scale;
                    p.initialized = true;
                }
            }
            ScaleHolder::Indicator(id) => {
                if let Some(s) = self.scene.indicator_scales.get_mut(id) {
                    s.manual_scale = Some(scale);
                    s.scale = scale;
                    s.scale_target = scale;
                    s.initialized = true;
                }
            }
        }
        self.invalidate(InvalidateLevel::Full);
    }

    /// Grab the price axis: snapshot the grabbed scale's window and freeze it into
    /// manual mode.
    pub(crate) fn begin_price_scale(&mut self, x: f64, y: f64) {
        let Some((holder, height)) = self.resolve_scale_holder(x, y) else {
            return;
        };
        let Some(scale) = self.holder_scale(&holder) else {
            return;
        };
        self.scale_drag_height = height;
        self.scale_drag_start = Some(scale);
        self.set_holder_manual(&holder, Some(scale));
        self.scale_drag_holder = Some(holder);
        self.invalidate(InvalidateLevel::Full);
    }

    /// Rescale the grabbed scale around its center by the total drag (down: zoom out).
    pub(crate) fn price_scale_by(&mut self, dy_total: f64) {
        let (Some(holder), Some(start)) = (self.scale_drag_holder.clone(), self.scale_drag_start)
        else {
            return;
        };
        self.set_manual_scale(
            &holder,
            rescale_around(&start, (dy_total * PRICE_SCALE_K).exp()),
        );
    }

    /// Grab inside the data area: vertical price pan is available only when the pane
    /// is already manual, or when `unlock` freezes its visible window first.
    pub(crate) fn begin_price_pan(&mut self, y: f64, unlock: bool) -> bool {
        let Some((id, height, scale, has_manual)) = self.pane_node_at_y(y).map(|p| {
            (
                p.id.clone(),
                p.bounds.height,
                p.scale,
                p.manual_scale.is_some(),
            )
        }) else {
            return false;
        };
        if !has_manual {
            if !unlock {
                return false;
            }
            if let Some(p) = self.scene.panes.get_mut(&id) {
                p.manual_scale = Some(scale);
            }
            self.invalidate(InvalidateLevel::Full);
        }
        self.scale_drag_holder = Some(ScaleHolder::Pane(id));
        self.scale_drag_height = height;
        self.scale_drag_start = Some(scale);
        true
    }

    /// Pan the grabbed pane's price window by the total drag (down: show lower prices).
    pub(crate) fn price_pan_by(&mut self, dy_total: f64) {
        let (Some(holder), Some(start)) = (self.scale_drag_holder.clone(), self.scale_drag_start)
        else {
            return;
        };
        let height = self.scale_drag_height;
        self.set_manual_scale(&holder, shift_scale(&start, dy_total, height));
    }

    /// Double-click the price axis: drop manual mode for that scale (autoscale resumes).
    pub(crate) fn reset_price_scale(&mut self, x: f64, y: f64) {
        let Some((holder, _)) = self.resolve_scale_holder(x, y) else {
            return;
        };
        self.set_holder_manual(&holder, None);
        self.invalidate(InvalidateLevel::Full);
    }
}

fn pane_has_measurable_content(
    master_models: &[&IndicatorModel],
    drawings: Option<ValueExtent>,
) -> bool {
    if drawings.is_some() {
        return true;
    }
    master_models
        .iter()
        .any(|m| !m.price_lines.is_empty() || m.series.iter().any(|s| !s.overlay))
}

/// `array[index]` for a signed index.
fn slot<T>(items: &[T], index: i64) -> Option<&T> {
    usize::try_from(index).ok().and_then(|i| items.get(i))
}
