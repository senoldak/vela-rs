//! Test doubles for the `Intl` seam.
//!
//! [`FakeIntl`] stands in for a browser backend on the few locales the original's tests
//! name (German, Polish, Russian and Arabic separators and plural rules, and a handful of
//! zones as fixed offsets) and otherwise behaves like [`EnglishIntl`]. [`RecordingIntl`]
//! answers with recognizable strings and records what was requested. Neither is real
//! locale data: cases that depend on the full CLDR stay in the "needs a browser backend"
//! list of the report.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use vela_core::i18n::{LocalePack, LocaleRegistry, LocaleState, LocalizationOptions};
use vela_core::intl::{
    DateTimeFormatOptions, EnglishIntl, FormatPart, Granularity, Intl, LocaleInfo, Notation,
    NumberFormatOptions, PartType, PluralCategory, PluralKind, Segment, TextDirection,
};

pub fn english() -> Arc<dyn Intl> {
    Arc::new(EnglishIntl)
}

pub fn fake() -> Arc<dyn Intl> {
    Arc::new(FakeIntl)
}

pub fn registry(intl: &Arc<dyn Intl>) -> Arc<LocaleRegistry> {
    LocaleRegistry::new(Arc::clone(intl))
}

pub fn locale(tag: &str) -> LocalizationOptions {
    LocalizationOptions {
        locale: Some(tag.to_string()),
        ..LocalizationOptions::default()
    }
}

/// A translator over `intl` with its own empty registry.
pub fn translator(intl: &Arc<dyn Intl>, options: LocalizationOptions) -> LocaleState {
    LocaleState::new(options, Arc::clone(intl), registry(intl))
}

pub fn pack(tag: &str, messages: &[(&str, &str)]) -> LocalePack {
    LocalePack {
        locale: tag.to_string(),
        direction: None,
        messages: Arc::new(
            messages
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        ),
    }
}

fn language(tag: &str) -> Option<String> {
    EnglishIntl.parse_locale(tag).map(|l| l.language)
}

/// Zones the fake knows, as the fixed offsets they have in late October.
const ZONES: &[(&str, &str)] = &[
    ("America/New_York", "-04:00"),
    ("America/Chicago", "-05:00"),
    ("Asia/Tokyo", "+09:00"),
    ("Asia/Kolkata", "+05:30"),
    ("Pacific/Marquesas", "-09:30"),
    ("Europe/Paris", "+02:00"),
];

fn map_zone(options: &DateTimeFormatOptions) -> DateTimeFormatOptions {
    let mut out = options.clone();
    if let Some(zone) = &options.time_zone
        && let Some((_, offset)) = ZONES.iter().find(|(z, _)| z == zone)
    {
        out.time_zone = Some((*offset).to_string());
    }
    out
}

fn ru_pl_category(kind: PluralKind, n: f64, polish: bool) -> PluralCategory {
    if kind == PluralKind::Ordinal {
        return PluralCategory::Other;
    }
    if polish && n == 1.0 {
        return PluralCategory::One;
    }
    if n.fract() != 0.0 {
        return PluralCategory::Other;
    }
    let (m10, m100) = (n % 10.0, n % 100.0);
    if !polish && m10 == 1.0 && m100 != 11.0 {
        PluralCategory::One
    } else if (2.0..=4.0).contains(&m10) && !(12.0..=14.0).contains(&m100) {
        PluralCategory::Few
    } else {
        PluralCategory::Many
    }
}

fn arabic_category(kind: PluralKind, n: f64) -> PluralCategory {
    if kind == PluralKind::Ordinal || n.fract() != 0.0 {
        return PluralCategory::Other;
    }
    let m100 = n % 100.0;
    if n == 0.0 {
        PluralCategory::Zero
    } else if n == 1.0 {
        PluralCategory::One
    } else if n == 2.0 {
        PluralCategory::Two
    } else if (3.0..=10.0).contains(&m100) {
        PluralCategory::Few
    } else if (11.0..=99.0).contains(&m100) {
        PluralCategory::Many
    } else {
        PluralCategory::Other
    }
}

/// A browser stand-in for German, Polish, Russian and Arabic numbers, plural rules and
/// times, and a few zones.
pub struct FakeIntl;

