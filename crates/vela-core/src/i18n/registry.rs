//! The locale-pack registry: packs registered by the host (core packs, plugin packs, the
//! host's own), merged per fallback chain on first use and cached until the next change.
//!
//! The original keeps one registry per module; here it is an object the host creates and
//! hands to its translators (`Arc<LocaleRegistry>`), so there is no global state.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use crate::intl::Intl;

use super::locale::canonical_locale;
use super::types::LocalePack;

struct Entry {
    id: u64,
    tag: String,
    messages: Arc<HashMap<String, String>>,
}

#[derive(Default)]
struct Inner {
    entries: Vec<Entry>,
    next_id: u64,
    version: u64,
    merged: HashMap<String, (u64, Arc<HashMap<String, String>>)>,
}

/// The registered locale packs.
pub struct LocaleRegistry {
    intl: Arc<dyn Intl>,
    inner: Mutex<Inner>,
}

/// The handle `register` returns: removes its pack again.
pub struct Registration {
    registry: Weak<LocaleRegistry>,
    id: u64,
}

impl Registration {
    /// Remove the pack this registration added. A second call (or a call after the
    /// registry is gone) is harmless.
    pub fn dispose(&self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.remove(self.id);
        }
    }
}

impl LocaleRegistry {
    /// An empty registry; `intl` canonicalizes the tags of the packs registered.
    pub fn new(intl: Arc<dyn Intl>) -> Arc<Self> {
        Arc::new(Self {
            intl,
            inner: Mutex::new(Inner::default()),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Register a locale pack: a locale tag and its messages (id to ICU message). Several
    /// packs may share a tag (the core's, a plugin's, the host's); for the same id the pack
    /// registered last wins. Registration is a call, never an import side effect. The
    /// returned handle removes this pack again.
    pub fn register(self: &Arc<Self>, pack: &LocalePack) -> Registration {
        let tag = canonical_locale(self.intl.as_ref(), Some(&pack.locale));
        let mut inner = self.lock();
        let id = inner.next_id;
        inner.next_id += 1;
        inner.entries.push(Entry {
            id,
            tag,
            messages: Arc::clone(&pack.messages),
        });
        inner.version += 1;
        Registration {
            registry: Arc::downgrade(self),
            id,
        }
    }

    fn remove(&self, id: u64) {
        let mut inner = self.lock();
        if let Some(at) = inner.entries.iter().position(|e| e.id == id) {
            inner.entries.remove(at);
            inner.version += 1;
        }
    }

    /// The tags of every registered pack, in registration order (duplicates collapsed).
    pub fn registered_locales(&self) -> Vec<String> {
        let inner = self.lock();
        let mut out: Vec<String> = Vec::new();
        for e in &inner.entries {
            if !out.contains(&e.tag) {
                out.push(e.tag.clone());
            }
        }
        out
    }

    /// The messages a fallback chain resolves to: per id, the most specific tag that has it.
    pub fn messages_for(&self, chain: &[String]) -> Arc<HashMap<String, String>> {
        let key = chain.join("|");
        let mut inner = self.lock();
        if let Some((version, map)) = inner.merged.get(&key)
            && *version == inner.version
        {
            return Arc::clone(map);
        }
        let mut map: HashMap<String, String> = HashMap::new();
        // Least specific first, so more specific tags (and later packs) overwrite.
        for tag in chain.iter().rev() {
            for e in inner.entries.iter().filter(|e| e.tag == *tag) {
                for (id, text) in e.messages.iter() {
                    map.insert(id.clone(), text.clone());
                }
            }
        }
        let map = Arc::new(map);
        let version = inner.version;
        inner.merged.insert(key, (version, Arc::clone(&map)));
        map
    }
}
