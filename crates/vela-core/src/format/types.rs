//! The formatting vocabulary of chart text: what every built-in formatter receives, and
//! the shape a host implements to replace one (`LocalizationOptions::formatters`).

use std::fmt;
use std::ops::Deref;
use std::sync::{Arc, OnceLock};

use crate::intl::{Intl, default_intl};

use super::numbers::NumberSymbols;

/// Where a formatted value appears. Formatters may style a surface differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FormatSurface {
    PriceAxis,
    Crosshair,
    LastPrice,
    StatusLine,
    DataWindow,
    Legend,
    Drawing,
    TimeAxis,
    Tooltip,
    Replay,
    Clock,
}

impl FormatSurface {
    /// The surface's name as the original spells it (`"priceAxis"`, `"statusLine"`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            FormatSurface::PriceAxis => "priceAxis",
            FormatSurface::Crosshair => "crosshair",
            FormatSurface::LastPrice => "lastPrice",
            FormatSurface::StatusLine => "statusLine",
            FormatSurface::DataWindow => "dataWindow",
            FormatSurface::Legend => "legend",
            FormatSurface::Drawing => "drawing",
            FormatSurface::TimeAxis => "timeAxis",
            FormatSurface::Tooltip => "tooltip",
            FormatSurface::Replay => "replay",
            FormatSurface::Clock => "clock",
        }
    }
}

/// The calendar unit a time-axis tick marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimeTickUnit {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
}

/// What a price formatter formats: a price, a percent change or a value indexed to 100.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PriceMode {
    #[default]
    Price,
    Percent,
    Indexed,
}

/// How a duration reads: like a clock (`04:59`) or by naming units (`2d 4h`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DurationStyle {
    #[default]
    Countdown,
    Span,
}

/// The clock of chart times: `'h23'` (`14:30`), `'h12'`, or the locale's own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HourCycleSetting {
    #[default]
    H23,
    H12,
    Locale,
}

impl HourCycleSetting {
    /// The option's spelling (`"h23"`, `"h12"`, `"locale"`).
    pub fn as_str(self) -> &'static str {
        match self {
            HourCycleSetting::H23 => "h23",
            HourCycleSetting::H12 => "h12",
            HourCycleSetting::Locale => "locale",
        }
    }
}

/// What every host formatter receives besides its own fields.
pub struct FormatContext<'a> {
    /// The chart's locale (a BCP 47 tag).
    pub locale: &'a str,
    /// The display time zone (an IANA id, or `"UTC"`). The date of a daily or coarser
    /// bar reads in `"UTC"` whatever the display zone.
    pub time_zone: &'a str,
    /// Where the value appears.
    pub surface: FormatSurface,
    fallback: &'a dyn Fn() -> String,
}

impl FormatContext<'_> {
    /// The built-in formatting of the same value: return it to keep the default.
    pub fn fallback(&self) -> String {
        (self.fallback)()
    }
}

