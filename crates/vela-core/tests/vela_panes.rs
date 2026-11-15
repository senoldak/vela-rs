//! `chart.panes()` and `IndicatorHandle::move_to` through the public API
//! (`pane-management.test.ts`).

mod vela_common;

use std::cell::Cell;
use std::rc::Rc;

use vela_common::*;
use vela_core::engine::indicator_handle::IndicatorHandle;
use vela_core::events::types::ChartEventName;
use vela_core::model::PaneKind;
use vela_core::options::{AddIndicatorOptions, MoveTarget, NewPanePlacement, PaneInfo};
use vela_core::ports::chart_renderer::{PaneAction, PaneMoveDir};

const STUDY: &str = "//@version=5\nindicator(\"S\")\nplot(close)";
const OVERLAY: &str = "//@version=5\nindicator(\"O\", overlay=true)\nplot(close)";

fn rig_with(management: bool, sizing: bool) -> Rig {
    let rig = Rig::builder()
        .renderer(move |r| {
            r.pane_management.set(management);
            r.sizing.set(sizing);
        })
        .build();
    rig.ready().expect("ready");
    rig.flush();
    rig
}

fn rig() -> Rig {
    rig_with(true, true)
}

fn add(rig: &Rig, source: &str) -> Rc<dyn IndicatorHandle> {
    let handle = rig
        .chart
        .add_indicator(source, AddIndicatorOptions::default())
        .expect("added");
    rig.flush();
    handle
}

