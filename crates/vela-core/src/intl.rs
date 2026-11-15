//! The `Intl` seam: everything the original reads from the browser's `Intl` API (and
//! two `Date`/`String` locale helpers), as one object-safe trait over plain Rust data.
//!
//! The crate never links to a JavaScript engine. All logic that sits on top of `Intl`
//! (message formatting, fallback chains, number and date shapes, digit localization,
//! zone offsets) lives in this crate and calls an [`Intl`] backend where the original
//! calls `Intl`. Hosts hand the backend in explicitly (`Arc<dyn Intl>` held by the
//! translator and the format settings); there is no global.
//!
//! Two backends exist: [`EnglishIntl`] here (English only, no platform support) and a
//! browser one in `vela-web` over `js_sys::Intl`.
//!
//! # Contract for implementors
//!
//! - Every method maps to one JavaScript expression, named in its doc comment. A method
//!   that can throw in JavaScript (an invalid locale, an unknown time zone, an invalid
//!   option) returns `None`; the callers reproduce the original's `try`/`catch`
//!   fallbacks from that. A backend must never panic.
//! - Backends should cache the `Intl` objects they create, keyed on the locale and the
//!   option struct (every option type is `Eq + Hash`). Callers do not cache them.
//! - `ms` arguments are epoch milliseconds that are already a valid `Date` time value
//!   (finite, within +-8.64e15, truncated to an integer), so `new Date(ms)` never
//!   yields an invalid date.
//! - Text arguments are Rust `&str` (UTF-8); text results are UTF-16-agnostic except
//!   [`Segment::index`], which keeps JavaScript's UTF-16 unit index.

use std::sync::Arc;

/// A writing direction (`'ltr'` | `'rtl'`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextDirection {
    #[default]
    Ltr,
    Rtl,
}

impl TextDirection {
    /// The CSS / BCP 47 spelling: `"ltr"` or `"rtl"`.
    pub fn as_str(self) -> &'static str {
        match self {
            TextDirection::Ltr => "ltr",
            TextDirection::Rtl => "rtl",
        }
    }
}

/// The fields the original reads off an `Intl.Locale`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocaleInfo {
    /// `locale.language` (`"pt"`).
    pub language: String,
    /// `locale.script` (`"Latn"`), when the tag carries one.
    pub script: Option<String>,
    /// `locale.region` (`"BR"`), when the tag carries one.
    pub region: Option<String>,
    /// `locale.baseName`: language, script, region and variants, without extensions.
    pub base_name: String,
    /// `locale.numberingSystem`: the `-u-nu-` extension (`"arab"`), when present.
    pub numbering_system: Option<String>,
}

/// `Intl.PluralRules` `type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PluralKind {
    Cardinal,
    Ordinal,
}

/// A CLDR plural category (the result of `Intl.PluralRules#select`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PluralCategory {
    Zero,
    One,
    Two,
    Few,
    Many,
    Other,
}

impl PluralCategory {
    /// The selector a message option is keyed on (`"one"`, `"few"`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            PluralCategory::Zero => "zero",
            PluralCategory::One => "one",
            PluralCategory::Two => "two",
            PluralCategory::Few => "few",
            PluralCategory::Many => "many",
            PluralCategory::Other => "other",
        }
    }

    /// The category named by a `select` result, `None` for an unknown name.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "zero" => PluralCategory::Zero,
            "one" => PluralCategory::One,
            "two" => PluralCategory::Two,
            "few" => PluralCategory::Few,
            "many" => PluralCategory::Many,
            "other" => PluralCategory::Other,
            _ => return None,
        })
    }
}

/// `Intl.NumberFormat` `style`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NumberStyle {
    #[default]
    Decimal,
    Percent,
    /// A measurement unit; [`NumberFormatOptions::unit`] names it and
    /// [`NumberFormatOptions::unit_display`] how it is written.
    Unit,
}

/// `Intl.NumberFormat` `unit`: the units of [`crate::format`]'s span durations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NumberUnit {
    Day,
    Hour,
    Minute,
    Second,
}

impl NumberUnit {
    /// The sanctioned unit identifier (`"day"`, `"hour"`, `"minute"`, `"second"`).
    pub fn as_str(self) -> &'static str {
        match self {
            NumberUnit::Day => "day",
            NumberUnit::Hour => "hour",
            NumberUnit::Minute => "minute",
            NumberUnit::Second => "second",
        }
    }
}

/// `Intl.NumberFormat` `unitDisplay` (only `"narrow"` is ever requested).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnitDisplay {
    Narrow,
}

/// `Intl.NumberFormat` `notation`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Notation {
    #[default]
    Standard,
    Compact,
}

/// `Intl.NumberFormat` `compactDisplay` (only `"short"` is ever requested).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompactDisplay {
    Short,
}

/// `Intl.NumberFormat` `signDisplay`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SignDisplay {
    #[default]
    Auto,
    Always,
}

/// The `Intl.NumberFormat` options the original passes. `None` leaves the engine default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct NumberFormatOptions {
    pub style: NumberStyle,
    /// Required (and only meaningful) when `style` is [`NumberStyle::Unit`].
    pub unit: Option<NumberUnit>,
    pub unit_display: Option<UnitDisplay>,
    pub notation: Notation,
    pub compact_display: Option<CompactDisplay>,
    pub sign_display: Option<SignDisplay>,
    /// `useGrouping`; the original only ever passes `Some(false)`.
    pub use_grouping: Option<bool>,
    pub minimum_fraction_digits: Option<u32>,
    pub maximum_fraction_digits: Option<u32>,
    /// `numberingSystem` (`"latn"`, `"arab"`, ...).
    pub numbering_system: Option<String>,
}

/// `Intl.DateTimeFormat` `hourCycle`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HourCycle {
    H11,
    H12,
    H23,
    H24,
}

/// A numeric field style (`year`, `day`, `hour`, `minute`, `second`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NumericStyle {
    Numeric,
    TwoDigit,
}

/// `Intl.DateTimeFormat` `month`, as the original requests it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MonthStyle {
    TwoDigit,
    Short,
    Long,
}

/// `Intl.DateTimeFormat` `weekday` (only `"short"` is ever requested).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WeekdayStyle {
    Short,
}

/// `Intl.DateTimeFormat` `timeZoneName` (only `"shortOffset"` is ever requested).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimeZoneNameStyle {
    ShortOffset,
}

/// The `Intl.DateTimeFormat` options the original passes. With no date or time field set
/// the engine's own default applies (year, month and day), as in JavaScript.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct DateTimeFormatOptions {
    /// An IANA id, `"UTC"` or an offset id; `None` is the host's zone.
    pub time_zone: Option<String>,
    /// `calendar` (the original only passes `"gregory"`).
    pub calendar: Option<String>,
    pub numbering_system: Option<String>,
    pub hour_cycle: Option<HourCycle>,
    /// `hour12` (the original only passes `Some(false)`).
    pub hour12: Option<bool>,
    pub weekday: Option<WeekdayStyle>,
    pub year: Option<NumericStyle>,
    pub month: Option<MonthStyle>,
    pub day: Option<NumericStyle>,
    pub hour: Option<NumericStyle>,
    pub minute: Option<NumericStyle>,
    pub second: Option<NumericStyle>,
    pub time_zone_name: Option<TimeZoneNameStyle>,
}

/// The `type` of a `formatToParts` entry. [`PartType::Other`] carries any name the
/// original never reads.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PartType {
    Literal,
    Integer,
    Group,
    Decimal,
    Fraction,
    MinusSign,
    PlusSign,
    PercentSign,
    Compact,
    Unit,
    Nan,
    Infinity,
    Weekday,
    Year,
    Month,
    Day,
    DayPeriod,
    Hour,
    Minute,
    Second,
    TimeZoneName,
    Other(String),
}

impl PartType {
    /// The part type for a `formatToParts` `type` string.
    pub fn from_name(name: &str) -> Self {
        match name {
            "literal" => PartType::Literal,
            "integer" => PartType::Integer,
            "group" => PartType::Group,
            "decimal" => PartType::Decimal,
            "fraction" => PartType::Fraction,
            "minusSign" => PartType::MinusSign,
            "plusSign" => PartType::PlusSign,
            "percentSign" => PartType::PercentSign,
            "compact" => PartType::Compact,
            "unit" => PartType::Unit,
            "nan" => PartType::Nan,
            "infinity" => PartType::Infinity,
            "weekday" => PartType::Weekday,
            "year" => PartType::Year,
            "month" => PartType::Month,
            "day" => PartType::Day,
            "dayPeriod" => PartType::DayPeriod,
            "hour" => PartType::Hour,
            "minute" => PartType::Minute,
            "second" => PartType::Second,
            "timeZoneName" => PartType::TimeZoneName,
            other => PartType::Other(other.to_string()),
        }
    }

