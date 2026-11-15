//! `Vela::run_indicator`, `Vela::run_script` and host-supplied indicator ids,
//! through the public API only (`orchestrator.test.ts`).

mod vela_common;

use std::cell::RefCell;
use std::rc::Rc;

use vela_common::*;
use vela_core::engine::orchestrator::IndicatorIdError;
use vela_core::events::types::ChartEventName;
use vela_core::options::AddIndicatorOptions;
use vela_core::script_run::{ScriptKind, ScriptRun, ScriptRunCause};
use vela_core::vela::VelaError;

fn rig() -> Rig {
    let rig = Rig::builder().build();
    rig.loaded();
    rig
}

fn with_id(id: &str) -> AddIndicatorOptions {
    AddIndicatorOptions {
        id: Some(id.to_string()),
        ..AddIndicatorOptions::default()
    }
}

#[test]
fn run_indicator_resolves_ok_with_the_live_handle_after_the_first_evaluation() {
    let rig = rig();
    let result = rig.block_on(
        rig.chart
            .run_indicator("plot(close)", AddIndicatorOptions::default()),
    );
    assert!(result.ok);
    assert_eq!(result.error, None);
    let handle = result.handle.expect("a handle");
    assert!(rig.indicator_ids().contains(&handle.id().to_string()));
}

#[test]
fn run_indicator_resolves_not_ok_with_the_error_and_removes_the_failed_indicator() {
    let rig = rig();
    rig.engine.hold.set(true);
    let before = rig.chart.indicators().len();
    let pending = rig
        .chart
        .run_indicator("plot(close)", AddIndicatorOptions::default());
    rig.flush();
    assert_eq!(
        rig.chart.indicators().len(),
        before + 1,
        "mounted while running"
    );
    rig.engine.fail_all("boom: bad script");
    let result = rig.block_on(pending);
    assert!(!result.ok);
    assert!(result.handle.is_none());
    assert!(
        result
            .error
            .as_deref()
            .is_some_and(|e| e.contains("boom: bad script"))
    );
    rig.flush();
    assert_eq!(
        rig.chart.indicators().len(),
        before,
        "no dead legend row left behind"
    );
}

#[test]
fn run_indicator_reads_the_context_of_a_failed_run_when_the_engine_has_one() {
    let rig = rig();
    rig.engine.hold.set(true);
    *rig.engine.context.borrow_mut() = Some(run_context());
    let pending = rig
        .chart
        .run_indicator("plot(close)", AddIndicatorOptions::default());
    rig.flush();
    rig.engine.fail_all("boom");
    let result = rig.block_on(pending);
    assert!(!result.ok);
    assert_eq!(result.context.map(|c| c.bar_index), Some(9));
}

#[test]
fn run_indicator_registers_its_listeners_when_called_not_when_polled() {
    let rig = rig();
    rig.engine.hold.set(true);
    // The future is not polled until after `finish`, yet the run still settles.
    let pending = rig
        .chart
        .run_indicator("plot(close)", AddIndicatorOptions::default());
    rig.flush();
    rig.engine.finish();
    rig.flush();
    assert!(rig.block_on(pending).ok);
}

#[test]
fn run_script_resolves_the_first_run_then_follows_later_ones_through_on_update() {
    let rig = rig();
    *rig.engine.context.borrow_mut() = Some(run_context());
    let result = rig.block_on(
        rig.chart
            .run_script("plot(close)", AddIndicatorOptions::default()),
    );
    assert!(result.ok);
    let run = result.run.clone().expect("the run, not just ready");
    assert_eq!(run.title, "Mock");
    assert_eq!(run.kind, ScriptKind::Strategy);
    assert!(result.error.is_none());

    let later: Rc<RefCell<Vec<ScriptRunCause>>> = Rc::default();
    let sink = Rc::clone(&later);
    let _off = result.on_update(move |run: &ScriptRun| sink.borrow_mut().push(run.cause));
    let handle = rig.chart.indicators().pop().expect("the script indicator");
    handle.set_input("Length", vela_core::model::InputValue::Number(21.0));
    rig.rerun(handle.id());
    assert!(
        later.borrow().contains(&ScriptRunCause::Inputs),
        "{:?}",
        later.borrow()
    );

    result.remove();
    rig.flush();
    assert!(rig.chart.indicators().is_empty());
}

#[test]
fn run_script_on_update_can_be_unsubscribed() {
    let rig = rig();
    *rig.engine.context.borrow_mut() = Some(run_context());
    let result = rig.block_on(
        rig.chart
            .run_script("plot(close)", AddIndicatorOptions::default()),
    );
    let count: Rc<RefCell<usize>> = Rc::default();
    let sink = Rc::clone(&count);
    let off = result.on_update(move |_| *sink.borrow_mut() += 1);
    let control: Rc<RefCell<usize>> = Rc::default();
    let sink = Rc::clone(&control);
    rig.chart
        .on(ChartEventName::ScriptRun, move |_| *sink.borrow_mut() += 1);
    off();
    let handle = rig.chart.indicators().pop().expect("the script indicator");
    handle.set_input("Length", vela_core::model::InputValue::Number(30.0));
    rig.rerun(handle.id());
    assert_eq!(*count.borrow(), 0);
    assert!(*control.borrow() > 0, "the run did happen");
}

