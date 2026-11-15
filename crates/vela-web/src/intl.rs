//! [`vela_core::intl::Intl`] on top of the browser's own `Intl` API.
//!
//! Each trait method is one small JavaScript function in the snippet below.
//! A call that throws in JavaScript (an invalid locale tag or time zone, an
//! engine without the feature) comes back as `undefined`, which the trait
//! reports as `None`; core then takes the same fallback the original takes in
//! its `catch` blocks. The snippet caches the formatter objects, which are
//! costly to construct, by locale and options.

use js_sys::{Array, Object, Reflect};
use vela_core::intl::{
    CompactDisplay, DateTimeFormatOptions, FormatPart, Granularity, HourCycle, Intl, LocaleInfo,
    MonthStyle, Notation, NumberFormatOptions, NumberStyle, NumericStyle, PartType, PluralCategory,
    PluralKind, Segment, SignDisplay, TextDirection, TimeZoneNameStyle, UnitDisplay, WeekdayStyle,
};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = r#"
const formatters = new Map();

function cached(kind, locale, options, make) {
    const key = kind + '\u0000' + locale + '\u0000' + JSON.stringify(options);
    let hit = formatters.get(key);
    if (hit === undefined) {
        hit = make();
        formatters.set(key, hit);
    }
    return hit;
}

const numberFormat = (locale, options) => cached('n', locale, options, () => new Intl.NumberFormat(locale, options));
const dateFormat = (locale, options) => cached('d', locale, options, () => new Intl.DateTimeFormat(locale, options));
const flatParts = (parts) => parts.flatMap((p) => [p.type, p.value]);

export function vela_canonicalize_locale(tag) {
    try {
        return Intl.getCanonicalLocales(tag)[0];
    } catch {
        return undefined;
    }
}

export function vela_locale_info(tag, maximize) {
    try {
        let locale = new Intl.Locale(tag);
        if (maximize) locale = locale.maximize();
        return [locale.language, locale.script ?? null, locale.region ?? null, locale.baseName, locale.numberingSystem ?? null];
    } catch {
        return undefined;
    }
}

export function vela_locale_text_direction(tag) {
    try {
        const locale = new Intl.Locale(tag);
        const info = typeof locale.getTextInfo === 'function' ? locale.getTextInfo() : locale.textInfo;
        return info?.direction;
    } catch {
        return undefined;
    }
}

export function vela_locale_week_first_day(tag) {
    try {
        const locale = new Intl.Locale(tag);
        const info = typeof locale.getWeekInfo === 'function' ? locale.getWeekInfo() : locale.weekInfo;
        return info?.firstDay;
    } catch {
        return undefined;
    }
}

export function vela_plural_category(locale, ordinal, n) {
    try {
        const type = ordinal ? 'ordinal' : 'cardinal';
        return cached('p', locale, type, () => new Intl.PluralRules(locale, { type })).select(n);
    } catch {
        return undefined;
    }
}

export function vela_format_number(locale, options, value) {
    try {
        return numberFormat(locale, options).format(value);
    } catch {
        return undefined;
    }
}

export function vela_format_number_to_parts(locale, options, value) {
    try {
        return flatParts(numberFormat(locale, options).formatToParts(value));
    } catch {
        return undefined;
    }
}

export function vela_resolved_numbering_system(locale, requested) {
    try {
        const options = requested == null ? undefined : { numberingSystem: requested };
        return new Intl.NumberFormat(locale, options).resolvedOptions().numberingSystem;
    } catch {
        return undefined;
    }
}

export function vela_format_date_time(locale, options, ms) {
    try {
        return dateFormat(locale, options).format(ms);
    } catch {
        return undefined;
    }
}

export function vela_format_date_time_to_parts(locale, options, ms) {
    try {
        return flatParts(dateFormat(locale, options).formatToParts(ms));
    } catch {
        return undefined;
    }
}

export function vela_default_time_zone() {
    try {
        return new Intl.DateTimeFormat().resolvedOptions().timeZone;
    } catch {
        return undefined;
    }
}

export function vela_format_default_time(ms) {
    return new Date(ms).toLocaleTimeString();
}

export function vela_segment(locale, granularity, text) {
    if (typeof Intl.Segmenter !== 'function') return undefined;
    try {
        const segmenter = cached('s', locale ?? '', granularity, () => new Intl.Segmenter(locale ?? undefined, { granularity }));
        const out = [];
        for (const s of segmenter.segment(text)) out.push(s.segment, s.index, s.isWordLike ?? null);
        return out;
    } catch {
        return undefined;
    }
}