    /// The `formatToParts` `type` string.
    pub fn name(&self) -> &str {
        match self {
            PartType::Literal => "literal",
            PartType::Integer => "integer",
            PartType::Group => "group",
            PartType::Decimal => "decimal",
            PartType::Fraction => "fraction",
            PartType::MinusSign => "minusSign",
            PartType::PlusSign => "plusSign",
            PartType::PercentSign => "percentSign",
            PartType::Compact => "compact",
            PartType::Unit => "unit",
            PartType::Nan => "nan",
            PartType::Infinity => "infinity",
            PartType::Weekday => "weekday",
            PartType::Year => "year",
            PartType::Month => "month",
            PartType::Day => "day",
            PartType::DayPeriod => "dayPeriod",
            PartType::Hour => "hour",
            PartType::Minute => "minute",
            PartType::Second => "second",
            PartType::TimeZoneName => "timeZoneName",
            PartType::Other(name) => name,
        }
    }
}

/// One entry of a `formatToParts` result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatPart {
    pub kind: PartType,
    pub value: String,
}

impl FormatPart {
    pub fn new(kind: PartType, value: impl Into<String>) -> Self {
        Self {
            kind,
            value: value.into(),
        }
    }
}

/// `Intl.Segmenter` `granularity`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Granularity {
    Grapheme,
    Word,
}

/// One `Intl.Segmenter` segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub segment: String,
    /// The segment's start as a UTF-16 code unit index into the text, as JavaScript
    /// reports it.
    pub index: usize,
    /// `isWordLike`: `Some` for word granularity, `None` for grapheme granularity.
    pub is_word_like: Option<bool>,
}

/// The browser `Intl` capabilities the library uses. See the module docs for the
/// contract. Every method is one JavaScript expression.
pub trait Intl: Send + Sync {
    /// `Intl.getCanonicalLocales(tag)[0]` (`"pt-br"` becomes `"pt-BR"`, `"iw"` becomes
    /// `"he"`). `None` when it throws.
    fn canonicalize_locale(&self, tag: &str) -> Option<String>;

    /// `new Intl.Locale(tag)`, read into plain fields. `None` when it throws. The fields
    /// are the canonical ones (`new Intl.Locale('iw').language` is `"he"`).
    fn parse_locale(&self, tag: &str) -> Option<LocaleInfo>;

    /// `new Intl.Locale(tag).maximize()` (add likely subtags: `"pt"` becomes `pt-Latn-BR`).
    /// `None` when `tag` is invalid or the engine cannot maximize.
    fn maximize_locale(&self, tag: &str) -> Option<LocaleInfo>;

    /// `new Intl.Locale(tag).getTextInfo().direction` (older engines:
    /// `.textInfo.direction`). `None` when the tag is invalid or the engine has no text
    /// info; the caller then infers the direction from the script.
    fn locale_text_direction(&self, tag: &str) -> Option<TextDirection>;

    /// `new Intl.Locale(tag).getWeekInfo().firstDay` (older engines: `.weekInfo.firstDay`),
    /// the raw `1..=7` value (1 Monday, 7 Sunday). `None` when the tag is invalid or the
    /// engine has no week info; the caller then uses its region table.
    fn locale_week_first_day(&self, tag: &str) -> Option<u32>;

    /// `new Intl.PluralRules(locale, { type }).select(n)`. `None` when the constructor
    /// throws (the caller retries with `"en"`).
    fn plural_category(&self, locale: &str, kind: PluralKind, n: f64) -> Option<PluralCategory>;

    /// `new Intl.NumberFormat(locale, options).format(value)`. `None` when the
    /// constructor throws (an unknown locale, a malformed option).
    fn format_number(
        &self,
        locale: &str,
        options: &NumberFormatOptions,
        value: f64,
    ) -> Option<String>;

    /// `new Intl.NumberFormat(locale, options).formatToParts(value)`.
    fn format_number_to_parts(
        &self,
        locale: &str,
        options: &NumberFormatOptions,
        value: f64,
    ) -> Option<Vec<FormatPart>>;

    /// `new Intl.NumberFormat(locale, requested.map(|n| ({ numberingSystem: n }))).resolvedOptions().numberingSystem`:
    /// the numbering system a locale (and an optional request) resolves to. `None` when
    /// the constructor throws.
    fn resolved_numbering_system(&self, locale: &str, requested: Option<&str>) -> Option<String>;

    /// `new Intl.DateTimeFormat(locale, options).format(new Date(ms))`. `None` when the
    /// constructor throws (an unknown locale or time zone).
    fn format_date_time(
        &self,
        locale: &str,
        options: &DateTimeFormatOptions,
        ms: f64,
    ) -> Option<String>;

    /// `new Intl.DateTimeFormat(locale, options).formatToParts(new Date(ms))`.
    fn format_date_time_to_parts(
        &self,
        locale: &str,
        options: &DateTimeFormatOptions,
        ms: f64,
    ) -> Option<Vec<FormatPart>>;

    /// `new Intl.DateTimeFormat().resolvedOptions().timeZone`: the host's own zone.
    fn default_time_zone(&self) -> Option<String>;

    /// `new Date(ms).toLocaleTimeString()`: the time of day in the host's own locale and
    /// zone, the way the page has always written it.
    fn format_default_time(&self, ms: f64) -> String;

    /// `[...new Intl.Segmenter(locale, { granularity }).segment(text)]`. `None` when the
    /// engine has no `Intl.Segmenter` or the constructor throws.
    fn segment(
        &self,
        locale: Option<&str>,
        granularity: Granularity,
        text: &str,
    ) -> Option<Vec<Segment>>;

    /// `text.toLocaleUpperCase(locale)`. `None` when the locale tag is rejected (the
    /// caller then uses the default casing).
    fn to_upper_case(&self, text: &str, locale: Option<&str>) -> Option<String>;
}

/// The built-in backend of a host that supplies none: [`EnglishIntl`].
pub fn default_intl() -> Arc<dyn Intl> {
    Arc::new(EnglishIntl)
}

/// A built-in [`Intl`] that needs no platform support and is correct for English.
///
/// # What it supports
///
/// - Locale tags: parsing, canonicalization (case, `iw`/`in`/`ji`/`jw`/`mo` aliases,
///   subtag order) and maximization from a small likely-subtags table (the shipped
///   packs' languages plus a few common ones). Text direction and week information are
///   reported as unavailable, so callers use their script and region tables.
/// - Plural rules: English cardinal and ordinal.
/// - Numbers (locales `en`, `en-XX`): grouping (`en-IN` groups in lakhs), minimum and
///   maximum fraction digits, `percent`, `compact` short (`K`, `M`, `B`, `T`),
///   narrow `day`/`hour`/`minute`/`second` units, sign display, and the digits of the
///   common numbering systems (`arab`, `deva`, `thai`, ...). Rounding is half away from
///   zero on the shortest decimal form, as ICU does.
/// - Dates and clocks (locales `en`, `en-XX`): every field combination the library
///   requests, in the English patterns, with the Gregorian calendar, `h11`/`h12`/`h23`/`h24`
///   hour cycles and `timeZoneName: shortOffset`. A 12-hour clock writes `9:05 PM` with
///   an ordinary space (newer ICU versions write a narrow no-break space).
/// - Time zones: `UTC` and its aliases, `Etc/GMT+N`/`Etc/GMT-N` and offset ids
///   (`+05:30`). The default zone is UTC.
/// - Segmentation: an approximate grapheme segmenter (combining marks, variation
///   selectors, joiner sequences, regional-indicator pairs, CRLF) and a simple word
///   segmenter (letter and digit runs).
/// - Upper-casing: Unicode default casing, plus the dotted `i` of Turkish and Azeri.
///
/// # What it does not support
///
/// - Any non-English locale for plurals, numbers and dates: those calls return `None`,
///   which is how `Intl` reports an unusable locale, so callers degrade to English the
///   way the original does (a `try`/`catch` retry with `"en"`).
/// - IANA zone rules (`America/New_York` and every other region id): `None`, so a zone
///   offset reads 0 and a zone name reads `UTC`.
/// - Locale-specific week data and text direction (see above), other calendars, and
///   anything beyond the options in this module.
#[derive(Clone, Copy, Debug, Default)]
pub struct EnglishIntl;