macro_rules! context {
    ($(#[$meta:meta])* $name:ident { $($(#[$fmeta:meta])* $field:ident : $ty:ty),* $(,)? }) => {
        $(#[$meta])*
        pub struct $name<'a> {
            pub common: FormatContext<'a>,
            $($(#[$fmeta])* pub $field: $ty,)*
        }

        impl<'a> Deref for $name<'a> {
            type Target = FormatContext<'a>;
            fn deref(&self) -> &FormatContext<'a> {
                &self.common
            }
        }
    };
}

context!(
    /// What a host price formatter receives.
    PriceFormatContext {
        decimals: f64,
        mintick: Option<f64>,
        mode: PriceMode,
        pane_id: Option<&'a str>,
        grouping: bool,
    }
);
context!(
    /// What a host percent formatter receives.
    PercentFormatContext {
        decimals: f64,
        signed: bool
    }
);
context!(
    /// What a host compact formatter receives.
    CompactFormatContext {
        decimals: f64,
        trim_zeros: bool
    }
);
context!(
    /// What a host time-tick formatter receives.
    TimeTickFormatContext {
        unit: TimeTickUnit,
        major: bool,
        offset_ms: f64
    }
);
context!(
    /// What a host time-stamp formatter receives.
    TimeStampFormatContext { with_time: bool }
);
context!(
    /// What a host duration formatter receives.
    DurationFormatContext {
        style: DurationStyle
    }
);
context!(
    /// What a host clock formatter receives.
    ClockFormatContext { seconds: bool }
);

impl<'a> FormatContext<'a> {
    pub(crate) fn new(
        locale: &'a str,
        time_zone: &'a str,
        surface: FormatSurface,
        fallback: &'a dyn Fn() -> String,
    ) -> Self {
        Self {
            locale,
            time_zone,
            surface,
            fallback,
        }
    }
}

/// Options of [`ResolvedFormatters::price`](super::ResolvedFormatters::price).
#[derive(Clone, Debug, PartialEq)]
pub struct PriceFormatOptions {
    pub surface: Option<FormatSurface>,
    /// Fraction digits to show (the symbol's tick size or the axis zoom decides them).
    pub decimals: f64,
    /// The symbol's tick size, when known.
    pub mintick: Option<f64>,
    /// `Price` (default), `Percent` (a percent change, signed) or `Indexed` (indexed to 100).
    pub mode: Option<PriceMode>,
    /// The pane the value belongs to.
    pub pane_id: Option<String>,
    /// Insert the locale's group separators (`1,234.50`). Default false: axis labels stay
    /// ungrouped so they fit the fixed-width price gutter.
    pub grouping: Option<bool>,
}

impl PriceFormatOptions {
    /// Options for `decimals` fraction digits, everything else at its default.
    pub fn new(decimals: impl Into<f64>) -> Self {
        Self {
            surface: None,
            decimals: decimals.into(),
            mintick: None,
            mode: None,
            pane_id: None,
            grouping: None,
        }
    }
}

/// Options of [`ResolvedFormatters::percent`](super::ResolvedFormatters::percent).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PercentFormatOptions {
    pub surface: Option<FormatSurface>,
    /// Fraction digits. Default 2.
    pub decimals: Option<f64>,
    /// Always show the sign, `+` included. Default true.
    pub signed: Option<bool>,
}

/// Options of [`ResolvedFormatters::compact`](super::ResolvedFormatters::compact).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompactFormatOptions {
    pub surface: Option<FormatSurface>,
    /// Maximum fraction digits. Default 2.
    pub decimals: Option<f64>,
    /// Drop trailing zeros (`1.2M`); false keeps every digit (`1.20M`). Default true.
    pub trim_zeros: Option<bool>,
}

/// Options of [`ResolvedFormatters::time_tick`](super::ResolvedFormatters::time_tick).
#[derive(Clone, Debug, PartialEq)]
pub struct TimeTickFormatOptions {
    pub surface: Option<FormatSurface>,
    /// The calendar unit the tick marks.
    pub unit: TimeTickUnit,
    /// A major tick (a day, month or year boundary) rather than an intermediate one.
    pub major: bool,
    /// The display time zone. Default `"UTC"`.
    pub time_zone: Option<String>,
    /// The zone's UTC offset in ms that the axis aligned the tick with (sampled once per
    /// visible range): `ms + offset_ms` read with UTC fields is the wall-clock time the
    /// label names. Default 0.
    pub offset_ms: Option<f64>,
}

impl TimeTickFormatOptions {
    pub fn new(unit: TimeTickUnit, major: bool) -> Self {
        Self {
            surface: None,
            unit,
            major,
            time_zone: None,
            offset_ms: None,
        }
    }
}

/// Options of [`ResolvedFormatters::time_stamp`](super::ResolvedFormatters::time_stamp).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimeStampFormatOptions {
    pub surface: Option<FormatSurface>,
    /// Append the time of day. Default true; false on daily and coarser bars.
    pub with_time: Option<bool>,
    /// The display time zone. Default `"UTC"`.
    pub time_zone: Option<String>,
}

/// Options of [`ResolvedFormatters::clock`](super::ResolvedFormatters::clock).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClockFormatOptions {
    pub surface: Option<FormatSurface>,
    /// The zone whose wall clock to read (an IANA id). Default `"UTC"`.
    pub time_zone: Option<String>,
    /// Show the seconds. Default true.
    pub seconds: Option<bool>,
}

/// Options of [`ResolvedFormatters::duration`](super::ResolvedFormatters::duration).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DurationFormatOptions {
    pub surface: Option<FormatSurface>,
    /// `Countdown` (default) reads like a clock (`04:59`, `1:01:05`); `Span` names the
    /// units (`2d 4h`).
    pub style: Option<DurationStyle>,
}

