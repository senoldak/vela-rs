//! The panes section of the orchestrator: pane order and collapse/maximize
//! state, `list_panes`, `move_pane`, `remove_pane_and_indicators`,
//! `set_pane_height_weight`, `move_indicator` and its helpers,
//! `handle_pane_action`, `ensure_pane_for`, `pane_still_used`, `forget_pane`.

use super::*;
use crate::model::{IndicatorModel, Pane, PaneKind};
use crate::options::PaneIndicatorInfo;
use crate::ports::chart_renderer::{NewPanePlacement, PaneAction};

/// A move target resolved to a concrete pane id, the own-scale flag, and an
/// optional insertion point (the original's `{ paneId, ownScale, insert? }`).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ResolvedMove {
    pub pane_id: String,
    pub own_scale: bool,
    pub insert: Option<NewPanePlacement>,
}

/// JavaScript's `String(number)` for the values the warnings print.
fn js_number(n: f64) -> String {
    if n.is_nan() {
        "NaN".to_string()
    } else if n.is_infinite() {
        if n > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
    } else {
        format!("{n}")
    }
}

impl EngineOrchestrator {
    pub fn pane_management_supported(&self) -> bool {
        self.inner.renderer.capabilities().pane_management
            && self.inner.renderer.supports_set_indicator_pane()
    }

    pub fn list_panes(&self) -> Vec<PaneInfo> {
        let mut by_pane: HashMap<String, Vec<PaneIndicatorInfo>> = HashMap::new();
        for record in self.inner.registry.all() {
            let r = record.borrow();
            let Some(model) = r.model.as_ref() else {
                continue;
            };
            // Host-owned chrome: no row anywhere, the handle is the control.
            if model.legend == Some(false) {
                continue;
            }
            let pane_id = model.pane_id.clone().unwrap_or_else(|| "price".to_string());
            by_pane.entry(pane_id).or_default().push(PaneIndicatorInfo {
                id: r.id.clone(),
                title: if model.title.is_empty() {
                    r.title.clone()
                } else {
                    model.title.clone()
                },
                shorttitle: model.shorttitle.clone().filter(|s| !s.is_empty()),
                own_scale: model.own_scale,
            });
        }
        let order = self.inner.state.borrow().pane_order.clone();
        let (collapsed, maximized) = {
            let st = self.inner.state.borrow();
            (st.collapsed_panes.clone(), st.maximized_pane_id.clone())
        };
        let mut panes: Vec<PaneInfo> = Vec::new();
        for pane_id in order {
            // The price pane always shows; a study pane only while it holds indicators.
            if pane_id != "price" && !by_pane.contains_key(&pane_id) {
                continue;
            }
            let height_weight = self.inner.renderer.pane_height_weight(&pane_id);
            panes.push(PaneInfo {
                kind: if pane_id == "price" {
                    PaneKind::Price
                } else {
                    PaneKind::Study
                },
                order: panes.len(),
                collapsed: collapsed.contains(&pane_id),
                maximized: maximized.as_deref() == Some(pane_id.as_str()),
                height_weight,
                indicators: by_pane.remove(&pane_id).unwrap_or_default(),
                id: pane_id,
            });
        }
        panes
    }

    pub fn move_pane(&self, pane_id: &str, dir: PaneMoveDir) {
        if pane_id == "price" {
            return; // price stays pinned on top
        }
        let mut existing = self.existing_pane_ids();
        let Some(i) = existing.iter().position(|p| p == pane_id) else {
            return;
        };
        let j = match dir {
            PaneMoveDir::Up => i.checked_sub(1),
            PaneMoveDir::Down => Some(i + 1),
        };
        // Never move above the price pane (index 0) or past the last pane.
        let Some(j) = j.filter(|j| *j > 0 && *j < existing.len()) else {
            return;
        };
        existing.swap(i, j);
        self.reorder_panes(existing);
        self.emit(ChartEvent::PaneMoved {
            pane_id: pane_id.to_string(),
            dir,
        });
    }

    pub fn remove_pane_and_indicators(&self, pane_id: &str) {
        if pane_id == "price" {
            return;
        }
        for record in self.inner.registry.all() {
            let (id, on_pane) = {
                let r = record.borrow();
                (
                    r.id.clone(),
                    r.model.as_ref().and_then(|m| m.pane_id.as_deref()) == Some(pane_id),
                )
            };
            if on_pane {
                self.remove_indicator(&id);
            }
        }
    }

    pub fn collapse_pane(&self, pane_id: &str, collapsed: bool) {
        {
            let mut st = self.inner.state.borrow_mut();
            if collapsed {
                st.collapsed_panes.insert(pane_id.to_string());
            } else {
                st.collapsed_panes.remove(pane_id);
            }
        }
        self.inner.renderer.set_pane_collapsed(pane_id, collapsed);
        self.emit(ChartEvent::PaneChanged);
    }

