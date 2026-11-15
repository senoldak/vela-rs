//! The English catalog and the locale packs stay in step: every English message is valid
//! message syntax, every shipped pack parses and names only messages that exist, with the
//! same arguments as the English. Ported from `i18n-catalog.test.ts`; the check that the
//! catalog lists every `msg()` of the TypeScript source has no counterpart here.

mod i18n_common;

use std::collections::{BTreeSet, HashMap};

use i18n_common::{english, locale, registry, translator};
use vela_core::i18n::icu::{Node, parse_message};
use vela_core::i18n::{
    LocaleState, LocalizationOptions, MessageDescriptor, MessageLike, Translator,
};
use vela_core::locales;

fn english_catalog() -> HashMap<String, String> {
    let json = include_str!("../locales/en.json");
    serde_json::from_str(json).unwrap_or_default()
}

/// The argument names a message uses (`{name}`, `{n, plural, ...}`), sorted.
fn argument_names(message: &str) -> Result<Vec<String>, String> {
    fn walk(nodes: &[Node], names: &mut BTreeSet<String>) {
        for node in nodes {
            match node {
                Node::Text(_) | Node::Pound => {}
                Node::Arg { name, .. } => {
                    names.insert(name.clone());
                }
                Node::Plural { name, options, .. } | Node::Select { name, options } => {
                    names.insert(name.clone());
                    for branch in options.values() {
                        walk(branch, names);
                    }
                }
            }
        }
    }
    let nodes = parse_message(message).map_err(|e| e.message)?;
    let mut names = BTreeSet::new();
    walk(&nodes, &mut names);
    Ok(names.into_iter().collect())
}

#[test]
fn the_english_catalog_is_not_empty_and_every_message_is_valid_syntax() {
    let catalog = english_catalog();
    assert!(catalog.len() > 1000, "{} messages", catalog.len());
    let broken: Vec<&String> = catalog
        .iter()
        .filter(|(_, text)| parse_message(text).is_err())
        .map(|(id, _)| id)
        .collect();
    assert!(broken.is_empty(), "{broken:?}");
}

#[test]
fn the_shipped_english_pack_is_the_catalog() {
    assert_eq!(*locales::en().messages, english_catalog());
}

#[test]
fn the_english_pack_changes_nothing_on_screen() {
    let intl = english();
    let plain = translator(&intl, LocalizationOptions::default());
    let registry = registry(&intl);
    registry.register(&locales::en());
    let with_pack = LocaleState::new(LocalizationOptions::default(), intl, registry);
    for (id, text) in english_catalog() {
        let message = MessageLike::Message(MessageDescriptor {
            id: id.clone().into(),
            default_message: text.into(),
            values: Vec::new(),
        });
        assert_eq!(with_pack.t(&message), plain.t(&message), "{id}");
    }
}

#[test]
fn each_shipped_pack_parses_names_only_known_messages_and_keeps_their_arguments() {
    let english = english_catalog();
    let mut problems: Vec<String> = Vec::new();
    for pack in locales::all().into_iter().filter(|p| p.locale != "en") {
        assert!(!pack.messages.is_empty(), "{} parsed empty", pack.locale);
        for (id, text) in pack.messages.iter() {
            let Some(source) = english.get(id) else {
                problems.push(format!("{}: unknown message id {id}", pack.locale));
                continue;
            };
            match (argument_names(text), argument_names(source)) {
                (Ok(ours), Ok(theirs)) if ours == theirs => {}
                (Ok(_), Ok(_)) => {
                    problems.push(format!("{}: {id} changes its arguments", pack.locale))
                }
                _ => problems.push(format!("{}: {id} does not parse", pack.locale)),
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn every_message_of_every_pack_is_served_by_its_translator_instead_of_the_english_default() {
    let intl = english();
    for pack in locales::all() {
        let registry = registry(&intl);
        registry.register(&pack);
        let t = LocaleState::new(locale(&pack.locale), intl.clone(), registry);
        for (id, text) in pack.messages.iter() {
            let message = MessageLike::Message(MessageDescriptor {
                id: id.clone().into(),
                default_message: "placeholder".into(),
                values: Vec::new(),
            });
            assert_ne!(
                t.t(&message),
                "placeholder",
                "{}: {id} = {text}",
                pack.locale
            );
        }
    }
}
