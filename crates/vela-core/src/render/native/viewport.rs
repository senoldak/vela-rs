//! The viewport: visible range, pan, zoom, fling, the eased scroll and the frame
//! of animation that drives them (`getVisibleRange` .. `emitViewportChange` of the
//! original).

use crate::indicators::classics::math::{js_max, js_min};
use crate::ports::chart_renderer::LayoutMode;
use crate::ports::chart_renderer::VisibleRange;
use crate::render::core::scene_graph::PaneNode;
use crate::render::core::scheduler::InvalidateLevel;
use crate::render::core::viewport_state::{MAX_BAR_SPACING, MIN_BAR_SPACING};
use crate::render::core::{PriceScale, ViewportState, clamp_bar_spacing, ease_toward};

use super::host::ScrollButtonState;
use super::state::{
    Event, FLING_STOP_PX, MIN_VISIBLE_BARS, SCROLL_BTN_PROXIMITY_PX, SCROLL_BTN_SIZE, State,
    ZOOM_OUT_MARGIN_BARS,
};

impl State {
    pub(crate) fn get_visible_range(&self) -> Option<VisibleRange> {
        let n = self.coords.bar_count();
        if n == 0 || self.coords.width() == 0.0 {
            return None;
        }
        // Data-bounded like LwC: clamp to actual bar indices so `from`/`to` are REAL
        // bar times. Visible-range scripts compare `time == chart.right_visible_bar_time`
        // exactly, so the extrapolated whitespace edge would never match a bar.
        let vr = self.coords.visible_logical_range();
        let from = js_max(0.0, vr.from.ceil());
        let to = js_min((n - 1) as f64, vr.to.floor());
        if to < from {
            return None; // scrolled entirely into the whitespace
        }
        Some(VisibleRange {
            from: self.coords.logical_to_time(from) as i64,
            to: self.coords.logical_to_time(to) as i64,
        })
    }

    pub(crate) fn set_visible_range(&mut self, range: VisibleRange, right_margin: bool) {
        let from_l = self.coords.time_to_logical(range.from as f64);
        let to_l = self.coords.time_to_logical(range.to as f64);
        let span = js_max(1e-6, to_l - from_l);
        // A preset frame leaves the right margin's whitespace past `to`, sharing the
        // width with the range.
        let margin = if right_margin {
            self.scene.style.margins.right
        } else {
            0.0
        };
        let bar_spacing = clamp_bar_spacing(
            self.coords.width() / ((span + margin) * self.coords.spacing_scale()),
        );
        let right_offset = to_l - (self.coords.bar_count() as f64 - 1.0) + margin;
        self.apply_viewport(ViewportState {
            bar_spacing,
            right_offset,
        });
    }

    /// Pan by a fraction of the visible width at constant zoom (positive: toward the
    /// latest bars). Mirrors a drag: the target is clamped by the same viewport bounds
    /// and eases on the scroll-to-latest glide; repeated calls retarget the running
    /// glide, so a held key reads as one continuous scroll.
    pub(crate) fn pan_by(&mut self, fraction: f64) {
        if self.coords.bar_count() == 0 || self.coords.width() == 0.0 {
            return;
        }
        let vp = self.viewport();
        let vis_bars = self.coords.width() / self.coords.px_per_bar();
        let base = self.scroll_target_ro.unwrap_or(vp.right_offset);
        let target = self
            .clamp_viewport(vp.bar_spacing, base + fraction * vis_bars)
            .right_offset;
        self.glide_right_offset(target);
    }

    /// Instant viewport set (drag, freeze-on-touch, setVisibleRange): stops any animation.
    pub(crate) fn apply_viewport(&mut self, vp: ViewportState) {
        self.anim.stop();
        self.pan_velocity = 0.0;
        self.scroll_target_ro = None;
        let clamped = self.clamp_viewport(vp.bar_spacing, vp.right_offset);
        self.coords.set_viewport(clamped);
        self.target_bar_spacing = clamped.bar_spacing;
        self.invalidate(InvalidateLevel::Full);
        self.emit_viewport_change();
    }