    pub fn maximize_pane(&self, pane_id: Option<&str>) {
        self.inner.state.borrow_mut().maximized_pane_id = pane_id.map(str::to_string);
        self.inner.renderer.set_pane_maximized(pane_id);
        self.emit(ChartEvent::PaneChanged);
    }

    pub fn set_pane_height_weight(&self, pane_id: &str, weight: f64) {
        if !self.inner.renderer.supports_set_pane_height_weight() {
            self.warn(
                "[vela] the active renderer does not support pane sizing \u{2014} chart.panes.setHeightWeight ignored.",
            );
            return;
        }
        if !weight.is_finite() || weight <= 0.0 {
            self.warn(&format!(
                "[vela] chart.panes.setHeightWeight: weight must be a positive number (got {}).",
                js_number(weight)
            ));
            return;
        }
        if !self.existing_pane_ids().iter().any(|p| p == pane_id) {
            self.warn(&format!(
                "[vela] chart.panes.setHeightWeight: no pane \"{pane_id}\"."
            ));
            return;
        }
        self.inner.renderer.set_pane_height_weight(pane_id, weight);
        self.emit(ChartEvent::PaneChanged);
    }

    /// IndicatorController/PaneController: move (or merge) an indicator to
    /// another pane.
    pub fn move_indicator(&self, id: &str, target: MoveTarget) {
        if !self.pane_management_supported() {
            self.warn(
                "[vela] the active renderer does not support moving indicators between panes.",
            );
            return;
        }
        let Some(record) = self.inner.registry.get(id) else {
            return;
        };
        let (render_handle, model) = {
            let r = record.borrow();
            match (r.render_handle.clone(), r.model.clone()) {
                (Some(handle), Some(model)) => (handle, model),
                _ => return,
            }
        };
        let old_pane_id = model.pane_id.clone().unwrap_or_else(|| "price".to_string());
        let Some(resolved) = self.resolve_move_target(id, &model, &target) else {
            return;
        };
        let ResolvedMove {
            pane_id,
            own_scale,
            insert,
        } = resolved;
        if pane_id == old_pane_id && model.own_scale == own_scale {
            return; // no-op
        }

        if let Some(insert) = &insert {
            self.insert_pane_order(&pane_id, insert);
        } else {
            let mut st = self.inner.state.borrow_mut();
            if !st.pane_order.contains(&pane_id) {
                st.pane_order.push(pane_id.clone());
            }
        }

        // The model is stamped in place: recomputes must honor this placement,
        // not re-route by default.
        let placed = {
            let mut r = record.borrow_mut();
            r.pane_locked = true;
            r.model.take()
        };
        if let Some(mut placed) = placed {
            placed.own_scale = own_scale;
            self.place_model(&mut placed, id, &pane_id);
            record.borrow_mut().model = Some(placed);
        }
        self.ensure_pane_for(&pane_id);
        self.inner
            .renderer
            .set_indicator_pane(&render_handle, &pane_id, own_scale);

        if old_pane_id != "price" && old_pane_id != pane_id && !self.pane_still_used(&old_pane_id) {
            self.inner.renderer.remove_pane(&old_pane_id);
            self.forget_pane(&old_pane_id);
        }
        self.reorder_panes(self.existing_pane_ids());
        self.emit(ChartEvent::IndicatorMoved {
            id: id.to_string(),
            pane_id,
        });
        self.emit(ChartEvent::PaneChanged);
    }

    /// Resolve a move target to a concrete pane id + own-scale flag + optional
    /// insertion.
    pub(super) fn resolve_move_target(
        &self,
        id: &str,
        model: &IndicatorModel,
        target: &MoveTarget,
    ) -> Option<ResolvedMove> {
        match target {
            // Merge onto price: a price-unit overlay (declared overlay=true) keeps
            // sharing the price scale; anything else gets its own scale column.
            MoveTarget::Price => Some(ResolvedMove {
                pane_id: "price".to_string(),
                own_scale: !model.overlay,
                insert: None,
            }),
            MoveTarget::Pane(pane) if pane == "price" => Some(ResolvedMove {
                pane_id: "price".to_string(),
                own_scale: !model.overlay,
                insert: None,
            }),
            MoveTarget::Pane(pane) => {
                if !self.existing_pane_ids().contains(pane) {
                    return None;
                }
                Some(ResolvedMove {
                    pane_id: pane.clone(),
                    own_scale: true,
                    insert: None,
                })
            }
            // A brand-new pane: the indicator owns it (shares its scale with itself
            // -> no own-scale column). The natural name `pane-${id}` collides with
            // this indicator's OWN pane when it is the master (that pane was seeded
            // from its id), which the move's no-op guard would then reject: so pick
            // an id distinct from both the current pane and every existing pane.
            MoveTarget::NewPane(spec) => Some(ResolvedMove {
                pane_id: self.fresh_pane_id(id, model.pane_id.as_deref().unwrap_or("price")),
                own_scale: false,
                insert: Some(spec.clone()),
            }),
        }
    }

