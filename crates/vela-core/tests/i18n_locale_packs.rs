//! The language packs Vela ships (`locales::<tag>()`) and the right-to-left languages
//! among them. Ported from `locale-packs.test.ts` and `locale-packs-right-to-left.test.ts`
//! without their workspace and chart cases (a pack's plural forms against the engine's
//! categories need a real `Intl`).

mod i18n_common;

use std::collections::{BTreeSet, HashMap};

use i18n_common::{english, locale, registry, translator};
use vela_core::i18n::icu::{Node, parse_message};
use vela_core::i18n::{
    DirectionOption, LocalePack, LocaleState, LocalizationOptions, TextDirection, Translator,
    canonical_locale, locale_direction, msg,
};
use vela_core::locales;

fn english_catalog() -> HashMap<String, String> {
    serde_json::from_str(include_str!("../locales/en.json")).unwrap_or_default()
}

fn packs() -> Vec<LocalePack> {
    locales::all()
        .into_iter()
        .filter(|p| p.locale != "en")
        .collect()
}

/// The values a message takes, each with its kind: `count:plural`, `price:number/integer`.
fn argument_shape(message: &str) -> BTreeSet<String> {
    fn walk(nodes: &[Node], out: &mut BTreeSet<String>) {
        for node in nodes {
            match node {
                Node::Text(_) | Node::Pound => {}
                Node::Arg {
                    name,
                    number,
                    style,
                } => {
                    let kind = if *number { "number" } else { "text" };
                    let style = style.as_ref().map(|s| format!("/{s}")).unwrap_or_default();
                    out.insert(format!("{name}:{kind}{style}"));
                }
                Node::Select { name, options } => {
                    out.insert(format!("{name}:select"));
                    options.values().for_each(|b| walk(b, out));
                }
                Node::Plural {
                    name,
                    ordinal,
                    options,
                    ..
                } => {
                    out.insert(format!(
                        "{name}:{}",
                        if *ordinal { "selectordinal" } else { "plural" }
                    ));
                    options.values().for_each(|b| walk(b, out));
                }
            }
        }
    }
    let mut out = BTreeSet::new();
    if let Ok(nodes) = parse_message(message) {
        walk(&nodes, &mut out);
    }
    out
}

/// Characters of a Unicode decimal-digit block other than ASCII `0` to `9`.
fn is_non_ascii_decimal_digit(c: char) -> bool {
    matches!(
        c as u32,
        0x0660..=0x0669
            | 0x06F0..=0x06F9
            | 0x07C0..=0x07C9
            | 0x0966..=0x096F
            | 0x09E6..=0x09EF
            | 0x0A66..=0x0A6F
            | 0x0AE6..=0x0AEF
            | 0x0B66..=0x0B6F
            | 0x0BE6..=0x0BEF
            | 0x0C66..=0x0C6F
            | 0x0CE6..=0x0CEF
            | 0x0D66..=0x0D6F
            | 0x0E50..=0x0E59
            | 0x0ED0..=0x0ED9
            | 0x0F20..=0x0F29
            | 0x1040..=0x1049
            | 0x17E0..=0x17E9
            | 0x1810..=0x1819
            | 0xFF10..=0xFF19
    )
}

const PRODUCT_NAMES: [&str; 5] = ["LuxAlgo", "Vela", "Quant", "PineTS", "Pine Script®"];

#[test]
fn twenty_packs_ship_and_each_is_published_under_its_own_canonical_tag() {
    assert_eq!(locales::TAGS.len(), 20);
    let intl = english();
    for pack in locales::all() {
        assert_eq!(
            canonical_locale(intl.as_ref(), Some(&pack.locale)),
            pack.locale
        );
        let expected = if pack.direction == Some(TextDirection::Rtl) {
            TextDirection::Rtl
        } else {
            TextDirection::Ltr
        };
        assert_eq!(
            locale_direction(intl.as_ref(), &pack.locale),
            expected,
            "{}",
            pack.locale
        );
    }
}

#[test]
fn a_pack_names_only_messages_english_has() {
    let english = english_catalog();
    for pack in packs() {
        let unknown: Vec<&String> = pack
            .messages
            .keys()
            .filter(|id| !english.contains_key(*id))
            .collect();
        assert!(unknown.is_empty(), "{}: {unknown:?}", pack.locale);
    }
}

