//! Gesture-level input: what the original's `InputController` and
//! `KeyboardController` call on the renderer. The DOM listeners are the browser
//! crate's; the public entry points on
//! [`NativeRenderer`](super::NativeRenderer) wrap these.

use crate::drawings::SnapMode;
use crate::indicators::classics::math::{js_max, js_min, js_round};
use crate::marks::MarkClickEvent;
use crate::model::{Millis, PaneKind, SeriesData};
use crate::ports::chart_renderer::{ClickEvent, CrosshairEvent, CrosshairOhlc};
use crate::render::chrome::marks_paint::MARK_PULSE_MS;
use crate::render::core::input_logic::{Edge, KeyAction};
use crate::render::core::scene_graph::{CrosshairPoint, MarksFlash};
use crate::render::core::scheduler::InvalidateLevel;
use crate::render::core::{ViewportState, clamp_bar_spacing};

use super::host::MarkPopoverAction;
use super::state::{
    Event, ExpandedBy, KEY_ZOOM_STEP, MARK_FLASH_MS, PRICE_PANE_ID, State, WEAK_SNAP_PX,
};

impl State {
    /// A pointer move over the plot (`None`: it left, or a touch released).
    pub(crate) fn handle_pointer_move(&mut self, x: Option<f64>, y: Option<f64>) {
        let (Some(x), Some(y)) = (x, y) else {
            return self.pointer_gone();
        };
        if self.coords.bar_count() == 0 {
            return self.pointer_gone();
        }
        let in_data = x >= 0.0 && y >= 0.0 && x <= self.coords.width() && y <= self.coords.height();
        self.scene.crosshair = in_data.then_some(CrosshairPoint { x, y });
        self.last_pointer = in_data.then_some((x, y));
        // The separator spans data + gutter, so its hover highlight tracks the full
        // width: the divider stays a visible, grabbable handle over the scale too.
        self.hover_separator_y = if x >= 0.0 && y >= 0.0 && y <= self.coords.height() {
            self.separator_hover_y(y)
        } else {
            None
        };
        // Cursor tier: repaint only the crosshair overlay, not the data layer.
        self.invalidate(InvalidateLevel::Cursor);
        // A multi-group mark stack fans out while the pointer is over it.
        let stack = if in_data {
            self.chrome.mark_stack_at(x, y).map(|s| s as i64)
        } else {
            None
        };
        self.set_marks_expanded(stack, ExpandedBy::Hover);
        let hover_key = if in_data {
            self.chrome
                .mark_glyph_at(x, y)
                .map(|g| g.cluster.key.clone())
        } else {
            None
        };
        self.set_mark_hover(hover_key);

        // Only report a time/value when the cursor is over a real bar (not the
        // right-offset whitespace): matches LWC emitting null off any data point.
        let logical = js_round(self.coords.x_to_logical(x));
        let on_bar = logical >= 0.0 && logical < self.coords.bar_count() as f64;
        let time = on_bar.then(|| self.coords.logical_to_time(logical) as Millis);
        let (pane_kind, pane_id, price) = match self.pane_node_at_y(y) {
            Some(p) if in_data => (
                Some(p.kind),
                Some(p.id.clone()),
                Some(self.coords.y_to_price(y, &p.scale, &p.bounds)),
            ),
            _ => (None, None, None),
        };
        let mut values: Vec<(String, f64)> = Vec::new();
        if on_bar {
            let li = logical as i64;
            for model in self.scene.indicators.values() {
                let off = self.scene.offset_of(&model.id);
                for s in &model.series {
                    if let SeriesData::LineLike { points, .. } = &s.data
                        && let Some(v) = usize::try_from(li - off)
                            .ok()
                            .and_then(|i| points.get(i))
                            .and_then(|p| p.value)
                    {
                        // A Map keyed by series id: a repeat keeps its first position.
                        match values.iter_mut().find(|(k, _)| *k == s.id) {
                            Some(e) => e.1 = v,
                            None => values.push((s.id.clone(), v)),
                        }
                    }
                }
            }
        }
        let ohlc = if on_bar {
            self.bars.get(logical as usize).map(|b| CrosshairOhlc {
                time: b.time,
                open: b.open,
                high: b.high,
                low: b.low,
                close: b.close,
                volume: b.volume,
            })
        } else {
            None
        };
        self.hover_logical = on_bar.then_some(logical as i64);
        self.emit(Event::Crosshair(CrosshairEvent {
            time,
            price,
            pane_kind,
            pane_id,
            values,
            ohlc,
        }));
    }