export function vela_to_upper_case(text, locale) {
    try {
        return text.toLocaleUpperCase(locale ?? undefined);
    } catch {
        return undefined;
    }
}
"#)]
extern "C" {
    fn vela_canonicalize_locale(tag: &str) -> Option<String>;
    fn vela_locale_info(tag: &str, maximize: bool) -> JsValue;
    fn vela_locale_text_direction(tag: &str) -> Option<String>;
    fn vela_locale_week_first_day(tag: &str) -> Option<f64>;
    fn vela_plural_category(locale: &str, ordinal: bool, n: f64) -> Option<String>;
    fn vela_format_number(locale: &str, options: &Object, value: f64) -> Option<String>;
    fn vela_format_number_to_parts(locale: &str, options: &Object, value: f64) -> JsValue;
    fn vela_resolved_numbering_system(locale: &str, requested: Option<String>) -> Option<String>;
    fn vela_format_date_time(locale: &str, options: &Object, ms: f64) -> Option<String>;
    fn vela_format_date_time_to_parts(locale: &str, options: &Object, ms: f64) -> JsValue;
    fn vela_default_time_zone() -> Option<String>;
    fn vela_format_default_time(ms: f64) -> String;
    fn vela_segment(locale: Option<String>, granularity: &str, text: &str) -> JsValue;
    fn vela_to_upper_case(text: &str, locale: Option<String>) -> Option<String>;
}

/// The browser's `Intl`, as core's [`Intl`] backend.
#[derive(Clone, Copy, Debug, Default)]
pub struct BrowserIntl;

fn set(target: &Object, key: &str, value: impl Into<JsValue>) {
    // Setting a plain data property on a fresh object cannot fail.
    let _ = Reflect::set(target, &JsValue::from_str(key), &value.into());
}

fn numeric(style: NumericStyle) -> &'static str {
    match style {
        NumericStyle::Numeric => "numeric",
        NumericStyle::TwoDigit => "2-digit",
    }
}

fn number_options(o: &NumberFormatOptions) -> Object {
    let out = Object::new();
    match o.style {
        NumberStyle::Decimal => {}
        NumberStyle::Percent => set(&out, "style", "percent"),
        NumberStyle::Unit => set(&out, "style", "unit"),
    }
    if let Some(unit) = o.unit {
        set(&out, "unit", unit.as_str());
    }
    if let Some(UnitDisplay::Narrow) = o.unit_display {
        set(&out, "unitDisplay", "narrow");
    }
    if o.notation == Notation::Compact {
        set(&out, "notation", "compact");
    }
    if let Some(CompactDisplay::Short) = o.compact_display {
        set(&out, "compactDisplay", "short");
    }
    if let Some(sign) = o.sign_display {
        set(
            &out,
            "signDisplay",
            match sign {
                SignDisplay::Auto => "auto",
                SignDisplay::Always => "always",
            },
        );
    }
    if let Some(grouping) = o.use_grouping {
        set(&out, "useGrouping", grouping);
    }
    if let Some(digits) = o.minimum_fraction_digits {
        set(&out, "minimumFractionDigits", digits);
    }
    if let Some(digits) = o.maximum_fraction_digits {
        set(&out, "maximumFractionDigits", digits);
    }
    if let Some(system) = &o.numbering_system {
        set(&out, "numberingSystem", system.as_str());
    }
    out
}

fn date_options(o: &DateTimeFormatOptions) -> Object {
    let out = Object::new();
    if let Some(zone) = &o.time_zone {
        set(&out, "timeZone", zone.as_str());
    }
    if let Some(calendar) = &o.calendar {
        set(&out, "calendar", calendar.as_str());
    }
    if let Some(system) = &o.numbering_system {
        set(&out, "numberingSystem", system.as_str());
    }
    if let Some(cycle) = o.hour_cycle {
        set(
            &out,
            "hourCycle",
            match cycle {
                HourCycle::H11 => "h11",
                HourCycle::H12 => "h12",
                HourCycle::H23 => "h23",
                HourCycle::H24 => "h24",
            },
        );
    }
    if let Some(hour12) = o.hour12 {
        set(&out, "hour12", hour12);
    }
    if let Some(WeekdayStyle::Short) = o.weekday {
        set(&out, "weekday", "short");
    }
    if let Some(year) = o.year {
        set(&out, "year", numeric(year));
    }
    if let Some(month) = o.month {
        set(
            &out,
            "month",
            match month {
                MonthStyle::TwoDigit => "2-digit",
                MonthStyle::Short => "short",
                MonthStyle::Long => "long",
            },
        );
    }
    if let Some(day) = o.day {
        set(&out, "day", numeric(day));
    }
    if let Some(hour) = o.hour {
        set(&out, "hour", numeric(hour));
    }
    if let Some(minute) = o.minute {
        set(&out, "minute", numeric(minute));
    }
    if let Some(second) = o.second {
        set(&out, "second", numeric(second));
    }
    if let Some(TimeZoneNameStyle::ShortOffset) = o.time_zone_name {
        set(&out, "timeZoneName", "shortOffset");
    }
    out
}