    /// A study-pane id unique among existing panes and distinct from `avoid`
    /// (the indicator's current pane).
    pub(super) fn fresh_pane_id(&self, id: &str, avoid: &str) -> String {
        let taken: std::collections::HashSet<String> =
            self.existing_pane_ids().into_iter().collect();
        let mut pane_id = format!("pane-{id}");
        let mut n = 2;
        while pane_id == avoid || taken.contains(&pane_id) {
            pane_id = format!("pane-{id}-{n}");
            n += 1;
        }
        pane_id
    }

    /// Splice a pane id into the order relative to a neighbor (default: bottom).
    pub(super) fn insert_pane_order(&self, pane_id: &str, at: &NewPanePlacement) {
        let mut st = self.inner.state.borrow_mut();
        let order = &mut st.pane_order;
        if let Some(i) = order.iter().position(|p| p == pane_id)
            && i > 0
        {
            order.remove(i);
        }
        let mut idx = order.len();
        let before = at
            .before
            .as_deref()
            .filter(|b| !b.is_empty())
            .and_then(|b| order.iter().position(|p| p == b));
        let after = at
            .after
            .as_deref()
            .filter(|a| !a.is_empty())
            .and_then(|a| order.iter().position(|p| p == a));
        if let Some(i) = before {
            idx = i;
        } else if let Some(i) = after {
            idx = i + 1;
        }
        // Never above the price pane.
        let idx = idx.max(1).min(order.len());
        order.insert(idx, pane_id.to_string());
    }

    /// Pane ids that currently exist (price + study panes holding at least one
    /// indicator), in order.
    pub(super) fn existing_pane_ids(&self) -> Vec<String> {
        let order = self.inner.state.borrow().pane_order.clone();
        order
            .into_iter()
            .filter(|p| p == "price" || self.pane_still_used(p))
            .collect()
    }

    /// Apply a new pane order to the mirror + renderer.
    pub(super) fn reorder_panes(&self, order: Vec<String>) {
        self.inner.state.borrow_mut().pane_order = order.clone();
        self.inner.renderer.order_panes(&order);
        self.emit(ChartEvent::PaneChanged);
    }

    /// React to a renderer-initiated pane action (hover buttons / double-clicks).
    pub(super) fn handle_pane_action(&self, action: &PaneAction) {
        match action {
            PaneAction::Remove { pane_id } => self.remove_pane_and_indicators(pane_id),
            PaneAction::Move { pane_id, dir } => {
                // The renderer already reordered: mirror it (swap with the neighbor).
                let mut existing = self.existing_pane_ids();
                let Some(i) = existing.iter().position(|p| p == pane_id) else {
                    return;
                };
                let j = match dir {
                    PaneMoveDir::Up => i.checked_sub(1),
                    PaneMoveDir::Down => Some(i + 1),
                };
                if let Some(j) = j
                    && i > 0
                    && j > 0
                    && j < existing.len()
                {
                    existing.swap(i, j);
                    self.inner.state.borrow_mut().pane_order = existing;
                    self.emit(ChartEvent::PaneChanged);
                    self.emit(ChartEvent::PaneMoved {
                        pane_id: pane_id.clone(),
                        dir: *dir,
                    });
                }
            }
            PaneAction::Collapse { pane_id, collapsed } => {
                {
                    let mut st = self.inner.state.borrow_mut();
                    if *collapsed {
                        st.collapsed_panes.insert(pane_id.clone());
                    } else {
                        st.collapsed_panes.remove(pane_id);
                    }
                }
                self.emit(ChartEvent::PaneChanged);
            }
            PaneAction::Maximize { pane_id, maximized } => {
                self.inner.state.borrow_mut().maximized_pane_id =
                    maximized.then(|| pane_id.clone());
                self.emit(ChartEvent::PaneChanged);
            }
            PaneAction::Resize { .. } => {
                // The renderer holds the weights; `list()` reads them back.
                self.emit(ChartEvent::PaneChanged);
            }
        }
    }

    /// Make sure the renderer has the pane; true when it is new to the layout
    /// (the caller emits `pane:changed` once the indicator's model sits on it, so
    /// `list()` shows it).
    pub(super) fn ensure_pane_for(&self, pane_id: &str) -> bool {
        let (created, order) = {
            let mut st = self.inner.state.borrow_mut();
            let created = !st.pane_order.iter().any(|p| p == pane_id);
            if created {
                st.pane_order.push(pane_id.to_string());
            }
            let order = st.pane_order.iter().position(|p| p == pane_id).unwrap_or(0);
            (created, order)
        };
        // No height_weight: the renderer keeps the weight an existing pane already has.
        self.inner.renderer.ensure_pane(&Pane {
            id: pane_id.to_string(),
            kind: if pane_id == "price" {
                PaneKind::Price
            } else {
                PaneKind::Study
            },
            order: order as i32,
            height_weight: None,
            title: None,
        });
        created
    }

