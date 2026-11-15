//! Subscriber lists: the original's `Set<callback>` fields.
//!
//! Emission works on a snapshot of the list and never holds the list borrowed
//! while a callback runs, so a callback may subscribe or unsubscribe (itself
//! included). A callback that is already running (a re-entrant emit of the same
//! event from inside it) is skipped instead of panicking on a double borrow.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::runtime::Unsubscribe;

pub(crate) type Slot<F> = Rc<RefCell<Box<F>>>;

pub(crate) struct Subs<F: ?Sized> {
    next: Cell<u64>,
    items: RefCell<Vec<(u64, Slot<F>)>>,
}

impl<F: ?Sized + 'static> Subs<F> {
    pub(crate) fn new() -> Rc<Self> {
        Rc::new(Self {
            next: Cell::new(0),
            items: RefCell::new(Vec::new()),
        })
    }

    /// Register `cb`; the returned [`Unsubscribe`] removes it.
    pub(crate) fn add(self: &Rc<Self>, cb: Box<F>) -> Unsubscribe {
        let id = self.next.get();
        self.next.set(id + 1);
        self.items
            .borrow_mut()
            .push((id, Rc::new(RefCell::new(cb))));
        let weak = Rc::downgrade(self);
        Box::new(move || {
            if let Some(subs) = weak.upgrade() {
                subs.items.borrow_mut().retain(|(i, _)| *i != id);
            }
        })
    }

    /// The callbacks to run for one emit, in subscription order.
    pub(crate) fn snapshot(&self) -> Vec<Slot<F>> {
        self.items.borrow().iter().map(|(_, s)| s.clone()).collect()
    }
}