    fn pointer_gone(&mut self) {
        self.scene.crosshair = None;
        self.hover_separator_y = None;
        self.last_pointer = None;
        // A touch release also lands here (no resting pointer): a fan a tap opened
        // must survive it.
        if self.marks_expanded_by == ExpandedBy::Hover {
            self.set_marks_expanded(None, ExpandedBy::Hover);
        }
        self.set_mark_hover(None);
        self.invalidate(InvalidateLevel::Cursor);
        self.hover_logical = None; // off the plot: the readout falls back to the latest bar
        self.emit(Event::Crosshair(CrosshairEvent {
            time: None,
            price: None,
            pane_kind: None,
            pane_id: None,
            values: Vec::new(),
            ohlc: None,
        }));
    }

    pub(crate) fn handle_click(&mut self, x: f64, y: f64) {
        if self.coords.bar_count() == 0 {
            return;
        }
        let logical = js_round(self.coords.x_to_logical(x));
        let on_bar = logical >= 0.0 && logical < self.coords.bar_count() as f64;
        let in_data = x >= 0.0 && y >= 0.0 && x <= self.coords.width() && y <= self.coords.height();
        let pane = if in_data {
            self.pane_node_at_y(y).map(|p| {
                (
                    p.kind,
                    p.id.clone(),
                    self.coords.y_to_price(y, &p.scale, &p.bounds),
                )
            })
        } else {
            None
        };
        self.emit(Event::Click(ClickEvent {
            time: on_bar.then(|| self.coords.logical_to_time(logical) as Millis),
            price: pane.as_ref().map(|p| p.2),
            pane_kind: pane.as_ref().map(|p| p.0),
            pane_id: pane.map(|p| p.1),
        }));
    }

    // ── timeline marks ──

    /// Fan out (or collapse, with `None`) a multi-group mark stack; repaints the
    /// chrome tier when it changes. A stack whose glyph holds the open popup stays
    /// fanned: the pointer leaving the plot for the popup must not bury the glyph.
    pub(crate) fn set_marks_expanded(&mut self, stack: Option<i64>, by: ExpandedBy) {
        if self.scene.marks_expanded_stack == stack {
            return;
        }
        if stack.is_none()
            && let Some(key) = &self.mark_popover_key
            && let Some(open) = self.chrome.mark_glyph_by_key(key)
            && Some(open.stack as i64) == self.scene.marks_expanded_stack
        {
            return;
        }
        self.scene.marks_expanded_stack = stack;
        self.marks_expanded_by = by;
        self.invalidate(InvalidateLevel::Chrome);
    }

    /// A click on the mark lane: a collapsed deck fans out (the touch path), a glyph
    /// reports its cluster (`on_mark_click`) and opens the popup when any of its marks
    /// carries content. True when the click landed on the lane.
    pub(crate) fn handle_mark_click(&mut self, x: f64, y: f64) -> bool {
        let Some(glyph) = self.chrome.mark_glyph_at(x, y).cloned() else {
            if self.scene.marks_expanded_stack.is_some()
                && self.chrome.mark_stack_at(x, y).is_none()
            {
                self.set_marks_expanded(None, ExpandedBy::Hover);
            }
            return false;
        };
        if glyph.decked {
            self.set_marks_expanded(Some(glyph.stack as i64), ExpandedBy::Tap);
            return true;
        }
        let marks = &glyph.cluster.marks;
        let Some(first) = marks.first() else {
            return true;
        };
        self.emit(Event::MarkClick(MarkClickEvent {
            id: first.id.clone(),
            ids: marks.iter().map(|m| m.id.clone()).collect(),
            time: first.time,
            group: glyph.cluster.group.clone(),
        }));
        if marks.iter().any(|m| m.content.is_some()) {
            let key = glyph.cluster.key.clone();
            self.mark_popover_key = Some(key.clone());
            self.scene.marks_active_key = Some(key.clone()); // the glyph paints filled while its popup shows
            self.invalidate(InvalidateLevel::Chrome);
            self.host.mark_popover(&MarkPopoverAction::Open {
                key,
                mark_ids: marks.iter().map(|m| m.id.clone()).collect(),
                x: glyph.x,
                y: glyph.y,
                size: glyph.size,
            });
        } else {
            // Nothing to open: a brief filled flash acknowledges the click all the same.
            self.scene.marks_flash = Some(MarksFlash {
                key: glyph.cluster.key.clone(),
                until: self.host.frame_now_ms() + MARK_FLASH_MS,
            });
            self.sync_mark_pulse();
        }
        true
    }

