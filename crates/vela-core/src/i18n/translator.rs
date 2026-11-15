//! The chart's translator: resolved locale settings, message lookup and formatters, in one
//! object that stays the same across updates (it re-resolves in place).

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::format::calendar::locale_week_start;
use crate::format::formatters::{ResolvedFormatters, resolve_formatters};
use crate::format::types::{FormatSettings, HourCycleSetting};
use crate::intl::{Intl, TextDirection, default_intl};

use super::icu::{MessageValue, MessageValues, format_message};
use super::locale::{
    canonical_locale, fallback_chain, language_of, locale_direction, resolve_numbering_system,
};
use super::message::{MessageArg, MessageDescriptor, MessageLike, bind};
use super::registry::LocaleRegistry;
use super::types::{DirectionOption, LocalizationOptions, LocalizationPatch, Translator};

/// Message arguments with nested messages resolved through `resolve`.
pub(crate) fn resolve_args(
    args: &[(Cow<'static, str>, MessageArg)],
    resolve: &dyn Fn(&MessageDescriptor) -> String,
) -> MessageValues {
    let mut out: MessageValues = HashMap::with_capacity(args.len());
    for (name, value) in args {
        let value = match value {
            MessageArg::Text(t) => MessageValue::Text(t.to_string()),
            MessageArg::Number(n) => MessageValue::Number(*n),
            MessageArg::Message(d) => MessageValue::Text(resolve(d)),
        };
        out.insert(name.to_string(), value);
    }
    out
}

/// A descriptor's bound values overlaid with call-site arguments (which win).
pub(crate) fn merged_args(
    bound: &[(Cow<'static, str>, MessageArg)],
    args: &[(&str, MessageArg)],
) -> Vec<(Cow<'static, str>, MessageArg)> {
    let mut all = bound.to_vec();
    for (name, value) in args {
        bind(&mut all, Cow::Owned((*name).to_string()), value.clone());
    }
    all
}

/// The concrete translator. Hosts receive it as a [`Translator`].
pub struct LocaleState {
    opts: LocalizationOptions,
    registry: Arc<LocaleRegistry>,
    intl: Arc<dyn Intl>,
    settings: FormatSettings,
    chain: Vec<String>,
    direction: TextDirection,
    week_start: u8,
    formatters: ResolvedFormatters,
}

impl LocaleState {
    /// A translator over `intl` that reads packs from `registry`.
    pub fn new(
        options: LocalizationOptions,
        intl: Arc<dyn Intl>,
        registry: Arc<LocaleRegistry>,
    ) -> Self {
        let settings = FormatSettings::english_with(Arc::clone(&intl));
        let formatters = resolve_formatters(settings.clone(), None);
        let mut state = Self {
            opts: options,
            registry,
            intl,
            settings,
            chain: Vec::new(),
            direction: TextDirection::Ltr,
            week_start: 0,
            formatters,
        };
        state.resolve();
        state
    }

    /// The options as last set (unresolved).
    pub fn options(&self) -> &LocalizationOptions {
        &self.opts
    }

    /// Whether the host chose a locale; Vela stamps `lang` only then.
    pub fn locale_explicit(&self) -> bool {
        self.opts.locale.is_some()
    }

    /// Whether the host chose a direction; Vela stamps `dir` only then.
    pub fn direction_explicit(&self) -> bool {
        self.opts.direction.is_some()
    }

    /// The backend this translator formats through.
    pub fn intl(&self) -> &Arc<dyn Intl> {
        &self.intl
    }

    /// The registry this translator reads packs from.
    pub fn registry(&self) -> &Arc<LocaleRegistry> {
        &self.registry
    }

    /// Apply a change: fields set replace, cleared fields restore the default, kept
    /// fields keep their value.
    pub fn update(&mut self, patch: LocalizationPatch) {
        patch.locale.apply(&mut self.opts.locale);
        patch.direction.apply(&mut self.opts.direction);
        patch.messages.apply(&mut self.opts.messages);
        patch
            .numbering_system
            .apply(&mut self.opts.numbering_system);
        patch.hour_cycle.apply(&mut self.opts.hour_cycle);
        patch.week_start.apply(&mut self.opts.week_start);
        patch.formatters.apply(&mut self.opts.formatters);
        self.resolve();
    }

    /// Switch the locale, leaving every other option as it is.
    pub fn set_locale(&mut self, tag: impl Into<String>) {
        self.update(LocalizationPatch::locale(tag));
    }

    fn resolve(&mut self) {
        let o = &self.opts;
        let intl = self.intl.as_ref();
        let locale = canonical_locale(intl, o.locale.as_deref());
        let numbering_system =
            resolve_numbering_system(intl, &locale, o.numbering_system.as_deref());
        let hour_cycle = match o.hour_cycle {
            Some(HourCycleSetting::H12) => HourCycleSetting::H12,
            Some(HourCycleSetting::Locale) => HourCycleSetting::Locale,
            _ => HourCycleSetting::H23,
        };
        let english = language_of(intl, &locale) == "en";
        self.settings = FormatSettings::new(
            locale.clone(),
            numbering_system,
            hour_cycle,
            english,
            Arc::clone(&self.intl),
        );
        self.chain = fallback_chain(intl, &locale);
        self.direction = match o.direction {
            Some(DirectionOption::Rtl) => TextDirection::Rtl,
            Some(DirectionOption::Ltr) | None => TextDirection::Ltr,
            Some(DirectionOption::Auto) => locale_direction(intl, &locale),
        };
        self.week_start = match o.week_start {
            Some(w @ (0 | 1 | 6)) => w,
            _ => locale_week_start(intl, &locale),
        };
        self.formatters = resolve_formatters(self.settings.clone(), o.formatters.as_ref());
    }

    /// The resolved settings the built-in formatters and calendar helpers read.
    pub fn format_settings(&self) -> &FormatSettings {
        &self.settings
    }

    fn format_descriptor(&self, d: &MessageDescriptor, args: &[(&str, MessageArg)]) -> String {
        let all = merged_args(&d.values, args);
        let values = resolve_args(&all, &|m| self.t(&MessageLike::Message(m.clone())));
        let override_text = self
            .opts
            .messages
            .as_ref()
            .and_then(|m| m.get(d.id.as_ref()));
        let packs;
        let source: &str = match override_text {
            Some(text) => text,
            None => {
                packs = self.registry.messages_for(&self.chain);
                packs
                    .get(d.id.as_ref())
                    .map_or(d.default_message.as_ref(), String::as_str)
            }
        };
        match format_message(source, Some(&values), &self.settings) {
            Ok(text) => text,
            Err(_) => {
                if source == d.default_message {
                    return d.default_message.to_string();
                }
                format_message(&d.default_message, Some(&values), &self.settings)
                    .unwrap_or_else(|_| d.default_message.to_string())
            }
        }
    }
}

impl Translator for LocaleState {
    fn locale(&self) -> &str {
        &self.settings.locale
    }

    fn direction(&self) -> TextDirection {
        self.direction
    }

    fn numbering_system(&self) -> &str {
        &self.settings.numbering_system
    }

    fn hour_cycle(&self) -> HourCycleSetting {
        self.settings.hour_cycle
    }

    fn week_start(&self) -> u8 {
        self.week_start
    }

    fn t_with(&self, message: &MessageLike, args: &[(&str, MessageArg)]) -> String {
        match message {
            MessageLike::Text(t) => t.to_string(),
            MessageLike::Message(d) => self.format_descriptor(d, args),
        }
    }

    fn format(&self) -> &ResolvedFormatters {
        &self.formatters
    }

    fn locale_explicit(&self) -> bool {
        LocaleState::locale_explicit(self)
    }
}

/// A standalone translator for UI built outside a chart (a kit component, a host panel).
/// Charts, workspaces and widgets carry their own.
pub fn create_translator(
    options: LocalizationOptions,
    intl: Arc<dyn Intl>,
    registry: Arc<LocaleRegistry>,
) -> LocaleState {
    LocaleState::new(options, intl, registry)
}

/// The English translator components fall back to when none is handed in: no packs,
/// English text, the built-in [`crate::intl::EnglishIntl`].
pub fn english_translator() -> &'static LocaleState {
    static ENGLISH: OnceLock<LocaleState> = OnceLock::new();
    ENGLISH.get_or_init(|| {
        let intl = default_intl();
        let registry = LocaleRegistry::new(Arc::clone(&intl));
        LocaleState::new(LocalizationOptions::default(), intl, registry)
    })
}

/// The format settings behind any translator.
pub fn format_settings_of(i18n: Option<&dyn Translator>) -> FormatSettings {
    match i18n {
        Some(t) => t.format().settings().clone(),
        None => english_translator().format_settings().clone(),
    }
}
