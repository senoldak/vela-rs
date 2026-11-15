//! Vela's message system: plain strings stay verbatim, descriptors translate through the
//! registered packs with English as the last fallback, and the ICU subset handles the
//! plural rules of the launch languages. Ported from `i18n-messages.test.ts`.
//!
//! Plural rules and separators of de, ar, pl and ru come from the `FakeIntl` test double,
//! not from CLDR.

mod i18n_common;

use i18n_common::{english, fake, locale, pack, registry, translator};
use vela_core::i18n::{
    LocaleState, LocalizationOptions, MessageArg, MessageLike, Translator, fallback_chain, msg,
    with_values,
};
use vela_core::locales;

fn bars() -> MessageLike {
    msg("test.bars", "{count, plural, one {# bar} other {# bars}}")
}

fn title() -> MessageLike {
    msg("test.title", "Keyboard shortcuts")
}

fn n(v: f64) -> MessageArg {
    MessageArg::Number(v)
}

#[test]
fn a_plain_string_label_renders_verbatim_braces_and_all() {
    let t = translator(&fake(), locale("de"));
    assert_eq!(t.t(&"Volume {x}".into()), "Volume {x}");
    assert_eq!(t.t(&"l'indicateur".into()), "l'indicateur");
}

#[test]
fn an_untranslated_message_reads_english_with_numbers_in_the_charts_locale() {
    let t = translator(&fake(), locale("de"));
    assert_eq!(
        t.t(&msg("test.title", "Keyboard shortcuts")),
        "Keyboard shortcuts"
    );
    assert_eq!(t.t_with(&bars(), &[("count", n(1.0))]), "1 bar");
    assert_eq!(t.t_with(&bars(), &[("count", n(1200.0))]), "1.200 bars");
    // The built-in backend has no German data: numbers read English, as `Intl` would
    // after a throwing constructor.
    let built_in = translator(&english(), locale("de"));
    assert_eq!(
        built_in.t_with(&bars(), &[("count", n(1200.0))]),
        "1,200 bars"
    );
}

#[test]
fn a_registered_pack_translates_a_message_and_its_handle_removes_the_pack_again() {
    let intl = english();
    let registry = registry(&intl);
    let t = LocaleState::new(locale("de"), intl, registry.clone());
    let off = registry.register(&pack("de", &[("test.title", "Tastenkürzel")]));
    assert_eq!(t.t(&title()), "Tastenkürzel");
    assert!(registry.registered_locales().contains(&"de".to_string()));
    off.dispose();
    assert_eq!(t.t(&title()), "Keyboard shortcuts");
    assert!(!registry.registered_locales().contains(&"de".to_string()));
    off.dispose(); // a second call is harmless
}

#[test]
fn a_key_missing_from_a_pack_falls_back_to_english() {
    let intl = english();
    let registry = registry(&intl);
    registry.register(&pack("fr", &[("test.title", "Raccourcis clavier")]));
    let t = LocaleState::new(locale("fr"), intl, registry);
    assert_eq!(t.t(&title()), "Raccourcis clavier");
    assert_eq!(t.t(&msg("test.other", "Mouse")), "Mouse");
}

#[test]
fn a_regional_locale_falls_back_to_its_language_then_to_english() {
    let intl = english();
    let chain = |tag: &str| fallback_chain(intl.as_ref(), tag);
    assert_eq!(chain("pt-BR"), ["pt-BR", "pt-Latn", "pt", "en"]);
    assert_eq!(chain("zh-TW"), ["zh-TW", "zh-Hant", "zh", "en"]);
    assert_eq!(chain("en-IN"), ["en-IN", "en-Latn", "en", "en-US"]);
    assert_eq!(chain("pt-PT"), ["pt-PT", "pt-Latn", "pt", "pt-BR", "en"]);

    let registry = registry(&intl);
    registry.register(&pack("pt", &[("test.title", "Atalhos (pt)")]));
    registry.register(&pack("zh-Hant", &[("test.title", "鍵盤快捷鍵")]));
    registry.register(&pack("es", &[("test.title", "Atajos de teclado")]));
    let read =
        |tag: &str| LocaleState::new(locale(tag), intl.clone(), registry.clone()).t(&title());
    assert_eq!(read("pt-BR"), "Atalhos (pt)");
    assert_eq!(read("zh-TW"), "鍵盤快捷鍵");
    assert_eq!(read("es-MX"), "Atajos de teclado");
    assert_eq!(read("zh-Hans"), "Keyboard shortcuts");
}

