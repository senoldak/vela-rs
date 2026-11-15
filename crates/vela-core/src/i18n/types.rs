//! The public vocabulary of Vela's message system.

use std::collections::HashMap;
use std::sync::Arc;

pub use crate::intl::TextDirection;

use crate::format::ResolvedFormatters;
use crate::format::types::{HourCycleSetting, VelaFormatters};

use super::message::{MessageArg, MessageLike};

/// One language's messages: id to ICU message. Several packs may share a locale.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalePack {
    /// BCP 47 tag: `"de"`, `"pt-BR"`, `"zh-Hant"`.
    pub locale: String,
    /// The direction the language reads in, set on right-to-left packs. It is
    /// information for the host: the chrome follows the `direction` option, and
    /// `Auto` resolves to the same from the locale's script.
    pub direction: Option<TextDirection>,
    pub messages: Arc<HashMap<String, String>>,
}

/// The `direction` option: a fixed direction, or follow the locale's script.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DirectionOption {
    Ltr,
    Rtl,
    Auto,
}

/// Language, direction, text and number formatting of a chart, a workspace or a widget.
/// Every field is optional: with none set the chart reads as it always has (English,
/// left to right, Western digits, 24-hour clock).
#[derive(Clone, Debug, Default)]
pub struct LocalizationOptions {
    /// BCP 47 tag for UI text, dates and numbers. Default `"en"`. Register the matching
    /// pack for translated text; numbers and dates follow the tag without one.
    pub locale: Option<String>,
    /// Direction of the chart's chrome. `Auto` follows the locale's script (Arabic,
    /// Hebrew, Persian, Urdu read right to left). Default left to right. The plot itself
    /// never mirrors.
    pub direction: Option<DirectionOption>,
    /// Per-message overrides on top of the registered packs, for the current locale.
    pub messages: Option<HashMap<String, String>>,
    /// Digits of every chart number. Default `latn`: Western digits in every language,
    /// Arabic included. `locale` uses the locale's own digits; any CLDR numbering system
    /// id (`arab`, `deva`, ...) picks one.
    pub numbering_system: Option<String>,
    /// Clock of chart times. Default `H23` (`14:30`).
    pub hour_cycle: Option<HourCycleSetting>,
    /// First day of the week in date pickers: 0 Sunday, 1 Monday, 6 Saturday (any other
    /// value is ignored). Default: the locale's.
    pub week_start: Option<u8>,
    /// Replace any chart formatter.
    pub formatters: Option<VelaFormatters>,
}

/// One field of a [`LocalizationPatch`]: leave it, set it, or restore its default.
#[derive(Clone, Debug, Default)]
pub enum Patch<T> {
    #[default]
    Keep,
    Set(T),
    /// The original's explicit `undefined`: restore the default.
    Clear,
}

impl<T> Patch<T> {
    pub(crate) fn apply(self, slot: &mut Option<T>) {
        match self {
            Patch::Keep => {}
            Patch::Set(v) => *slot = Some(v),
            Patch::Clear => *slot = None,
        }
    }
}

/// A change to a translator's options: fields set replace, cleared fields restore the
/// default, kept fields stay.
#[derive(Clone, Debug, Default)]
pub struct LocalizationPatch {
    pub locale: Patch<String>,
    pub direction: Patch<DirectionOption>,
    pub messages: Patch<HashMap<String, String>>,
    pub numbering_system: Patch<String>,
    pub hour_cycle: Patch<HourCycleSetting>,
    pub week_start: Patch<u8>,
    pub formatters: Patch<VelaFormatters>,
}

impl LocalizationPatch {
    /// A patch that only sets the locale.
    pub fn locale(tag: impl Into<String>) -> Self {
        Self {
            locale: Patch::Set(tag.into()),
            ..Self::default()
        }
    }
}

/// A chart's translator (`chart.i18n`, `ctx.i18n`): its resolved locale settings, message
/// lookup and formatters. One object per chart for the chart's lifetime: after an update
/// it reads the new settings.
pub trait Translator: Send + Sync {
    /// The canonical locale tag.
    fn locale(&self) -> &str;
    /// The chrome direction (`Auto` resolved).
    fn direction(&self) -> TextDirection;
    /// The numbering system of chart numbers (`latn` unless asked otherwise).
    fn numbering_system(&self) -> &str;
    /// The clock of chart times.
    fn hour_cycle(&self) -> HourCycleSetting;
    /// The first day of the week in date pickers (0 Sunday, 1 Monday, 6 Saturday).
    fn week_start(&self) -> u8;
    /// Resolve display text. Plain text comes back verbatim. A descriptor resolves
    /// through the `messages` overrides, then the registered packs for the locale and its
    /// fallbacks (`pt-BR`, `pt`, `en`), then its English `default_message`, and is
    /// formatted with its bound values and `args` (which take precedence). Never fails: a
    /// malformed translation falls back to English.
    fn t_with(&self, message: &MessageLike, args: &[(&str, MessageArg)]) -> String;
    /// The chart's resolved formatters.
    fn format(&self) -> &ResolvedFormatters;

    /// Resolve display text with no extra arguments.
    fn t(&self, message: &MessageLike) -> String {
        self.t_with(message, &[])
    }

    /// Whether the host chose a locale; the wall-clock helpers read the page's own
    /// conventions until it does. A host implementation that always has one keeps `true`.
    fn locale_explicit(&self) -> bool {
        true
    }
}

/// Payload of the `locale:changed` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocaleChangedEvent {
    pub locale: String,
    pub direction: TextDirection,
}