    /// Eased cursor-anchored zoom: glide barSpacing toward the target, pinning the
    /// anchor logical at its pixel.
    pub(crate) fn zoom_to(&mut self, target: f64, anchor_logical: f64, anchor_x: f64) {
        // Clamp the target to the dynamic zoom range so the animator settles inside it.
        let bar_spacing = self
            .clamp_viewport(target, self.viewport().right_offset)
            .bar_spacing;
        self.zoom_anchor_logical = anchor_logical;
        self.zoom_anchor_x = anchor_x;
        self.pan_velocity = 0.0;
        self.scroll_target_ro = None;
        if !self.anim_zoom.on() {
            let v = self.clamp_viewport(bar_spacing, self.anchored_right_offset(bar_spacing));
            self.coords.set_viewport(v);
            self.target_bar_spacing = v.bar_spacing;
            self.invalidate(InvalidateLevel::Full);
            self.emit_viewport_change();
            return;
        }
        self.target_bar_spacing = bar_spacing;
        self.anim.start();
    }

    /// Inertial pan: continue with a rightOffset velocity (logical units / ms) that decays.
    pub(crate) fn fling(&mut self, velocity: f64) {
        if !self.anim_pan.on() {
            return; // pan animation off: drag-release stops dead
        }
        self.scroll_target_ro = None; // a fresh flick cancels an in-flight scroll-to-latest glide
        self.pan_velocity = velocity;
        self.anim.start();
    }

    pub(crate) fn anchored_right_offset(&self, bar_spacing: f64) -> f64 {
        // Pinning a pixel while zooming is a px-to-bar conversion, so it uses the
        // EFFECTIVE pitch (zoom x spacing multiplier), else the anchor drifts.
        let pitch = bar_spacing * self.coords.spacing_scale();
        self.zoom_anchor_logical + (self.coords.width() - self.zoom_anchor_x) / pitch
            - (self.coords.bar_count() as f64 - 1.0)
    }

    /// Bound the viewport so the user cannot zoom or pan into a useless state: zoom
    /// out no further than "all bars + a small margin fill the width", zoom in no
    /// further than keeping at least [`MIN_VISIBLE_BARS`] candles on screen, and pan
    /// no further than a bounded right whitespace / the same minimum visible. Applied
    /// at every viewport write.
    pub(crate) fn clamp_viewport(&self, bar_spacing: f64, right_offset: f64) -> ViewportState {
        let n = self.coords.bar_count();
        let w = self.coords.width();
        if n <= 1 || w <= 0.0 {
            return ViewportState {
                bar_spacing: clamp_bar_spacing(bar_spacing),
                right_offset,
            };
        }
        let nf = n as f64;
        let min_visible = js_min(MIN_VISIBLE_BARS, nf);
        // Bars-on-screen depends on the EFFECTIVE pitch (barSpacing x spacing
        // multiplier), so the multiplier folds into these bar-count limits.
        let scale = self.coords.spacing_scale();
        let max_bs = js_min(MAX_BAR_SPACING, w / (min_visible * scale));
        // The breathing room is at least the configured right margin, so every bar AND
        // that margin fit at full zoom-out.
        let room = js_max(ZOOM_OUT_MARGIN_BARS, self.scene.style.margins.right);
        let min_bs = js_max(MIN_BAR_SPACING, js_min(max_bs, w / ((nf + room) * scale)));
        let bs = js_max(min_bs, js_min(max_bs, bar_spacing));
        // The ONLY pan limit is keeping at least minVisible candles on screen.
        let vis_bars = w / (bs * scale);
        let max_ro = vis_bars - (min_visible - 1.0);
        let min_ro = min_visible - nf;
        let ro = js_max(min_ro, js_min(max_ro, right_offset));
        ViewportState {
            bar_spacing: bs,
            right_offset: ro,
        }
    }

