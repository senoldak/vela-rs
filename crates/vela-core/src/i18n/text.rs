//! Display text versus stored identity. A [`MessageLike`] renders through a translator,
//! while the code that keys on a name (series ids, persisted option values,
//! script-readable titles) reads its English source text, which no locale pack or
//! override can change.

use std::sync::OnceLock;

use crate::format::dates::time_clip;
use crate::format::numbers::localize_ascii;
use crate::format::types::{ClockFormatOptions, FormatSettings};
use crate::intl::{Granularity, Intl, Segment};

use super::icu::format_message;
use super::message::{MessageArg, MessageLike};
use super::translator::{english_translator, format_settings_of, merged_args, resolve_args};
use super::types::Translator;

fn english_settings() -> &'static FormatSettings {
    static SETTINGS: OnceLock<FormatSettings> = OnceLock::new();
    SETTINGS.get_or_init(FormatSettings::english)
}

/// The English source text of display text, with its bound values: what identities key
/// on. Registered packs and `messages` overrides never change it. Plain text comes back
/// as is.
pub fn source_text(message: &MessageLike) -> String {
    source_text_with(message, &[])
}

/// [`source_text`] with extra values, which take precedence over the bound ones.
pub fn source_text_with(message: &MessageLike, values: &[(&str, MessageArg)]) -> String {
    match message {
        MessageLike::Text(t) => t.to_string(),
        MessageLike::Message(d) => {
            let all = merged_args(&d.values, values);
            let resolved = resolve_args(&all, &|m| source_text(&MessageLike::Message(m.clone())));
            format_message(&d.default_message, Some(&resolved), english_settings())
                .unwrap_or_else(|_| d.default_message.to_string())
        }
    }
}

/// Display text through `i18n`, or in English when no translator is at hand.
pub fn text_of(
    i18n: Option<&dyn Translator>,
    message: &MessageLike,
    values: &[(&str, MessageArg)],
) -> String {
    match i18n {
        Some(t) => t.t_with(message, values),
        None => english_translator().t_with(message, values),
    }
}

/// Upper-case text by the casing rules of `locale` (Turkish dotted I, ...); a tag the
/// engine rejects falls back to the default rules.
pub fn upper_case(intl: &dyn Intl, text: &str, locale: Option<&str>) -> String {
    intl.to_upper_case(text, locale)
        .unwrap_or_else(|| text.to_uppercase())
}

/// Number text written with ASCII digits, `.` and `-` (a ratio, a `toFixed` price), in
/// the digits and separators of `i18n`'s locale; unchanged in English.
pub fn localize_number_text(text: &str, i18n: Option<&dyn Translator>) -> String {
    localize_ascii(text, &format_settings_of(i18n))
}

/// When something happened (an alert fired), as a wall-clock time in the viewer's own
/// zone. Until the host chooses a locale it reads as the page has always written it (the
/// host's conventions, through `intl`); once it does, it follows the chart's clock,
/// digits and hour cycle.
pub fn event_time_text(ms: f64, i18n: Option<&dyn Translator>, intl: &dyn Intl) -> String {
    let Some(i18n) = i18n.filter(|t| t.locale_explicit()) else {
        return match time_clip(ms) {
            Some(ms) => intl.format_default_time(ms),
            None => "Invalid Date".to_string(),
        };
    };
    let options = ClockFormatOptions {
        surface: Some(crate::format::FormatSurface::Clock),
        time_zone: intl.default_time_zone(),
        ..ClockFormatOptions::default()
    };
    i18n.format().clock(ms, &options)
}

/// The first `count` user-perceived characters of `text` (grapheme clusters: an emoji, a
/// letter with its accents, a Thai syllable stays whole), for truncating display text
/// without splitting a character in two. Counts code points where the backend has no
/// segmenter.
pub fn slice_graphemes(intl: &dyn Intl, text: &str, count: usize) -> String {
    if count == 0 {
        return String::new();
    }
    // Printable ASCII has one character per UTF-16 unit: a plain slice is exact.
    if text.bytes().all(|b| (b' '..=b'~').contains(&b)) {
        return text.chars().take(count).collect();
    }
    match intl.segment(None, Granularity::Grapheme, text) {
        Some(segments) => segments
            .into_iter()
            .take(count)
            .map(|s| s.segment)
            .collect(),
        None => text.chars().take(count).collect(),
    }
}

/// The segments of `text` by `intl.Segmenter` for `locale` (graphemes or words), or
/// `None` where the backend has none.
pub fn text_segments(
    intl: &dyn Intl,
    locale: Option<&str>,
    granularity: Granularity,
    text: &str,
) -> Option<Vec<Segment>> {
    intl.segment(locale, granularity, text)
}