#[test]
fn a_failing_script_resolves_not_ok_and_leaves_no_dead_legend_row() {
    let rig = rig();
    *rig.engine.prepare_error.borrow_mut() = Some("compile blew up".to_string());
    let result = rig.block_on(rig.chart.run_script("bad(", AddIndicatorOptions::default()));
    assert!(!result.ok);
    assert!(result.run.is_none());
    assert_eq!(result.error.as_deref(), Some("compile blew up"));
    rig.flush();
    assert!(rig.chart.indicators().is_empty());
    // Removing a failed result does nothing.
    result.remove();
}

#[test]
fn the_supplied_id_is_the_indicator_id_on_every_surface() {
    let rig = rig();
    let handle = rig
        .chart
        .add_indicator("plot(close)", with_id("doc:rsi-1"))
        .expect("added");
    assert_eq!(handle.id(), "doc:rsi-1");
    rig.flush();
    assert_eq!(rig.indicator_ids(), ["doc:rsi-1"]);
    let added = rig.log.of(ChartEventName::IndicatorAdded);
    assert_eq!(added.len(), 1);
    assert!(format!("{:?}", added[0]).contains("doc:rsi-1"));
    handle.remove();
    assert_eq!(rig.log.count(ChartEventName::IndicatorRemoved), 1);
    assert!(
        rig.renderer
            .removed
            .borrow()
            .contains(&"doc:rsi-1".to_string())
    );
}

#[test]
fn a_minted_id_never_collides_with_a_host_id_already_live() {
    let rig = rig();
    let host = rig
        .chart
        .add_indicator("plot(close)", with_id("ind-2"))
        .expect("added");
    let first = rig
        .chart
        .add_indicator("plot(close)", AddIndicatorOptions::default())
        .expect("added");
    let second = rig
        .chart
        .add_indicator("plot(close)", AddIndicatorOptions::default())
        .expect("added");
    assert_eq!(host.id(), "ind-2");
    assert_eq!(first.id(), "ind-1");
    assert_eq!(second.id(), "ind-3");
}

#[test]
fn add_indicator_refuses_an_id_already_live_and_leaves_the_live_one_untouched() {
    let rig = rig();
    let first = rig
        .chart
        .add_indicator("plot(close)", with_id("dup"))
        .expect("added");
    rig.flush();
    let err = rig
        .chart
        .add_indicator("plot(close)", with_id("dup"))
        .err()
        .expect("refused");
    assert_eq!(
        err,
        VelaError::Indicator(IndicatorIdError::Duplicate("dup".to_string()))
    );
    assert!(err.to_string().contains("\"dup\" is already live"));
    assert_eq!(rig.chart.indicators().len(), 1);
    // Freed by removal, the id can be claimed again.
    first.remove();
    assert_eq!(
        rig.chart
            .add_indicator("plot(close)", with_id("dup"))
            .expect("added")
            .id(),
        "dup"
    );
}

#[test]
fn add_indicator_refuses_an_empty_id() {
    let rig = rig();
    let err = rig
        .chart
        .add_indicator("plot(close)", with_id(""))
        .err()
        .expect("refused");
    assert_eq!(err, VelaError::Indicator(IndicatorIdError::Empty));
    assert!(rig.chart.indicators().is_empty());
}

#[test]
fn run_indicator_and_run_script_resolve_not_ok_on_a_duplicate_id_and_never_touch_the_live_one() {
    let rig = rig();
    let live = rig.block_on(
        rig.chart
            .run_indicator("plot(close)", with_id("editor-tab-3")),
    );
    assert!(live.ok);
    let live_handle = live.handle.clone().expect("handle");
    assert_eq!(live_handle.id(), "editor-tab-3");

    let via_run = rig.block_on(
        rig.chart
            .run_indicator("plot(close)", with_id("editor-tab-3")),
    );
    assert!(!via_run.ok);
    assert!(via_run.handle.is_none());
    assert!(
        via_run
            .error
            .as_deref()
            .is_some_and(|e| e.contains("\"editor-tab-3\" is already live"))
    );

    let via_script = rig.block_on(rig.chart.run_script("plot(close)", with_id("editor-tab-3")));
    assert!(!via_script.ok);
    assert!(via_script.run.is_none());
    assert!(
        via_script
            .error
            .as_deref()
            .is_some_and(|e| e.contains("\"editor-tab-3\" is already live"))
    );

    rig.flush();
    assert_eq!(rig.indicator_ids(), ["editor-tab-3"]);

    // runScript resolves with the host id on the run itself.
    live_handle.remove();
    let fresh = rig.block_on(rig.chart.run_script("plot(close)", with_id("editor-tab-3")));
    assert!(fresh.ok);
    assert_eq!(fresh.run.expect("run").id, "editor-tab-3");
}