impl FakeIntl {
    fn number_parts(
        &self,
        locale: &str,
        options: &NumberFormatOptions,
        value: f64,
    ) -> Option<Vec<FormatPart>> {
        let lang = language(locale)?;
        if lang == "en" {
            return EnglishIntl.format_number_to_parts(locale, options, value);
        }
        if !matches!(lang.as_str(), "de" | "pl" | "ru" | "ar")
            || options.notation == Notation::Compact
        {
            return None;
        }
        let arab = options.numbering_system.as_deref() == Some("arab");
        let parts = EnglishIntl.format_number_to_parts("en", options, value)?;
        let mut out = Vec::new();
        for part in parts {
            let FormatPart { kind, value } = part;
            match (lang.as_str(), &kind) {
                ("de", PartType::Decimal) => out.push(FormatPart::new(kind, ",")),
                ("de", PartType::Group) => out.push(FormatPart::new(kind, ".")),
                ("pl" | "ru", PartType::Decimal) => out.push(FormatPart::new(kind, ",")),
                ("pl" | "ru", PartType::Group) => out.push(FormatPart::new(kind, "\u{a0}")),
                ("de" | "ru", PartType::PercentSign) => {
                    out.push(FormatPart::new(PartType::Literal, "\u{a0}"));
                    out.push(FormatPart::new(kind, value));
                }
                ("ar", PartType::Decimal) => {
                    out.push(FormatPart::new(kind, if arab { "\u{66b}" } else { "." }))
                }
                ("ar", PartType::Group) => {
                    out.push(FormatPart::new(kind, if arab { "\u{66c}" } else { "," }))
                }
                ("ar", PartType::MinusSign) => out.push(FormatPart::new(kind, "\u{61c}-")),
                ("ar", PartType::PlusSign) => out.push(FormatPart::new(kind, "\u{61c}+")),
                _ => out.push(FormatPart::new(kind, value)),
            }
        }
        Some(out)
    }
}

impl Intl for FakeIntl {
    fn canonicalize_locale(&self, tag: &str) -> Option<String> {
        EnglishIntl.canonicalize_locale(tag)
    }

    fn parse_locale(&self, tag: &str) -> Option<LocaleInfo> {
        EnglishIntl.parse_locale(tag)
    }

    fn maximize_locale(&self, tag: &str) -> Option<LocaleInfo> {
        EnglishIntl.maximize_locale(tag)
    }

    fn locale_text_direction(&self, _tag: &str) -> Option<TextDirection> {
        None
    }

    fn locale_week_first_day(&self, _tag: &str) -> Option<u32> {
        None
    }

    fn plural_category(&self, locale: &str, kind: PluralKind, n: f64) -> Option<PluralCategory> {
        let abs = n.abs();
        match language(locale)?.as_str() {
            "en" => EnglishIntl.plural_category(locale, kind, n),
            "de" => Some(match kind {
                PluralKind::Cardinal if abs == 1.0 => PluralCategory::One,
                _ => PluralCategory::Other,
            }),
            "ar" => Some(arabic_category(kind, abs)),
            "pl" => Some(ru_pl_category(kind, abs, true)),
            "ru" => Some(ru_pl_category(kind, abs, false)),
            _ => None,
        }
    }

    fn format_number(
        &self,
        locale: &str,
        options: &NumberFormatOptions,
        value: f64,
    ) -> Option<String> {
        self.number_parts(locale, options, value)
            .map(|parts| parts.into_iter().map(|p| p.value).collect())
    }

    fn format_number_to_parts(
        &self,
        locale: &str,
        options: &NumberFormatOptions,
        value: f64,
    ) -> Option<Vec<FormatPart>> {
        self.number_parts(locale, options, value)
    }

    fn resolved_numbering_system(&self, locale: &str, requested: Option<&str>) -> Option<String> {
        let resolved = EnglishIntl.resolved_numbering_system(locale, requested)?;
        let arabic_default = language(locale).as_deref() == Some("ar") && requested.is_none();
        let has_extension = EnglishIntl.parse_locale(locale)?.numbering_system.is_some();
        Some(if arabic_default && !has_extension {
            "arab".to_string()
        } else {
            resolved
        })
    }

    fn format_date_time(
        &self,
        locale: &str,
        options: &DateTimeFormatOptions,
        ms: f64,
    ) -> Option<String> {
        let lang = language(locale)?;
        let locale = if matches!(lang.as_str(), "de" | "ar") {
            "en"
        } else {
            locale
        };
        EnglishIntl.format_date_time(locale, &map_zone(options), ms)
    }

    fn format_date_time_to_parts(
        &self,
        locale: &str,
        options: &DateTimeFormatOptions,
        ms: f64,
    ) -> Option<Vec<FormatPart>> {
        let lang = language(locale)?;
        let locale = if matches!(lang.as_str(), "de" | "ar") {
            "en"
        } else {
            locale
        };
        EnglishIntl.format_date_time_to_parts(locale, &map_zone(options), ms)
    }

    fn default_time_zone(&self) -> Option<String> {
        Some("Asia/Tokyo".to_string())
    }

    fn format_default_time(&self, ms: f64) -> String {
        format!("default-time:{ms}")
    }

