//! Display text versus stored identity, and the text helpers of `core/i18n/text.ts`:
//! `source_text`, `text_of`, `upper_case`, `localize_number_text`, `event_time_text` and
//! the grapheme truncation. The original has no dedicated test file for these; the cases
//! follow the doc comments of the TypeScript.

mod i18n_common;

use std::sync::Arc;

use i18n_common::{RecordingIntl, english, fake, locale, pack, registry, translator};
use vela_core::format::ClockFormatOptions;
use vela_core::i18n::{
    LocaleState, LocalizationOptions, MessageArg, MessageLike, Translator, event_time_text,
    localize_number_text, msg, slice_graphemes, source_text, source_text_with, text_of, upper_case,
    with_values,
};
use vela_core::intl::Intl;

#[test]
fn source_text_ignores_packs_and_overrides() {
    let intl = english();
    let registry = registry(&intl);
    registry.register(&pack("de", &[("test.title", "Tastenkürzel")]));
    let over = LocalizationOptions {
        messages: Some(i18n_common::map(&[("test.title", "Shortcuts")])),
        ..locale("de")
    };
    let t = LocaleState::new(over, intl, registry);
    let title = msg("test.title", "Keyboard shortcuts");
    assert_eq!(t.t(&title), "Shortcuts");
    assert_eq!(source_text(&title), "Keyboard shortcuts");
}

#[test]
fn source_text_binds_values_and_lets_call_values_win() {
    let band = msg("x.band", "{kind} Band #{n}");
    let bound = with_values(&band, [("kind", "Upper".into()), ("n", 2usize.into())]);
    assert_eq!(source_text(&bound), "Upper Band #2");
    assert_eq!(
        source_text_with(&bound, &[("n", MessageArg::Number(9.0))]),
        "Upper Band #9"
    );
    assert_eq!(source_text(&"plain {x}".into()), "plain {x}");
}

#[test]
fn text_of_reads_english_when_no_translator_is_at_hand() {
    let title = msg("test.title", "Keyboard shortcuts");
    assert_eq!(text_of(None, &title, &[]), "Keyboard shortcuts");
    assert_eq!(
        text_of(
            None,
            &msg("x.n", "{n, plural, one {# bar} other {# bars}}"),
            &[("n", MessageArg::Number(3.0))]
        ),
        "3 bars"
    );
    let t = translator(
        &english(),
        LocalizationOptions {
            messages: Some(i18n_common::map(&[("test.title", "Shortcuts")])),
            ..Default::default()
        },
    );
    assert_eq!(text_of(Some(&t), &title, &[]), "Shortcuts");
}

#[test]
fn a_nested_message_translates_through_the_translator_that_resolves_it() {
    let intl = english();
    let registry = registry(&intl);
    registry.register(&pack("es", &[("pane.main", "Gráfico principal")]));
    let t = LocaleState::new(locale("es"), intl, registry);
    let pane = match msg("pane.main", "Main chart") {
        MessageLike::Message(d) => d,
        MessageLike::Text(_) => unreachable!("msg builds a descriptor"),
    };
    let message = with_values(
        &msg("x.moveTo", "Move to {pane}"),
        [("pane", MessageArg::Message(pane))],
    );
    assert_eq!(t.t(&message), "Move to Gráfico principal");
    assert_eq!(source_text(&message), "Move to Main chart");
}

#[test]
fn upper_case_follows_the_locale_and_a_rejected_tag_falls_back_to_the_default_rules() {
    let intl = english();
    assert_eq!(
        upper_case(intl.as_ref(), "istanbul", Some("tr")),
        "\u{130}STANBUL"
    );
    assert_eq!(
        upper_case(intl.as_ref(), "istanbul", Some("en")),
        "ISTANBUL"
    );
    assert_eq!(upper_case(intl.as_ref(), "istanbul", None), "ISTANBUL");
    assert_eq!(
        upper_case(intl.as_ref(), "istanbul", Some("not a tag")),
        "ISTANBUL"
    );
    assert_eq!(
        upper_case(intl.as_ref(), "stra\u{df}e", Some("de")),
        "STRASSE"
    );
    // A backend with no casing support at all reads the default rules too.
    let recording: Arc<dyn Intl> = RecordingIntl::new();
    assert_eq!(upper_case(recording.as_ref(), "abc", Some("tr")), "ABC");
}

#[test]
fn number_text_takes_the_digits_and_separators_of_the_locale() {
    assert_eq!(localize_number_text("1234.5", None), "1234.5");
    let de = translator(&fake(), locale("de"));
    assert_eq!(localize_number_text("-1234.5", Some(&de)), "-1234,5");
    let arab = translator(
        &fake(),
        LocalizationOptions {
            numbering_system: Some("arab".into()),
            ..locale("ar")
        },
    );
    assert_eq!(
        localize_number_text("-1.5", Some(&arab)),
        "-\u{661}\u{66b}\u{665}"
    );
}

#[test]
fn an_alert_time_reads_as_the_page_always_wrote_it_until_the_host_picks_a_locale() {
    let intl = fake();
    let at = 1_788_618_600_000.0;
    assert_eq!(
        event_time_text(at, None, intl.as_ref()),
        format!("default-time:{at}")
    );
    let silent = translator(&intl, LocalizationOptions::default());
    assert_eq!(
        event_time_text(at, Some(&silent), intl.as_ref()),
        format!("default-time:{at}")
    );
    assert_eq!(
        event_time_text(f64::NAN, None, intl.as_ref()),
        "Invalid Date"
    );

    // With a locale chosen it follows the chart's clock, in the host's own zone (Tokyo here).
    let en = translator(&intl, locale("en"));
    assert_eq!(event_time_text(at, Some(&en), intl.as_ref()), "23:30:00");
    let h12 = translator(
        &intl,
        LocalizationOptions {
            hour_cycle: Some(vela_core::format::HourCycleSetting::H12),
            ..locale("en")
        },
    );
    assert_eq!(
        event_time_text(at, Some(&h12), intl.as_ref()),
        "11:30:00 PM"
    );
    // Same through the formatter directly.
    let clock = ClockFormatOptions {
        time_zone: Some("Asia/Tokyo".into()),
        ..Default::default()
    };
    assert_eq!(en.format().clock(at, &clock), "23:30:00");
}

#[test]
fn graphemes_stay_whole_when_text_is_truncated() {
    let intl = english();
    let slice = |text: &str, n: usize| slice_graphemes(intl.as_ref(), text, n);
    assert_eq!(slice("Volume", 3), "Vol");
    assert_eq!(slice("Volume", 0), "");
    assert_eq!(slice("", 5), "");
    assert_eq!(slice("ab", 9), "ab");
    assert_eq!(slice("e\u{301}tude", 2), "e\u{301}t");
    assert_eq!(
        slice("\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}x", 1),
        "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}"
    );
    assert_eq!(
        slice("\u{1f1e9}\u{1f1ea}\u{1f1eb}\u{1f1f7}", 1),
        "\u{1f1e9}\u{1f1ea}"
    );
    // No segmenter: code points, which split a letter from its accent.
    let none: Arc<dyn Intl> = RecordingIntl::new();
    assert_eq!(
        slice_graphemes(none.as_ref(), "e\u{301}tude", 2),
        "e\u{301}"
    );
}
