//! A small typed publish/subscribe bus.
//!
//! The original is generic over an event map (`Record<string, unknown>`) whose
//! keys select a payload type. Rust has no such map, so the bus is generic over
//! an event key `K` and one payload type `P`; a caller with several payload
//! shapes makes `P` an enum (or `K` the event enum and `P` the payload enum).
//! [`types::ChartEventBus`] is the instance a chart owns.
//!
//! The bus is a shared handle: cloning it is cheap and every clone talks to the
//! same subscriptions, like the original object passed by reference between the
//! orchestrator, the controllers and user code. All methods take `&self`, and a
//! handler may subscribe, unsubscribe and emit from inside a handler.
//!
//! Subscribing returns a [`Subscription`] token; pass it to
//! [`TypedEventBus::off`] to unsubscribe. The original returns an unsubscribe
//! closure and also offers `off(event, handler)` by function identity; closures
//! have no identity in Rust, so the token plays both roles. Dropping a token
//! does not unsubscribe. Unsubscribing twice is a no-op. A handler that holds a
//! clone of its own bus forms a reference cycle until it is unsubscribed or the
//! bus is cleared; hold a `Weak` of whatever the handler needs to outlive.
//!
//! # Emit semantics (the original iterates a live `Set`)
//!
//! - Handlers of one event run in registration order.
//! - A handler unsubscribed during a round, before its turn, is not called in
//!   that round.
//! - A handler subscribed to the same event during a round IS called later in
//!   that same round (a `Set` visits entries added while iterating).
//! - [`TypedEventBus::clear`] during a round does not stop the round: the
//!   original's loop keeps the `Set` it started with, which `clear` only
//!   detaches from the map.
//! - Subscribing the same function twice registers it once in the original;
//!   closures have no identity here, so it registers twice.

pub mod types;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::Hash;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

/// Handle to one registered handler, returned by [`TypedEventBus::on`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subscription<K> {
    event: K,
    id: u64,
}

impl<K> Subscription<K> {
    /// The event this subscription listens to.
    pub fn event(&self) -> &K {
        &self.event
    }
}

type Handler<P> = Rc<dyn Fn(&P)>;

/// The handlers of one event, in registration (id) order. Kept in the map even
/// when empty: an emit in flight keeps iterating the bucket it started with.
struct Bucket<P> {
    entries: RefCell<Vec<(u64, Handler<P>)>>,
}

struct Shared<K, P> {
    buckets: RefCell<HashMap<K, Rc<Bucket<P>>>>,
    next_id: Cell<u64>,
}

/// Minimal typed pub/sub, shared by cloning. Handler panics are isolated: a
/// panicking handler does not stop the remaining handlers or unwind into the
/// emitter (with `panic = "abort"` there is nothing to isolate).
pub struct TypedEventBus<K, P> {
    shared: Rc<Shared<K, P>>,
}

impl<K, P> Clone for TypedEventBus<K, P> {
    fn clone(&self) -> Self {
        Self {
            shared: Rc::clone(&self.shared),
        }
    }
}

impl<K, P> Default for TypedEventBus<K, P> {
    fn default() -> Self {
        Self {
            shared: Rc::new(Shared {
                buckets: RefCell::new(HashMap::new()),
                next_id: Cell::new(0),
            }),
        }
    }
}

