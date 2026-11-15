//! The canonical display-timezone catalog: ONE list for every zone picker (the widget's
//! bottom bar, the time-axis context menu, the renderer's settings dialog), so they can
//! never drift apart. Ordered by standard UTC offset (UTC first); every whole, half and
//! quarter-hour offset from UTC-12 to UTC+14 is represented. Offsets are computed live
//! (DST-aware), never hardcoded in labels.
//!
//! Offsets come from the [`Intl`] backend: with the built-in English one only UTC and
//! fixed-offset zones resolve, and every other zone reads `UTC`.

use std::borrow::Cow;
use std::sync::OnceLock;

use crate::format::dates::zone_name_part;
use crate::i18n::{MessageDescriptor, MessageLike, Translator, msg, source_text, text_of};
use crate::intl::Intl;

/// One zone of the catalog.
#[derive(Clone, Debug, PartialEq)]
pub struct TimezoneEntry {
    /// The IANA id a pick stores.
    pub value: &'static str,
    /// The place the zone is named after, in English.
    pub label: String,
    /// The same name as a message, for pickers in other languages.
    pub name: MessageDescriptor,
}

const ROWS: &[(&str, &str, &str)] = &[
    ("Etc/UTC", "timezone.Etc.UTC", "UTC"),
    (
        "Etc/GMT+12",
        "timezone.Etc.GMTPlus12",
        "International Date Line West",
    ),
    (
        "Pacific/Pago_Pago",
        "timezone.Pacific.PagoPago",
        "Pago Pago",
    ),
    ("Pacific/Honolulu", "timezone.Pacific.Honolulu", "Honolulu"),
    (
        "Pacific/Marquesas",
        "timezone.Pacific.Marquesas",
        "Marquesas Islands",
    ),
    (
        "America/Anchorage",
        "timezone.America.Anchorage",
        "Anchorage",
    ),
    (
        "America/Los_Angeles",
        "timezone.America.LosAngeles",
        "Los Angeles",
    ),
    ("America/Phoenix", "timezone.America.Phoenix", "Phoenix"),
    ("America/Denver", "timezone.America.Denver", "Denver"),
    ("America/Chicago", "timezone.America.Chicago", "Chicago"),
    (
        "America/Mexico_City",
        "timezone.America.MexicoCity",
        "Mexico City",
    ),
    ("America/New_York", "timezone.America.NewYork", "New York"),
    ("America/Bogota", "timezone.America.Bogota", "Bogotá"),
    ("America/Caracas", "timezone.America.Caracas", "Caracas"),
    ("America/Santiago", "timezone.America.Santiago", "Santiago"),
    ("America/St_Johns", "timezone.America.StJohns", "St. John's"),
    (
        "America/Sao_Paulo",
        "timezone.America.SaoPaulo",
        "São Paulo",
    ),
    (
        "America/Argentina/Buenos_Aires",
        "timezone.America.Argentina.BuenosAires",
        "Buenos Aires",
    ),
    (
        "America/Noronha",
        "timezone.America.Noronha",
        "Fernando de Noronha",
    ),
    ("Atlantic/Azores", "timezone.Atlantic.Azores", "Azores"),
    (
        "Atlantic/Reykjavik",
        "timezone.Atlantic.Reykjavik",
        "Reykjavik",
    ),
    ("Europe/London", "timezone.Europe.London", "London"),
    ("Europe/Paris", "timezone.Europe.Paris", "Paris"),
    ("Europe/Berlin", "timezone.Europe.Berlin", "Berlin"),
    ("Europe/Athens", "timezone.Europe.Athens", "Athens"),
    ("Africa/Cairo", "timezone.Africa.Cairo", "Cairo"),
    (
        "Africa/Johannesburg",
        "timezone.Africa.Johannesburg",
        "Johannesburg",
    ),
    ("Europe/Moscow", "timezone.Europe.Moscow", "Moscow"),
    ("Europe/Istanbul", "timezone.Europe.Istanbul", "Istanbul"),
    ("Asia/Tehran", "timezone.Asia.Tehran", "Tehran"),
    ("Asia/Dubai", "timezone.Asia.Dubai", "Dubai"),
    ("Asia/Kabul", "timezone.Asia.Kabul", "Kabul"),
    ("Asia/Karachi", "timezone.Asia.Karachi", "Karachi"),
    ("Asia/Kolkata", "timezone.Asia.Kolkata", "Mumbai"),
    ("Asia/Kathmandu", "timezone.Asia.Kathmandu", "Kathmandu"),
    ("Asia/Dhaka", "timezone.Asia.Dhaka", "Dhaka"),
    ("Asia/Yangon", "timezone.Asia.Yangon", "Yangon"),
    ("Asia/Bangkok", "timezone.Asia.Bangkok", "Bangkok"),
    ("Asia/Shanghai", "timezone.Asia.Shanghai", "Shanghai"),
    ("Asia/Hong_Kong", "timezone.Asia.HongKong", "Hong Kong"),
    ("Asia/Singapore", "timezone.Asia.Singapore", "Singapore"),
    ("Australia/Eucla", "timezone.Australia.Eucla", "Eucla"),
    ("Asia/Tokyo", "timezone.Asia.Tokyo", "Tokyo"),
    ("Asia/Seoul", "timezone.Asia.Seoul", "Seoul"),
    (
        "Australia/Adelaide",
        "timezone.Australia.Adelaide",
        "Adelaide",
    ),
    ("Australia/Sydney", "timezone.Australia.Sydney", "Sydney"),
    (
        "Australia/Lord_Howe",
        "timezone.Australia.LordHowe",
        "Lord Howe Island",
    ),
    ("Pacific/Noumea", "timezone.Pacific.Noumea", "Nouméa"),
    ("Pacific/Auckland", "timezone.Pacific.Auckland", "Auckland"),
    (
        "Pacific/Chatham",
        "timezone.Pacific.Chatham",
        "Chatham Islands",
    ),
    ("Pacific/Apia", "timezone.Pacific.Apia", "Apia"),
    (
        "Pacific/Kiritimati",
        "timezone.Pacific.Kiritimati",
        "Kiritimati",
    ),
];