#[test]
fn a_pack_reads_every_message_with_the_charts_parser_taking_the_same_values_as_english() {
    let english = english_catalog();
    let mut problems: Vec<String> = Vec::new();
    for pack in packs() {
        for (id, text) in pack.messages.iter() {
            let Some(source) = english.get(id) else {
                continue;
            };
            if let Err(e) = parse_message(text) {
                problems.push(format!("{} {id}: {e}", pack.locale));
                continue;
            }
            let (ours, theirs) = (argument_shape(text), argument_shape(source));
            if ours != theirs {
                problems.push(format!(
                    "{} {id}: takes {ours:?}, English takes {theirs:?}",
                    pack.locale
                ));
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn a_pack_keeps_the_product_names_as_written() {
    let english = english_catalog();
    let mut problems: Vec<String> = Vec::new();
    for pack in packs() {
        for (id, text) in pack.messages.iter() {
            let Some(source) = english.get(id) else {
                continue;
            };
            for name in PRODUCT_NAMES {
                if source.contains(name) && !text.contains(name) {
                    problems.push(format!("{} {id}: drops {name}", pack.locale));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn a_pack_writes_numbers_with_western_digits_only() {
    for pack in packs() {
        let other: Vec<&String> = pack
            .messages
            .iter()
            .filter(|(_, t)| t.chars().any(is_non_ascii_decimal_digit))
            .map(|(id, _)| id)
            .collect();
        assert!(other.is_empty(), "{}: {other:?}", pack.locale);
    }
}

#[test]
fn a_browsers_regional_language_tag_picks_the_closest_shipped_language() {
    let intl = english();
    let registry = registry(&intl);
    for pack in packs() {
        registry.register(&pack);
    }
    let english = english_catalog();
    const ID: &str = "widget.indicatorPicker.title";
    let title = msg("widget.indicatorPicker.title", "Indicators");
    let shipped: HashMap<String, String> = packs()
        .into_iter()
        .map(|p| {
            let text = p
                .messages
                .get(ID)
                .cloned()
                .unwrap_or_else(|| english[ID].clone());
            (p.locale, text)
        })
        .collect();
    let picks = |browser_tag: &str| {
        LocaleState::new(locale(browser_tag), intl.clone(), registry.clone()).t(&title)
    };
    let expected = [
        ("pt", "pt-BR"),
        ("pt-PT", "pt-BR"),
        ("pt-BR", "pt-BR"),
        ("zh", "zh-Hans"),
        ("zh-CN", "zh-Hans"),
        ("zh-SG", "zh-Hans"),
        ("zh-TW", "zh-Hant"),
        ("zh-HK", "zh-Hant"),
        ("zh-MO", "zh-Hant"),
        ("es-419", "es"),
        ("es-MX", "es"),
        ("ar-EG", "ar"),
        ("ar-SA", "ar"),
        ("iw", "he"),
        ("in", "id"),
    ];
    for (browser_tag, tag) in expected {
        assert!(shipped.contains_key(tag), "{tag}");
        assert_eq!(picks(browser_tag), shipped[tag], "{browser_tag}");
    }
    // Simplified and Traditional Chinese write the word differently, so the check tells them apart.
    assert_ne!(shipped["zh-Hans"], shipped["zh-Hant"]);
    for tag in ["en", "en-GB", "en-IN", "en-US"] {
        assert_eq!(picks(tag), english[ID], "{tag}");
    }
    let hebrew = translator(
        &intl,
        LocalizationOptions {
            direction: Some(DirectionOption::Auto),
            ..locale("iw")
        },
    );
    assert_eq!(hebrew.direction(), TextDirection::Rtl);
}

mod right_to_left {
    use super::*;

    #[test]
    fn the_right_to_left_languages_ship_as_arabic_and_hebrew_packs_that_say_so() {
        let mut tags: Vec<String> = packs()
            .into_iter()
            .filter(|p| p.direction == Some(TextDirection::Rtl))
            .map(|p| p.locale)
            .collect();
        tags.sort();
        assert_eq!(tags, ["ar", "he"]);
    }

    #[test]
    fn a_translator_set_to_a_right_to_left_pack_reads_its_chrome_right_to_left_in_its_own_language()
    {
        let intl = english();
        for pack in packs()
            .into_iter()
            .filter(|p| p.direction == Some(TextDirection::Rtl))
        {
            let registry = registry(&intl);
            registry.register(&pack);
            let options = LocalizationOptions {
                direction: Some(DirectionOption::Auto),
                ..locale(&pack.locale)
            };
            let t = LocaleState::new(options, intl.clone(), registry);
            assert_eq!(t.direction(), TextDirection::Rtl, "{}", pack.locale);
            let topbar = t.t(&msg("widget.topbar.indicators", "Indicators"));
            let expected = pack
                .messages
                .get("widget.topbar.indicators")
                .cloned()
                .unwrap_or_else(|| "Indicators".to_string());
            assert_eq!(topbar, expected, "{}", pack.locale);
            // The chart's own numbers stay Western digits in a left-to-right run.
            assert_eq!(t.numbering_system(), "latn");
        }
    }
}