    /// True when any (still-registered) indicator sits on the given pane.
    pub(super) fn pane_still_used(&self, pane_id: &str) -> bool {
        self.inner
            .registry
            .all()
            .iter()
            .any(|r| r.borrow().model.as_ref().and_then(|m| m.pane_id.as_deref()) == Some(pane_id))
    }

    /// Drop a now-empty pane from the order + collapse/maximize mirror.
    pub(super) fn forget_pane(&self, pane_id: &str) {
        let mut st = self.inner.state.borrow_mut();
        if let Some(i) = st.pane_order.iter().position(|p| p == pane_id)
            && i > 0
        {
            st.pane_order.remove(i); // never remove 'price' (index 0)
        }
        st.collapsed_panes.remove(pane_id);
        if st.maximized_pane_id.as_deref() == Some(pane_id) {
            st.maximized_pane_id = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::super::indicators::fixtures::*;
    use super::super::indicators::wiring::*;
    use super::*;
    use crate::engine::indicator_handle::IndicatorHandle;
    use crate::events::types::ChartEventName;
    use crate::model::InputValue;
    use crate::ports::chart_renderer::InputChangeEvent;

    fn chart_with(manage: bool, sizing: bool) -> (Wired, Rc<PaneRec>) {
        Wired::panes(manage, sizing)
    }

    fn pane_of(w: &Wired, id: &str) -> PaneInfo {
        w.chart
            .list_panes()
            .into_iter()
            .find(|p| p.indicators.iter().any(|i| i.id == id))
            .expect("a pane holds it")
    }

    fn pane_ids(w: &Wired) -> Vec<String> {
        w.chart.list_panes().into_iter().map(|p| p.id).collect()
    }

    fn warned(w: &Wired) -> usize {
        w.platform.warnings().len()
    }

    fn two_studies() -> (
        Wired,
        Rc<PaneRec>,
        Rc<IndicatorHandleImpl>,
        Rc<IndicatorHandleImpl>,
    ) {
        let (w, rec) = chart_with(true, true);
        let a = w.add(STUDY);
        let b = w.add(STUDY);
        w.ready();
        (w, rec, a, b)
    }

    // ── move / merge ──

    #[test]
    fn a_study_merges_into_another_study_pane_with_its_own_scale_dropping_the_vacated_pane() {
        let (w, rec, a, b) = two_studies();
        let before = w.chart.list_panes();
        assert_eq!(
            before.iter().filter(|p| p.kind == PaneKind::Study).count(),
            2
        );
        let pane_a = pane_of(&w, a.id());

        b.move_to(MoveTarget::Pane(pane_a.id.clone()));
        w.flush();

        let studies: Vec<PaneInfo> = w
            .chart
            .list_panes()
            .into_iter()
            .filter(|p| p.kind == PaneKind::Study)
            .collect();
        assert_eq!(studies.len(), 1, "b's old pane was emptied and removed");
        assert_eq!(studies[0].id, pane_a.id);
        let mut ids: Vec<String> = studies[0].indicators.iter().map(|i| i.id.clone()).collect();
        ids.sort();
        let mut want = vec![a.id().to_string(), b.id().to_string()];
        want.sort();
        assert_eq!(ids, want);
        // b got its own scale column (it doesn't own the pane); a still shares the pane scale.
        assert!(
            studies[0]
                .indicators
                .iter()
                .find(|i| i.id == b.id())
                .expect("b")
                .own_scale
        );
        assert!(
            !studies[0]
                .indicators
                .iter()
                .find(|i| i.id == a.id())
                .expect("a")
                .own_scale
        );
        assert!(rec.set_pane_calls.borrow().contains(&(
            b.id().to_string(),
            pane_a.id.clone(),
            true
        )));
        assert!(!rec.inner.removed_panes.borrow().is_empty());
        assert_eq!(w.events.count(ChartEventName::IndicatorMoved), 1);
    }

    #[test]
    fn moving_onto_the_price_pane_gives_a_non_overlay_its_own_scale_and_an_overlay_shares_it() {
        let (w, _rec) = chart_with(true, true);
        let study = w.add(STUDY);
        let overlay = w.add(OVERLAY);
        w.ready();

        study.move_to(MoveTarget::Price);
        w.flush();
        let price = w
            .chart
            .list_panes()
            .into_iter()
            .find(|p| p.kind == PaneKind::Price)
            .expect("price");
        assert!(
            price
                .indicators
                .iter()
                .find(|i| i.id == study.id())
                .expect("study")
                .own_scale
        );

        // The overlay already lives on price sharing its scale; re-asserting 'price' keeps it shared.
        overlay.move_to(MoveTarget::Price);
        w.flush();
        let price = w
            .chart
            .list_panes()
            .into_iter()
            .find(|p| p.kind == PaneKind::Price)
            .expect("price");
        assert!(
            !price
                .indicators
                .iter()
                .find(|i| i.id == overlay.id())
                .expect("overlay")
                .own_scale
        );
        // `{pane: 'price'}` is the same target.
        overlay.move_to(MoveTarget::Pane("price".to_string()));
        assert_eq!(
            w.events.count(ChartEventName::IndicatorMoved),
            1,
            "the repeats were no-ops"
        );
    }

    #[test]
    fn a_merged_indicators_own_scale_survives_a_recompute() {
        let (w, rec, a, b) = two_studies();
        let pane_a = pane_of(&w, a.id()).id;
        b.move_to(MoveTarget::Pane(pane_a.clone()));
        w.flush();
        let own = |w: &Wired| {
            w.chart
                .list_panes()
                .into_iter()
                .find(|p| p.id == pane_a)
                .and_then(|p| p.indicators.into_iter().find(|i| i.id == b.id()))
                .map(|i| i.own_scale)
        };
        assert_eq!(own(&w), Some(true));

        // A recompute (an input change here; a live tick is the same path) must not reset the merge.
        rec.inner.fire_input_change(InputChangeEvent {
            indicator_id: b.id().to_string(),
            key: "Length".to_string(),
            value: InputValue::Number(5.0),
            kind: None,
        });
        w.flush();
        let merged = w
            .chart
            .list_panes()
            .into_iter()
            .find(|p| p.id == pane_a)
            .expect("pane");
        assert_eq!(merged.indicators.len(), 2, "still merged");
        assert_eq!(own(&w), Some(true), "own-scale survives");
    }

    #[test]
    fn moving_to_a_new_pane_creates_one_the_indicator_owns() {
        let (w, rec) = chart_with(true, true);
        let overlay = w.add(OVERLAY);
        w.ready();
        assert_eq!(pane_of(&w, overlay.id()).kind, PaneKind::Price);

        overlay.move_to(MoveTarget::NewPane(NewPanePlacement {
            before: None,
            after: Some("price".to_string()),
        }));
        w.flush();
        let own = w
            .chart
            .list_panes()
            .into_iter()
            .find(|p| {
                p.kind == PaneKind::Study && p.indicators.iter().any(|i| i.id == overlay.id())
            })
            .expect("its own pane");
        assert!(
            !own.indicators[0].own_scale,
            "owns the pane: shares its scale"
        );
        assert!(
            rec.set_pane_calls
                .borrow()
                .iter()
                .any(|c| c.0 == overlay.id() && !c.2)
        );
        assert_eq!(own.order, 1);
    }

    #[test]
    fn a_new_pane_for_the_master_of_its_own_pane_gets_a_distinct_id() {
        let (w, _rec) = chart_with(true, true);
        let a = w.add(STUDY);
        w.ready();
        let seeded = pane_of(&w, a.id()).id;
        assert_eq!(seeded, format!("pane-{}", a.id()));
        // The natural id collides with its OWN pane: pick another, so the move is not rejected as a no-op.
        assert_eq!(
            w.chart.fresh_pane_id(a.id(), &seeded),
            format!("pane-{}-2", a.id())
        );
        a.move_to(MoveTarget::NewPane(NewPanePlacement::default()));
        w.flush();
        assert_eq!(
            pane_ids(&w),
            ["price".to_string(), format!("pane-{}-2", a.id())]
        );
        assert_eq!(w.events.count(ChartEventName::IndicatorMoved), 1);
    }

    #[test]
    fn moving_to_an_unknown_pane_or_a_missing_indicator_does_nothing() {
        let (w, rec, a, _b) = two_studies();
        a.move_to(MoveTarget::Pane("nope".to_string()));
        w.chart.move_indicator("missing", MoveTarget::Price);
        assert!(rec.set_pane_calls.borrow().is_empty());
        assert_eq!(w.events.count(ChartEventName::IndicatorMoved), 0);
        // A study asked to merge into the pane it already owns alone keeps its scale: a no-op
        // only when the scale flag matches, so merging onto itself turns the column on.
        let own = pane_of(&w, a.id()).id;
        a.move_to(MoveTarget::Pane(own));
        assert!(rec.set_pane_calls.borrow().iter().all(|c| c.2));
    }

    // ── order, collapse, maximize ──

    #[test]
    fn move_pane_reorders_study_panes_and_pushes_the_order_to_the_renderer() {
        let (w, rec, _a, b) = two_studies();
        let b_pane = pane_of(&w, b.id());
        assert_eq!(b_pane.order, 2, "price(0), A(1), B(2)");

        w.chart.move_pane(&b_pane.id, PaneMoveDir::Up);
        w.flush();
        assert_eq!(
            w.chart
                .list_panes()
                .into_iter()
                .find(|p| p.id == b_pane.id)
                .map(|p| p.order),
            Some(1)
        );
        assert_eq!(
            rec.order_calls
                .borrow()
                .last()
                .and_then(|o| o.first().cloned())
                .as_deref(),
            Some("price")
        );
        assert_eq!(w.events.count(ChartEventName::PaneMoved), 1);

        // Already first below price: no further up; the price pane and unknown panes never move.
        let calls = rec.order_calls.borrow().len();
        w.chart.move_pane(&b_pane.id, PaneMoveDir::Up);
        w.chart.move_pane("price", PaneMoveDir::Down);
        w.chart.move_pane("nope", PaneMoveDir::Down);
        let last = pane_ids(&w).last().cloned().expect("panes");
        w.chart.move_pane(&last, PaneMoveDir::Down);
        assert_eq!(rec.order_calls.borrow().len(), calls);
    }

    #[test]
    fn collapse_and_maximize_flow_through_to_the_renderer_and_the_pane_list() {
        let (w, rec) = chart_with(true, true);
        let a = w.add(STUDY);
        w.ready();
        let pane_id = pane_of(&w, a.id()).id;

        w.chart.collapse_pane(&pane_id, true);
        assert!(
            w.chart
                .list_panes()
                .iter()
                .find(|p| p.id == pane_id)
                .expect("pane")
                .collapsed
        );
        assert!(
            rec.collapse_calls
                .borrow()
                .contains(&(pane_id.clone(), true))
        );

        w.chart.maximize_pane(Some(&pane_id));
        assert!(
            w.chart
                .list_panes()
                .iter()
                .find(|p| p.id == pane_id)
                .expect("pane")
                .maximized
        );
        assert_eq!(
            rec.maximize_calls.borrow().last(),
            Some(&Some(pane_id.clone()))
        );

        w.chart.maximize_pane(None);
        assert!(w.chart.list_panes().iter().all(|p| !p.maximized));
        w.chart.collapse_pane(&pane_id, false);
        assert!(w.chart.list_panes().iter().all(|p| !p.collapsed));
    }

    // ── renderer-initiated actions ──

    #[test]
    fn a_pane_remove_action_tears_down_every_indicator_in_that_pane() {
        let (w, rec) = chart_with(true, true);
        let a = w.add(STUDY);
        w.ready();
        let pane_id = pane_of(&w, a.id()).id;

        rec.inner.fire_pane_action(PaneAction::Remove {
            pane_id: pane_id.clone(),
        });
        w.flush();
        assert!(rec.inner.removed.borrow().contains(&a.id().to_string()));
        assert!(!pane_ids(&w).contains(&pane_id));
        // The price pane cannot be removed.
        w.chart.remove_pane_and_indicators("price");
        assert_eq!(pane_ids(&w), ["price"]);
    }

    #[test]
    fn a_legend_move_request_routes_through_move_indicator() {
        let (w, rec, a, b) = two_studies();
        let pane_a = pane_of(&w, a.id()).id;
        rec.inner
            .fire_move_indicator(b.id(), MoveTarget::Pane(pane_a.clone()));
        w.flush();
        let merged = w
            .chart
            .list_panes()
            .into_iter()
            .find(|p| p.id == pane_a)
            .expect("pane");
        assert_eq!(merged.indicators.len(), 2);
    }

    #[test]
    fn renderer_collapse_maximize_move_and_resize_actions_keep_the_mirror_in_sync() {
        let (w, rec, a, b) = two_studies();
        let (pa, pb) = (pane_of(&w, a.id()).id, pane_of(&w, b.id()).id);
        w.events.clear();

        rec.inner.fire_pane_action(PaneAction::Collapse {
            pane_id: pa.clone(),
            collapsed: true,
        });
        assert!(
            w.chart
                .list_panes()
                .iter()
                .find(|p| p.id == pa)
                .expect("pane")
                .collapsed
        );
        rec.inner.fire_pane_action(PaneAction::Collapse {
            pane_id: pa.clone(),
            collapsed: false,
        });
        assert!(
            !w.chart
                .list_panes()
                .iter()
                .find(|p| p.id == pa)
                .expect("pane")
                .collapsed
        );
        rec.inner.fire_pane_action(PaneAction::Maximize {
            pane_id: pb.clone(),
            maximized: true,
        });
        assert!(
            w.chart
                .list_panes()
                .iter()
                .find(|p| p.id == pb)
                .expect("pane")
                .maximized
        );
        rec.inner.fire_pane_action(PaneAction::Maximize {
            pane_id: pb.clone(),
            maximized: false,
        });
        assert!(w.chart.list_panes().iter().all(|p| !p.maximized));
        assert_eq!(w.events.count(ChartEventName::PaneChanged), 4);

        // The renderer already reordered: the mirror swaps with the neighbor and does not drive the renderer.
        let orders = rec.order_calls.borrow().len();
        rec.inner.fire_pane_action(PaneAction::Move {
            pane_id: pb.clone(),
            dir: PaneMoveDir::Up,
        });
        assert_eq!(pane_ids(&w), ["price".to_string(), pb.clone(), pa.clone()]);
        assert_eq!(rec.order_calls.borrow().len(), orders);
        assert_eq!(w.events.count(ChartEventName::PaneMoved), 1);
        // Off the edge: nothing.
        rec.inner.fire_pane_action(PaneAction::Move {
            pane_id: pb.clone(),
            dir: PaneMoveDir::Up,
        });
        rec.inner.fire_pane_action(PaneAction::Move {
            pane_id: "nope".to_string(),
            dir: PaneMoveDir::Down,
        });
        assert_eq!(w.events.count(ChartEventName::PaneMoved), 1);

        let changed = w.events.count(ChartEventName::PaneChanged);
        rec.inner.fire_pane_action(PaneAction::Resize {
            pane_ids: vec!["price".to_string(), pa],
        });
        assert_eq!(w.events.count(ChartEventName::PaneChanged), changed + 1);
    }

    #[test]
    fn removing_a_collapsed_or_maximized_panes_last_indicator_forgets_its_state() {
        let (w, _rec) = chart_with(true, true);
        let a = w.add(STUDY);
        w.ready();
        let pane_id = pane_of(&w, a.id()).id;
        w.chart.collapse_pane(&pane_id, true);
        w.chart.maximize_pane(Some(&pane_id));
        a.remove();
        let state = w.chart.inner.state.borrow();
        assert!(!state.collapsed_panes.contains(&pane_id));
        assert_eq!(state.maximized_pane_id, None);
        assert_eq!(state.pane_order, ["price"]);
    }

    // ── sizing ──

    #[test]
    fn set_height_weight_drives_the_renderer_shows_in_the_list_and_fires_pane_changed() {
        let (w, rec) = chart_with(true, true);
        let a = w.add(STUDY);
        w.ready();
        let pane_id = pane_of(&w, a.id()).id;
        assert_eq!(
            w.chart
                .list_panes()
                .iter()
                .find(|p| p.id == pane_id)
                .and_then(|p| p.height_weight),
            Some(1.0)
        );

        w.events.clear();
        w.chart.set_pane_height_weight("price", 6.0);
        w.chart.set_pane_height_weight(&pane_id, 2.0);
        assert_eq!(rec.weights.borrow().get("price"), Some(&6.0));
        let list = w.chart.list_panes();
        assert_eq!(
            list.iter()
                .find(|p| p.id == "price")
                .and_then(|p| p.height_weight),
            Some(6.0)
        );
        assert_eq!(
            list.iter()
                .find(|p| p.id == pane_id)
                .and_then(|p| p.height_weight),
            Some(2.0)
        );
        assert_eq!(w.events.count(ChartEventName::PaneChanged), 2);
    }

    #[test]
    fn a_weight_that_is_not_positive_or_an_unknown_pane_is_rejected_with_a_warning() {
        let (w, rec) = chart_with(true, true);
        w.ready();
        let warned_before = warned(&w);
        w.events.clear();
        w.chart.set_pane_height_weight("price", 0.0);
        w.chart.set_pane_height_weight("price", f64::NAN);
        w.chart.set_pane_height_weight("nope", 2.0);
        assert!(
            !rec.weights.borrow().contains_key("price"),
            "nothing was sent to the renderer"
        );
        assert!(!rec.weights.borrow().contains_key("nope"));
        assert_eq!(w.events.count(ChartEventName::PaneChanged), 0);
        let warnings = w.platform.warnings();
        assert_eq!(warnings.len() - warned_before, 3);
        assert!(warnings.iter().any(|m| m.contains("got NaN")));
        assert!(warnings.iter().any(|m| m.contains("no pane \"nope\"")));
    }

    #[test]
    fn mounting_onto_an_existing_pane_never_sends_a_weight() {
        let (w, rec) = chart_with(true, true);
        w.add(OVERLAY);
        w.ready();
        w.chart.set_pane_height_weight("price", 6.0);
        rec.ensure_calls.borrow_mut().clear();

        w.add(OVERLAY);
        w.flush();
        let price_calls: Vec<Pane> = rec
            .ensure_calls
            .borrow()
            .iter()
            .filter(|p| p.id == "price")
            .cloned()
            .collect();
        assert!(!price_calls.is_empty());
        assert!(price_calls.iter().all(|p| p.height_weight.is_none()));
        assert_eq!(
            w.chart
                .list_panes()
                .iter()
                .find(|p| p.id == "price")
                .and_then(|p| p.height_weight),
            Some(6.0)
        );
    }

    #[test]
    fn a_renderer_without_pane_sizing_warns_and_the_list_leaves_the_weight_out() {
        let (w, _rec) = chart_with(true, false);
        w.ready();
        w.chart.set_pane_height_weight("price", 4.0);
        assert!(
            w.platform
                .warnings()
                .iter()
                .any(|m| m.contains("does not support pane sizing"))
        );
        assert_eq!(w.chart.list_panes()[0].height_weight, None);
    }

    // ── lifecycle events ──

    #[test]
    fn pane_changed_fires_when_a_study_pane_is_created_with_the_pane_already_listed() {
        let (w, _rec) = chart_with(true, true);
        let seen: Rc<RefCell<Vec<Vec<String>>>> = Rc::default();
        {
            let (sink, chart) = (Rc::clone(&seen), w.chart.clone());
            w.chart.events().on(ChartEventName::PaneChanged, move |_| {
                sink.borrow_mut()
                    .push(chart.list_panes().into_iter().map(|p| p.id).collect());
            });
        }
        let a = w.add(STUDY);
        w.ready();
        let pane_id = pane_of(&w, a.id()).id;
        assert!(!seen.borrow().is_empty());
        assert!(seen.borrow()[0].contains(&pane_id));
    }

    #[test]
    fn an_overlay_joining_the_price_pane_fires_no_pane_changed() {
        let (w, _rec) = chart_with(true, true);
        w.events.clear();
        w.add(OVERLAY);
        w.ready();
        assert_eq!(w.events.count(ChartEventName::PaneChanged), 0);
    }

    #[test]
    fn removing_the_last_indicator_of_a_study_pane_removes_the_pane_and_announces_it() {
        let (w, rec, a, b) = two_studies();
        w.events.clear();
        let pane_a = pane_of(&w, a.id()).id;
        a.remove();
        assert_eq!(
            rec.inner.removed_panes.borrow().as_slice(),
            std::slice::from_ref(&pane_a)
        );
        assert_eq!(w.events.count(ChartEventName::PaneChanged), 1);
        assert!(!pane_ids(&w).contains(&pane_a));
        // A pane that still holds another indicator stays.
        let pane_b = pane_of(&w, b.id()).id;
        assert!(pane_ids(&w).contains(&pane_b));
    }

    // ── capability gating ──

    #[test]
    fn a_renderer_without_pane_management_reports_unsupported_and_no_ops_moves_with_a_warning() {
        let (w, rec) = chart_with(false, true);
        w.add(STUDY);
        let b = w.add(STUDY);
        w.ready();
        assert!(!w.chart.pane_management_supported());
        let before = w.chart.list_panes().len();
        let warned_before = warned(&w);

        b.move_to(MoveTarget::Pane("price".to_string())); // warns + no-op
        w.chart.move_pane("whatever", PaneMoveDir::Up);
        w.flush();
        assert_eq!(w.chart.list_panes().len(), before);
        assert!(warned(&w) > warned_before);
        assert!(rec.set_pane_calls.borrow().is_empty());
    }

    #[test]
    fn a_supporting_renderer_reports_supported() {
        let (w, _rec) = chart_with(true, true);
        assert!(w.chart.pane_management_supported());
    }

    // ── helpers ──

    #[test]
    fn insert_pane_order_places_relative_to_a_neighbor_and_never_above_price() {
        let (w, _rec) = chart_with(true, true);
        w.chart.inner.state.borrow_mut().pane_order =
            ["price", "p1", "p2", "p3"].map(str::to_string).to_vec();
        let order = |w: &Wired| w.chart.inner.state.borrow().pane_order.clone();
        w.chart.insert_pane_order(
            "p3",
            &NewPanePlacement {
                before: Some("p1".to_string()),
                after: None,
            },
        );
        assert_eq!(order(&w), ["price", "p3", "p1", "p2"]);
        w.chart.insert_pane_order(
            "p3",
            &NewPanePlacement {
                before: None,
                after: Some("p2".to_string()),
            },
        );
        assert_eq!(order(&w), ["price", "p1", "p2", "p3"]);
        // `before: price` still lands below it.
        w.chart.insert_pane_order(
            "p3",
            &NewPanePlacement {
                before: Some("price".to_string()),
                after: None,
            },
        );
        assert_eq!(order(&w), ["price", "p3", "p1", "p2"]);
        // An unknown neighbor means the bottom.
        w.chart.insert_pane_order(
            "p9",
            &NewPanePlacement {
                before: Some("zzz".to_string()),
                after: None,
            },
        );
        assert_eq!(order(&w), ["price", "p3", "p1", "p2", "p9"]);
    }

    #[test]
    fn ensure_pane_for_reports_a_new_pane_once_and_forget_pane_never_drops_price() {
        let (w, rec) = chart_with(true, true);
        assert!(w.chart.ensure_pane_for("pane-x"));
        assert!(!w.chart.ensure_pane_for("pane-x"));
        assert_eq!(
            rec.ensure_calls
                .borrow()
                .last()
                .map(|p| (p.id.clone(), p.kind, p.order)),
            Some(("pane-x".to_string(), PaneKind::Study, 1))
        );
        assert!(!w.chart.ensure_pane_for("price"));
        w.chart.forget_pane("price");
        w.chart.forget_pane("pane-x");
        assert_eq!(w.chart.inner.state.borrow().pane_order, ["price"]);
        assert!(!w.chart.pane_still_used("pane-x"));
    }
}