// ---------------------------------------------------------------------------------------
// Locale tags
// ---------------------------------------------------------------------------------------

struct Tag {
    language: String,
    script: Option<String>,
    region: Option<String>,
    variants: Vec<String>,
    /// Extensions as (singleton, subtags), sorted by singleton.
    extensions: Vec<(char, Vec<String>)>,
    private_use: Vec<String>,
}

impl Tag {
    fn base_name(&self) -> String {
        let mut out = self.language.clone();
        for part in self
            .script
            .iter()
            .chain(self.region.iter())
            .chain(self.variants.iter())
        {
            out.push('-');
            out.push_str(part);
        }
        out
    }

    fn numbering_system(&self) -> Option<String> {
        let (_, subtags) = self.extensions.iter().find(|(s, _)| *s == 'u')?;
        let at = subtags.iter().position(|t| t == "nu")?;
        subtags
            .get(at + 1)
            .filter(|v| (3..=8).contains(&v.len()))
            .cloned()
    }

    fn canonical(&self) -> String {
        let mut out = self.base_name();
        for (singleton, subtags) in &self.extensions {
            out.push('-');
            out.push(*singleton);
            for s in subtags {
                out.push('-');
                out.push_str(s);
            }
        }
        if !self.private_use.is_empty() {
            out.push_str("-x");
            for s in &self.private_use {
                out.push('-');
                out.push_str(s);
            }
        }
        out
    }

    fn info(&self) -> LocaleInfo {
        LocaleInfo {
            language: self.language.clone(),
            script: self.script.clone(),
            region: self.region.clone(),
            base_name: self.base_name(),
            numbering_system: self.numbering_system(),
        }
    }
}

fn is_alpha(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphabetic())
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_alnum(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => {
            first.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
        }
        None => String::new(),
    }
}

/// Parse and canonicalize a BCP 47 tag the way `Intl.Locale` accepts it (`None` where it
/// throws a `RangeError`).
fn parse_tag(tag: &str) -> Option<Tag> {
    let mut parts = tag.split('-').peekable();
    let language = parts.next()?;
    if !is_alpha(language) || !matches!(language.len(), 2 | 3 | 5..=8) {
        return None;
    }
    let language = match language.to_ascii_lowercase().as_str() {
        "iw" => "he".to_string(),
        "in" => "id".to_string(),
        "ji" => "yi".to_string(),
        "jw" => "jv".to_string(),
        "mo" => "ro".to_string(),
        other => other.to_string(),
    };
    let mut out = Tag {
        language,
        script: None,
        region: None,
        variants: Vec::new(),
        extensions: Vec::new(),
        private_use: Vec::new(),
    };
    if let Some(p) = parts.peek()
        && p.len() == 4
        && is_alpha(p)
    {
        out.script = Some(title_case(p));
        parts.next();
    }
    if let Some(p) = parts.peek()
        && ((p.len() == 2 && is_alpha(p)) || (p.len() == 3 && is_digits(p)))
    {
        out.region = Some(p.to_ascii_uppercase());
        parts.next();
    }
    while let Some(p) = parts.peek() {
        let variant = (5..=8).contains(&p.len()) && is_alnum(p)
            || (p.len() == 4 && p.as_bytes()[0].is_ascii_digit() && is_alnum(p));
        if !variant {
            break;
        }
        let lower = p.to_ascii_lowercase();
        if out.variants.contains(&lower) {
            return None;
        }
        out.variants.push(lower);
        parts.next();
    }
    out.variants.sort();
    while let Some(p) = parts.next() {
        if p.len() != 1 || !is_alnum(p) {
            return None;
        }
        let singleton = p.to_ascii_lowercase().chars().next()?;
        let mut subtags = Vec::new();
        if singleton == 'x' {
            for s in parts.by_ref() {
                if !is_alnum(s) || s.len() > 8 {
                    return None;
                }
                subtags.push(s.to_ascii_lowercase());
            }
            if subtags.is_empty() {
                return None;
            }
            out.private_use = subtags;
            break;
        }
        while let Some(s) = parts.peek() {
            if s.len() == 1 {
                break;
            }
            if !is_alnum(s) || !(2..=8).contains(&s.len()) {
                return None;
            }
            subtags.push(s.to_ascii_lowercase());
            parts.next();
        }
        if subtags.is_empty() || out.extensions.iter().any(|(s, _)| *s == singleton) {
            return None;
        }
        out.extensions.push((singleton, subtags));
    }
    out.extensions.sort_by_key(|(s, _)| *s);
    Some(out)
}

/// Likely subtags: language, script, region.
const LIKELY: &[(&str, &str, &str)] = &[
    ("ar", "Arab", "EG"),
    ("bg", "Cyrl", "BG"),
    ("bn", "Beng", "BD"),
    ("ca", "Latn", "ES"),
    ("ckb", "Arab", "IQ"),
    ("cs", "Latn", "CZ"),
    ("da", "Latn", "DK"),
    ("de", "Latn", "DE"),
    ("dv", "Thaa", "MV"),
    ("el", "Grek", "GR"),
    ("en", "Latn", "US"),
    ("es", "Latn", "ES"),
    ("fa", "Arab", "IR"),
    ("fi", "Latn", "FI"),
    ("fr", "Latn", "FR"),
    ("he", "Hebr", "IL"),
    ("hi", "Deva", "IN"),
    ("hr", "Latn", "HR"),
    ("hu", "Latn", "HU"),
    ("id", "Latn", "ID"),
    ("it", "Latn", "IT"),
    ("ja", "Jpan", "JP"),
    ("ko", "Kore", "KR"),
    ("ks", "Arab", "IN"),
    ("ms", "Latn", "MY"),
    ("nb", "Latn", "NO"),
    ("nl", "Latn", "NL"),
    ("no", "Latn", "NO"),
    ("pl", "Latn", "PL"),
    ("ps", "Arab", "AF"),
    ("pt", "Latn", "BR"),
    ("ro", "Latn", "RO"),
    ("ru", "Cyrl", "RU"),
    ("sd", "Arab", "PK"),
    ("sk", "Latn", "SK"),
    ("sl", "Latn", "SI"),
    ("sr", "Cyrl", "RS"),
    ("sv", "Latn", "SE"),
    ("ta", "Taml", "IN"),
    ("te", "Telu", "IN"),
    ("th", "Thai", "TH"),
    ("tr", "Latn", "TR"),
    ("ug", "Arab", "CN"),
    ("uk", "Cyrl", "UA"),
    ("ur", "Arab", "PK"),
    ("vi", "Latn", "VN"),
    ("yi", "Hebr", "001"),
    ("zh", "Hans", "CN"),
];

fn maximize_tag(tag: &Tag) -> Tag {
    let mut out = Tag {
        language: tag.language.clone(),
        script: tag.script.clone(),
        region: tag.region.clone(),
        variants: tag.variants.clone(),
        extensions: tag.extensions.clone(),
        private_use: tag.private_use.clone(),
    };
    let Some((_, likely_script, likely_region)) =
        LIKELY.iter().find(|(l, _, _)| *l == tag.language)
    else {
        return out;
    };
    if tag.language == "zh" {
        if out.script.is_none() {
            let traditional = matches!(out.region.as_deref(), Some("TW" | "HK" | "MO"));
            out.script = Some(if traditional { "Hant" } else { "Hans" }.to_string());
        }
        if out.region.is_none() {
            out.region = Some(
                if out.script.as_deref() == Some("Hant") {
                    "TW"
                } else {
                    "CN"
                }
                .to_string(),
            );
        }
        return out;
    }
    if out.script.is_none() {
        out.script = Some((*likely_script).to_string());
    }
    if out.region.is_none() && out.script.as_deref() == Some(likely_script) {
        out.region = Some((*likely_region).to_string());
    }
    out
}

/// An English locale: language `en` with its region and `-u-nu-` extension.
struct EnglishLocale {
    region: Option<String>,
    numbering_system: Option<String>,
}

impl EnglishLocale {
    fn parse(tag: &str) -> Option<Self> {
        let parsed = parse_tag(tag)?;
        (parsed.language == "en").then(|| EnglishLocale {
            numbering_system: parsed.numbering_system(),
            region: parsed.region,
        })
    }