    /// One animation frame: ease zoom, integrate inertia, glide autoscale, then paint.
    pub(crate) fn anim_tick(&mut self, dt_ms: f64) -> bool {
        let vp = self.viewport();
        let mut bar_spacing = vp.bar_spacing;
        let mut right_offset = vp.right_offset;
        let mut active = false;

        let tbs = self.target_bar_spacing;
        if (bar_spacing - tbs).abs() > tbs * 1e-3 {
            bar_spacing =
                clamp_bar_spacing(ease_toward(bar_spacing, tbs, dt_ms, self.anim_zoom.tau()));
            right_offset = self.anchored_right_offset(bar_spacing);
            active = true;
        } else if bar_spacing != tbs {
            bar_spacing = tbs;
            right_offset = self.anchored_right_offset(bar_spacing);
        }

        // Zoom-invariant stop point.
        let stop_vel = FLING_STOP_PX / js_max(1e-6, bar_spacing * self.coords.spacing_scale());
        if self.pan_velocity.abs() > stop_vel {
            right_offset += self.pan_velocity * dt_ms;
            let tau = self.anim_pan.tau(); // 0 (switched off mid-fling): the glide ends this frame
            self.pan_velocity = if tau > 0.0 {
                self.pan_velocity * (-dt_ms / tau).exp()
            } else {
                0.0
            };
            if self.pan_velocity.abs() <= stop_vel {
                self.pan_velocity = 0.0;
            } else {
                active = true;
            }
        }

        // Scroll-to-latest glide: ease rightOffset toward the target margin at constant zoom.
        if let Some(target) = self.scroll_target_ro {
            let next = ease_toward(right_offset, target, dt_ms, self.anim_scroll.tau());
            if (next - target).abs() < 1e-3 {
                right_offset = target;
                self.scroll_target_ro = None;
            } else {
                right_offset = next;
                active = true;
            }
        }

        let clamped = self.clamp_viewport(bar_spacing, right_offset);
        // The clamp bounds depend on the chart WIDTH, so a resize can strand a target
        // outside the reachable range mid-ease. Snap a target the clamp rejected onto
        // the bound it chose, else the ease re-arms forever.
        if clamped.bar_spacing != bar_spacing {
            self.target_bar_spacing = clamped.bar_spacing;
        }
        if self.scroll_target_ro.is_some() && clamped.right_offset != right_offset {
            self.scroll_target_ro = Some(clamped.right_offset);
        }
        self.coords.set_viewport(clamped);
        self.compute_scales(); // sets pane.scale_target (animator active: no snap)
        if self.ease_scales(dt_ms) {
            active = true;
        }
        if self.ease_live_bar(dt_ms) {
            active = true; // glide the forming bar toward the latest tick
        }
        self.skeleton_clock_ms += dt_ms;
        self.paint_data();
        self.render_crosshair_layer();
        self.update_legend_values(); // the animator owns the frame: render_frame will not run
        self.emit_viewport_change();
        active
    }