#[test]
fn the_more_specific_pack_wins_and_the_pack_registered_last_wins_a_tie() {
    let intl = english();
    let registry = registry(&intl);
    registry.register(&pack("pt", &[("test.title", "genérico")]));
    registry.register(&pack("pt-BR", &[("test.title", "Brasil")]));
    let t = LocaleState::new(locale("pt-BR"), intl, registry.clone());
    let m = msg("test.title", "x");
    assert_eq!(t.t(&m), "Brasil");
    registry.register(&pack("pt-BR", &[("test.title", "Brasil, de novo")]));
    assert_eq!(t.t(&m), "Brasil, de novo");
}

#[test]
fn message_overrides_beat_the_registered_pack_and_an_english_override_rewords_english() {
    let intl = english();
    let registry = registry(&intl);
    registry.register(&pack("de", &[("test.title", "Tastenkürzel")]));
    let over = LocalizationOptions {
        locale: Some("de".into()),
        messages: Some(i18n_common::map(&[("test.title", "Kurzbefehle")])),
        ..LocalizationOptions::default()
    };
    let t = LocaleState::new(over, intl.clone(), registry);
    assert_eq!(t.t(&title()), "Kurzbefehle");
    let english_only = LocalizationOptions {
        messages: Some(i18n_common::map(&[("test.title", "Shortcuts")])),
        ..LocalizationOptions::default()
    };
    assert_eq!(translator(&intl, english_only).t(&title()), "Shortcuts");
}

#[test]
fn a_malformed_translation_falls_back_to_english_instead_of_breaking_the_chart() {
    let intl = english();
    let registry = registry(&intl);
    registry.register(&pack(
        "de",
        &[("test.bars", "{count, plural, one {# Balken")],
    ));
    let t = LocaleState::new(locale("de"), intl, registry);
    assert_eq!(t.t_with(&bars(), &[("count", n(2.0))]), "2 bars");
    // A malformed English text reads as its raw text.
    let broken = msg("test.broken", "{oops");
    assert_eq!(t.t(&broken), "{oops");
}

#[test]
fn registering_a_pack_is_a_call_and_the_shipped_english_pack_registers_like_any_other() {
    let intl = english();
    let registry = registry(&intl);
    assert!(registry.registered_locales().is_empty());
    let en = locales::en();
    assert_eq!(en.locale, "en");
    let off = registry.register(&en);
    assert_eq!(registry.registered_locales(), ["en"]);
    off.dispose();
    assert!(registry.registered_locales().is_empty());
}

#[test]
fn a_pack_registered_with_a_lowercase_tag_is_stored_canonically() {
    let intl = english();
    let registry = registry(&intl);
    registry.register(&pack("pt-br", &[]));
    registry.register(&pack("iw", &[]));
    assert_eq!(registry.registered_locales(), ["pt-BR", "he"]);
}

#[test]
fn updating_a_translator_in_place_reads_the_new_locale() {
    let intl = english();
    let registry = registry(&intl);
    registry.register(&pack("fr", &[("test.title", "Raccourcis clavier")]));
    let mut t = LocaleState::new(LocalizationOptions::default(), intl, registry);
    assert_eq!(t.locale(), "en");
    assert!(!t.locale_explicit());
    assert_eq!(t.t(&title()), "Keyboard shortcuts");
    t.set_locale("fr");
    assert_eq!(t.locale(), "fr");
    assert!(t.locale_explicit());
    assert_eq!(t.t(&title()), "Raccourcis clavier");
    t.update(vela_core::i18n::LocalizationPatch {
        locale: vela_core::i18n::Patch::Clear,
        ..Default::default()
    });
    assert_eq!(t.locale(), "en");
    assert_eq!(t.t(&title()), "Keyboard shortcuts");
}

#[test]
fn bound_values_are_overridden_by_call_arguments_and_nested_messages_translate_first() {
    let intl = english();
    let registry = registry(&intl);
    registry.register(&pack("fr", &[("test.main", "Graphique principal")]));
    let t = LocaleState::new(locale("fr"), intl, registry);
    let main = match msg("test.main", "Main chart") {
        MessageLike::Message(d) => d,
        MessageLike::Text(_) => unreachable!("msg builds a descriptor"),
    };
    let move_to = with_values(
        &msg("test.move", "Move to {pane} ({n})"),
        [("pane", MessageArg::Message(main)), ("n", n(1.0))],
    );
    assert_eq!(t.t(&move_to), "Move to Graphique principal (1)");
    assert_eq!(
        t.t_with(&move_to, &[("n", n(7.0))]),
        "Move to Graphique principal (7)"
    );
}

mod message_syntax {
    use super::*;

    fn t() -> LocaleState {
        translator(&english(), LocalizationOptions::default())
    }