/// The catalog, UTC first.
pub fn timezones() -> &'static [TimezoneEntry] {
    static CATALOG: OnceLock<Vec<TimezoneEntry>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        ROWS.iter()
            .map(|(value, id, label)| {
                let name = MessageDescriptor {
                    id: Cow::Borrowed(id),
                    default_message: Cow::Borrowed(label),
                    values: Vec::new(),
                };
                TimezoneEntry {
                    value,
                    label: source_text(&MessageLike::Message(name.clone())),
                    name,
                }
            })
            .collect()
    })
}

/// The "follow the market" choice: not a zone but a RULE, resolved per chart to the
/// symbol's own trading timezone (Chicago for CME futures, New York for US equities, UTC
/// for crypto). It is what the workspace stores and persists; a renderer only ever
/// receives the resolved IANA zone (see [`resolve_timezone`]).
pub const EXCHANGE_TIMEZONE: &str = "exchange";

pub fn is_exchange_timezone(zone: &str) -> bool {
    zone == EXCHANGE_TIMEZONE
}

/// The IANA zone a chart actually renders in. The exchange rule resolves to the market's
/// zone, falling back to UTC while symbol metadata is unknown (not landed yet, or a
/// provider that declares none); an explicit zone passes through untouched.
pub fn resolve_timezone(zone: &str, exchange_zone: Option<&str>) -> String {
    if !is_exchange_timezone(zone) {
        return zone.to_string();
    }
    match exchange_zone {
        Some(z) if !z.is_empty() => z.to_string(),
        _ => "Etc/UTC".to_string(),
    }
}

/// The renderer's config default is the bare `UTC` alias: fold it (and any other UTC
/// spelling) onto the catalog's `Etc/UTC` so selection checks land on one entry.
pub fn normalize_timezone(zone: &str) -> &str {
    match zone {
        "UTC" | "Etc/UTC" | "Etc/GMT" => "Etc/UTC",
        other => other,
    }
}

/// One row of a zone picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimezoneRow {
    /// The value a pick stores: an IANA zone, or [`EXCHANGE_TIMEZONE`].
    pub value: String,
    pub label: String,
    pub checked: bool,
}

fn exchange_label() -> MessageLike {
    msg("timezone.exchange", "Exchange")
}