    /// The lane glyph under the pointer swells once as the pointer lands (a
    /// frame-driven chrome repaint for the pulse's duration).
    pub(crate) fn set_mark_hover(&mut self, key: Option<String>) {
        if self.scene.marks_hover_key == key {
            return;
        }
        self.scene.marks_hover_key = key;
        self.scene.marks_hover_since = self.host.frame_now_ms();
        self.invalidate(InvalidateLevel::Chrome);
        self.sync_mark_pulse();
    }

    /// Run a chrome-tier repaint loop while a glyph's hover pulse or click flash
    /// plays; it stops itself once both are over.
    pub(crate) fn sync_mark_pulse(&mut self) {
        if self.mark_pulse_raf.is_some() || !self.mounted {
            return;
        }
        let handle = self.request_task(|r| r.mark_pulse_frame());
        self.mark_pulse_raf = Some(handle);
    }

    pub(crate) fn mark_pulse_step(&mut self) {
        self.mark_pulse_raf = None;
        let now = self.host.frame_now_ms();
        if let Some(f) = &self.scene.marks_flash
            && f.until <= now
        {
            self.scene.marks_flash = None;
        }
        self.invalidate(InvalidateLevel::Chrome);
        let pulsing = self.scene.marks_hover_key.is_some()
            && now - self.scene.marks_hover_since < MARK_PULSE_MS;
        if pulsing || self.scene.marks_flash.is_some() {
            self.sync_mark_pulse();
        }
    }

    // ── keyboard ──

    /// Pan the view by a whole number of bars (keyboard): `+bars` toward the latest.
    pub(crate) fn pan_by_bars(&mut self, bars: f64) {
        let vp = self.viewport();
        self.apply_viewport(ViewportState {
            bar_spacing: vp.bar_spacing,
            right_offset: vp.right_offset + bars,
        });
    }

    /// Zoom one keyboard notch, right-edge anchored (+1 in, -1 out).
    pub(crate) fn zoom_by_step(&mut self, direction: i32) {
        let vp = self.viewport();
        let target =
            clamp_bar_spacing(vp.bar_spacing * (f64::from(direction) * KEY_ZOOM_STEP).exp());
        let anchor = self.coords.right_edge_logical();
        let width = self.coords.width();
        self.zoom_to(target, anchor, width);
    }

    /// Move the focused bar by `delta` from the current crosshair bar (or the last bar).
    pub(crate) fn step_crosshair(&mut self, delta: i64) {
        let n = self.coords.bar_count() as i64;
        if n == 0 {
            return;
        }
        let cur = self.hover_logical.unwrap_or_else(|| {
            let last = js_round(self.coords.x_to_logical(self.coords.width()));
            js_min((n - 1) as f64, last) as i64
        });
        self.focus_bar(cur + delta);
    }

