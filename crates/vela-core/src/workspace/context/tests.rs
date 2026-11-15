//! The live widget context. The behaviors that need cells (following the active cell,
//! `set_symbol`, the cells list) are exercised with the workspace; here the context's
//! contract when the workspace is gone, and its write-through methods.

use std::cell::Cell;
use std::rc::Rc;

use crate::workspace::cell::ExternalIndicatorEntry;
use crate::workspace::host::ToastKind;
use crate::workspace::rig::Rig;
use crate::workspace::shell::WorkspaceEventName;

#[test]
fn a_context_over_a_dropped_workspace_reads_the_defaults_and_writes_nothing() {
    let ctx = {
        let rig = Rig::new("1", &[("only", "AAA")]);
        rig.ws.context()
    };
    assert!(ctx.chart().is_none());
    assert_eq!(ctx.symbol(), "");
    assert_eq!(ctx.timeframe(), "60");
    assert_eq!(ctx.price_style(), "candles");
    assert_eq!(ctx.active_cell_id(), "");
    assert!(ctx.cells().is_empty());
    assert!(ctx.i18n().is_none() && ctx.replay().is_none());
    ctx.set_symbol("BBB");
    ctx.set_timeframe("15");
    ctx.set_price_style("line");
    ctx.set_active_cell("only");
    ctx.add_native_indicator("sma");
    ctx.add_indicator(ExternalIndicatorEntry::default());
    ctx.state_changed();
    ctx.toast("nobody hears this", ToastKind::Info);
}

#[test]
fn the_write_through_methods_act_on_the_active_cell_only() {
    let rig = Rig::new("2h", &[("a", "AAA"), ("b", "BBB")]);
    let ctx = rig.ws.context();
    ctx.set_timeframe("15");
    ctx.set_price_style("line");
    ctx.add_native_indicator("sma");
    rig.settle();
    let a = rig.ws.cell("a").unwrap();
    let b = rig.ws.cell("b").unwrap();
    assert_eq!(a.timeframe(), "15");
    assert_eq!(a.requested_price_style(), "line");
    assert_eq!(a.indicator_count(), 1);
    assert_eq!((b.timeframe().as_str(), b.indicator_count()), ("60", 0));
    assert_eq!(
        ctx.cells()
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(ctx.cells()[1].symbol, "BBB");
}

#[test]
fn state_changed_marks_the_document_dirty_and_toasts_reach_the_host() {
    let rig = Rig::new("1", &[("only", "AAA")]);
    let changed = Rc::new(Cell::new(0));
    let c = changed.clone();
    rig.ws.on(WorkspaceEventName::StateChanged, move |_| {
        c.set(c.get() + 1)
    });
    let ctx = rig.ws.context();
    ctx.state_changed();
    rig.platform.advance(600.0);
    assert_eq!(changed.get(), 1);
    ctx.toast("hello", ToastKind::Success);
    assert_eq!(
        rig.host.toasts().last().cloned(),
        Some(("hello".to_string(), ToastKind::Success))
    );
}
