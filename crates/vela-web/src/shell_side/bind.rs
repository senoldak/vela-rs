//! Following the active cell: the panels and the pill are bound to ONE chart, the active
//! cell's, and move to the next one when it changes (`dock.onChart(cell.chart)` in the
//! original's `setActiveCell`).

use std::cell::RefCell;
use std::rc::Rc;

use vela_core::events::types::{ChartEvent, ChartEventName};
use vela_core::vela::Vela;
use vela_core::workspace::{Workspace, WorkspaceEventName};

use super::kit::{Cx, Disposers};

/// `chart.on(ev, f)` as an unsubscribe closure that holds the chart weakly.
pub fn chart_on(
    chart: &Vela,
    ev: ChartEventName,
    f: impl Fn(&ChartEvent) + 'static,
) -> Box<dyn FnOnce()> {
    let sub = chart.on(ev, f);
    let weak = chart.downgrade();
    Box::new(move || {
        if let Some(c) = weak.upgrade() {
            c.off(&sub);
        }
    })
}

/// A set of chart-side subscriptions, dropped together.
#[derive(Default)]
pub struct ChartSubs(RefCell<Vec<Box<dyn FnOnce()>>>);

impl ChartSubs {
    pub fn push(&self, f: Box<dyn FnOnce()>) {
        self.0.borrow_mut().push(f);
    }

    pub fn clear(&self) {
        let list: Vec<_> = self.0.borrow_mut().drain(..).collect();
        for f in list {
            f();
        }
    }
}

/// Calls `bind(chart)` for the active cell's chart now and again whenever the active cell (or
/// the set of cells around it) changes. `bind` receives the cell id and its chart, `None` when no
/// cell is left.
pub fn follow_active(
    cx: &Cx,
    ws: &Workspace,
    disposers: &Disposers,
    bind: impl Fn(Option<(&str, &Vela)>) + 'static,
) {
    let bound: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let bind = Rc::new(bind);
    let weak = ws.downgrade();
    let check = {
        let bound = bound.clone();
        let bind = bind.clone();
        let weak = weak.clone();
        Rc::new(move || {
            let Some(ws) = weak.upgrade() else { return };
            let now = ws
                .active()
                .and_then(|c| c.try_chart().map(|ch| (c.id().to_string(), ch)));
            let id = now.as_ref().map(|(id, _)| id.clone());
            if *bound.borrow() == id && id.is_some() {
                return;
            }
            *bound.borrow_mut() = id;
            bind(now.as_ref().map(|(id, c)| (id.as_str(), c)));
        })
    };
    for name in [
        WorkspaceEventName::CellActive,
        WorkspaceEventName::CellCreated,
        WorkspaceEventName::CellDestroyed,
        WorkspaceEventName::LayoutChanged,
    ] {
        let check = check.clone();
        let sub = ws.on(name, move |_| check());
        let weak = weak.clone();
        disposers.push(move || {
            if let Some(ws) = weak.upgrade() {
                ws.off(sub);
            }
        });
    }
    let _ = cx;
    check();
}