/// A host formatter returns `None` where the original's would throw or return a
/// non-string: the built-in result is used instead.
pub type PriceFormatter =
    Arc<dyn for<'a> Fn(f64, &PriceFormatContext<'a>) -> Option<String> + Send + Sync>;
pub type PercentFormatter =
    Arc<dyn for<'a> Fn(f64, &PercentFormatContext<'a>) -> Option<String> + Send + Sync>;
pub type CompactFormatter =
    Arc<dyn for<'a> Fn(f64, &CompactFormatContext<'a>) -> Option<String> + Send + Sync>;
pub type TimeTickFormatter =
    Arc<dyn for<'a> Fn(f64, &TimeTickFormatContext<'a>) -> Option<String> + Send + Sync>;
pub type TimeStampFormatter =
    Arc<dyn for<'a> Fn(f64, &TimeStampFormatContext<'a>) -> Option<String> + Send + Sync>;
pub type DurationFormatter =
    Arc<dyn for<'a> Fn(f64, &DurationFormatContext<'a>) -> Option<String> + Send + Sync>;
pub type ClockFormatter =
    Arc<dyn for<'a> Fn(f64, &ClockFormatContext<'a>) -> Option<String> + Send + Sync>;

/// Host replacements for chart formatting (`LocalizationOptions::formatters`). Supply any
/// subset; each receives the value and its context, and `ctx.fallback()` returns the
/// built-in result.
#[derive(Clone, Default)]
pub struct VelaFormatters {
    pub price: Option<PriceFormatter>,
    pub percent: Option<PercentFormatter>,
    pub compact: Option<CompactFormatter>,
    pub time_tick: Option<TimeTickFormatter>,
    pub time_stamp: Option<TimeStampFormatter>,
    pub duration: Option<DurationFormatter>,
    pub clock: Option<ClockFormatter>,
}

impl fmt::Debug for VelaFormatters {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VelaFormatters")
            .field("price", &self.price.is_some())
            .field("percent", &self.percent.is_some())
            .field("compact", &self.compact.is_some())
            .field("time_tick", &self.time_tick.is_some())
            .field("time_stamp", &self.time_stamp.is_some())
            .field("duration", &self.duration.is_some())
            .field("clock", &self.clock.is_some())
            .finish()
    }
}

/// The resolved settings the built-in formatters read, with the [`Intl`] backend they
/// format through.
#[derive(Clone)]
pub struct FormatSettings {
    /// Canonical BCP 47 tag.
    pub locale: String,
    /// Resolved numbering system id (`"latn"`, `"arab"`, ...).
    pub numbering_system: String,
    pub hour_cycle: HourCycleSetting,
    /// The locale's language is English: Vela's own English shapes apply.
    pub english: bool,
    pub intl: Arc<dyn Intl>,
    symbols: Arc<OnceLock<NumberSymbols>>,
}

impl FormatSettings {
    pub fn new(
        locale: impl Into<String>,
        numbering_system: impl Into<String>,
        hour_cycle: HourCycleSetting,
        english: bool,
        intl: Arc<dyn Intl>,
    ) -> Self {
        Self {
            locale: locale.into(),
            numbering_system: numbering_system.into(),
            hour_cycle,
            english,
            intl,
            symbols: Arc::new(OnceLock::new()),
        }
    }

    /// Settings of the default chart (English, Western digits, 24-hour clock) over the
    /// built-in [`crate::intl::EnglishIntl`].
    pub fn english() -> Self {
        Self::english_with(default_intl())
    }

    /// The default chart's settings over a given backend.
    pub fn english_with(intl: Arc<dyn Intl>) -> Self {
        Self::new("en", "latn", HourCycleSetting::H23, true, intl)
    }

    pub(crate) fn symbols(&self) -> &OnceLock<NumberSymbols> {
        &self.symbols
    }
}

impl fmt::Debug for FormatSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FormatSettings")
            .field("locale", &self.locale)
            .field("numbering_system", &self.numbering_system)
            .field("hour_cycle", &self.hour_cycle)
            .field("english", &self.english)
            .finish_non_exhaustive()
    }
}
