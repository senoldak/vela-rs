//! Workspace persistence: the storage seam and the default in-memory adapter
//! (`persist.ts`).
//!
//! The state DOCUMENT itself (types and the validating codec) lives in
//! [`crate::state`], shared verbatim with the widget: one format for both shells, the
//! widget being the single-cell case. Re-exported here so the workspace keeps its public
//! surface.
//!
//! # The storage trait
//!
//! The original shares one `VelaStorage` contract between both shells in
//! `widget/persist.ts` (`get` / `set` / `remove`, each synchronous or promise-returning).
//! That file belongs to the widget port; [`WorkspaceStorage`] is the workspace's own
//! small trait with the same shape, so the two can be unified later: a host adapter for
//! the widget's trait forwards `get` / `set` / `remove` one to one. A synchronous answer
//! is [`StorageRead::Ready`]; a store that answers later returns
//! [`StorageRead::Pending`], and the workspace applies the document when it lands.
//! Writes are fire and forget: an async backend spawns its own work (through the
//! platform) inside `set`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::runtime::LocalBoxFuture;

pub use crate::state::{
    CellState, ChartState, DrawingToolsState, PanelsState, WorkspaceState, decode_state,
    encode_state, sanitize_state,
};

/// The answer of [`WorkspaceStorage::get`].
pub enum StorageRead {
    /// The stored text, or `None` when the key is absent.
    Ready(Option<String>),
    /// An asynchronous store: the text when it lands.
    Pending(LocalBoxFuture<'static, Option<String>>),
}

/// Where the workspace persists its state document (see the module documentation).
pub trait WorkspaceStorage {
    fn get(&self, key: &str) -> StorageRead;
    /// Store `value` under `key`. Best effort: a failing backend must not panic.
    fn set(&self, key: &str, value: &str);
    fn remove(&self, key: &str);
}

/// An in-memory, session-lived store: the OPT-IN alternative to a durable default when
/// persisted state must not outlive the session. Clones share one map, so a destroyed and
/// re-created workspace given a clone restores (the original's module-level store behind
/// `memoryStorageAdapter`; here the host decides how long the map lives by keeping a
/// clone).
#[derive(Clone, Default)]
pub struct MemoryStorage {
    map: Rc<RefCell<HashMap<String, String>>>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

/// A fresh in-memory adapter (`memoryStorageAdapter()`).
pub fn memory_storage_adapter() -> MemoryStorage {
    MemoryStorage::new()
}

impl WorkspaceStorage for MemoryStorage {
    fn get(&self, key: &str) -> StorageRead {
        StorageRead::Ready(self.map.borrow().get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) {
        self.map
            .borrow_mut()
            .insert(key.to_string(), value.to_string());
    }

    fn remove(&self, key: &str) {
        self.map.borrow_mut().remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_adapter_round_trips_and_clones_share() {
        let a = memory_storage_adapter();
        let b = a.clone();
        assert!(matches!(a.get("k"), StorageRead::Ready(None)));
        a.set("k", "v");
        assert!(matches!(b.get("k"), StorageRead::Ready(Some(v)) if v == "v"));
        b.remove("k");
        assert!(matches!(a.get("k"), StorageRead::Ready(None)));
        a.remove("absent");
    }
}
