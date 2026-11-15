//! Locale tags: canonical form, the fallback chain message lookup walks, the writing
//! direction a locale implies, and the numbering system chart numbers use.

use crate::intl::{Intl, TextDirection};

/// The canonical form of a BCP 47 tag (`pt-br` becomes `pt-BR`, `iw` becomes `he`); the
/// tag itself when the engine rejects it, `"en"` when it is empty.
pub fn canonical_locale(intl: &dyn Intl, tag: Option<&str>) -> String {
    let raw = tag.map(str::trim).unwrap_or_default();
    if raw.is_empty() {
        return "en".to_string();
    }
    intl.canonicalize_locale(raw)
        .unwrap_or_else(|| raw.to_string())
}

/// The tag's language subtag (`pt-BR` becomes `pt`), lower-cased.
pub fn language_of(intl: &dyn Intl, tag: &str) -> String {
    match intl.parse_locale(tag) {
        Some(loc) => loc.language.to_lowercase(),
        None => tag.split(['-', '_']).next().unwrap_or(tag).to_lowercase(),
    }
}

/// The tags message lookup tries, most specific first, ending in English: `pt-BR` gives
/// `pt-BR`, `pt-Latn`, `pt`, `en`; `zh-TW` gives `zh-TW`, `zh-Hant`, `zh`, `en`; `es-MX`
/// gives `es-MX`, `es`, `en`. A region that implies a script slots `language-Script` in
/// before the bare language, so a `zh-Hant` pack serves Taiwan and Hong Kong. After the
/// bare language comes the region it stands for (its likely subtags: `pt` is Brazil's,
/// `zh` is China's), when that region writes the same script, so a `pt-BR` pack serves
/// `pt` and `pt-PT` before English.
pub fn fallback_chain(intl: &dyn Intl, tag: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |t: &str| {
        if !t.is_empty() && !out.iter().any(|o| o == t) {
            out.push(t.to_string());
        }
    };
    add(tag);
    if let Some(loc) = intl.parse_locale(tag) {
        add(&loc.base_name);
        let parts: Vec<&str> = loc.base_name.split('-').collect();
        let implied_script: Option<String> = if loc.script.is_none() {
            intl.maximize_locale(tag).and_then(|m| m.script)
        } else {
            None
        };
        for i in (1..parts.len()).rev() {
            if i == 1
                && let (Some(script), Some(_)) = (&implied_script, &loc.region)
            {
                add(&format!("{}-{script}", loc.language));
            }
            add(&parts[..i].join("-"));
        }
        if let (Some(script), None) = (&implied_script, &loc.region) {
            add(&format!("{}-{script}", loc.language));
        }
        if let Some(usual) = intl.maximize_locale(&loc.language)
            && let Some(region) = &usual.region
            && usual.script == loc.script.clone().or(implied_script)
        {
            add(&format!("{}-{region}", loc.language));
        }
    }
    add("en");
    out
}

const RTL_SCRIPTS: [&str; 10] = [
    "Arab", "Hebr", "Thaa", "Syrc", "Nkoo", "Adlm", "Rohg", "Mand", "Samr", "Yezi",
];
const RTL_LANGUAGES: [&str; 12] = [
    "ar", "he", "iw", "fa", "ur", "ps", "sd", "ug", "yi", "dv", "ckb", "ks",
];

/// The writing direction of a locale's script: right to left for Arabic, Hebrew,
/// Persian, Urdu and the other right-to-left scripts, left to right otherwise.
pub fn locale_direction(intl: &dyn Intl, tag: &str) -> TextDirection {
    if let Some(loc) = intl.parse_locale(tag) {
        if let Some(direction) = intl.locale_text_direction(tag) {
            return direction;
        }
        let script = loc
            .script
            .clone()
            .or_else(|| intl.maximize_locale(tag).and_then(|m| m.script));
        if let Some(script) = script {
            return if RTL_SCRIPTS.contains(&script.as_str()) {
                TextDirection::Rtl
            } else {
                TextDirection::Ltr
            };
        }
    }
    if RTL_LANGUAGES.contains(&language_of(intl, tag).as_str()) {
        TextDirection::Rtl
    } else {
        TextDirection::Ltr
    }
}

/// The numbering system chart numbers use. An explicit choice wins (`latn`, `arab`, ...;
/// `locale` is the locale's own default); without one, a `-u-nu-` extension in the tag;
/// otherwise Western digits, `latn`, in every language. An id the engine does not know
/// falls back to `latn`.
pub fn resolve_numbering_system(intl: &dyn Intl, tag: &str, requested: Option<&str>) -> String {
    let resolved = match requested.filter(|s| !s.is_empty()) {
        Some("locale") => intl.resolved_numbering_system(tag, None),
        Some(system) => intl.resolved_numbering_system(tag, Some(system)),
        None => match intl.parse_locale(tag).and_then(|l| l.numbering_system) {
            Some(_) => intl.resolved_numbering_system(tag, None),
            None => Some("latn".to_string()),
        },
    };
    resolved.unwrap_or_else(|| "latn".to_string())
}
