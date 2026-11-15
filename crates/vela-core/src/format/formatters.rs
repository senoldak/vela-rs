//! Assembles a chart's resolved formatters: the built-in formatting for its settings,
//! with any host replacement layered on top. The built-in path allocates nothing beyond
//! its result; a host formatter gets a context object with `fallback()` to the built-in.

use super::dates::{format_clock, format_duration, format_time_stamp, format_time_tick};
use super::numbers::{format_compact, format_percent, format_price};
use super::types::{
    ClockFormatContext, ClockFormatOptions, CompactFormatContext, CompactFormatOptions,
    DurationFormatContext, DurationFormatOptions, DurationStyle, FormatContext, FormatSettings,
    FormatSurface, PercentFormatContext, PercentFormatOptions, PriceFormatContext,
    PriceFormatOptions, PriceMode, TimeStampFormatContext, TimeStampFormatOptions,
    TimeTickFormatContext, TimeTickFormatOptions, VelaFormatters,
};

/// The resolved formatters of a chart (`chart.i18n.format`): the built-in formatting for
/// the chart's locale, with any host replacement applied. Every method returns plain text
/// without bidi control marks; right-to-left pages isolate numbers where they render.
#[derive(Clone, Debug)]
pub struct ResolvedFormatters {
    settings: FormatSettings,
    host: Option<VelaFormatters>,
}

/// The built-in formatters for one set of settings.
pub fn builtin_formatters(settings: FormatSettings) -> ResolvedFormatters {
    ResolvedFormatters {
        settings,
        host: None,
    }
}

/// The resolved formatters: built-ins for `settings`, replaced per method by `overrides`.
pub fn resolve_formatters(
    settings: FormatSettings,
    overrides: Option<&VelaFormatters>,
) -> ResolvedFormatters {
    ResolvedFormatters {
        settings,
        host: overrides.cloned(),
    }
}

/// The default chart's formatters: English, Western digits, 24-hour clock, over the
/// built-in backend. Every surface uses these when no chart hands it its own.
pub fn english_format() -> ResolvedFormatters {
    builtin_formatters(FormatSettings::english())
}

impl ResolvedFormatters {
    /// The settings these formatters read.
    pub fn settings(&self) -> &FormatSettings {
        &self.settings
    }

    fn context<'a>(
        &'a self,
        surface: Option<FormatSurface>,
        default_surface: FormatSurface,
        time_zone: Option<&'a str>,
        fallback: &'a dyn Fn() -> String,
    ) -> FormatContext<'a> {
        FormatContext::new(
            &self.settings.locale,
            time_zone.unwrap_or("UTC"),
            surface.unwrap_or(default_surface),
            fallback,
        )
    }

    /// A price, a percent change or an indexed value at a fixed number of decimals.
    pub fn price(&self, value: f64, options: &PriceFormatOptions) -> String {
        let fallback = || format_price(value, options, &self.settings);
        let Some(host) = self.host.as_ref().and_then(|h| h.price.as_ref()) else {
            return fallback();
        };
        let ctx = PriceFormatContext {
            common: self.context(options.surface, FormatSurface::PriceAxis, None, &fallback),
            decimals: options.decimals,
            mintick: options.mintick,
            mode: options.mode.unwrap_or(PriceMode::Price),
            pane_id: options.pane_id.as_deref(),
            grouping: options.grouping.unwrap_or(false),
        };
        host(value, &ctx).unwrap_or_else(fallback)
    }

    /// A percent value (`2.34` reads `+2.34%`).
    pub fn percent(&self, value: f64, options: &PercentFormatOptions) -> String {
        let fallback = || format_percent(value, options, &self.settings);
        let Some(host) = self.host.as_ref().and_then(|h| h.percent.as_ref()) else {
            return fallback();
        };
        let ctx = PercentFormatContext {
            common: self.context(options.surface, FormatSurface::StatusLine, None, &fallback),
            decimals: options.decimals.unwrap_or(2.0),
            signed: options.signed.unwrap_or(true),
        };
        host(value, &ctx).unwrap_or_else(fallback)
    }

    /// A large magnitude abbreviated (`1.2M`, `120万`).
    pub fn compact(&self, value: f64, options: &CompactFormatOptions) -> String {
        let fallback = || format_compact(value, options, &self.settings);
        let Some(host) = self.host.as_ref().and_then(|h| h.compact.as_ref()) else {
            return fallback();
        };
        let ctx = CompactFormatContext {
            common: self.context(options.surface, FormatSurface::PriceAxis, None, &fallback),
            decimals: options.decimals.unwrap_or(2.0),
            trim_zeros: options.trim_zeros.unwrap_or(true),
        };
        host(value, &ctx).unwrap_or_else(fallback)
    }

    /// A time-axis label.
    pub fn time_tick(&self, ms: f64, options: &TimeTickFormatOptions) -> String {
        let fallback = || format_time_tick(ms, options, &self.settings);
        let Some(host) = self.host.as_ref().and_then(|h| h.time_tick.as_ref()) else {
            return fallback();
        };
        let ctx = TimeTickFormatContext {
            common: self.context(
                options.surface,
                FormatSurface::TimeAxis,
                options.time_zone.as_deref(),
                &fallback,
            ),
            unit: options.unit,
            major: options.major,
            offset_ms: options.offset_ms.unwrap_or(0.0),
        };
        host(ms, &ctx).unwrap_or_else(fallback)
    }

    /// A full date, with the time of day unless `with_time` is false.
    pub fn time_stamp(&self, ms: f64, options: &TimeStampFormatOptions) -> String {
        let fallback = || format_time_stamp(ms, options, &self.settings);
        let Some(host) = self.host.as_ref().and_then(|h| h.time_stamp.as_ref()) else {
            return fallback();
        };
        let ctx = TimeStampFormatContext {
            common: self.context(
                options.surface,
                FormatSurface::Crosshair,
                options.time_zone.as_deref(),
                &fallback,
            ),
            with_time: options.with_time.unwrap_or(true),
        };
        host(ms, &ctx).unwrap_or_else(fallback)
    }

    /// A length of time.
    pub fn duration(&self, ms: f64, options: &DurationFormatOptions) -> String {
        let fallback = || format_duration(ms, options, &self.settings);
        let Some(host) = self.host.as_ref().and_then(|h| h.duration.as_ref()) else {
            return fallback();
        };
        let ctx = DurationFormatContext {
            common: self.context(options.surface, FormatSurface::LastPrice, None, &fallback),
            style: options.style.unwrap_or(DurationStyle::Countdown),
        };
        host(ms, &ctx).unwrap_or_else(fallback)
    }

    /// A time of day in a zone, like a clock (`14:05:09`).
    pub fn clock(&self, ms: f64, options: &ClockFormatOptions) -> String {
        let fallback = || format_clock(ms, options, &self.settings);
        let Some(host) = self.host.as_ref().and_then(|h| h.clock.as_ref()) else {
            return fallback();
        };
        let ctx = ClockFormatContext {
            common: self.context(
                options.surface,
                FormatSurface::Clock,
                options.time_zone.as_deref(),
                &fallback,
            ),
            seconds: options.seconds.unwrap_or(true),
        };
        host(ms, &ctx).unwrap_or_else(fallback)
    }
}
