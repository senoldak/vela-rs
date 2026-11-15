//! Panes: layout, collapse, maximize, order, weights, separator resize and the
//! published layout facts (`layoutPanes` and its neighbours in the original).

use crate::indicators::classics::math::js_max;
use crate::ports::chart_renderer::{PaneAction, PaneMoveDir};
use crate::render::chrome::axis_layout::{AXIS_MASTER_W, AXIS_MERGED_W};
use crate::render::core::pane_resize::{MIN_PANE_PX, PaneSplit, resize_split};
use crate::render::core::scheduler::InvalidateLevel;

use super::host::AttributionState;
use super::state::{
    COLLAPSED_PANE_H, Event, PRICE_PANE_ID, SCROLL_BTN_BOTTOM, SCROLL_BTN_RIGHT_INSET,
    SEPARATOR_HIT_PX, State, TIME_AXIS_H,
};
use super::util::is_dark_color;

/// `weight || 1`: a zero or NaN weight reads as 1.
fn weight_or_one(w: f64) -> f64 {
    if w == 0.0 || w.is_nan() { 1.0 } else { w }
}

impl State {
    pub(crate) fn layout_panes(&mut self) {
        let ids = self.scene.ordered_pane_ids();
        let data_height = self.coords.height();
        // Tell the legend which panes are collapsed (so it shows only the master row).
        let masters = self.collapsed_master_map();
        self.host.legend_collapsed_panes(&masters);
        // Maximized: the chosen pane fills the plot; the rest collapse to zero height.
        let max_id = self
            .maximized_pane_id
            .clone()
            .filter(|id| self.scene.panes.contains_key(id));
        if let Some(max_id) = max_id {
            for id in &ids {
                if let Some(pane) = self.scene.panes.get_mut(id) {
                    pane.bounds = if *id == max_id {
                        crate::render::core::PaneBounds {
                            top: 0.0,
                            height: data_height,
                        }
                    } else {
                        crate::render::core::PaneBounds {
                            top: data_height,
                            height: 0.0,
                        }
                    };
                }
            }
            self.after_layout();
            return;
        }
        // Collapsed panes take a fixed strip; the rest share the remaining height by weight.
        let collapsed_n = ids
            .iter()
            .filter(|id| {
                self.scene
                    .panes
                    .get(id.as_str())
                    .is_some_and(|p| p.collapsed)
            })
            .count();
        let strip_total = collapsed_n as f64 * COLLAPSED_PANE_H;
        let flex_height = js_max(0.0, data_height - strip_total);
        let mut total_weight = 0.0;
        for id in &ids {
            if let Some(p) = self.scene.panes.get(id.as_str())
                && !p.collapsed
            {
                total_weight += weight_or_one(p.height_weight);
            }
        }
        if total_weight == 0.0 || total_weight.is_nan() {
            total_weight = 1.0;
        }
        let mut top = 0.0;
        for id in &ids {
            if let Some(pane) = self.scene.panes.get_mut(id) {
                let height = if pane.collapsed {
                    COLLAPSED_PANE_H
                } else {
                    (flex_height * weight_or_one(pane.height_weight)) / total_weight
                };
                pane.bounds = crate::render::core::PaneBounds { top, height };
                top += height;
            }
        }
        self.after_layout();
    }

    /// What follows every layout: the host repositions its pane-pinned DOM, and the
    /// scroll button, attribution mark and published bounds follow the new panes.
    fn after_layout(&mut self) {
        self.host.layout_changed();
        self.reposition_scroll_button();
        self.position_attribution();
        self.publish_price_pane_bounds();
    }

    /// For each collapsed pane, the master (master-scale) indicator id whose legend
    /// row stays in the strip: merged own-scale indicators are hidden while collapsed.
    pub(crate) fn collapsed_master_map(&self) -> Vec<(String, Option<String>)> {
        let mut out = Vec::new();
        for pane in self.scene.panes.values() {
            if !pane.collapsed {
                continue;
            }
            // Only models with a legend row can front a collapsed strip.
            let models: Vec<&crate::model::IndicatorModel> = self
                .scene
                .ordered_indicators_for_pane(&pane.id)
                .into_iter()
                .filter(|m| m.legend != Some(false))
                .collect();
            let merged: std::collections::HashSet<&str> = self
                .scene
                .own_scale_indicators_for_pane(&pane.id)
                .iter()
                .map(|m| m.id.as_str())
                .collect();
            let master = models
                .iter()
                .find(|m| !merged.contains(m.id.as_str()))
                .or_else(|| models.first());
            out.push((pane.id.clone(), master.map(|m| m.id.clone())));
        }
        out
    }