/// A JS array, or `None` for `undefined` (the call threw).
fn array(value: JsValue) -> Option<Array> {
    value.dyn_into::<Array>().ok()
}

/// `[type, value, type, value, …]` → parts.
fn parts(value: JsValue) -> Option<Vec<FormatPart>> {
    let flat = array(value)?;
    let mut out = Vec::with_capacity(flat.length() as usize / 2);
    let mut i = 0;
    while i + 1 < flat.length() {
        let kind = flat.get(i).as_string()?;
        let text = flat.get(i + 1).as_string()?;
        out.push(FormatPart::new(PartType::from_name(&kind), text));
        i += 2;
    }
    Some(out)
}

fn locale_info(tag: &str, maximize: bool) -> Option<LocaleInfo> {
    let fields = array(vela_locale_info(tag, maximize))?;
    Some(LocaleInfo {
        language: fields.get(0).as_string()?,
        script: fields.get(1).as_string(),
        region: fields.get(2).as_string(),
        base_name: fields.get(3).as_string()?,
        numbering_system: fields.get(4).as_string(),
    })
}

impl Intl for BrowserIntl {
    fn canonicalize_locale(&self, tag: &str) -> Option<String> {
        vela_canonicalize_locale(tag)
    }

    fn parse_locale(&self, tag: &str) -> Option<LocaleInfo> {
        locale_info(tag, false)
    }

    fn maximize_locale(&self, tag: &str) -> Option<LocaleInfo> {
        locale_info(tag, true)
    }

    fn locale_text_direction(&self, tag: &str) -> Option<TextDirection> {
        match vela_locale_text_direction(tag)?.as_str() {
            "rtl" => Some(TextDirection::Rtl),
            "ltr" => Some(TextDirection::Ltr),
            _ => None,
        }
    }

    fn locale_week_first_day(&self, tag: &str) -> Option<u32> {
        let day = vela_locale_week_first_day(tag)?;
        (day.is_finite() && (1.0..=7.0).contains(&day)).then_some(day as u32)
    }

    fn plural_category(&self, locale: &str, kind: PluralKind, n: f64) -> Option<PluralCategory> {
        PluralCategory::from_name(&vela_plural_category(
            locale,
            kind == PluralKind::Ordinal,
            n,
        )?)
    }

    fn format_number(
        &self,
        locale: &str,
        options: &NumberFormatOptions,
        value: f64,
    ) -> Option<String> {
        vela_format_number(locale, &number_options(options), value)
    }

    fn format_number_to_parts(
        &self,
        locale: &str,
        options: &NumberFormatOptions,
        value: f64,
    ) -> Option<Vec<FormatPart>> {
        parts(vela_format_number_to_parts(
            locale,
            &number_options(options),
            value,
        ))
    }

    fn resolved_numbering_system(&self, locale: &str, requested: Option<&str>) -> Option<String> {
        vela_resolved_numbering_system(locale, requested.map(str::to_string))
    }

    fn format_date_time(
        &self,
        locale: &str,
        options: &DateTimeFormatOptions,
        ms: f64,
    ) -> Option<String> {
        vela_format_date_time(locale, &date_options(options), ms)
    }

    fn format_date_time_to_parts(
        &self,
        locale: &str,
        options: &DateTimeFormatOptions,
        ms: f64,
    ) -> Option<Vec<FormatPart>> {
        parts(vela_format_date_time_to_parts(
            locale,
            &date_options(options),
            ms,
        ))
    }

    fn default_time_zone(&self) -> Option<String> {
        vela_default_time_zone()
    }

    fn format_default_time(&self, ms: f64) -> String {
        vela_format_default_time(ms)
    }

    fn segment(
        &self,
        locale: Option<&str>,
        granularity: Granularity,
        text: &str,
    ) -> Option<Vec<Segment>> {
        let name = match granularity {
            Granularity::Grapheme => "grapheme",
            Granularity::Word => "word",
        };
        let flat = array(vela_segment(locale.map(str::to_string), name, text))?;
        let mut out = Vec::with_capacity(flat.length() as usize / 3);
        let mut i = 0;
        while i + 2 < flat.length() {
            out.push(Segment {
                segment: flat.get(i).as_string()?,
                index: flat.get(i + 1).as_f64()? as usize,
                is_word_like: flat.get(i + 2).as_bool(),
            });
            i += 3;
        }
        Some(out)
    }

    fn to_upper_case(&self, text: &str, locale: Option<&str>) -> Option<String> {
        vela_to_upper_case(text, locale.map(str::to_string))
    }
}
