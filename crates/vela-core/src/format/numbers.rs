//! Built-in number formatting for chart text. English keeps Vela's established shapes
//! (`toFixed` on the axes, en-US grouping in the status line) so its output never moves;
//! other languages take their separators, signs and abbreviations from the [`Intl`]
//! backend.
//!
//! [`Intl`]: crate::intl::Intl

use crate::i18n::number_text;
use crate::indicators::classics::math::{js_max, js_min};
use crate::intl::{
    FormatPart, Notation, NumberFormatOptions, NumberStyle, NumberUnit, PartType, SignDisplay,
    UnitDisplay,
};
use crate::util::to_fixed;

use super::types::{
    CompactFormatOptions, FormatSettings, PercentFormatOptions, PriceFormatOptions, PriceMode,
};

/// LRM, RLM and ALM: `Intl` wraps signs in them for right-to-left pages. Chart numbers
/// render as left-to-right runs wherever they appear, so the marks are dropped.
pub fn strip_bidi_marks(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '\u{200E}' | '\u{200F}' | '\u{061C}'))
        .collect()
}

/// A number through the settings' locale and numbering system; an unusable locale falls
/// back to English (without the numbering system), as the original's `catch` does.
pub(crate) fn number_format_text(
    settings: &FormatSettings,
    options: &NumberFormatOptions,
    value: f64,
) -> String {
    let with_system = NumberFormatOptions {
        numbering_system: Some(settings.numbering_system.clone()),
        ..options.clone()
    };
    settings
        .intl
        .format_number(&settings.locale, &with_system, value)
        .or_else(|| settings.intl.format_number("en", options, value))
        .unwrap_or_else(|| number_text(value))
}

/// The parts of a number through the settings' locale, with the same English fallback.
pub(crate) fn number_format_parts(
    settings: &FormatSettings,
    options: &NumberFormatOptions,
    value: f64,
) -> Vec<FormatPart> {
    let with_system = NumberFormatOptions {
        numbering_system: Some(settings.numbering_system.clone()),
        ..options.clone()
    };
    settings
        .intl
        .format_number_to_parts(&settings.locale, &with_system, value)
        .or_else(|| settings.intl.format_number_to_parts("en", options, value))
        .unwrap_or_default()
}

/// The symbols an ASCII `toFixed` string maps onto for a locale.
pub(crate) struct NumberSymbols {
    /// True when the locale writes `toFixed` output unchanged (`.` decimal, `-`, ASCII digits).
    ascii: bool,
    decimal: String,
    minus: String,
    digits: Vec<String>,
}

fn number_symbols(settings: &FormatSettings) -> &NumberSymbols {
    settings.symbols().get_or_init(|| {
        let probe = NumberFormatOptions {
            use_grouping: Some(false),
            minimum_fraction_digits: Some(1),
            maximum_fraction_digits: Some(1),
            ..NumberFormatOptions::default()
        };
        let parts = number_format_parts(settings, &probe, -1.5);
        let symbol = |kind: PartType, default: &str| {
            let found = parts
                .iter()
                .find(|p| p.kind == kind)
                .map(|p| strip_bidi_marks(&p.value));
            found
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| default.to_string())
        };
        let decimal = symbol(PartType::Decimal, ".");
        let minus = symbol(PartType::MinusSign, "-");
        let plain = NumberFormatOptions {
            use_grouping: Some(false),
            maximum_fraction_digits: Some(0),
            ..NumberFormatOptions::default()
        };
        let digits: Vec<String> = (0..10)
            .map(|d| strip_bidi_marks(&number_format_text(settings, &plain, f64::from(d))))
            .collect();
        let ascii = decimal == "."
            && minus == "-"
            && digits.iter().enumerate().all(|(i, d)| *d == i.to_string());
        NumberSymbols {
            ascii,
            decimal,
            minus,
            digits,
        }
    })
}

/// Rewrite ASCII number text (digits, `.`, `-`) in the locale's symbols.
pub fn localize_ascii(text: &str, settings: &FormatSettings) -> String {
    let sym = number_symbols(settings);
    if sym.ascii {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '0'..='9' => out.push_str(&sym.digits[ch as usize - '0' as usize]),
            '.' => out.push_str(&sym.decimal),
            '-' => out.push_str(&sym.minus),
            other => out.push(other),
        }
    }
    out
}

/// Only the digits of ASCII text, in the settings' numbering system (clock-like text).
pub fn localize_digits(text: &str, settings: &FormatSettings) -> String {
    let sym = number_symbols(settings);
    if sym
        .digits
        .iter()
        .enumerate()
        .all(|(i, d)| *d == i.to_string())
    {
        return text.to_string();
    }
    text.chars()
        .map(|ch| match ch {
            '0'..='9' => sym.digits[ch as usize - '0' as usize].clone(),
            other => other.to_string(),
        })
        .collect()
}

/// A fraction-digit count as `toFixed` takes it: truncated, 0 to 20, 2 when not finite.
pub(crate) fn clamp_decimals(d: f64) -> usize {
    if d.is_finite() {
        js_max(0.0, js_min(20.0, d.trunc())) as usize
    } else {
        2
    }
}