impl<K: Eq + Hash + Clone, P> TypedEventBus<K, P> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `handler` for `event`. Handlers run in registration order.
    pub fn on(&self, event: K, handler: impl Fn(&P) + 'static) -> Subscription<K> {
        let id = self.shared.next_id.get();
        self.shared.next_id.set(id + 1);
        let bucket = Rc::clone(
            self.shared
                .buckets
                .borrow_mut()
                .entry(event.clone())
                .or_insert_with(|| {
                    Rc::new(Bucket {
                        entries: RefCell::new(Vec::new()),
                    })
                }),
        );
        bucket.entries.borrow_mut().push((id, Rc::new(handler)));
        Subscription { event, id }
    }

    /// Unsubscribe the handler behind `subscription`. Returns whether it was
    /// still registered.
    pub fn off(&self, subscription: &Subscription<K>) -> bool {
        let Some(bucket) = self.bucket(&subscription.event) else {
            return false;
        };
        let mut entries = bucket.entries.borrow_mut();
        let before = entries.len();
        entries.retain(|(id, _)| *id != subscription.id);
        entries.len() != before
    }

    /// Whether anyone is listening: lets an emitter skip building an expensive
    /// payload.
    pub fn has_listeners(&self, event: &K) -> bool {
        self.bucket(event)
            .is_some_and(|bucket| !bucket.entries.borrow().is_empty())
    }

    /// Call every handler of `event` with `payload` (see the module doc for the
    /// round semantics). Returns how many handlers panicked (each panic is
    /// contained).
    pub fn emit(&self, event: &K, payload: &P) -> usize {
        let Some(bucket) = self.bucket(event) else {
            return 0;
        };
        let mut panicked = 0;
        let mut last: Option<u64> = None;
        loop {
            // The borrow ends before the handler runs, so it may re-enter the bus.
            let next = bucket
                .entries
                .borrow()
                .iter()
                .find(|(id, _)| last.is_none_or(|seen| *id > seen))
                .map(|(id, handler)| (*id, Rc::clone(handler)));
            let Some((id, handler)) = next else { break };
            last = Some(id);
            if catch_unwind(AssertUnwindSafe(|| handler(payload))).is_err() {
                panicked += 1;
            }
        }
        panicked
    }

    /// Drop every handler of every event.
    pub fn clear(&self) {
        let dropped = std::mem::take(&mut *self.shared.buckets.borrow_mut());
        drop(dropped);
    }

    fn bucket(&self, event: &K) -> Option<Rc<Bucket<P>>> {
        self.shared.buckets.borrow().get(event).cloned()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::*;

    type Log = Rc<RefCell<Vec<String>>>;

    fn recorder(log: &Log, tag: &'static str) -> impl Fn(&i32) + 'static {
        let log = Rc::clone(log);
        move |n| log.borrow_mut().push(format!("{tag}{n}"))
    }

    #[test]
    fn handlers_run_in_registration_order_for_their_event_only() {
        let log: Log = Rc::default();
        let bus = TypedEventBus::<&str, i32>::new();
        bus.on("a", recorder(&log, "first"));
        bus.on("a", recorder(&log, "second"));
        bus.on("b", recorder(&log, "other"));
        assert_eq!(bus.emit(&"a", &1), 0);
        assert_eq!(*log.borrow(), ["first1", "second1"]);
        assert_eq!(bus.emit(&"missing", &2), 0);
    }

    #[test]
    fn off_unsubscribes_one_handler_and_is_idempotent() {
        let log: Log = Rc::default();
        let bus = TypedEventBus::<&str, i32>::new();
        let first = bus.on("a", recorder(&log, "first"));
        bus.on("a", recorder(&log, "second"));
        assert!(bus.off(&first));
        assert!(!bus.off(&first));
        bus.emit(&"a", &7);
        assert_eq!(*log.borrow(), ["second7"]);
        assert_eq!(first.event(), &"a");
    }

    #[test]
    fn has_listeners_tracks_subscriptions() {
        let bus = TypedEventBus::<&str, i32>::new();
        assert!(!bus.has_listeners(&"a"));
        let sub = bus.on("a", |_| {});
        assert!(bus.has_listeners(&"a"));
        assert!(!bus.has_listeners(&"b"));
        bus.off(&sub);
        assert!(!bus.has_listeners(&"a"));
    }

    #[test]
    fn clear_drops_everything() {
        let log: Log = Rc::default();
        let bus = TypedEventBus::<&str, i32>::new();
        bus.on("a", recorder(&log, "x"));
        bus.on("b", recorder(&log, "y"));
        bus.clear();
        bus.emit(&"a", &1);
        bus.emit(&"b", &1);
        assert!(log.borrow().is_empty());
        assert!(!bus.has_listeners(&"a"));
    }

    #[test]
    fn a_panicking_handler_is_isolated_and_counted() {
        let log: Log = Rc::default();
        let bus = TypedEventBus::<&str, i32>::new();
        bus.on("a", recorder(&log, "before"));
        bus.on("a", |_| panic!("handler failure"));
        bus.on("a", recorder(&log, "after"));
        let silence = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let failed = bus.emit(&"a", &3);
        std::panic::set_hook(silence);
        assert_eq!(failed, 1);
        assert_eq!(*log.borrow(), ["before3", "after3"]);
    }

    #[test]
    fn payload_enums_carry_several_shapes_on_one_bus() {
        #[derive(Debug, PartialEq)]
        enum Payload {
            Count(u32),
            Name(String),
        }
        let seen: Rc<RefCell<Vec<Payload>>> = Rc::default();
        let bus = TypedEventBus::<&str, Payload>::new();
        let sink = Rc::clone(&seen);
        bus.on("count", move |p| {
            if let Payload::Count(n) = p {
                sink.borrow_mut().push(Payload::Count(*n));
            }
        });
        let sink = Rc::clone(&seen);
        bus.on("name", move |p| {
            if let Payload::Name(s) = p {
                sink.borrow_mut().push(Payload::Name(s.clone()));
            }
        });
        bus.emit(&"count", &Payload::Count(2));
        bus.emit(&"name", &Payload::Name("x".into()));
        assert_eq!(
            *seen.borrow(),
            [Payload::Count(2), Payload::Name("x".into())]
        );
    }

    #[test]
    fn clones_share_one_set_of_subscriptions() {
        let log: Log = Rc::default();
        let bus = TypedEventBus::<&str, i32>::new();
        let other = bus.clone();
        let sub = other.on("a", recorder(&log, "x"));
        bus.emit(&"a", &1);
        assert!(bus.off(&sub));
        other.emit(&"a", &2);
        assert_eq!(*log.borrow(), ["x1"]);
    }

    #[test]
    fn a_handler_removed_during_a_round_is_not_called_afterwards() {
        let log: Log = Rc::default();
        let bus = TypedEventBus::<&str, i32>::new();
        let victim: Rc<RefCell<Option<Subscription<&'static str>>>> = Rc::default();
        {
            let bus2 = bus.clone();
            let victim = Rc::clone(&victim);
            bus.on("a", move |_| {
                if let Some(sub) = victim.borrow_mut().take() {
                    bus2.off(&sub);
                }
            });
        }
        *victim.borrow_mut() = Some(bus.on("a", recorder(&log, "victim")));
        bus.on("a", recorder(&log, "last"));
        bus.emit(&"a", &1);
        assert_eq!(*log.borrow(), ["last1"]);
    }

    #[test]
    fn a_handler_added_during_a_round_runs_in_that_round() {
        let log: Log = Rc::default();
        let bus = TypedEventBus::<&str, i32>::new();
        let added = Rc::new(Cell::new(false));
        {
            let bus2 = bus.clone();
            let log = Rc::clone(&log);
            let added = Rc::clone(&added);
            bus.on("a", move |_| {
                if !added.replace(true) {
                    bus2.on("a", recorder(&log, "late"));
                }
            });
        }
        bus.emit(&"a", &1);
        assert_eq!(*log.borrow(), ["late1"]);
        bus.emit(&"a", &2);
        assert_eq!(*log.borrow(), ["late1", "late2"]);
    }

    #[test]
    fn a_handler_may_emit_another_event_and_reenter_the_same_one() {
        let log: Log = Rc::default();
        let bus = TypedEventBus::<&str, i32>::new();
        bus.on("b", recorder(&log, "b"));
        {
            let bus2 = bus.clone();
            bus.on("a", move |n| {
                if *n < 2 {
                    bus2.emit(&"a", &(n + 1));
                }
                bus2.emit(&"b", n);
            });
        }
        bus.emit(&"a", &0);
        assert_eq!(*log.borrow(), ["b2", "b1", "b0"]);
    }

    #[test]
    fn clear_during_a_round_does_not_stop_it() {
        let log: Log = Rc::default();
        let bus = TypedEventBus::<&str, i32>::new();
        {
            let bus2 = bus.clone();
            bus.on("a", move |_| bus2.clear());
        }
        bus.on("a", recorder(&log, "after"));
        bus.emit(&"a", &1);
        assert_eq!(*log.borrow(), ["after1"]);
        assert!(!bus.has_listeners(&"a"));
    }
}