    #[test]
    fn names_numbers_and_select_fill_in_their_values() {
        let t = t();
        assert_eq!(
            t.t_with(&msg("test.a", "Delta {value}"), &[("value", "1.2K".into())]),
            "Delta 1.2K"
        );
        assert_eq!(
            t.t_with(
                &msg("test.b", "{n, number} of {total, number, integer}"),
                &[("n", n(1234.5)), ("total", n(9876.5))]
            ),
            "1,234.5 of 9,877"
        );
        let side = msg(
            "test.c",
            "{side, select, buy {Bought} sell {Sold} other {Traded}}",
        );
        assert_eq!(t.t_with(&side, &[("side", "buy".into())]), "Bought");
        assert_eq!(t.t_with(&side, &[("side", "short".into())]), "Traded");
        assert_eq!(t.t(&msg("test.d", "Hello {name}")), "Hello {name}");
    }

    #[test]
    fn exact_matches_and_offsets_win_over_the_plural_categories() {
        let t = t();
        let m = msg(
            "test.e",
            "{n, plural, offset:1 =0 {nobody} =1 {only {who}} one {{who} and # other} other {{who} and # others}}",
        );
        let at = |count: f64| t.t_with(&m, &[("n", n(count)), ("who", "Ana".into())]);
        assert_eq!(at(0.0), "nobody");
        assert_eq!(at(1.0), "only Ana");
        assert_eq!(at(2.0), "Ana and 1 other");
        assert_eq!(at(5.0), "Ana and 4 others");
    }

    #[test]
    fn ordinals_pick_their_own_categories() {
        let t = t();
        let m = msg(
            "test.f",
            "{n, selectordinal, one {#st} two {#nd} few {#rd} other {#th}}",
        );
        let got: Vec<String> = [1.0, 2.0, 3.0, 4.0, 11.0, 21.0, 22.0]
            .iter()
            .map(|v| t.t_with(&m, &[("n", n(*v))]))
            .collect();
        assert_eq!(got, ["1st", "2nd", "3rd", "4th", "11th", "21st", "22nd"]);
    }

    #[test]
    fn an_apostrophe_in_a_translation_needs_no_escaping_and_quoted_braces_stay_literal() {
        let intl = english();
        let registry = registry(&intl);
        registry.register(&pack(
            "fr",
            &[
                ("test.g", "l'indicateur {name}"),
                ("test.h", "'{name}' n''est pas {name}"),
            ],
        ));
        let fr = LocaleState::new(locale("fr"), intl, registry);
        assert_eq!(
            fr.t_with(
                &msg("test.g", "the {name} indicator"),
                &[("name", "RSI".into())]
            ),
            "l'indicateur RSI"
        );
        assert_eq!(
            fr.t_with(&msg("test.h", "x"), &[("name", "RSI".into())]),
            "{name} n'est pas RSI"
        );
    }
}

mod plural_rules_of_the_launch_languages {
    use super::*;

    fn read(tag: &str, text: &'static str, counts: &[f64]) -> Vec<String> {
        let intl = fake();
        let registry = registry(&intl);
        registry.register(&pack(tag, &[("test.bars", text)]));
        let t = LocaleState::new(locale(tag), intl, registry);
        counts
            .iter()
            .map(|c| t.t_with(&bars(), &[("count", n(*c))]))
            .collect()
    }

    #[test]
    fn arabic_picks_among_six_forms() {
        let got = read(
            "ar",
            "{count, plural, zero {لا أشرطة} one {شريط واحد} two {شريطان} few {# أشرطة} many {# شريطًا} other {# شريط}}",
            &[0.0, 1.0, 2.0, 3.0, 11.0, 100.0],
        );
        assert_eq!(
            got,
            [
                "لا أشرطة",
                "شريط واحد",
                "شريطان",
                "3 أشرطة",
                "11 شريطًا",
                "100 شريط"
            ]
        );
    }

    #[test]
    fn polish_picks_among_one_few_and_many() {
        let got = read(
            "pl",
            "{count, plural, one {# świeca} few {# świece} many {# świec} other {# świecy}}",
            &[1.0, 2.0, 5.0, 22.0, 25.0, 1.5],
        );
        assert_eq!(
            got,
            [
                "1 świeca",
                "2 świece",
                "5 świec",
                "22 świece",
                "25 świec",
                "1,5 świecy"
            ]
        );
    }

    #[test]
    fn russian_picks_among_one_few_and_many() {
        let got = read(
            "ru",
            "{count, plural, one {# свеча} few {# свечи} many {# свечей} other {# свечи}}",
            &[1.0, 21.0, 3.0, 5.0, 11.0, 111.0],
        );
        assert_eq!(
            got,
            [
                "1 свеча",
                "21 свеча",
                "3 свечи",
                "5 свечей",
                "11 свечей",
                "111 свечей"
            ]
        );
    }
}
