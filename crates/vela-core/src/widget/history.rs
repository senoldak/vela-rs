//! Unified undo/redo timeline: app-level ops (indicator add/remove) and core drawing
//! edits share ONE stack, so Ctrl+Z / Ctrl+Y step through both in the order they happened.
//! Drawing entries delegate to the core's own drawing history.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use crate::events::Subscription;
use crate::events::types::ChartEventName;
use crate::runtime::Unsubscribe;
use crate::vela::{Vela, WeakVela};

/// A reversible action.
#[derive(Clone)]
pub struct HistoryAction {
    undo: Rc<dyn Fn()>,
    redo: Rc<dyn Fn()>,
}

impl HistoryAction {
    pub fn new(undo: impl Fn() + 'static, redo: impl Fn() + 'static) -> Self {
        Self {
            undo: Rc::new(undo),
            redo: Rc::new(redo),
        }
    }
}

type ChartResolver = Rc<dyn Fn() -> Option<Vela>>;
type Listener = (u64, Rc<dyn Fn()>);

struct Inner {
    undo_stack: RefCell<Vec<HistoryAction>>,
    redo_stack: RefCell<Vec<HistoryAction>>,
    listeners: RefCell<Vec<Listener>>,
    next_listener: Cell<u64>,
    subscriptions: RefCell<Vec<(WeakVela, Subscription<ChartEventName>)>>,
    muted: Cell<bool>,
    /// Late-resolves the CURRENT chart: drawing steps recorded before a chart rebuild must
    /// undo on the chart that exists when the user presses Ctrl+Z, not on a destroyed
    /// instance captured at record time.
    get_chart: ChartResolver,
}

impl Inner {
    fn push(&self, action: HistoryAction) {
        if self.muted.get() {
            return;
        }
        self.undo_stack.borrow_mut().push(action);
        self.redo_stack.borrow_mut().clear();
        self.notify();
    }

    /// Replaying an action must not re-record the drawing events it triggers.
    fn muted_run(&self, f: impl FnOnce()) {
        struct Unmute<'a>(&'a Cell<bool>);
        impl Drop for Unmute<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        self.muted.set(true);
        let _guard = Unmute(&self.muted);
        f();
    }

    fn notify(&self) {
        let snapshot: Vec<Rc<dyn Fn()>> = self
            .listeners
            .borrow()
            .iter()
            .map(|(_, cb)| Rc::clone(cb))
            .collect();
        for cb in snapshot {
            cb();
        }
    }

    fn push_drawing_step(self: &Rc<Self>) {
        let (undo_chart, redo_chart) = (Rc::clone(&self.get_chart), Rc::clone(&self.get_chart));
        self.push(HistoryAction::new(
            move || {
                if let Some(chart) = undo_chart() {
                    chart.drawings().undo();
                }
            },
            move || {
                if let Some(chart) = redo_chart() {
                    chart.drawings().redo();
                }
            },
        ));
    }

    fn drop_subscriptions(&self) {
        let subs = std::mem::take(&mut *self.subscriptions.borrow_mut());
        for (chart, sub) in subs {
            if let Some(chart) = chart.upgrade() {
                chart.off(&sub);
            }
        }
    }
}

pub struct WidgetHistory {
    inner: Rc<Inner>,
}

impl WidgetHistory {
    /// `get_chart` late-resolves the CURRENT chart (see [`Inner::get_chart`]).
    pub fn new(get_chart: impl Fn() -> Option<Vela> + 'static) -> Self {
        Self {
            inner: Rc::new(Inner {
                undo_stack: RefCell::new(Vec::new()),
                redo_stack: RefCell::new(Vec::new()),
                listeners: RefCell::new(Vec::new()),
                next_listener: Cell::new(0),
                subscriptions: RefCell::new(Vec::new()),
                muted: Cell::new(false),
                get_chart: Rc::new(get_chart),
            }),
        }
    }

    /// A history with no chart to delegate drawing steps to.
    pub fn detached() -> Self {
        Self::new(|| None)
    }

    /// Record a reversible action (a fresh edit forks history: the redo branch clears).
    pub fn push(&self, action: HistoryAction) {
        self.inner.push(action);
    }

    pub fn undo(&self) {
        let action = self.inner.undo_stack.borrow_mut().pop();
        let Some(a) = action else {
            return;
        };
        self.inner.muted_run(|| (a.undo)());
        self.inner.redo_stack.borrow_mut().push(a);
        self.inner.notify();
    }

    pub fn redo(&self) {
        let action = self.inner.redo_stack.borrow_mut().pop();
        let Some(a) = action else {
            return;
        };
        self.inner.muted_run(|| (a.redo)());
        self.inner.undo_stack.borrow_mut().push(a);
        self.inner.notify();
    }