fn menu_label() -> MessageLike {
    msg("timezone.menuLabel", "({offset}) {location}")
}

/// The rows every zone PICKER offers (bottom bar, time-axis menu, mobile sheet, the
/// workspace's chart-settings row): UTC, then the exchange rule, then the rest of the
/// catalog. `current` is the stored choice (rule or zone). The exchange row reads plain
/// "Exchange": it is a rule, not a zone, so it carries no offset (the bar's clock/offset
/// label shows the resolved zone). The renderer's own settings row does NOT use this: it
/// edits a resolved zone and lists [`timezones`] alone.
///
/// `at_ms` is the instant the offsets are read at (the original reads the current time).
pub fn timezone_menu_rows(current: &str, i18n: &dyn Translator, at_ms: f64) -> Vec<TimezoneRow> {
    let active = normalize_timezone(current);
    let mut rows: Vec<TimezoneRow> = timezones()
        .iter()
        .map(|t| TimezoneRow {
            value: t.value.to_string(),
            label: tz_menu_label(
                t.value,
                &text_of(Some(i18n), &MessageLike::Message(t.name.clone()), &[]),
                i18n,
                at_ms,
            ),
            checked: t.value == active,
        })
        .collect();
    let exchange = TimezoneRow {
        value: EXCHANGE_TIMEZONE.to_string(),
        label: text_of(Some(i18n), &exchange_label(), &[]),
        checked: is_exchange_timezone(current),
    };
    rows.insert(1.min(rows.len()), exchange);
    rows
}

/// Current UTC offset of an IANA zone as `"UTC"`, `"UTC+2"` or `"UTC-9:30"`. The
/// formatter stays pinned to en-US whatever the chart's locale: the `GMT+h:mm` it emits is
/// PARSED here, and a localized one could spell the offset differently. A zone the
/// backend rejects reads `"UTC"`.
pub fn tz_offset(intl: &dyn Intl, zone: &str, at_ms: f64) -> String {
    let raw = zone_name_part(intl, zone, at_ms).unwrap_or_default();
    parse_offset(&raw).unwrap_or_else(|| "UTC".to_string())
}

/// `GMT`, `GMT+2`, `GMT-9:30` (the first `GMT` in `raw`) as a `UTC` label.
fn parse_offset(raw: &str) -> Option<String> {
    let rest = &raw[raw.find("GMT")? + 3..];
    let mut chars = rest.chars();
    let sign = match chars.next()? {
        '+' => '+',
        '-' => '-',
        _ => return None,
    };
    let after = chars.as_str();
    let hours: String = after
        .chars()
        .take_while(char::is_ascii_digit)
        .take(2)
        .collect();
    if hours.is_empty() {
        return None;
    }
    let tail = &after[hours.len()..];
    let minutes = tail
        .strip_prefix(':')
        .map(|m| {
            m.chars()
                .take_while(char::is_ascii_digit)
                .take(2)
                .collect::<String>()
        })
        .filter(|m| m.len() == 2);
    Some(match minutes {
        Some(m) => format!("UTC{sign}{hours}:{m}"),
        None => format!("UTC{sign}{hours}"),
    })
}

/// Dropdown row label: UTC omits the offset prefix. `location` is the zone's place name
/// as the picker shows it; `i18n` places the offset the way its language writes it.
pub fn tz_menu_label(zone: &str, location: &str, i18n: &dyn Translator, at_ms: f64) -> String {
    if normalize_timezone(zone) == "Etc/UTC" {
        return location.to_string();
    }
    let offset = tz_offset(i18n.format().settings().intl.as_ref(), zone, at_ms);
    text_of(
        Some(i18n),
        &menu_label(),
        &[
            ("offset", offset.into()),
            ("location", location.to_string().into()),
        ],
    )
}

/// Compact label for the bottom-bar button: just the offset (`UTC`, `UTC+2`, `UTC-9:30`).
pub fn tz_button_label(intl: &dyn Intl, zone: &str, at_ms: f64) -> String {
    // Some ICU builds format Etc/UTC as "GMT+0": fold that back to the bare "UTC".
    if normalize_timezone(zone) == "Etc/UTC" {
        return "UTC".to_string();
    }
    tz_offset(intl, zone, at_ms)
}