    /// The lowest open pane's bottom edge (data-area coords): the anchor of the
    /// scroll button and the attribution mark.
    fn lowest_open_pane_bottom(&self, data_height: f64) -> f64 {
        let max_pane = self
            .maximized_pane_id
            .as_ref()
            .and_then(|id| self.scene.panes.get(id));
        let bottoms: Vec<f64> = match max_pane {
            Some(p) => vec![p.bounds.top + p.bounds.height],
            None => self
                .scene
                .ordered_panes()
                .iter()
                .filter(|p| !p.collapsed)
                .map(|p| p.bounds.top + p.bounds.height)
                .collect(),
        };
        if data_height > 0.0 && !bottoms.is_empty() {
            bottoms.iter().copied().fold(f64::NEG_INFINITY, js_max)
        } else {
            data_height
        }
    }

    /// Pin the scroll-to-realtime button above the bottom-most EXPANDED pane's data
    /// area: with all lower sub-panes collapsed it settles into the lowest open pane.
    pub(crate) fn reposition_scroll_button(&mut self) {
        let data_height = self.coords.height();
        if data_height <= 0.0 {
            return;
        }
        let pane_bottom = self.lowest_open_pane_bottom(data_height);
        // Extra collapsed strips below that pane push the button up by exactly their height.
        self.scroll_btn_bottom_px = SCROLL_BTN_BOTTOM + js_max(0.0, data_height - pane_bottom);
        // The same inset is the `--vela-bottom-gutter` (time axis + collapsed strips).
        self.layout.bottom_gutter = Some(TIME_AXIS_H + js_max(0.0, data_height - pane_bottom));
        self.push_layout();
        // Clear the whole scale gutter (wider with merged own-scale columns).
        self.scroll_btn_right_px = self.right_axis_w + SCROLL_BTN_RIGHT_INSET;
        // Re-announce the button's geometry.
        self.scroll_btn_sent = None;
        self.update_scroll_to_realtime_button();
    }

    pub(crate) fn push_layout(&mut self) {
        if !self.mounted {
            return;
        }
        if self.layout_sent != Some(self.layout) {
            self.layout_sent = Some(self.layout);
            self.host.publish_layout(&self.layout);
        }
    }

    /// Publish the gutters (`--vela-toolbar-gutter`, `--vela-scale-gutter`).
    pub(crate) fn publish_gutters(&mut self) {
        self.layout.toolbar_gutter = self.toolbar_gutter;
        self.layout.scale_gutter = self.right_axis_w;
        self.push_layout();
    }

    /// Publish the price pane's vertical insets so a symbol watermark (or any host
    /// overlay) can clip to the price pane. A maximized study pane collapses the box
    /// to zero height.
    pub(crate) fn publish_price_pane_bounds(&mut self) {
        if !self.mounted {
            return;
        }
        let plot_h = self.coords.height() + TIME_AXIS_H;
        let (top, height) = match self.scene.panes.get(PRICE_PANE_ID) {
            Some(p) => (p.bounds.top, p.bounds.height),
            None => (0.0, self.coords.height()),
        };
        let bottom = js_max(0.0, plot_h - (top + height));
        self.layout.price_pane_top = Some(top);
        self.layout.price_pane_bottom = Some(bottom);
        self.push_layout();
    }

    /// Bottom-left of the LOWEST visible, non-collapsed pane, above the time axis,
    /// clear of the drawings toolbar.
    pub(crate) fn position_attribution(&mut self) {
        if !self.mounted {
            return;
        }
        let data_height = self.coords.height();
        let pane_bottom = self.lowest_open_pane_bottom(data_height);
        let state = AttributionState {
            enabled: self.attribution_enabled,
            html: self.attribution_html.clone(),
            left_px: self.toolbar_gutter + 12.0,
            bottom_px: TIME_AXIS_H + 10.0 + js_max(0.0, data_height - pane_bottom),
            ink: self.attribution_ink(),
        };
        if self.attribution_sent.as_ref() != Some(&state) {
            self.attribution_sent = Some(state.clone());
            self.host.attribution(&state);
        }
    }

    /// The ink of the attribution mark: a hard two-way switch on the plot background.
    pub(crate) fn attribution_ink(&self) -> String {
        let bg = super::util::theme_string(&self.theme, "background");
        if is_dark_color(&bg) {
            "#ffffff"
        } else {
            "#000000"
        }
        .to_string()
    }

    // ── own-scale columns (the right gutter) ──

    /// The number of merged (own-scale) scale columns needed: the max across panes.
    fn max_own_scale_columns(&self) -> usize {
        self.scene
            .panes
            .values()
            .map(|p| self.scene.own_scale_indicators_for_pane(&p.id).len())
            .max()
            .unwrap_or(0)
    }

