//! Locale packs on a page.
//!
//! The original ships one module per language and leaves the registration to the host
//! (`registerLocale(tr)`); a chart without its pack falls back to English. The port ships the
//! 20 packs inside the binary (`vela_core::locales`) and parses a pack only when a locale that
//! needs it is asked for (the `locale` option, `setLocale`): [`ensure`] registers it lazily on
//! the environment's registry, once. A host's own pack (`registerLocale({ locale, messages })`)
//! is registered AFTER the shipped one of the same language, so its entries win, and it reaches
//! every widget already on the page.
//!
//! ```js
//! registerLocale({ locale: 'tr', messages: { 'widget.sidePanel.close': 'Kapat' } });  // -> off()
//! builtinLocales();   // ['ar', 'de', 'en', ..., 'zh-Hant']
//! ```

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::{Arc, Weak};

use js_sys::{Function, Object, Reflect};
use vela_core::i18n::{LocalePack, LocaleRegistry, Registration, TextDirection};
use wasm_bindgen::prelude::*;

struct Tracked {
    registry: Weak<LocaleRegistry>,
    /// The shipped tags already registered on this registry.
    shipped: Vec<&'static str>,
    /// The host packs registered on it: (host pack id, registration).
    host: Vec<(u64, Registration)>,
}

thread_local! {
    static HOST: RefCell<Vec<(u64, LocalePack)>> = const { RefCell::new(Vec::new()) };
    static TRACKED: RefCell<Vec<Tracked>> = const { RefCell::new(Vec::new()) };
    static NEXT: Cell<u64> = const { Cell::new(1) };
}

/// The shipped tag that serves `tag`: an exact match, else the language (`pt` serves
/// `pt-BR`, `zh-TW` serves `zh-Hant`). `None` for a language without a shipped pack.
pub fn shipped_tag_for(tag: &str) -> Option<&'static str> {
    let lower = tag.trim().replace('_', "-").to_lowercase();
    if lower.is_empty() {
        return None;
    }
    let exact = vela_core::locales::TAGS
        .iter()
        .find(|t| t.to_lowercase() == lower);
    if let Some(t) = exact {
        return Some(t);
    }
    let primary = lower.split('-').next().unwrap_or_default();
    match primary {
        "zh" => {
            let traditional = ["hant", "tw", "hk", "mo"]
                .iter()
                .any(|m| lower.split('-').any(|part| part == *m));
            Some(if traditional { "zh-Hant" } else { "zh-Hans" })
        }
        "pt" => Some("pt-BR"),
        _ => vela_core::locales::TAGS
            .iter()
            .find(|t| t.to_lowercase() == primary)
            .copied(),
    }
}

fn with_tracked<R>(registry: &Arc<LocaleRegistry>, f: impl FnOnce(&mut Tracked) -> R) -> R {
    TRACKED.with(|t| {
        let mut all = t.borrow_mut();
        all.retain(|x| x.registry.strong_count() > 0);
        let at = match all
            .iter()
            .position(|x| std::ptr::eq(x.registry.as_ptr(), Arc::as_ptr(registry)))
        {
            Some(at) => at,
            None => {
                all.push(Tracked {
                    registry: Arc::downgrade(registry),
                    shipped: Vec::new(),
                    host: Vec::new(),
                });
                all.len() - 1
            }
        };
        f(&mut all[at])
    })
}

/// Track `registry`: the host packs registered so far (and later) reach it.
pub fn attach(registry: &Arc<LocaleRegistry>) {
    let packs: Vec<(u64, LocalePack)> = HOST.with(|h| h.borrow().clone());
    with_tracked(registry, |t| {
        for (id, pack) in &packs {
            if !t.host.iter().any(|(i, _)| i == id) {
                t.host.push((*id, registry.register(pack)));
            }
        }
    });
}

