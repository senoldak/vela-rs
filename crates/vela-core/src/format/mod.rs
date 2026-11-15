//! Chart text formatting: numbers, dates and times, durations and calendar names, in the
//! chart's locale. Built on the [`Intl`](crate::intl::Intl) seam; English keeps Vela's
//! own shapes so its output never moves.
//!
//! `FormatSettings` carries the resolved locale and the backend; `ResolvedFormatters`
//! is what a chart hands to every surface, with host replacements applied.

pub mod calendar;
pub mod dates;
pub mod formatters;
pub mod numbers;
pub mod types;

pub use calendar::{
    NameWidth, locale_week_start, month_names, plain_integer, weekday_names, year_before_month,
    year_label,
};
pub use dates::zone_offset_ms;
pub use formatters::{ResolvedFormatters, builtin_formatters, english_format, resolve_formatters};
pub use numbers::{format_message_number, localize_ascii, localize_digits, strip_bidi_marks};
pub use types::{
    ClockFormatContext, ClockFormatOptions, ClockFormatter, CompactFormatContext,
    CompactFormatOptions, CompactFormatter, DurationFormatContext, DurationFormatOptions,
    DurationFormatter, DurationStyle, FormatContext, FormatSettings, FormatSurface,
    HourCycleSetting, PercentFormatContext, PercentFormatOptions, PercentFormatter,
    PriceFormatContext, PriceFormatOptions, PriceFormatter, PriceMode, TimeStampFormatContext,
    TimeStampFormatOptions, TimeStampFormatter, TimeTickFormatContext, TimeTickFormatOptions,
    TimeTickFormatter, TimeTickUnit, VelaFormatters,
};