    /// Whether the locale's clock is 24-hour by default.
    fn default_hour_cycle(&self) -> HourCycle {
        match self.region.as_deref() {
            Some("GB" | "IE" | "ZA") => HourCycle::H23,
            _ => HourCycle::H12,
        }
    }

    /// Grouping sizes: `(primary, secondary)`.
    fn grouping(&self) -> (usize, usize) {
        match self.region.as_deref() {
            Some("IN") => (3, 2),
            _ => (3, 3),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Numbering systems
// ---------------------------------------------------------------------------------------

/// Zero digits of the numbering systems whose digits are a contiguous run.
const ZERO_DIGITS: &[(&str, u32)] = &[
    ("arab", 0x0660),
    ("arabext", 0x06F0),
    ("beng", 0x09E6),
    ("deva", 0x0966),
    ("fullwide", 0xFF10),
    ("gujr", 0x0AE6),
    ("guru", 0x0A66),
    ("khmr", 0x17E0),
    ("knda", 0x0CE6),
    ("laoo", 0x0ED0),
    ("mlym", 0x0D66),
    ("mymr", 0x1040),
    ("orya", 0x0B66),
    ("tamldec", 0x0BE6),
    ("telu", 0x0C66),
    ("thai", 0x0E50),
    ("tibt", 0x0F20),
];

/// The zero digit of the numbering system a request and a locale extension resolve to
/// (`'0'` for `latn`). `None` when the request is malformed (`RangeError`).
fn resolve_digits(requested: Option<&str>, extension: Option<&str>) -> Option<(String, char)> {
    if let Some(r) = requested
        && !((3..=8).contains(&r.len()) && is_alnum(r))
    {
        return None;
    }
    for candidate in requested.into_iter().chain(extension) {
        let id = candidate.to_ascii_lowercase();
        if id == "latn" {
            return Some((id, '0'));
        }
        if let Some((_, zero)) = ZERO_DIGITS.iter().find(|(n, _)| *n == id)
            && let Some(zero) = char::from_u32(*zero)
        {
            return Some((id, zero));
        }
    }
    Some(("latn".to_string(), '0'))
}

fn map_digits(text: &str, zero: char) -> String {
    if zero == '0' {
        return text.to_string();
    }
    text.chars()
        .map(|c| match c.to_digit(10) {
            Some(d) => char::from_u32(zero as u32 + d).unwrap_or(c),
            None => c,
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// Numbers
// ---------------------------------------------------------------------------------------

/// A non-negative decimal: `0.DIGITS x 10^point`. No digits means zero.
struct Decimal {
    digits: Vec<u8>,
    point: i32,
}

impl Decimal {
    /// The shortest decimal that round-trips `abs` (finite, non-negative), which is the
    /// value ICU rounds.
    fn from_f64(abs: f64) -> Decimal {
        let text = format!("{abs:e}");
        let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
        let mut digits: Vec<u8> = mantissa
            .bytes()
            .filter(u8::is_ascii_digit)
            .map(|b| b - b'0')
            .collect();
        while digits.last() == Some(&0) {
            digits.pop();
        }
        if digits.is_empty() {
            return Decimal { digits, point: 0 };
        }
        Decimal {
            digits,
            point: exponent.parse::<i32>().unwrap_or(0).saturating_add(1),
        }
    }

    fn is_zero(&self) -> bool {
        self.digits.is_empty()
    }

    /// Round half away from zero so that at most `max_fraction` fraction digits remain.
    fn round_to_fraction(&mut self, max_fraction: i32) {
        if self.is_zero() {
            return;
        }
        let keep = self.point.saturating_add(max_fraction);
        if keep < 0 {
            self.digits.clear();
            self.point = 0;
            return;
        }
        let keep = keep as usize;
        if keep >= self.digits.len() {
            return;
        }
        let up = self.digits[keep] >= 5;
        self.digits.truncate(keep);
        if up {
            let mut i = self.digits.len();
            loop {
                if i == 0 {
                    self.digits.insert(0, 1);
                    self.point += 1;
                    break;
                }
                i -= 1;
                if self.digits[i] == 9 {
                    self.digits[i] = 0;
                } else {
                    self.digits[i] += 1;
                    break;
                }
            }
        }
        while self.digits.last() == Some(&0) {
            self.digits.pop();
        }
        if self.digits.is_empty() {
            self.point = 0;
        }
    }

    fn integer_len(&self) -> usize {
        if self.is_zero() || self.point <= 0 {
            1
        } else {
            self.point as usize
        }
    }

    /// The integer digits (`"0"` when there are none).
    fn integer(&self) -> String {
        if self.is_zero() || self.point <= 0 {
            return "0".to_string();
        }
        let point = self.point as usize;
        (0..point)
            .map(|i| (b'0' + self.digits.get(i).copied().unwrap_or(0)) as char)
            .collect()
    }

    /// The fraction digits, trailing zeros trimmed down to `min_fraction`.
    fn fraction(&self, min_fraction: usize) -> String {
        let mut out: Vec<u8> = Vec::new();
        if !self.is_zero() {
            if self.point < 0 {
                out.extend(std::iter::repeat_n(0u8, (-self.point) as usize));
                out.extend(&self.digits);
            } else if (self.point as usize) < self.digits.len() {
                out.extend(&self.digits[self.point as usize..]);
            }
        }
        while out.len() > min_fraction && out.last() == Some(&0) {
            out.pop();
        }
        while out.len() < min_fraction {
            out.push(0);
        }
        out.iter().map(|d| (b'0' + d) as char).collect()
    }
}

fn group_integer(
    int: &str,
    (primary, secondary): (usize, usize),
    parts: &mut Vec<FormatPart>,
    zero: char,
) {
    let chars: Vec<char> = int.chars().collect();
    let n = chars.len();
    // Group boundaries counted from the right: `primary`, then every `secondary`.
    let mut cuts: Vec<usize> = Vec::new();
    let mut at = primary;
    while at < n {
        cuts.push(n - at);
        at += secondary;
    }
    cuts.reverse();
    let mut start = 0;
    for cut in cuts.into_iter().chain(std::iter::once(n)) {
        if start > 0 {
            parts.push(FormatPart::new(PartType::Group, ","));
        }
        let chunk: String = chars[start..cut].iter().collect();
        parts.push(FormatPart::new(PartType::Integer, map_digits(&chunk, zero)));
        start = cut;
    }
}

const COMPACT_SUFFIX: [&str; 5] = ["", "K", "M", "B", "T"];

fn english_number_parts(
    locale: &EnglishLocale,
    o: &NumberFormatOptions,
    value: f64,
) -> Option<Vec<FormatPart>> {
    let (_, zero) = resolve_digits(
        o.numbering_system.as_deref(),
        locale.numbering_system.as_deref(),
    )?;
    let compact = o.notation == Notation::Compact;
    let (default_min, default_max) = match o.style {
        NumberStyle::Percent => (0, 0),
        _ => (0, 3),
    };
    if o.style == NumberStyle::Unit && o.unit.is_none() {
        return None;
    }
    let explicit = o.minimum_fraction_digits.is_some() || o.maximum_fraction_digits.is_some();
    let (min_fd, max_fd) = match (o.minimum_fraction_digits, o.maximum_fraction_digits) {
        (Some(min), Some(max)) => (min, max),
        (Some(min), None) => (min, min.max(default_max)),
        (None, Some(max)) => (default_min.min(max), max),
        (None, None) => (default_min, default_max),
    };
    if min_fd > max_fd || max_fd > 100 {
        return None;
    }
    let negative = value.is_sign_negative() && !value.is_nan();
    let mut parts: Vec<FormatPart> = Vec::new();
    let sign = match o.sign_display.unwrap_or_default() {
        SignDisplay::Auto => negative.then_some(("-", PartType::MinusSign)),
        SignDisplay::Always => Some(if negative {
            ("-", PartType::MinusSign)
        } else {
            ("+", PartType::PlusSign)
        }),
    };
    if let Some((text, kind)) = sign {
        parts.push(FormatPart::new(kind, text));
    }
    if value.is_nan() {
        parts.push(FormatPart::new(PartType::Nan, "NaN"));
    } else if value.is_infinite() {
        parts.push(FormatPart::new(PartType::Infinity, "\u{221e}"));
    } else {
        let mut dec = Decimal::from_f64(value.abs());
        if o.style == NumberStyle::Percent && !dec.is_zero() {
            dec.point += 2;
        }
        let mut suffix = "";
        let mut min_fraction = min_fd as usize;
        if compact {
            let magnitude = if dec.is_zero() { 0 } else { dec.point - 1 };
            let mut tier = ((magnitude.clamp(0, 14)) / 3) as usize;
            loop {
                let mut scaled = Decimal {
                    digits: dec.digits.clone(),
                    point: dec.point - 3 * tier as i32,
                };
                if explicit {
                    scaled.round_to_fraction(max_fd as i32);
                } else {
                    min_fraction = 0;
                    scaled.round_to_fraction(if scaled.integer_len() <= 1 { 1 } else { 0 });
                }
                if scaled.integer_len() > 3 && tier < 4 {
                    tier += 1;
                    continue;
                }
                suffix = COMPACT_SUFFIX[tier];
                dec = scaled;
                break;
            }
        } else {
            dec.round_to_fraction(max_fd as i32);
        }
        let int = dec.integer();
        let min_grouping_ok = !compact || int.len() >= 5;
        if o.use_grouping != Some(false) && min_grouping_ok {
            group_integer(&int, locale.grouping(), &mut parts, zero);
        } else {
            parts.push(FormatPart::new(PartType::Integer, map_digits(&int, zero)));
        }
        let fraction = dec.fraction(min_fraction);
        if !fraction.is_empty() {
            parts.push(FormatPart::new(PartType::Decimal, "."));
            parts.push(FormatPart::new(
                PartType::Fraction,
                map_digits(&fraction, zero),
            ));
        }
        if !suffix.is_empty() {
            parts.push(FormatPart::new(PartType::Compact, suffix));
        }
    }
    match o.style {
        NumberStyle::Percent => parts.push(FormatPart::new(PartType::PercentSign, "%")),
        NumberStyle::Unit => {
            let narrow = match o.unit? {
                NumberUnit::Day => "d",
                NumberUnit::Hour => "h",
                NumberUnit::Minute => "m",
                NumberUnit::Second => "s",
            };
            parts.push(FormatPart::new(PartType::Unit, narrow));
        }
        NumberStyle::Decimal => {}
    }
    Some(parts)
}

// ---------------------------------------------------------------------------------------
// Time zones and dates
// ---------------------------------------------------------------------------------------

const UTC_ALIASES: &[&str] = &[
    "utc",
    "etc/utc",
    "etc/gmt",
    "gmt",
    "etc/uct",
    "uct",
    "etc/universal",
    "universal",
    "etc/zulu",
    "zulu",
    "etc/greenwich",
    "greenwich",
    "etc/gmt0",
    "etc/gmt+0",
    "etc/gmt-0",
    "gmt0",
    "gmt+0",
    "gmt-0",
];

/// The UTC offset in minutes of a zone id this backend knows (`None` for any other).
fn fixed_offset_minutes(id: &str) -> Option<i32> {
    let lower = id.to_ascii_lowercase();
    if UTC_ALIASES.contains(&lower.as_str()) {
        return Some(0);
    }
    if let Some(rest) = lower.strip_prefix("etc/gmt") {
        let (sign, digits) = match rest.as_bytes().first()? {
            b'+' => (-1, &rest[1..]),
            b'-' => (1, &rest[1..]),
            _ => return None,
        };
        if !is_digits(digits) || digits.len() > 2 || digits.starts_with('0') {
            return None;
        }
        let hours: i32 = digits.parse().ok()?;
        let in_range = if sign < 0 { hours <= 12 } else { hours <= 14 };
        return in_range.then_some(sign * hours * 60);
    }
    let sign = match lower.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let body = &lower[1..];
    let (h, m) = match body.len() {
        2 => (body, "00"),
        4 if is_digits(body) => (&body[..2], &body[2..]),
        5 if body.as_bytes()[2] == b':' => (&body[..2], &body[3..]),
        _ => return None,
    };
    if !is_digits(h) || !is_digits(m) {
        return None;
    }
    let (h, m): (i32, i32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(sign * (h * 60 + m))
}

struct Civil {
    year: i64,
    month: u32,
    day: u32,
    weekday: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian, month 1 to 12).
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date from days since 1970-01-01 (proleptic Gregorian).
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn civil_at(ms: f64, offset_minutes: i32) -> Option<Civil> {
    if !ms.is_finite() || ms.abs() > 8.64e15 {
        return None;
    }
    let local = ms.trunc() as i64 + i64::from(offset_minutes) * 60_000;
    let days = local.div_euclid(86_400_000);
    let rem = local.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    Some(Civil {
        year,
        month,
        day,
        weekday: (days + 4).rem_euclid(7) as u32,
        hour: (rem / 3_600_000) as u32,
        minute: (rem / 60_000 % 60) as u32,
        second: (rem / 1000 % 60) as u32,
    })
}

const MONTHS_SHORT: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTHS_LONG: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAYS_SHORT: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

fn padded(n: u32, style: NumericStyle) -> String {
    match style {
        NumericStyle::Numeric => n.to_string(),
        NumericStyle::TwoDigit => format!("{:02}", n % 100),
    }
}

fn offset_name(offset_minutes: i32) -> String {
    if offset_minutes == 0 {
        return "GMT".to_string();
    }
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let abs = offset_minutes.abs();
    if abs % 60 == 0 {
        format!("GMT{sign}{}", abs / 60)
    } else {
        format!("GMT{sign}{}:{:02}", abs / 60, abs % 60)
    }
}

fn english_date_parts(
    locale: &EnglishLocale,
    o: &DateTimeFormatOptions,
    ms: f64,
) -> Option<Vec<FormatPart>> {
    let (_, zero) = resolve_digits(
        o.numbering_system.as_deref(),
        locale.numbering_system.as_deref(),
    )?;
    let offset = match o.time_zone.as_deref() {
        Some(id) => fixed_offset_minutes(id)?,
        None => 0,
    };
    let c = civil_at(ms, offset)?;
    let has_time = o.hour.is_some() || o.minute.is_some() || o.second.is_some();
    let has_date = o.weekday.is_some() || o.year.is_some() || o.month.is_some() || o.day.is_some();
    // With no date or time field the engine shows the numeric date.
    let defaults = !has_time && !has_date;
    let year = if defaults {
        Some(NumericStyle::Numeric)
    } else {
        o.year
    };
    let day = if defaults {
        Some(NumericStyle::Numeric)
    } else {
        o.day
    };
    let numeric_month = defaults.then_some(NumericStyle::Numeric);
    let month_text = o.month.filter(|m| *m != MonthStyle::TwoDigit);
    let month_numeric = match o.month {
        Some(MonthStyle::TwoDigit) => Some(NumericStyle::TwoDigit),
        _ => numeric_month,
    };

    let mut parts: Vec<FormatPart> = Vec::new();
    let lit =
        |parts: &mut Vec<FormatPart>, s: &str| parts.push(FormatPart::new(PartType::Literal, s));
    let num = |kind: PartType, text: String| FormatPart::new(kind, map_digits(&text, zero));
    let year_text = year.map(|y| match y {
        NumericStyle::Numeric => {
            let shown = if c.year <= 0 { 1 - c.year } else { c.year };
            shown.to_string()
        }
        NumericStyle::TwoDigit => format!("{:02}", c.year.rem_euclid(100)),
    });

    if let Some(w) = o.weekday {
        let WeekdayStyle::Short = w;
        parts.push(FormatPart::new(
            PartType::Weekday,
            WEEKDAYS_SHORT[c.weekday as usize % 7],
        ));
        if year.is_some() || o.month.is_some() || day.is_some() || month_numeric.is_some() {
            lit(&mut parts, ", ");
        }
    }
    if let Some(style) = month_text {
        let name = match style {
            MonthStyle::Long => MONTHS_LONG[(c.month as usize - 1) % 12],
            _ => MONTHS_SHORT[(c.month as usize - 1) % 12],
        };
        parts.push(FormatPart::new(PartType::Month, name));
        if let Some(d) = day {
            lit(&mut parts, " ");
            parts.push(num(PartType::Day, padded(c.day, d)));
        }
        if let Some(y) = year_text {
            lit(&mut parts, if day.is_some() { ", " } else { " " });
            parts.push(num(PartType::Year, y));
        }
    } else {
        let mut first = true;
        let mut sep = |parts: &mut Vec<FormatPart>| {
            if !first {
                parts.push(FormatPart::new(PartType::Literal, "/"));
            }
            first = false;
        };
        if let Some(m) = month_numeric {
            sep(&mut parts);
            parts.push(num(PartType::Month, padded(c.month, m)));
        }
        if let Some(d) = day {
            sep(&mut parts);
            parts.push(num(PartType::Day, padded(c.day, d)));
        }
        if let Some(y) = year_text {
            sep(&mut parts);
            parts.push(num(PartType::Year, y));
        }
    }

    if has_time {
        if !parts.is_empty() {
            lit(&mut parts, ", ");
        }
        let cycle = match (o.hour12, o.hour_cycle) {
            (Some(true), _) => HourCycle::H12,
            (Some(false), _) => HourCycle::H23,
            (None, Some(cycle)) => cycle,
            (None, None) => locale.default_hour_cycle(),
        };
        let twelve = matches!(cycle, HourCycle::H11 | HourCycle::H12);
        if let Some(h) = o.hour {
            let shown = match cycle {
                HourCycle::H12 if c.hour % 12 == 0 => 12,
                HourCycle::H12 | HourCycle::H11 => c.hour % 12,
                HourCycle::H24 if c.hour == 0 => 24,
                HourCycle::H24 | HourCycle::H23 => c.hour,
            };
            let style = if twelve { h } else { NumericStyle::TwoDigit };
            parts.push(num(PartType::Hour, padded(shown, style)));
        }
        if let Some(m) = o.minute {
            if o.hour.is_some() {
                lit(&mut parts, ":");
            }
            let style = if o.hour.is_some() {
                NumericStyle::TwoDigit
            } else {
                m
            };
            parts.push(num(PartType::Minute, padded(c.minute, style)));
        }
        if let Some(s) = o.second {
            if o.hour.is_some() || o.minute.is_some() {
                lit(&mut parts, ":");
            }
            let style = if o.hour.is_some() || o.minute.is_some() {
                NumericStyle::TwoDigit
            } else {
                s
            };
            parts.push(num(PartType::Second, padded(c.second, style)));
        }
        if twelve && o.hour.is_some() {
            lit(&mut parts, " ");
            parts.push(FormatPart::new(
                PartType::DayPeriod,
                if c.hour < 12 { "AM" } else { "PM" },
            ));
        }
    }
    if let Some(TimeZoneNameStyle::ShortOffset) = o.time_zone_name {
        lit(&mut parts, if has_time { " " } else { ", " });
        parts.push(FormatPart::new(PartType::TimeZoneName, offset_name(offset)));
    }
    Some(parts)
}

// ---------------------------------------------------------------------------------------
// Segmentation
// ---------------------------------------------------------------------------------------

fn is_grapheme_extend(c: char) -> bool {
    matches!(
        c as u32,
        0x0300..=0x036F
            | 0x0483..=0x0489
            | 0x0591..=0x05BD
            | 0x05BF
            | 0x05C1..=0x05C2
            | 0x05C4..=0x05C5
            | 0x05C7
            | 0x0610..=0x061A
            | 0x064B..=0x065F
            | 0x0670
            | 0x06D6..=0x06DC
            | 0x06DF..=0x06E4
            | 0x06E7..=0x06E8
            | 0x06EA..=0x06ED
            | 0x0900..=0x0903
            | 0x093A..=0x094F
            | 0x0951..=0x0957
            | 0x0962..=0x0963
            | 0x0E31
            | 0x0E33..=0x0E3A
            | 0x0E47..=0x0E4E
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x200C..=0x200D
            | 0x20D0..=0x20FF
            | 0x3099..=0x309A
            | 0xFE00..=0xFE0F
            | 0xFE20..=0xFE2F
            | 0x1F3FB..=0x1F3FF
            | 0xE0020..=0xE007F
            | 0xE0100..=0xE01EF
    )
}

fn is_pictographic(c: char) -> bool {
    matches!(c as u32, 0x2190..=0x2BFF | 0x1F000..=0x1FAFF)
}

fn is_regional_indicator(c: char) -> bool {
    matches!(c as u32, 0x1F1E6..=0x1F1FF)
}

fn grapheme_ranges(chars: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        let first = chars[i];
        i += 1;
        if first == '\r' && chars.get(i) == Some(&'\n') {
            i += 1;
        } else if first != '\r' && first != '\n' {
            if is_regional_indicator(first)
                && chars.get(i).is_some_and(|c| is_regional_indicator(*c))
            {
                i += 1;
            }
            while let Some(&c) = chars.get(i) {
                if is_grapheme_extend(c) {
                    i += 1;
                    if c == '\u{200D}' && chars.get(i).is_some_and(|n| is_pictographic(*n)) {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
        }
        out.push((start, i));
    }
    out
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x309F | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}

fn word_ranges(chars: &[char]) -> Vec<(usize, usize, bool)> {
    let word_char = |c: char| (c.is_alphanumeric() || c == '_') && !is_cjk(c);
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        let c = chars[i];
        i += 1;
        let word_like = if is_cjk(c) {
            true
        } else if word_char(c) {
            while let Some(&n) = chars.get(i) {
                if word_char(n) || is_grapheme_extend(n) {
                    i += 1;
                    continue;
                }
                let joins = chars.get(i + 1).is_some_and(|after| {
                    let prev = chars[i - 1];
                    (matches!(n, '\'' | '.' | ':' | '\u{2019}')
                        && prev.is_alphabetic()
                        && after.is_alphabetic())
                        || (matches!(n, ',' | '.' | ';') && prev.is_numeric() && after.is_numeric())
                });
                if joins {
                    i += 2;
                } else {
                    break;
                }
            }
            true
        } else if c == '\r' && chars.get(i) == Some(&'\n') {
            i += 1;
            false
        } else if c.is_whitespace() && c != '\n' && c != '\r' {
            while chars
                .get(i)
                .is_some_and(|n| n.is_whitespace() && *n != '\n' && *n != '\r')
            {
                i += 1;
            }
            false
        } else {
            false
        };
        out.push((start, i, word_like));
    }
    out
}

fn segment_text(granularity: Granularity, text: &str) -> Vec<Segment> {
    let chars: Vec<char> = text.chars().collect();
    let ranges: Vec<(usize, usize, Option<bool>)> = match granularity {
        Granularity::Grapheme => grapheme_ranges(&chars)
            .into_iter()
            .map(|(a, b)| (a, b, None))
            .collect(),
        Granularity::Word => word_ranges(&chars)
            .into_iter()
            .map(|(a, b, w)| (a, b, Some(w)))
            .collect(),
    };
    let mut utf16 = 0;
    ranges
        .into_iter()
        .map(|(a, b, word)| {
            let segment: String = chars[a..b].iter().collect();
            let index = utf16;
            utf16 += segment.encode_utf16().count();
            Segment {
                segment,
                index,
                is_word_like: word,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// The backend
// ---------------------------------------------------------------------------------------

fn join(parts: Vec<FormatPart>) -> String {
    parts.into_iter().map(|p| p.value).collect()
}

/// The part of `time_clip` a backend relies on: a valid time value or nothing.
fn valid_time(ms: f64) -> Option<f64> {
    (ms.is_finite() && ms.abs() <= 8.64e15).then_some(ms)
}

impl Intl for EnglishIntl {
    fn canonicalize_locale(&self, tag: &str) -> Option<String> {
        parse_tag(tag).map(|t| t.canonical())
    }

    fn parse_locale(&self, tag: &str) -> Option<LocaleInfo> {
        parse_tag(tag).map(|t| t.info())
    }

    fn maximize_locale(&self, tag: &str) -> Option<LocaleInfo> {
        parse_tag(tag).map(|t| maximize_tag(&t).info())
    }

    fn locale_text_direction(&self, _tag: &str) -> Option<TextDirection> {
        None
    }

    fn locale_week_first_day(&self, _tag: &str) -> Option<u32> {
        None
    }

    fn plural_category(&self, locale: &str, kind: PluralKind, n: f64) -> Option<PluralCategory> {
        EnglishLocale::parse(locale)?;
        let abs = n.abs();
        Some(match kind {
            PluralKind::Cardinal => {
                if abs == 1.0 {
                    PluralCategory::One
                } else {
                    PluralCategory::Other
                }
            }
            PluralKind::Ordinal => {
                let (m10, m100) = (abs % 10.0, abs % 100.0);
                if m10 == 1.0 && m100 != 11.0 {
                    PluralCategory::One
                } else if m10 == 2.0 && m100 != 12.0 {
                    PluralCategory::Two
                } else if m10 == 3.0 && m100 != 13.0 {
                    PluralCategory::Few
                } else {
                    PluralCategory::Other
                }
            }
        })
    }

    fn format_number(
        &self,
        locale: &str,
        options: &NumberFormatOptions,
        value: f64,
    ) -> Option<String> {
        self.format_number_to_parts(locale, options, value)
            .map(join)
    }

    fn format_number_to_parts(
        &self,
        locale: &str,
        options: &NumberFormatOptions,
        value: f64,
    ) -> Option<Vec<FormatPart>> {
        english_number_parts(&EnglishLocale::parse(locale)?, options, value)
    }

    fn resolved_numbering_system(&self, locale: &str, requested: Option<&str>) -> Option<String> {
        let tag = parse_tag(locale)?;
        // Only English data exists here: other languages resolve like English would.
        resolve_digits(requested, tag.numbering_system().as_deref()).map(|(id, _)| id)
    }

    fn format_date_time(
        &self,
        locale: &str,
        options: &DateTimeFormatOptions,
        ms: f64,
    ) -> Option<String> {
        self.format_date_time_to_parts(locale, options, ms)
            .map(join)
    }

    fn format_date_time_to_parts(
        &self,
        locale: &str,
        options: &DateTimeFormatOptions,
        ms: f64,
    ) -> Option<Vec<FormatPart>> {
        english_date_parts(&EnglishLocale::parse(locale)?, options, valid_time(ms)?)
    }

    fn default_time_zone(&self) -> Option<String> {
        Some("UTC".to_string())
    }

    fn format_default_time(&self, ms: f64) -> String {
        let options = DateTimeFormatOptions {
            hour: Some(NumericStyle::Numeric),
            minute: Some(NumericStyle::TwoDigit),
            second: Some(NumericStyle::TwoDigit),
            hour_cycle: Some(HourCycle::H12),
            ..DateTimeFormatOptions::default()
        };
        valid_time(ms)
            .and_then(|ms| self.format_date_time("en-US", &options, ms))
            .unwrap_or_else(|| "Invalid Date".to_string())
    }

    fn segment(
        &self,
        _locale: Option<&str>,
        granularity: Granularity,
        text: &str,
    ) -> Option<Vec<Segment>> {
        Some(segment_text(granularity, text))
    }

    fn to_upper_case(&self, text: &str, locale: Option<&str>) -> Option<String> {
        let language = match locale {
            Some(tag) => parse_tag(tag)?.language,
            None => String::new(),
        };
        if matches!(language.as_str(), "tr" | "az") {
            let dotted: String = text
                .chars()
                .map(|c| if c == 'i' { '\u{130}' } else { c })
                .collect();
            return Some(dotted.to_uppercase());
        }
        Some(text.to_uppercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(options: NumberFormatOptions, value: f64) -> String {
        EnglishIntl
            .format_number("en", &options, value)
            .unwrap_or_default()
    }

    fn fixed(min: u32, max: u32) -> NumberFormatOptions {
        NumberFormatOptions {
            minimum_fraction_digits: Some(min),
            maximum_fraction_digits: Some(max),
            ..Default::default()
        }
    }

    #[test]
    fn locale_tags_canonicalize_like_intl() {
        let i = EnglishIntl;
        assert_eq!(i.canonicalize_locale("pt-br").as_deref(), Some("pt-BR"));
        assert_eq!(i.canonicalize_locale("iw").as_deref(), Some("he"));
        assert_eq!(i.canonicalize_locale("in").as_deref(), Some("id"));
        assert_eq!(
            i.canonicalize_locale("zh-hant-tw").as_deref(),
            Some("zh-Hant-TW")
        );
        assert_eq!(
            i.canonicalize_locale("en-u-nu-arab").as_deref(),
            Some("en-u-nu-arab")
        );
        assert_eq!(i.canonicalize_locale("en_US"), None);
        assert_eq!(i.canonicalize_locale(""), None);
        assert_eq!(i.canonicalize_locale("abcd"), None);
        let ar = i.parse_locale("ar-SA-u-nu-arab").expect("valid");
        assert_eq!(
            (
                ar.language.as_str(),
                ar.region.as_deref(),
                ar.numbering_system.as_deref()
            ),
            ("ar", Some("SA"), Some("arab"))
        );
        assert_eq!(ar.base_name, "ar-SA");
    }

    #[test]
    fn maximize_adds_likely_subtags() {
        let i = EnglishIntl;
        let pt = i.maximize_locale("pt").expect("known");
        assert_eq!(
            (pt.script.as_deref(), pt.region.as_deref()),
            (Some("Latn"), Some("BR"))
        );
        let tw = i.maximize_locale("zh-TW").expect("known");
        assert_eq!(tw.script.as_deref(), Some("Hant"));
        let unknown = i.maximize_locale("xx").expect("valid");
        assert_eq!(unknown.script, None);
    }

    #[test]
    fn english_plural_rules() {
        let p = |k, n| EnglishIntl.plural_category("en", k, n);
        assert_eq!(p(PluralKind::Cardinal, 1.0), Some(PluralCategory::One));
        assert_eq!(p(PluralKind::Cardinal, 0.0), Some(PluralCategory::Other));
        assert_eq!(p(PluralKind::Cardinal, 1.5), Some(PluralCategory::Other));
        let ord: Vec<_> = [
            1.0, 2.0, 3.0, 4.0, 11.0, 12.0, 13.0, 21.0, 22.0, 23.0, 101.0,
        ]
        .iter()
        .map(|n| p(PluralKind::Ordinal, *n).map(PluralCategory::as_str))
        .collect();
        assert_eq!(
            ord,
            [
                "one", "two", "few", "other", "other", "other", "other", "one", "two", "few", "one"
            ]
            .map(Some)
        );
        assert_eq!(
            EnglishIntl.plural_category("de", PluralKind::Cardinal, 1.0),
            None
        );
    }

    #[test]
    fn numbers_group_round_and_sign() {
        assert_eq!(num(NumberFormatOptions::default(), 1234.5), "1,234.5");
        assert_eq!(num(NumberFormatOptions::default(), 1234.56789), "1,234.568");
        assert_eq!(num(fixed(2, 2), 58_732.5), "58,732.50");
        assert_eq!(num(fixed(2, 2), 1.005), "1.01");
        assert_eq!(num(fixed(2, 2), -0.001), "-0.00");
        assert_eq!(num(fixed(0, 0), 9876.5), "9,877");
        assert_eq!(
            num(
                NumberFormatOptions {
                    use_grouping: Some(false),
                    ..fixed(1, 1)
                },
                -1.5
            ),
            "-1.5"
        );
        assert_eq!(num(NumberFormatOptions::default(), f64::NAN), "NaN");
        assert_eq!(
            num(NumberFormatOptions::default(), f64::NEG_INFINITY),
            "-\u{221e}"
        );
        assert_eq!(num(NumberFormatOptions::default(), 0.0), "0");
        assert_eq!(
            num(NumberFormatOptions::default(), 1e21),
            "1,000,000,000,000,000,000,000"
        );
        assert_eq!(num(NumberFormatOptions::default(), 1e-7), "0");
    }

    #[test]
    fn percent_compact_units_and_sign_display() {
        let pct = NumberFormatOptions {
            style: NumberStyle::Percent,
            ..Default::default()
        };
        assert_eq!(num(pct.clone(), 0.07), "7%");
        let signed = NumberFormatOptions {
            style: NumberStyle::Percent,
            sign_display: Some(SignDisplay::Always),
            ..fixed(2, 2)
        };
        assert_eq!(num(signed.clone(), 0.0234), "+2.34%");
        assert_eq!(num(signed, -0.012), "-1.20%");
        let compact = NumberFormatOptions {
            notation: Notation::Compact,
            ..Default::default()
        };
        let c = |v| num(compact.clone(), v);
        assert_eq!(c(999.0), "999");
        assert_eq!(c(1234.0), "1.2K");
        assert_eq!(c(12_345.0), "12K");
        assert_eq!(c(999_999.0), "1M");
        assert_eq!(c(1_234_567.0), "1.2M");
        let two = NumberFormatOptions {
            notation: Notation::Compact,
            ..fixed(0, 2)
        };
        assert_eq!(num(two, 1_234_567.0), "1.23M");
        let unit = NumberFormatOptions {
            style: NumberStyle::Unit,
            unit: Some(NumberUnit::Hour),
            unit_display: Some(UnitDisplay::Narrow),
            ..Default::default()
        };
        assert_eq!(num(unit, 4.0), "4h");
    }

    #[test]
    fn india_groups_in_lakhs_and_digits_follow_the_numbering_system() {
        let o = NumberFormatOptions::default();
        assert_eq!(
            EnglishIntl
                .format_number("en-IN", &fixed(2, 2), 12_345_678.9)
                .as_deref(),
            Some("1,23,45,678.90")
        );
        let arab = NumberFormatOptions {
            numbering_system: Some("arab".into()),
            ..fixed(2, 2)
        };
        assert_eq!(
            EnglishIntl.format_number("en", &arab, 58_732.5).as_deref(),
            Some("\u{665}\u{668},\u{667}\u{663}\u{662}.\u{665}\u{660}")
        );
        assert_eq!(
            EnglishIntl
                .resolved_numbering_system("en", Some("arab"))
                .as_deref(),
            Some("arab")
        );
        assert_eq!(
            EnglishIntl
                .resolved_numbering_system("en", Some("zzzz"))
                .as_deref(),
            Some("latn")
        );
        assert_eq!(EnglishIntl.resolved_numbering_system("en", Some("x")), None);
        assert_eq!(
            EnglishIntl
                .resolved_numbering_system("en-u-nu-deva", None)
                .as_deref(),
            Some("deva")
        );
        assert_eq!(EnglishIntl.format_number("de", &o, 1.0), None);
    }

    #[test]
    fn number_parts_name_the_symbols() {
        let parts = EnglishIntl.format_number_to_parts(
            "en",
            &NumberFormatOptions {
                use_grouping: Some(false),
                ..fixed(1, 1)
            },
            -1.5,
        );
        let kinds: Vec<_> = parts
            .unwrap_or_default()
            .into_iter()
            .map(|p| (p.kind, p.value))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (PartType::MinusSign, "-".to_string()),
                (PartType::Integer, "1".to_string()),
                (PartType::Decimal, ".".to_string()),
                (PartType::Fraction, "5".to_string()),
            ]
        );
    }

    const SEP_5_2026_1430: f64 = 1_788_618_600_000.0;

    fn date(locale: &str, options: DateTimeFormatOptions, ms: f64) -> Option<String> {
        EnglishIntl.format_date_time(locale, &options, ms)
    }

    fn utc() -> DateTimeFormatOptions {
        DateTimeFormatOptions {
            time_zone: Some("UTC".into()),
            ..Default::default()
        }
    }

    #[test]
    fn dates_follow_english_patterns() {
        let ms = SEP_5_2026_1430;
        assert_eq!(
            date(
                "en",
                DateTimeFormatOptions {
                    year: Some(NumericStyle::Numeric),
                    ..utc()
                },
                ms
            )
            .as_deref(),
            Some("2026")
        );
        assert_eq!(
            date(
                "en",
                DateTimeFormatOptions {
                    month: Some(MonthStyle::Short),
                    ..utc()
                },
                ms
            )
            .as_deref(),
            Some("Sep")
        );
        assert_eq!(
            date(
                "en",
                DateTimeFormatOptions {
                    month: Some(MonthStyle::Short),
                    day: Some(NumericStyle::Numeric),
                    ..utc()
                },
                ms
            )
            .as_deref(),
            Some("Sep 5")
        );
        assert_eq!(
            date(
                "en",
                DateTimeFormatOptions {
                    weekday: Some(WeekdayStyle::Short),
                    day: Some(NumericStyle::Numeric),
                    month: Some(MonthStyle::Short),
                    year: Some(NumericStyle::TwoDigit),
                    hour: Some(NumericStyle::TwoDigit),
                    minute: Some(NumericStyle::TwoDigit),
                    hour_cycle: Some(HourCycle::H23),
                    ..utc()
                },
                ms
            )
            .as_deref(),
            Some("Sat, Sep 5, 26, 14:30")
        );
        assert_eq!(date("en", utc(), ms).as_deref(), Some("9/5/2026"));
    }

    #[test]
    fn clocks_cover_every_hour_cycle() {
        let clock = |cycle, hour, ms| {
            date(
                "en",
                DateTimeFormatOptions {
                    hour: Some(hour),
                    minute: Some(NumericStyle::TwoDigit),
                    hour_cycle: Some(cycle),
                    ..utc()
                },
                ms,
            )
        };
        let nine_pm = 1_767_301_500_000.0; // 2026-01-01 21:05 UTC
        assert_eq!(
            clock(HourCycle::H12, NumericStyle::Numeric, nine_pm).as_deref(),
            Some("9:05 PM")
        );
        assert_eq!(
            clock(HourCycle::H23, NumericStyle::TwoDigit, nine_pm).as_deref(),
            Some("21:05")
        );
        let midnight = 1_767_225_600_000.0; // 2026-01-01 00:00 UTC
        assert_eq!(
            clock(HourCycle::H12, NumericStyle::Numeric, midnight).as_deref(),
            Some("12:00 AM")
        );
        assert_eq!(
            clock(HourCycle::H11, NumericStyle::Numeric, midnight).as_deref(),
            Some("0:00 AM")
        );
        assert_eq!(
            clock(HourCycle::H24, NumericStyle::TwoDigit, midnight).as_deref(),
            Some("24:00")
        );
        assert_eq!(
            clock(HourCycle::H23, NumericStyle::TwoDigit, midnight).as_deref(),
            Some("00:00")
        );
    }

    #[test]
    fn zones_are_utc_and_fixed_offsets_only() {
        let at = |zone: &str| {
            date(
                "en-US",
                DateTimeFormatOptions {
                    time_zone: Some(zone.into()),
                    time_zone_name: Some(TimeZoneNameStyle::ShortOffset),
                    ..Default::default()
                },
                SEP_5_2026_1430,
            )
        };
        assert_eq!(at("UTC").as_deref(), Some("9/5/2026, GMT"));
        assert_eq!(at("Etc/GMT+5").as_deref(), Some("9/5/2026, GMT-5"));
        assert_eq!(at("+05:30").as_deref(), Some("9/5/2026, GMT+5:30"));
        assert_eq!(at("America/New_York"), None);
        assert_eq!(at("Not/AZone"), None);
    }

    #[test]
    fn invalid_times_and_locales_yield_none() {
        assert_eq!(date("en", utc(), f64::NAN), None);
        assert_eq!(date("en", utc(), 9e15), None);
        assert_eq!(date("de", utc(), 0.0), None);
        assert_eq!(EnglishIntl.format_default_time(f64::NAN), "Invalid Date");
        assert_eq!(
            EnglishIntl.format_default_time(SEP_5_2026_1430),
            "2:30:00 PM"
        );
    }

    #[test]
    fn segmentation_and_casing() {
        let graphemes = |s: &str| -> Vec<String> {
            EnglishIntl
                .segment(None, Granularity::Grapheme, s)
                .unwrap_or_default()
                .into_iter()
                .map(|g| g.segment)
                .collect()
        };
        assert_eq!(graphemes("e\u{301}a"), ["e\u{301}", "a"]);
        assert_eq!(
            graphemes("\u{1F468}\u{200D}\u{1F469}x"),
            ["\u{1F468}\u{200D}\u{1F469}", "x"]
        );
        assert_eq!(
            graphemes("\u{1F1E9}\u{1F1EA}\u{1F1EB}\u{1F1F7}"),
            ["\u{1F1E9}\u{1F1EA}", "\u{1F1EB}\u{1F1F7}"]
        );
        assert_eq!(graphemes("\r\n"), ["\r\n"]);
        let words = EnglishIntl
            .segment(None, Granularity::Word, "can't stop, 3.5")
            .unwrap_or_default();
        let shown: Vec<_> = words
            .iter()
            .map(|w| (w.segment.as_str(), w.is_word_like))
            .collect();
        assert_eq!(
            shown,
            [
                ("can't", Some(true)),
                (" ", Some(false)),
                ("stop", Some(true)),
                (",", Some(false)),
                (" ", Some(false)),
                ("3.5", Some(true))
            ]
        );
        let astral = EnglishIntl
            .segment(None, Granularity::Grapheme, "\u{1F600}a")
            .unwrap_or_default();
        assert_eq!(astral[1].index, 2);
        assert_eq!(
            EnglishIntl.to_upper_case("istanbul", Some("tr")).as_deref(),
            Some("\u{130}STANBUL")
        );
        assert_eq!(
            EnglishIntl.to_upper_case("istanbul", Some("en")).as_deref(),
            Some("ISTANBUL")
        );
        assert_eq!(EnglishIntl.to_upper_case("x", Some("en_US")), None);
    }
}