/// Register the shipped pack that serves `tag` on `registry`, once, parsing it now. The host
/// packs of that language are registered again after it so they keep priority. Returns the
/// shipped tag, `None` when the language has no shipped pack.
pub fn ensure(registry: &Arc<LocaleRegistry>, tag: &str) -> Option<&'static str> {
    let shipped = shipped_tag_for(tag)?;
    let packs: Vec<(u64, LocalePack)> = HOST.with(|h| h.borrow().clone());
    with_tracked(registry, |t| {
        if t.shipped.contains(&shipped) {
            return;
        }
        if let Some(pack) = vela_core::locales::pack(shipped) {
            // The registration lives as long as the registry.
            let _ = registry.register(&pack);
            t.shipped.push(shipped);
        }
        // Host packs of this language go back on top of it.
        for (id, pack) in packs
            .iter()
            .filter(|(_, p)| shipped_tag_for(&p.locale) == Some(shipped))
        {
            if let Some(at) = t.host.iter().position(|(i, _)| i == id) {
                t.host.remove(at).1.dispose();
            }
            t.host.push((*id, registry.register(pack)));
        }
    });
    Some(shipped)
}

fn prop(o: &JsValue, key: &str) -> JsValue {
    Reflect::get(o, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

/// Register a host locale pack on the page: `{ locale, messages, direction? }`. Returns the
/// function that removes it again.
#[wasm_bindgen(js_name = registerLocale)]
pub fn register_locale(pack: &JsValue) -> Result<Function, JsValue> {
    let locale = prop(pack, "locale")
        .as_string()
        .filter(|l| !l.trim().is_empty())
        .ok_or_else(|| {
            JsError::new("registerLocale expects { locale: string, messages: object }")
        })?;
    let messages = prop(pack, "messages");
    if !messages.is_object() {
        return Err(
            JsError::new("registerLocale expects { locale: string, messages: object }").into(),
        );
    }
    let mut map: HashMap<String, String> = HashMap::new();
    for entry in Object::entries(messages.unchecked_ref()).iter() {
        let pair: js_sys::Array = entry.unchecked_into();
        if let (Some(k), Some(v)) = (pair.get(0).as_string(), pair.get(1).as_string()) {
            map.insert(k, v);
        }
    }
    let direction = match prop(pack, "direction").as_string().as_deref() {
        Some("rtl") => Some(TextDirection::Rtl),
        Some("ltr") => Some(TextDirection::Ltr),
        _ => None,
    };
    let pack = LocalePack {
        locale,
        direction,
        messages: Arc::new(map),
    };
    let id = NEXT.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    });
    // Later than the shipped pack of the language where one is already registered.
    HOST.with(|h| h.borrow_mut().push((id, pack.clone())));
    TRACKED.with(|t| {
        for tracked in t.borrow_mut().iter_mut() {
            let Some(registry) = tracked.registry.upgrade() else {
                continue;
            };
            tracked.host.push((id, registry.register(&pack)));
        }
    });
    let off = Closure::<dyn Fn()>::new(move || {
        HOST.with(|h| h.borrow_mut().retain(|(i, _)| *i != id));
        TRACKED.with(|t| {
            for tracked in t.borrow_mut().iter_mut() {
                while let Some(at) = tracked.host.iter().position(|(i, _)| *i == id) {
                    tracked.host.remove(at).1.dispose();
                }
            }
        });
    });
    Ok(off.into_js_value().unchecked_into())
}

/// The tags of the packs that ship with the library.
#[wasm_bindgen(js_name = builtinLocales)]
pub fn builtin_locales() -> Vec<String> {
    vela_core::locales::TAGS
        .iter()
        .map(|t| t.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_tags_resolve_by_language_and_script() {
        assert_eq!(shipped_tag_for("tr"), Some("tr"));
        assert_eq!(shipped_tag_for("tr-TR"), Some("tr"));
        assert_eq!(shipped_tag_for("TR"), Some("tr"));
        assert_eq!(shipped_tag_for("pt"), Some("pt-BR"));
        assert_eq!(shipped_tag_for("pt-PT"), Some("pt-BR"));
        assert_eq!(shipped_tag_for("zh"), Some("zh-Hans"));
        assert_eq!(shipped_tag_for("zh-CN"), Some("zh-Hans"));
        assert_eq!(shipped_tag_for("zh-TW"), Some("zh-Hant"));
        assert_eq!(shipped_tag_for("zh-Hant-HK"), Some("zh-Hant"));
        assert_eq!(shipped_tag_for("de_DE"), Some("de"));
        assert_eq!(shipped_tag_for("xx"), None);
        assert_eq!(shipped_tag_for(""), None);
    }

    #[test]
    fn every_shipped_tag_resolves_to_itself() {
        for tag in vela_core::locales::TAGS {
            assert_eq!(shipped_tag_for(tag), Some(*tag));
        }
    }
}
