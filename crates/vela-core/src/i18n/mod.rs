//! Vela's message system: `msg()` descriptors, locale packs, and the per-chart translator.
//!
//! A [`MessageLike`] is either plain text or a translatable message: a stable dotted id
//! plus its English source text in ICU message syntax. Code that keys on a name (series
//! ids, persisted option values) reads the English source through [`source_text`], which
//! no locale can change. A [`Translator`] resolves display text through the registered
//! packs of its locale and fallbacks, and formats numbers and dates through an
//! [`Intl`](crate::intl::Intl) backend.
//!
//! Packs live in a [`LocaleRegistry`] that the host creates and shares with its
//! translators; the packs that ship with the crate are in [`crate::locales`].

pub mod icu;
pub mod locale;
pub mod message;
pub mod registry;
pub mod text;
pub mod translator;
pub mod types;

pub use locale::{
    canonical_locale, fallback_chain, language_of, locale_direction, resolve_numbering_system,
};
pub use message::{MessageArg, MessageDescriptor, MessageLike, message_only, msg, with_values};
pub use registry::{LocaleRegistry, Registration};
pub use text::{
    event_time_text, localize_number_text, slice_graphemes, source_text, source_text_with, text_of,
    text_segments, upper_case,
};
pub use translator::{LocaleState, create_translator, english_translator, format_settings_of};
pub use types::{
    DirectionOption, LocaleChangedEvent, LocalePack, LocalizationOptions, LocalizationPatch, Patch,
    TextDirection, Translator,
};

/// A number the way JavaScript's `String(n)` writes the common cases.
pub(crate) fn number_text(n: f64) -> String {
    if n.is_nan() {
        "NaN".to_string()
    } else if n.is_infinite() {
        if n > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        }
    } else if n == n.trunc() && n.abs() < 1e21 {
        format!("{}", n as i128)
    } else {
        format!("{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_its_own_source() {
        assert_eq!(source_text(&"Close".into()), "Close");
    }

    #[test]
    fn bound_values_fill_simple_arguments() {
        let band = msg("x.band", "Upper Band #{n}");
        let bound = with_values(&band, [("n", 2usize.into())]);
        assert_eq!(source_text(&bound), "Upper Band #2");
    }

    #[test]
    fn plural_and_select_messages_format_through_the_english_runtime() {
        let m = msg("x.count", "{count, plural, one {# bar} other {# bars}}");
        assert_eq!(
            source_text(&with_values(&m, [("count", 1usize.into())])),
            "1 bar"
        );
        assert_eq!(
            source_text(&with_values(&m, [("count", 1200usize.into())])),
            "1,200 bars"
        );
        // Unbound arguments stay visible, as in the original.
        assert_eq!(source_text(&msg("x.n", "Band #{n}")), "Band #{n}");
        assert_eq!(source_text(&m), "# bars");
    }

    #[test]
    fn malformed_patterns_read_as_their_raw_text() {
        let m = msg("x.bad", "{count, plural, one {# bar");
        assert_eq!(source_text(&m), "{count, plural, one {# bar");
    }

    #[test]
    fn nested_messages_resolve_to_their_source_text() {
        let pane = msg("x.pane", "Main chart");
        let move_to = msg("x.move", "Move to {pane}");
        let arg = match pane {
            MessageLike::Message(d) => MessageArg::Message(d),
            MessageLike::Text(t) => MessageArg::Text(t),
        };
        assert_eq!(
            source_text(&with_values(&move_to, [("pane", arg)])),
            "Move to Main chart"
        );
    }

    #[test]
    fn message_only_keeps_descriptors() {
        let plain: MessageLike = "A".into();
        let m = msg("a", "A");
        assert!(message_only(Some(&plain)).is_none());
        assert!(message_only(Some(&m)).is_some());
    }
}
