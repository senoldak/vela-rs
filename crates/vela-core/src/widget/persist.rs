//! Shell state persistence: pluggable storage. Hosts inject any [`VelaStorage`] (sync like
//! `localStorage`, or async like a REST / IndexedDB backend). The persisted FORMAT is the
//! unified state document (`crate::state`), one codec for every shell.
//!
//! The original's default adapter, `localStorageAdapter`, reads `window.localStorage` and
//! swallows quota and privacy failures: that is the browser crate's (it implements
//! [`VelaStorage`] on `localStorage`). What is pure and kept here: the contract, the
//! pinned-key wrapper that `localStorageAdapter(storageKey)` is built on
//! ([`PinnedStorage`]) and an in-memory adapter ([`MemoryStorage`]) for session-lived state.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::runtime::LocalBoxFuture;

/// A value a storage hands back: now, or when its future resolves.
pub enum Stored<T> {
    Ready(T),
    Pending(LocalBoxFuture<'static, T>),
}

impl<T: 'static> Stored<T> {
    /// The value, awaiting it when it is pending.
    pub async fn resolve(self) -> T {
        match self {
            Stored::Ready(v) => v,
            Stored::Pending(f) => f.await,
        }
    }
}

/// The storage contract BOTH shells persist through. Methods answer at once or with a
/// future.
pub trait VelaStorage {
    fn get(&self, key: &str) -> Stored<Option<String>>;
    fn set(&self, key: &str, value: &str) -> Stored<()>;
    /// Optional in the original; a storage with nothing to remove keeps this no-op.
    fn remove(&self, _key: &str) -> Stored<()> {
        Stored::Ready(())
    }
}

/// Every read and write lands on one physical `storage_key`, whatever logical key the
/// shell passes: the way to choose where the state lives without touching the `persist`
/// option (one shell instance per adapter then; two shells sharing a pinned adapter would
/// overwrite each other).
pub struct PinnedStorage<S> {
    inner: S,
    storage_key: String,
}

impl<S: VelaStorage> PinnedStorage<S> {
    pub fn new(inner: S, storage_key: impl Into<String>) -> Self {
        Self {
            inner,
            storage_key: storage_key.into(),
        }
    }
}

impl<S: VelaStorage> VelaStorage for PinnedStorage<S> {
    fn get(&self, _key: &str) -> Stored<Option<String>> {
        self.inner.get(&self.storage_key)
    }
    fn set(&self, _key: &str, value: &str) -> Stored<()> {
        self.inner.set(&self.storage_key, value)
    }
    fn remove(&self, _key: &str) -> Stored<()> {
        self.inner.remove(&self.storage_key)
    }
}

/// A process-lived in-memory storage. Clones share the entries.
#[derive(Clone, Default)]
pub struct MemoryStorage {
    entries: Rc<RefCell<HashMap<String, String>>>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

impl VelaStorage for MemoryStorage {
    fn get(&self, key: &str) -> Stored<Option<String>> {
        Stored::Ready(self.entries.borrow().get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Stored<()> {
        self.entries
            .borrow_mut()
            .insert(key.to_string(), value.to_string());
        Stored::Ready(())
    }
    fn remove(&self, key: &str) -> Stored<()> {
        self.entries.borrow_mut().remove(key);
        Stored::Ready(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready<T>(s: Stored<T>) -> T {
        match s {
            Stored::Ready(v) => v,
            Stored::Pending(_) => panic!("expected an immediate answer"),
        }
    }

    #[test]
    fn memory_storage_round_trips_and_removes() {
        let s = MemoryStorage::new();
        assert_eq!(ready(s.get("k")), None);
        ready(s.set("k", "v"));
        assert_eq!(ready(s.get("k")), Some("v".to_string()));
        let clone = s.clone();
        ready(clone.remove("k"));
        assert_eq!(ready(s.get("k")), None);
    }

    #[test]
    fn a_pinned_storage_ignores_the_logical_key() {
        let mem = MemoryStorage::new();
        let pinned = PinnedStorage::new(mem.clone(), "physical");
        ready(pinned.set("vela-widget", "state"));
        assert_eq!(ready(mem.get("physical")), Some("state".to_string()));
        assert_eq!(ready(mem.get("vela-widget")), None);
        assert_eq!(ready(pinned.get("anything")), Some("state".to_string()));
        ready(pinned.remove("whatever"));
        assert_eq!(ready(mem.get("physical")), None);
    }

    #[test]
    fn a_pending_answer_resolves_through_the_future() {
        struct Slow;
        impl VelaStorage for Slow {
            fn get(&self, _key: &str) -> Stored<Option<String>> {
                Stored::Pending(Box::pin(std::future::ready(Some("late".to_string()))))
            }
            fn set(&self, _key: &str, _value: &str) -> Stored<()> {
                Stored::Ready(())
            }
        }
        let platform = crate::runtime::testing::ManualPlatform::new(0.0);
        let got = platform.block_on(Slow.get("k").resolve());
        assert_eq!(got, Some("late".to_string()));
    }
}