    /// Run `f` without recording: for programmatic state application (setState, ledger
    /// restore), whose indicator/drawing events are not user edits.
    pub fn silently(&self, f: impl FnOnce()) {
        self.inner.muted_run(f);
    }

    pub fn can_undo(&self) -> bool {
        !self.inner.undo_stack.borrow().is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.inner.redo_stack.borrow().is_empty()
    }

    pub fn on_change(&self, cb: impl Fn() + 'static) -> Unsubscribe {
        let id = self.inner.next_listener.get();
        self.inner.next_listener.set(id + 1);
        self.inner.listeners.borrow_mut().push((id, Rc::new(cb)));
        let inner: Weak<Inner> = Rc::downgrade(&self.inner);
        Box::new(move || {
            if let Some(inner) = inner.upgrade() {
                inner.listeners.borrow_mut().retain(|(i, _)| *i != id);
            }
        })
    }

    /// (Re)bind to a chart: drawing edits enter the unified stack as delegate steps.
    pub fn on_chart(&self, chart: &Vela) {
        self.inner.drop_subscriptions();
        let mut subs = Vec::new();
        for event in [
            ChartEventName::DrawingCreated,
            ChartEventName::DrawingEdited,
            ChartEventName::DrawingRemoved,
        ] {
            let weak = Rc::downgrade(&self.inner);
            let sub = chart.on(event, move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.push_drawing_step();
                }
            });
            subs.push((chart.downgrade(), sub));
        }
        *self.inner.subscriptions.borrow_mut() = subs;
    }

    pub fn destroy(&self) {
        self.inner.drop_subscriptions();
        self.inner.listeners.borrow_mut().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Log = Rc<RefCell<Vec<&'static str>>>;

    fn recorder() -> (Log, impl Fn(&'static str, &'static str) -> HistoryAction) {
        let log = Rc::new(RefCell::new(Vec::new()));
        let log2 = Rc::clone(&log);
        let make = move |u: &'static str, r: &'static str| {
            let (lu, lr) = (Rc::clone(&log2), Rc::clone(&log2));
            HistoryAction::new(
                move || lu.borrow_mut().push(u),
                move || lr.borrow_mut().push(r),
            )
        };
        (log, make)
    }

    #[test]
    fn undo_and_redo_step_through_the_actions_in_order() {
        let h = WidgetHistory::detached();
        let (log, make) = recorder();
        h.push(make("undo-a", "redo-a"));
        h.push(make("undo-b", "redo-b"));
        assert!(h.can_undo() && !h.can_redo());
        h.undo();
        h.undo();
        h.undo(); // nothing left: a no-op
        assert_eq!(*log.borrow(), ["undo-b", "undo-a"]);
        assert!(!h.can_undo() && h.can_redo());
        h.redo();
        h.redo();
        h.redo();
        assert_eq!(log.borrow()[2..], ["redo-a", "redo-b"]);
    }

    #[test]
    fn a_fresh_edit_forks_history_and_clears_the_redo_branch() {
        let h = WidgetHistory::detached();
        let (_, make) = recorder();
        h.push(make("u1", "r1"));
        h.undo();
        assert!(h.can_redo());
        h.push(make("u2", "r2"));
        assert!(!h.can_redo());
    }

    #[test]
    fn silently_and_replays_do_not_record() {
        let h = Rc::new(WidgetHistory::detached());
        let inner = Rc::clone(&h);
        let (_, make) = recorder();
        h.push(HistoryAction::new(
            {
                let inner = Rc::clone(&inner);
                let (_, again) = recorder();
                move || inner.push(again("x", "y")) // an action recording while it replays
            },
            || {},
        ));
        h.undo();
        assert!(!h.can_undo(), "the replay must not record");
        assert!(h.can_redo());
        h.silently(|| h.push(make("a", "b")));
        assert!(!h.can_undo());
        h.push(make("c", "d"));
        assert!(h.can_undo(), "recording resumes after a silent run");
    }

    #[test]
    fn a_panicking_replay_still_unmutes() {
        let h = WidgetHistory::detached();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            h.silently(|| panic!("boom"));
        }));
        assert!(r.is_err());
        let (_, make) = recorder();
        h.push(make("u", "r"));
        assert!(h.can_undo());
    }

    #[test]
    fn listeners_hear_every_change_until_they_unsubscribe() {
        let h = WidgetHistory::detached();
        let (_, make) = recorder();
        let n = Rc::new(Cell::new(0));
        let n2 = Rc::clone(&n);
        let off = h.on_change(move || n2.set(n2.get() + 1));
        h.push(make("u", "r"));
        h.undo();
        h.redo();
        assert_eq!(n.get(), 3);
        off();
        h.undo();
        assert_eq!(n.get(), 3);
        h.destroy();
    }
}
