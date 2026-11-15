//! `localStorage` as both storage contracts of the shells.
//!
//! The original's `localStorageAdapter(storageKey?)` (`widget/persist.ts`): reads and writes
//! `window.localStorage` and swallows quota and privacy failures. Here the one adapter
//! implements [`WorkspaceStorage`] (what the workspace persists through) and [`VelaStorage`]
//! (the widget shells' contract), both answering at once.
//!
//! `storage_key` pins the PHYSICAL entry: every read and write lands on that name whatever
//! logical key the shell passes (one shell per pinned adapter). Without it the shell's own
//! key is used as is, which is `vela-workspace` for a workspace with `persist: true`.

use vela_core::widget::persist::{Stored, VelaStorage};
use vela_core::workspace::{StorageRead, WorkspaceStorage};

use super::js;

/// `window.localStorage`, silent on failure. See the module documentation.
#[derive(Clone, Default)]
pub struct LocalStorageAdapter {
    storage_key: Option<String>,
}

impl LocalStorageAdapter {
    /// The adapter that uses the shell's own keys.
    pub fn new() -> Self {
        Self::default()
    }

    /// The adapter that pins every read and write to `storage_key`.
    pub fn pinned(storage_key: impl Into<String>) -> Self {
        Self {
            storage_key: Some(storage_key.into()),
        }
    }

    fn physical<'a>(&'a self, key: &'a str) -> &'a str {
        self.storage_key.as_deref().unwrap_or(key)
    }

    /// The stored text under `key`, `None` when absent or storage is unavailable.
    pub fn read(&self, key: &str) -> Option<String> {
        js::vela_ws_storage_get(self.physical(key))
    }

    /// Store `value` under `key` (best effort).
    pub fn write(&self, key: &str, value: &str) {
        js::vela_ws_storage_set(self.physical(key), value);
    }

    /// Remove `key` (best effort).
    pub fn erase(&self, key: &str) {
        js::vela_ws_storage_remove(self.physical(key));
    }
}

impl WorkspaceStorage for LocalStorageAdapter {
    fn get(&self, key: &str) -> StorageRead {
        StorageRead::Ready(self.read(key))
    }

    fn set(&self, key: &str, value: &str) {
        self.write(key, value);
    }

    fn remove(&self, key: &str) {
        self.erase(key);
    }
}

impl VelaStorage for LocalStorageAdapter {
    fn get(&self, key: &str) -> Stored<Option<String>> {
        Stored::Ready(self.read(key))
    }

    fn set(&self, key: &str, value: &str) -> Stored<()> {
        self.write(key, value);
        Stored::Ready(())
    }

    fn remove(&self, key: &str) -> Stored<()> {
        self.erase(key);
        Stored::Ready(())
    }
}
