//! The locale packs that ship with the crate: every core message in 20 languages.
//!
//! Each function returns a [`LocalePack`] parsed from the catalog embedded in the
//! binary. Nothing is parsed until a function is called, so an application that never
//! asks for a pack pays nothing at startup. Calling one registers nothing: pass the pack
//! to [`LocaleRegistry::register`](crate::i18n::LocaleRegistry::register), then set the
//! locale.

use std::collections::HashMap;
use std::sync::Arc;

use crate::i18n::{LocalePack, TextDirection};

fn load(locale: &str, direction: Option<TextDirection>, json: &str) -> LocalePack {
    // The catalogs are checked by this crate's tests; a malformed one would read as an
    // empty pack, which falls back to English.
    let messages: HashMap<String, String> = serde_json::from_str(json).unwrap_or_default();
    LocalePack {
        locale: locale.to_string(),
        direction,
        messages: Arc::new(messages),
    }
}

macro_rules! packs {
    ($($(#[$meta:meta])* $name:ident, $tag:literal, $direction:expr;)*) => {
        $(
            $(#[$meta])*
            pub fn $name() -> LocalePack {
                load($tag, $direction, include_str!(concat!("../locales/", $tag, ".json")))
            }
        )*

        /// The tags of the packs that ship with the crate.
        pub const TAGS: &[&str] = &[$($tag),*];

        /// The shipped pack for `tag` (as listed in [`TAGS`]), parsed now.
        pub fn pack(tag: &str) -> Option<LocalePack> {
            match tag {
                $($tag => Some($name()),)*
                _ => None,
            }
        }
    };
}

packs! {
    /// Arabic (reads right to left).
    ar, "ar", Some(TextDirection::Rtl);
    /// German.
    de, "de", None;
    /// English: the source catalog itself.
    en, "en", None;
    /// Spanish.
    es, "es", None;
    /// French.
    fr, "fr", None;
    /// Hebrew (reads right to left).
    he, "he", Some(TextDirection::Rtl);
    /// Hindi.
    hi, "hi", None;
    /// Indonesian.
    id, "id", None;
    /// Italian.
    it, "it", None;
    /// Japanese.
    ja, "ja", None;
    /// Korean.
    ko, "ko", None;
    /// Malay.
    ms, "ms", None;
    /// Polish.
    pl, "pl", None;
    /// Brazilian Portuguese.
    pt_br, "pt-BR", None;
    /// Russian.
    ru, "ru", None;
    /// Thai.
    th, "th", None;
    /// Turkish.
    tr, "tr", None;
    /// Vietnamese.
    vi, "vi", None;
    /// Simplified Chinese.
    zh_hans, "zh-Hans", None;
    /// Traditional Chinese.
    zh_hant, "zh-Hant", None;
}

/// Every shipped pack, parsed now (one parse per pack).
pub fn all() -> Vec<LocalePack> {
    TAGS.iter().filter_map(|tag| pack(tag)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_parses_and_carries_messages() {
        assert_eq!(TAGS.len(), 20);
        for tag in TAGS {
            let p = pack(tag).expect("listed");
            assert_eq!(p.locale, *tag);
            assert!(!p.messages.is_empty(), "{tag} parsed empty");
        }
        assert!(pack("xx").is_none());
    }

    #[test]
    fn only_arabic_and_hebrew_say_they_read_right_to_left() {
        let rtl: Vec<String> = all()
            .into_iter()
            .filter(|p| p.direction == Some(TextDirection::Rtl))
            .map(|p| p.locale)
            .collect();
        assert_eq!(rtl, ["ar", "he"]);
    }
}