    /// Ease each pane's rendered scale toward its autoscale target (snap within
    /// epsilon). Log panes ease in LOG space so the glide is uniform on screen.
    fn ease_scales(&mut self, dt_ms: f64) -> bool {
        let mut moving = false;
        let tau = self.anim_autoscale.tau(); // 0: every window snaps to its target this frame
        for pane in self.scene.panes.values_mut() {
            let t = pane.scale_target;
            let s = pane.scale;
            if t.is_log() && s.min > 0.0 && t.min > 0.0 {
                let lt0 = t.min.ln();
                let lt1 = t.max.ln();
                let lspan = js_max(1e-9, (lt1 - lt0).abs());
                let n0 = ease_toward(s.min.ln(), lt0, dt_ms, tau);
                let n1 = ease_toward(s.max.ln(), lt1, dt_ms, tau);
                if (n0 - lt0).abs() <= lspan * 1e-3 && (n1 - lt1).abs() <= lspan * 1e-3 {
                    pane.scale = log_scale(t.min, t.max);
                } else {
                    moving = true;
                    pane.scale = log_scale(n0.exp(), n1.exp());
                }
                continue;
            }
            let span = js_max(1e-9, (t.max - t.min).abs());
            let mut nmin = ease_toward(s.min, t.min, dt_ms, tau);
            let mut nmax = ease_toward(s.max, t.max, dt_ms, tau);
            if (nmin - t.min).abs() <= span * 1e-3 && (nmax - t.max).abs() <= span * 1e-3 {
                nmin = t.min;
                nmax = t.max;
            } else {
                moving = true;
            }
            pane.scale = PriceScale {
                min: nmin,
                max: nmax,
                log: t.log,
                invert: None,
            };
        }
        // Merged (own-scale) indicator windows glide the same way (always linear).
        for sl in self.scene.indicator_scales.values_mut() {
            let t = sl.scale_target;
            let s = sl.scale;
            let span = js_max(1e-9, (t.max - t.min).abs());
            let mut nmin = ease_toward(s.min, t.min, dt_ms, tau);
            let mut nmax = ease_toward(s.max, t.max, dt_ms, tau);
            if (nmin - t.min).abs() <= span * 1e-3 && (nmax - t.max).abs() <= span * 1e-3 {
                nmin = t.min;
                nmax = t.max;
            } else {
                moving = true;
            }
            sl.scale = PriceScale::new(nmin, nmax);
        }
        moving
    }

    pub(crate) fn emit_viewport_change(&mut self) {
        self.update_scroll_to_realtime_button(); // reveal/hide as the latest bar scrolls in/out
        if let Some(range) = self.get_visible_range() {
            self.emit(Event::Viewport(range));
        }
    }

    // ── fit / reframe / scroll ──

    pub(crate) fn fit_content(&mut self) {
        let n = self.coords.bar_count();
        let w = self.coords.width();
        if n == 0 || w <= 0.0 {
            return;
        }
        self.anim.stop(); // a hard re-frame cancels any in-flight gesture
        self.pan_velocity = 0.0;
        for pane in self.scene.panes.values_mut() {
            pane.manual_scale = None; // re-fit: autoscale resumes
        }
        for sl in self.scene.indicator_scales.values_mut() {
            sl.manual_scale = None;
        }
        let visible_bars = js_min(n as f64, 200.0);
        let right_offset = self.scene.style.margins.right;
        let v = self.clamp_viewport(
            w / ((visible_bars + right_offset) * self.coords.spacing_scale()),
            right_offset,
        );
        self.coords.set_viewport(v);
        self.target_bar_spacing = v.bar_spacing;
    }

    /// Re-frame after a series replacement (a symbol/timeframe switch): keep the user's
    /// zoom, re-anchor the newest bars at the configured right margin. The
    /// fit-all-bars floor of `clamp_viewport` deliberately does NOT apply: a
    /// progressive head may still be backfilling toward the previous depth.
    pub(crate) fn reframe_keep_zoom(&mut self) {
        self.anim.stop();
        self.pan_velocity = 0.0;
        for pane in self.scene.panes.values_mut() {
            pane.manual_scale = None;
        }
        for sl in self.scene.indicator_scales.values_mut() {
            sl.manual_scale = None;
        }
        let v = ViewportState {
            bar_spacing: clamp_bar_spacing(self.viewport().bar_spacing),
            right_offset: self.scene.style.margins.right,
        };
        self.coords.set_viewport(v);
        self.target_bar_spacing = v.bar_spacing;
    }

    /// Re-fit the view and drop every manual scale (keyboard `0`).
    pub(crate) fn reset_view(&mut self) {
        self.fit_content(); // clears all manual scales
        self.invalidate(InvalidateLevel::Full);
        self.emit_viewport_change();
    }

    /// Glide the view back to the most recent bars, keeping the current zoom.
    pub(crate) fn scroll_to_realtime(&mut self) {
        if self.coords.bar_count() == 0 {
            return;
        }
        let target = self.scene.style.margins.right;
        self.glide_right_offset(target);
    }