    fn segment(
        &self,
        locale: Option<&str>,
        granularity: Granularity,
        text: &str,
    ) -> Option<Vec<Segment>> {
        EnglishIntl.segment(locale, granularity, text)
    }

    fn to_upper_case(&self, text: &str, locale: Option<&str>) -> Option<String> {
        EnglishIntl.to_upper_case(text, locale)
    }
}

/// One request a [`RecordingIntl`] saw.
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    Number {
        locale: String,
        options: NumberFormatOptions,
        value: f64,
    },
    Date {
        locale: String,
        options: DateTimeFormatOptions,
        ms: f64,
    },
}

/// A backend that answers `N[locale]value` / `D[locale]ms` and records the requests. The
/// locales in `reject` fail like an `Intl` constructor that throws. Parts, locale and
/// plural queries are answered like [`EnglishIntl`] so symbol tables still build.
#[derive(Default)]
pub struct RecordingIntl {
    pub calls: Mutex<Vec<Call>>,
    pub reject: Mutex<Vec<String>>,
}

impl RecordingIntl {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn calls(&self) -> Vec<Call> {
        Self::lock(&self.calls).clone()
    }

    pub fn clear(&self) {
        Self::lock(&self.calls).clear();
    }

    pub fn reject_locale(&self, tag: &str) {
        Self::lock(&self.reject).push(tag.to_string());
    }

    fn rejected(&self, locale: &str) -> bool {
        Self::lock(&self.reject).iter().any(|r| r == locale)
    }
}

impl Intl for RecordingIntl {
    fn canonicalize_locale(&self, tag: &str) -> Option<String> {
        EnglishIntl.canonicalize_locale(tag)
    }

    fn parse_locale(&self, tag: &str) -> Option<LocaleInfo> {
        EnglishIntl.parse_locale(tag)
    }

    fn maximize_locale(&self, tag: &str) -> Option<LocaleInfo> {
        EnglishIntl.maximize_locale(tag)
    }

    fn locale_text_direction(&self, _tag: &str) -> Option<TextDirection> {
        None
    }

    fn locale_week_first_day(&self, _tag: &str) -> Option<u32> {
        None
    }

    fn plural_category(&self, locale: &str, kind: PluralKind, n: f64) -> Option<PluralCategory> {
        if self.rejected(locale) {
            return None;
        }
        EnglishIntl.plural_category("en", kind, n)
    }

    fn format_number(
        &self,
        locale: &str,
        options: &NumberFormatOptions,
        value: f64,
    ) -> Option<String> {
        Self::lock(&self.calls).push(Call::Number {
            locale: locale.to_string(),
            options: options.clone(),
            value,
        });
        (!self.rejected(locale)).then(|| format!("N[{locale}]{value}"))
    }

    fn format_number_to_parts(
        &self,
        locale: &str,
        options: &NumberFormatOptions,
        value: f64,
    ) -> Option<Vec<FormatPart>> {
        (!self.rejected(locale))
            .then(|| EnglishIntl.format_number_to_parts("en", options, value))
            .flatten()
    }

    fn resolved_numbering_system(&self, locale: &str, requested: Option<&str>) -> Option<String> {
        EnglishIntl.resolved_numbering_system(locale, requested)
    }

    fn format_date_time(
        &self,
        locale: &str,
        options: &DateTimeFormatOptions,
        ms: f64,
    ) -> Option<String> {
        Self::lock(&self.calls).push(Call::Date {
            locale: locale.to_string(),
            options: options.clone(),
            ms,
        });
        (!self.rejected(locale)).then(|| format!("D[{locale}]{ms}"))
    }

    fn format_date_time_to_parts(
        &self,
        locale: &str,
        options: &DateTimeFormatOptions,
        ms: f64,
    ) -> Option<Vec<FormatPart>> {
        Self::lock(&self.calls).push(Call::Date {
            locale: locale.to_string(),
            options: options.clone(),
            ms,
        });
        (!self.rejected(locale)).then(|| {
            vec![
                FormatPart::new(PartType::Year, "2000"),
                FormatPart::new(PartType::Month, "January"),
            ]
        })
    }

    fn default_time_zone(&self) -> Option<String> {
        None
    }

    fn format_default_time(&self, ms: f64) -> String {
        format!("default-time:{ms}")
    }

    fn segment(
        &self,
        _locale: Option<&str>,
        _granularity: Granularity,
        _text: &str,
    ) -> Option<Vec<Segment>> {
        None
    }

    fn to_upper_case(&self, _text: &str, _locale: Option<&str>) -> Option<String> {
        None
    }
}

/// Insert `(key, value)` rows into a map.
pub fn map(rows: &[(&str, &str)]) -> HashMap<String, String> {
    rows.iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}