    /// Recompute the right-gutter width (master column + one per merged scale).
    /// Re-sizes on change.
    pub(crate) fn refresh_axis_width(&mut self) -> bool {
        let w = AXIS_MASTER_W + AXIS_MERGED_W * self.max_own_scale_columns() as f64;
        if w == self.right_axis_w {
            return false;
        }
        self.right_axis_w = w;
        self.publish_gutters();
        if self.coords.width() > 0.0 {
            self.sync_size();
        }
        true
    }

    // ── pane management ──

    pub(crate) fn ensure_pane(&mut self, pane: &crate::model::Pane) {
        self.scene
            .ensure_pane(&pane.id, pane.kind, pane.order, pane.height_weight);
        self.layout_panes();
        self.invalidate(InvalidateLevel::Full);
    }

    pub(crate) fn remove_pane(&mut self, id: &str) {
        if id == PRICE_PANE_ID {
            return;
        }
        self.scene.remove_pane(id);
        if self.maximized_pane_id.as_deref() == Some(id) {
            self.maximized_pane_id = None;
        }
        self.refresh_axis_width();
        self.layout_panes();
        self.invalidate(InvalidateLevel::Full);
    }

    pub(crate) fn order_panes(&mut self, ordered_ids: &[String]) {
        let ids: Vec<&str> = ordered_ids.iter().map(String::as_str).collect();
        self.scene.order_panes(&ids);
        self.layout_panes();
        self.invalidate(InvalidateLevel::Full);
    }

    pub(crate) fn set_pane_collapsed(&mut self, pane_id: &str, collapsed: bool) {
        let Some(pane) = self.scene.panes.get_mut(pane_id) else {
            return;
        };
        if pane.collapsed == collapsed {
            return;
        }
        pane.collapsed = collapsed;
        self.layout_panes();
        self.invalidate(InvalidateLevel::Full);
    }

    pub(crate) fn set_pane_maximized(&mut self, pane_id: Option<&str>) {
        let pane_id = pane_id.filter(|id| self.scene.panes.contains_key(id));
        self.maximized_pane_id = pane_id.map(str::to_string);
        self.layout_panes();
        self.invalidate(InvalidateLevel::Full);
    }

    pub(crate) fn set_pane_height_weight(&mut self, pane_id: &str, weight: f64) {
        let Some(pane) = self.scene.panes.get_mut(pane_id) else {
            return;
        };
        #[allow(clippy::neg_cmp_op_on_partial_ord)] // NaN and non-positive weights are rejected
        if !(weight > 0.0) || pane.height_weight == weight {
            return;
        }
        pane.height_weight = weight;
        self.after_pane_layout_change();
    }

    /// Relayout and repaint after a collapse, maximize or order change.
    pub(crate) fn after_pane_layout_change(&mut self) {
        self.layout_panes();
        self.invalidate(InvalidateLevel::Full);
    }

    /// The collapse toggle of the pane controls (a user action, reported to core).
    pub(crate) fn toggle_collapse(&mut self, pane_id: &str) {
        let Some(pane) = self.scene.panes.get_mut(pane_id) else {
            return;
        };
        let collapsed = !pane.collapsed;
        pane.collapsed = collapsed;
        self.after_pane_layout_change();
        self.emit(Event::PaneAction(PaneAction::Collapse {
            pane_id: pane_id.to_string(),
            collapsed,
        }));
    }

    /// The maximize toggle of the pane controls (a user action, reported to core).
    pub(crate) fn toggle_maximize(&mut self, pane_id: &str) {
        let maximized = self.maximized_pane_id.as_deref() != Some(pane_id);
        self.maximized_pane_id = maximized.then(|| pane_id.to_string());
        self.after_pane_layout_change();
        self.emit(Event::PaneAction(PaneAction::Maximize {
            pane_id: pane_id.to_string(),
            maximized,
        }));
    }

    /// Reorder a study pane one slot up/down (renderer-applied, mirrored to core
    /// through the pane action).
    pub(crate) fn move_pane_local(&mut self, pane_id: &str, dir: PaneMoveDir) {
        let mut ids = self.scene.ordered_pane_ids();
        let Some(i) = ids.iter().position(|p| p == pane_id) else {
            return;
        };
        let j = match dir {
            PaneMoveDir::Up => i.checked_sub(1),
            PaneMoveDir::Down => Some(i + 1),
        };
        // The price pane stays pinned at index 0.
        let Some(j) = j.filter(|j| *j >= 1 && *j < ids.len()) else {
            return;
        };
        ids.swap(i, j);
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        self.scene.order_panes(&refs);
        self.after_pane_layout_change();
        self.emit(Event::PaneAction(PaneAction::Move {
            pane_id: pane_id.to_string(),
            dir,
        }));
    }