fn pane_of(rig: &Rig, id: &str) -> PaneInfo {
    rig.chart
        .panes()
        .list()
        .into_iter()
        .find(|p| p.indicators.iter().any(|i| i.id == id))
        .unwrap_or_else(|| panic!("no pane holds {id}"))
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

fn ids_of(pane: &PaneInfo) -> Vec<String> {
    sorted(pane.indicators.iter().map(|i| i.id.clone()).collect())
}

#[test]
fn merges_a_study_into_another_study_pane_with_its_own_scale_dropping_the_vacated_pane() {
    let rig = rig();
    let a = add(&rig, STUDY);
    let b = add(&rig, STUDY);
    let studies = |rig: &Rig| {
        rig.chart
            .panes()
            .list()
            .into_iter()
            .filter(|p| p.kind == PaneKind::Study)
            .collect::<Vec<_>>()
    };
    assert_eq!(studies(&rig).len(), 2);
    let pane_a = pane_of(&rig, a.id());

    b.move_to(MoveTarget::Pane(pane_a.id.clone()));
    rig.flush();

    let after = studies(&rig);
    assert_eq!(after.len(), 1, "b's old pane was emptied and removed");
    let merged = &after[0];
    assert_eq!(merged.id, pane_a.id);
    assert_eq!(
        ids_of(merged),
        sorted(vec![a.id().to_string(), b.id().to_string()])
    );
    let own = |id: &str| {
        merged
            .indicators
            .iter()
            .find(|i| i.id == id)
            .map(|i| i.own_scale)
    };
    assert_eq!(own(b.id()), Some(true), "b gets its own scale column");
    assert_eq!(own(a.id()), Some(false), "a keeps the pane scale");
    assert!(
        rig.renderer
            .set_pane_calls
            .borrow()
            .iter()
            .any(|(id, pane, own)| id == b.id() && *pane == pane_a.id && *own)
    );
    assert!(!rig.renderer.removed_panes.borrow().is_empty());
}

#[test]
fn moving_onto_the_price_pane_a_study_gets_its_own_scale_an_overlay_shares_it() {
    let rig = rig();
    let study = add(&rig, STUDY);
    let overlay = add(&rig, OVERLAY);

    study.move_to(MoveTarget::Price);
    rig.flush();
    let price = |rig: &Rig| {
        rig.chart
            .panes()
            .list()
            .into_iter()
            .find(|p| p.kind == PaneKind::Price)
            .expect("a price pane")
    };
    let own_scale = |pane: &PaneInfo, id: &str| {
        pane.indicators
            .iter()
            .find(|i| i.id == id)
            .map(|i| i.own_scale)
    };
    assert_eq!(own_scale(&price(&rig), study.id()), Some(true));

    overlay.move_to(MoveTarget::Price);
    rig.flush();
    assert_eq!(own_scale(&price(&rig), overlay.id()), Some(false));
}

#[test]
fn a_merged_indicator_keeps_its_own_scale_across_a_recompute() {
    let rig = rig();
    let a = add(&rig, STUDY);
    let b = add(&rig, STUDY);
    let pane_a = pane_of(&rig, a.id()).id;

    b.move_to(MoveTarget::Pane(pane_a.clone()));
    rig.flush();

    // A recompute (an input change here, a live tick is the same path) must not reset the merge.
    b.set_input("Length", vela_core::model::InputValue::Number(5.0));
    rig.rerun(b.id());
    let merged = rig
        .chart
        .panes()
        .list()
        .into_iter()
        .find(|p| p.id == pane_a)
        .expect("the pane");
    assert_eq!(
        ids_of(&merged),
        sorted(vec![a.id().to_string(), b.id().to_string()])
    );
    assert_eq!(
        merged
            .indicators
            .iter()
            .find(|i| i.id == b.id())
            .map(|i| i.own_scale),
        Some(true)
    );
}

#[test]
fn moving_to_a_new_pane_creates_one_the_indicator_owns() {
    let rig = rig();
    let overlay = add(&rig, OVERLAY);
    assert_eq!(pane_of(&rig, overlay.id()).kind, PaneKind::Price);

    overlay.move_to(MoveTarget::NewPane(NewPanePlacement {
        after: Some("price".to_string()),
        ..NewPanePlacement::default()
    }));
    rig.flush();

    let own = pane_of(&rig, overlay.id());
    assert_eq!(own.kind, PaneKind::Study);
    assert_eq!(
        own.indicators
            .iter()
            .find(|i| i.id == overlay.id())
            .map(|i| i.own_scale),
        Some(false),
        "owns the pane, so it shares its scale"
    );
    assert!(
        rig.renderer
            .set_pane_calls
            .borrow()
            .iter()
            .any(|(id, _, own)| id == overlay.id() && !own)
    );
}

#[test]
fn move_pane_reorders_study_panes_and_pushes_the_new_order_to_the_renderer() {
    let rig = rig();
    let _a = add(&rig, STUDY);
    let b = add(&rig, STUDY);
    let b_pane = pane_of(&rig, b.id());
    assert_eq!(b_pane.order, 2, "price(0), A(1), B(2)");

    rig.chart.panes().move_pane(&b_pane.id, PaneMoveDir::Up);
    rig.flush();

    assert_eq!(pane_of(&rig, b.id()).order, 1, "B is now above A");
    let orders = rig.renderer.order_calls.borrow();
    assert_eq!(
        orders.last().and_then(|o| o.first()).map(String::as_str),
        Some("price"),
        "price stays pinned on top"
    );
}

#[test]
fn collapse_and_maximize_flow_through_to_the_renderer_and_the_pane_list() {
    let rig = rig();
    let a = add(&rig, STUDY);
    let pane_id = pane_of(&rig, a.id()).id;

    rig.chart.panes().collapse(&pane_id, true);
    assert!(pane_of(&rig, a.id()).collapsed);
    assert!(
        rig.renderer
            .collapse_calls
            .borrow()
            .contains(&(pane_id.clone(), true))
    );

    rig.chart.panes().maximize(Some(&pane_id));
    assert!(pane_of(&rig, a.id()).maximized);
    assert_eq!(
        rig.renderer.maximize_calls.borrow().last().cloned(),
        Some(Some(pane_id.clone()))
    );

    rig.chart.panes().maximize(None);
    assert!(rig.chart.panes().list().iter().all(|p| !p.maximized));
}

#[test]
fn a_pane_remove_action_tears_down_every_indicator_in_that_pane() {
    let rig = rig();
    let a = add(&rig, STUDY);
    let pane_id = pane_of(&rig, a.id()).id;

    rig.renderer.fire_pane_action(PaneAction::Remove {
        pane_id: pane_id.clone(),
    });
    rig.flush();
    assert!(rig.renderer.removed.borrow().contains(&a.id().to_string()));
    assert!(!rig.chart.panes().list().iter().any(|p| p.id == pane_id));
}

#[test]
fn a_legend_move_request_routes_through_move_indicator() {
    let rig = rig();
    let a = add(&rig, STUDY);
    let b = add(&rig, STUDY);
    let pane_a = pane_of(&rig, a.id()).id;

    rig.renderer
        .fire_move_indicator(b.id(), MoveTarget::Pane(pane_a.clone()));
    rig.flush();
    let merged = rig
        .chart
        .panes()
        .list()
        .into_iter()
        .find(|p| p.id == pane_a)
        .expect("the pane");
    assert_eq!(
        ids_of(&merged),
        sorted(vec![a.id().to_string(), b.id().to_string()])
    );
}

#[test]
fn set_height_weight_drives_the_renderer_shows_in_list_and_fires_pane_changed() {
    let rig = rig();
    let a = add(&rig, STUDY);
    let pane_id = pane_of(&rig, a.id()).id;
    let weight = |rig: &Rig, id: &str| {
        rig.chart
            .panes()
            .list()
            .into_iter()
            .find(|p| p.id == id)
            .and_then(|p| p.height_weight)
    };
    assert_eq!(weight(&rig, &pane_id), Some(1.0));

    let changed = Rc::new(Cell::new(0));
    let sink = Rc::clone(&changed);
    rig.chart.on(ChartEventName::PaneChanged, move |_| {
        sink.set(sink.get() + 1)
    });
    rig.chart
        .panes()
        .set_height_weight("price", 6.0)
        .set_height_weight(&pane_id, 2.0);

    assert_eq!(rig.renderer.weights.borrow().get("price"), Some(&6.0));
    assert_eq!(weight(&rig, "price"), Some(6.0));
    assert_eq!(weight(&rig, &pane_id), Some(2.0));
    assert_eq!(changed.get(), 2);
}

#[test]
fn a_weight_that_is_not_positive_or_an_unknown_pane_is_rejected_with_a_warning() {
    let rig = rig();
    let changed = Rc::new(Cell::new(0));
    let sink = Rc::clone(&changed);
    rig.chart.on(ChartEventName::PaneChanged, move |_| {
        sink.set(sink.get() + 1)
    });
    let before = rig.platform.warnings().len();
    rig.chart.panes().set_height_weight("price", 0.0);
    rig.chart.panes().set_height_weight("price", f64::NAN);
    rig.chart.panes().set_height_weight("nope", 2.0);
    assert!(!rig.renderer.weights.borrow().contains_key("price"));
    assert!(!rig.renderer.weights.borrow().contains_key("nope"));
    assert_eq!(changed.get(), 0);
    assert_eq!(rig.platform.warnings().len() - before, 3);
}

#[test]
fn a_renderer_resize_action_fires_pane_changed_and_reads_the_weights_back() {
    let rig = rig();
    let a = add(&rig, STUDY);
    let pane_id = pane_of(&rig, a.id()).id;
    let changed = Rc::new(Cell::new(0));
    let sink = Rc::clone(&changed);
    rig.chart.on(ChartEventName::PaneChanged, move |_| {
        sink.set(sink.get() + 1)
    });
    rig.renderer
        .weights
        .borrow_mut()
        .insert(pane_id.clone(), 1.5);
    rig.renderer.fire_pane_action(PaneAction::Resize {
        pane_ids: vec!["price".to_string(), pane_id.clone()],
    });
    assert_eq!(changed.get(), 1);
    assert_eq!(pane_of(&rig, a.id()).height_weight, Some(1.5));
}

#[test]
fn a_renderer_without_pane_sizing_warns_and_leaves_the_height_weight_out() {
    let rig = rig_with(true, false);
    rig.chart.panes().set_height_weight("price", 4.0);
    assert!(!rig.platform.warnings().is_empty());
    assert_eq!(rig.chart.panes().list()[0].height_weight, None);
}

#[test]
fn pane_changed_fires_for_a_new_study_pane_with_the_pane_already_listed_and_not_for_an_overlay() {
    let rig = rig();
    let seen: Rc<std::cell::RefCell<Vec<Vec<String>>>> = Rc::default();
    let (sink, chart) = (Rc::clone(&seen), rig.chart.clone());
    rig.chart.on(ChartEventName::PaneChanged, move |_| {
        sink.borrow_mut()
            .push(chart.panes().list().into_iter().map(|p| p.id).collect());
    });
    add(&rig, OVERLAY);
    assert!(
        seen.borrow().is_empty(),
        "an overlay joining the price pane is silent"
    );

    let a = add(&rig, STUDY);
    let pane_id = pane_of(&rig, a.id()).id;
    assert!(!seen.borrow().is_empty());
    assert!(seen.borrow()[0].contains(&pane_id));
    // The handler held a chart clone: detach it so the chart can go.
    rig.chart.destroy();
}

#[test]
fn a_renderer_without_pane_management_reports_unsupported_and_no_ops_mutations_with_a_warning() {
    let rig = rig_with(false, false);
    let _a = add(&rig, STUDY);
    let b = add(&rig, STUDY);

    assert!(!rig.chart.panes().supported());
    let before = rig.chart.panes().list().len();
    b.move_to(MoveTarget::Pane("price".to_string())); // warns, no-op
    rig.chart.panes().move_pane("whatever", PaneMoveDir::Up);
    rig.flush();
    assert_eq!(rig.chart.panes().list().len(), before);
    assert!(!rig.platform.warnings().is_empty());
}