    /// Ease rightOffset to `target` at constant zoom; instant when the scroll glide
    /// is off. Shared by scroll-to-latest and `pan_by`.
    pub(crate) fn glide_right_offset(&mut self, target: f64) {
        let vp = self.viewport();
        if !self.anim_scroll.on() {
            self.apply_viewport(ViewportState {
                bar_spacing: vp.bar_spacing,
                right_offset: target,
            });
            return;
        }
        self.pan_velocity = 0.0;
        self.target_bar_spacing = vp.bar_spacing; // keep zoom fixed so anim_tick does not re-anchor
        self.scroll_target_ro = Some(target);
        self.anim.start();
    }

    /// Show the scroll button only when the latest bar is scrolled off the right
    /// edge. On desktop the cursor must also be nearby; on mobile (no hovering
    /// cursor) it appears whenever the latest bars are off-screen.
    pub(crate) fn update_scroll_to_realtime_button(&mut self) {
        if !self.mounted {
            return;
        }
        let latest_off_screen = self.coords.bar_count() > 0 && self.viewport().right_offset < 0.0;
        let mobile = self.layout_mode == LayoutMode::Mobile;
        let visible = if mobile {
            latest_off_screen
        } else {
            latest_off_screen && self.pointer_near_scroll_btn
        };
        self.push_scroll_button(visible);
    }

    pub(crate) fn push_scroll_button(&mut self, visible: bool) {
        let state = ScrollButtonState {
            visible,
            bottom_px: self.scroll_btn_bottom_px,
            right_px: self.scroll_btn_right_px,
            touch: self.layout_mode == LayoutMode::Mobile,
        };
        if self.scroll_btn_sent != Some(state) {
            self.scroll_btn_sent = Some(state);
            self.host.scroll_button(&state);
        }
    }

    /// Track cursor proximity to the scroll button (`x`, `y` plot-relative px, `width`
    /// and `height` the plot's CSS size).
    pub(crate) fn scroll_proximity_move(&mut self, x: f64, y: f64, width: f64, height: f64) {
        if !self.mounted {
            return;
        }
        let cx = width - self.scroll_btn_right_px - SCROLL_BTN_SIZE / 2.0;
        let cy = height - self.scroll_btn_bottom_px - SCROLL_BTN_SIZE / 2.0;
        let near = (x - cx).hypot(y - cy) <= SCROLL_BTN_PROXIMITY_PX;
        if near != self.pointer_near_scroll_btn {
            self.pointer_near_scroll_btn = near;
            self.update_scroll_to_realtime_button();
        }
    }

    pub(crate) fn scroll_proximity_leave(&mut self) {
        if !self.pointer_near_scroll_btn {
            return;
        }
        self.pointer_near_scroll_btn = false;
        self.update_scroll_to_realtime_button();
    }

    /// The pane node whose vertical bounds contain `y`.
    pub(crate) fn pane_node_at_y(&self, y: f64) -> Option<&PaneNode> {
        super::user_drawings::pane_node_at_y(&self.scene, y)
    }

    /// The pane whose bounds sit closest to `y`: the forgiving fallback for points
    /// that fall BETWEEN bounds (separator gaps, the strip under the last pane).
    pub(crate) fn nearest_pane_to_y(&self, y: f64) -> Option<&PaneNode> {
        let mut best: Option<&PaneNode> = None;
        let mut best_dist = f64::INFINITY;
        for pane in self.scene.panes.values() {
            let d = if y < pane.bounds.top {
                pane.bounds.top - y
            } else {
                y - (pane.bounds.top + pane.bounds.height)
            };
            if d < best_dist {
                best_dist = d;
                best = Some(pane);
            }
        }
        best
    }
}

fn log_scale(min: f64, max: f64) -> PriceScale {
    PriceScale {
        min,
        max,
        log: Some(true),
        invert: None,
    }
}