    /// Double-click inside the data area maximizes the clicked pane so it fills the
    /// plot and every other pane is fully hidden; a second double-click restores.
    pub(crate) fn data_dbl_click(&mut self, y: f64) {
        // Snap to the nearest pane when the point falls between bounds (separator gap,
        // the sliver under the last pane): a double-tap must never land in a dead zone.
        let id = self
            .pane_node_at_y(y)
            .or_else(|| self.nearest_pane_to_y(y))
            .map(|p| p.id.clone());
        let Some(id) = id else { return };
        self.toggle_maximize(&id);
    }

    // ── pane resize (drag the separator between stacked panes) ──

    /// Index (in the ordered panes) of the LOWER pane whose top separator is within
    /// the hit zone of `y`.
    fn separator_pane_index_at(&self, y: f64) -> Option<usize> {
        let panes = self.scene.ordered_panes();
        (1..panes.len()).find(|&i| (y - panes[i].bounds.top).abs() <= SEPARATOR_HIT_PX)
    }

    pub(crate) fn pane_separator_at(&self, y: f64) -> bool {
        self.separator_pane_index_at(y).is_some()
    }

    /// Pixel y of the draggable separator under the cursor (hover highlight).
    pub(crate) fn separator_hover_y(&self, y: f64) -> Option<f64> {
        if !self.pane_resize_enabled {
            return None;
        }
        let i = self.separator_pane_index_at(y)?;
        Some(self.scene.ordered_panes()[i].bounds.top)
    }

    /// Grab the separator at `y`: snapshot the adjacent panes and their shared span.
    pub(crate) fn begin_pane_resize(&mut self, y: f64) {
        let Some(i) = self.separator_pane_index_at(y) else {
            return;
        };
        let panes = self.scene.ordered_panes();
        let (above, below) = (panes[i - 1], panes[i]);
        let split = PaneSplit {
            combined_top: above.bounds.top,
            combined_height: above.bounds.height + below.bounds.height,
            combined_weight: above.height_weight + below.height_weight,
            start_boundary_y: below.bounds.top,
        };
        let (above_id, below_id, above_weight) =
            (above.id.clone(), below.id.clone(), above.height_weight);
        self.resize_above = Some(above_id);
        self.resize_below = Some(below_id);
        self.resize_start_above_weight = above_weight;
        self.resize_split_start = Some(split);
    }

    /// Resize the grabbed panes by the total drag (down: grow the upper pane), keeping
    /// the combined weight fixed so every other pane keeps its height.
    pub(crate) fn pane_resize_by(&mut self, dy_total: f64) {
        let (Some(above), Some(below), Some(split)) = (
            self.resize_above.clone(),
            self.resize_below.clone(),
            self.resize_split_start,
        ) else {
            return;
        };
        let next = resize_split(&split, dy_total, MIN_PANE_PX);
        if let Some(p) = self.scene.panes.get_mut(&above) {
            p.height_weight = next.above;
        }
        if let Some(p) = self.scene.panes.get_mut(&below) {
            p.height_weight = next.below;
        }
        self.layout_panes();
        self.invalidate(InvalidateLevel::Full);
    }

    /// Release the separator: report the resize once, at the end of the drag, if it
    /// changed anything.
    pub(crate) fn end_pane_resize(&mut self) {
        let above = self.resize_above.take();
        let below = self.resize_below.take();
        self.resize_split_start = None;
        let start = self.resize_start_above_weight;
        if let (Some(above), Some(below)) = (above, below) {
            let changed = self
                .scene
                .panes
                .get(&above)
                .is_some_and(|p| p.height_weight != start);
            if changed {
                self.emit(Event::PaneAction(PaneAction::Resize {
                    pane_ids: vec![above, below],
                }));
            }
        }
    }

    /// Double-click a separator: split the two adjacent panes evenly (each gets half
    /// of their combined weight).
    pub(crate) fn reset_pane_size(&mut self, y: f64) {
        let Some(i) = self.separator_pane_index_at(y) else {
            return;
        };
        let ids = self.scene.ordered_pane_ids();
        let (above, below) = (ids[i - 1].clone(), ids[i].clone());
        let sum = self
            .scene
            .panes
            .get(&above)
            .map_or(0.0, |p| p.height_weight)
            + self
                .scene
                .panes
                .get(&below)
                .map_or(0.0, |p| p.height_weight);
        let half = sum / 2.0;
        if let Some(p) = self.scene.panes.get_mut(&above) {
            p.height_weight = half;
        }
        if let Some(p) = self.scene.panes.get_mut(&below) {
            p.height_weight = half;
        }
        self.layout_panes();
        self.invalidate(InvalidateLevel::Full);
        self.emit(Event::PaneAction(PaneAction::Resize {
            pane_ids: vec![above, below],
        }));
    }
}