    /// Center-ish the given bar in view (scrolling minimally), draw the crosshair on
    /// it, and announce its OHLC and indicator values.
    pub(crate) fn focus_bar(&mut self, target: i64) {
        let n = self.coords.bar_count() as i64;
        if n == 0 {
            return;
        }
        let next = target.clamp(0, n - 1);
        // Scroll just enough to keep the focused bar a couple of bars inside the plot.
        let vp = self.viewport();
        let margin = 2.0;
        let left_l = self.coords.x_to_logical(0.0);
        let right_l = self.coords.x_to_logical(self.coords.width());
        let nf = next as f64;
        let mut right_offset = vp.right_offset;
        if nf < left_l + margin {
            right_offset -= left_l + margin - nf;
        } else if nf > right_l - margin {
            right_offset += nf - (right_l - margin);
        }
        if right_offset != vp.right_offset {
            self.apply_viewport(ViewportState {
                bar_spacing: vp.bar_spacing,
                right_offset,
            });
        }
        let price_pane = self
            .scene
            .ordered_panes()
            .iter()
            .find(|p| p.kind == PaneKind::Price)
            .copied()
            .or_else(|| self.scene.ordered_panes().first().copied());
        let y = self
            .scene
            .crosshair
            .map(|c| c.y)
            .unwrap_or_else(|| match price_pane {
                Some(p) => p.bounds.top + p.bounds.height / 2.0,
                None => self.coords.height() / 2.0,
            });
        let x = self.coords.logical_to_x(nf);
        self.handle_pointer_move(Some(x), Some(y));
        self.announce_bar(next);
    }

    pub(crate) fn handle_key_action(&mut self, action: KeyAction) {
        match action {
            KeyAction::Pan { bars } => self.pan_by_bars(bars),
            KeyAction::Zoom { direction } => self.zoom_by_step(direction),
            KeyAction::Step { delta } => self.step_crosshair(i64::from(delta)),
            KeyAction::Edge(edge) => {
                let n = self.coords.bar_count() as i64;
                self.focus_bar(match edge {
                    Edge::First => 0,
                    Edge::Last => n - 1,
                });
            }
            KeyAction::Reset => self.reset_view(),
            KeyAction::Realtime => self.scroll_to_realtime(),
            KeyAction::Clear => self.handle_pointer_move(None, None),
        }
    }

    // ── drawing magnet ──

    /// Snap a data point to the nearest candle for the drawing magnet: time to the
    /// nearest bar, price to that bar's closest OHLC value. Off the price pane
    /// (sub-panes have no OHLC) only the time snaps. `Weak` only snaps when the candle
    /// point is within [`WEAK_SNAP_PX`] of the cursor pixel; `Strong` always snaps;
    /// `Off` is a no-op.
    pub(crate) fn snap_to_candle(
        &self,
        point: (f64, f64),
        pane_id: &str,
        mode: SnapMode,
        cursor_px: Option<(f64, f64)>,
    ) -> (f64, f64) {
        if mode == SnapMode::Off {
            return point;
        }
        let n = self.bars.len();
        if n == 0 {
            return point;
        }
        let i = js_max(
            0.0,
            js_min(
                (n - 1) as f64,
                js_round(self.coords.time_to_logical(point.0)),
            ),
        );
        let Some(bar) = self.bars.get(i as usize) else {
            return point;
        };
        let snapped = if pane_id != PRICE_PANE_ID {
            (bar.time as f64, point.1)
        } else {
            let mut price = bar.open;
            for v in [bar.high, bar.low, bar.close] {
                if (v - point.1).abs() < (price - point.1).abs() {
                    price = v;
                }
            }
            (bar.time as f64, price)
        };
        if mode == SnapMode::Weak
            && let Some(c) = cursor_px
        {
            let gateway = self.user_drawings.as_ref().and_then(|h| h.series_gateway());
            let dist = super::user_drawings::with_projector(
                &self.scene,
                &self.coords,
                gateway.as_deref(),
                |proj| {
                    let sy = proj.y_of(snapped.1, pane_id);
                    (proj.x_of(snapped.0) - c.0).hypot(sy.unwrap_or(c.1) - c.1)
                },
            );
            if dist > WEAK_SNAP_PX {
                return point; // candle is too far: place freely
            }
        }
        snapped
    }

    /// Show/clear the external ghost crosshair. It only repaints the cursor overlay
    /// (cursor tier) and NEVER re-emits a crosshair move.
    pub(crate) fn set_external_crosshair(&mut self, time: Option<Millis>, price: Option<f64>) {
        let next = time.map(|t| (t, price));
        let prev = self.external_cross;
        if next.is_none() && prev.is_none() {
            return;
        }
        if next == prev {
            return; // idempotent: 60Hz streams stay cheap
        }
        self.external_cross = next;
        self.invalidate(InvalidateLevel::Cursor);
    }
}