/// A fixed-decimals number without grouping. `toFixed` decides the rounding in every
/// locale, so a price rounds the same whatever the language.
pub fn format_fixed(value: f64, decimals: f64, settings: &FormatSettings) -> String {
    localize_ascii(&to_fixed(value, clamp_decimals(decimals)), settings)
}

fn fraction_digits(d: usize) -> NumberFormatOptions {
    NumberFormatOptions {
        minimum_fraction_digits: Some(d as u32),
        maximum_fraction_digits: Some(d as u32),
        ..NumberFormatOptions::default()
    }
}

/// A fixed-decimals number with the locale's group separators (en: `1,234.50`).
pub fn format_grouped(value: f64, decimals: f64, settings: &FormatSettings) -> String {
    let d = clamp_decimals(decimals);
    strip_bidi_marks(&number_format_text(settings, &fraction_digits(d), value))
}

/// A plain number for message arguments (`{n}`, `{n, number}`): grouped, up to three
/// fraction digits, in the settings' numbering system.
pub fn format_message_number(value: f64, settings: &FormatSettings, style: Option<&str>) -> String {
    let options = match style {
        Some("integer") => NumberFormatOptions {
            maximum_fraction_digits: Some(0),
            ..NumberFormatOptions::default()
        },
        Some("percent") => NumberFormatOptions {
            style: NumberStyle::Percent,
            ..NumberFormatOptions::default()
        },
        _ => NumberFormatOptions::default(),
    };
    strip_bidi_marks(&number_format_text(settings, &options, value))
}

pub fn format_percent(
    value: f64,
    options: &PercentFormatOptions,
    settings: &FormatSettings,
) -> String {
    let d = clamp_decimals(options.decimals.unwrap_or(2.0));
    let signed = options.signed.unwrap_or(true);
    if settings.english {
        let text = if signed {
            let sign = if value >= 0.0 { '+' } else { '-' };
            format!("{sign}{}%", to_fixed(value.abs(), d))
        } else {
            format!("{}%", to_fixed(value, d))
        };
        return localize_ascii(&text, settings);
    }
    let nf = NumberFormatOptions {
        style: NumberStyle::Percent,
        sign_display: Some(if signed {
            SignDisplay::Always
        } else {
            SignDisplay::Auto
        }),
        ..fraction_digits(d)
    };
    strip_bidi_marks(&number_format_text(settings, &nf, value / 100.0))
}

pub fn format_price(value: f64, options: &PriceFormatOptions, settings: &FormatSettings) -> String {
    let mode = options.mode.unwrap_or_default();
    if mode == PriceMode::Percent {
        let percent = PercentFormatOptions {
            decimals: Some(options.decimals),
            signed: Some(true),
            ..PercentFormatOptions::default()
        };
        return format_percent(value, &percent, settings);
    }
    if mode == PriceMode::Indexed || !options.grouping.unwrap_or(false) {
        return format_fixed(value, options.decimals, settings);
    }
    format_grouped(value, options.decimals, settings)
}

const COMPACT_STEPS: [(f64, &str); 3] = [(1e9, "B"), (1e6, "M"), (1e3, "K")];

/// A magnitude abbreviated for volume readouts. English keeps Vela's `K`/`M`/`B` with two
/// decimals (`trim_zeros` drops trailing zeros: `1.2M`; without it: `1.20M`); other
/// languages use the locale's compact notation (`120万`, `1,2 Mio.`).
pub fn format_compact(
    value: f64,
    options: &CompactFormatOptions,
    settings: &FormatSettings,
) -> String {
    let d = clamp_decimals(options.decimals.unwrap_or(2.0));
    let trim = options.trim_zeros.unwrap_or(true);
    if settings.english {
        let a = value.abs();
        let (div, suffix) = COMPACT_STEPS
            .iter()
            .find(|(step, _)| a >= *step)
            .map_or((1.0, ""), |(step, s)| (*step, *s));
        let scaled = value / div;
        let fixed = to_fixed(scaled, d);
        let text = if trim {
            // `Number(text).toString()`: drops the trailing zeros and the sign of a zero.
            fixed
                .parse::<f64>()
                .map_or_else(|_| fixed.clone(), number_text)
        } else {
            fixed
        };
        return localize_ascii(&text, settings) + suffix;
    }
    let nf = NumberFormatOptions {
        notation: Notation::Compact,
        compact_display: Some(crate::intl::CompactDisplay::Short),
        maximum_fraction_digits: Some(d as u32),
        minimum_fraction_digits: Some(if trim { 0 } else { d as u32 }),
        ..NumberFormatOptions::default()
    };
    strip_bidi_marks(&number_format_text(settings, &nf, value))
}

/// A narrow unit through the locale (`2d`, `4h`).
pub(crate) fn format_unit(value: f64, unit: NumberUnit, settings: &FormatSettings) -> String {
    let nf = NumberFormatOptions {
        style: NumberStyle::Unit,
        unit: Some(unit),
        unit_display: Some(UnitDisplay::Narrow),
        ..NumberFormatOptions::default()
    };
    strip_bidi_marks(&number_format_text(settings, &nf, value))
}
