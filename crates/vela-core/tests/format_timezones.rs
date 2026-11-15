//! The display-timezone catalog and the exchange rule. Ported from the parts of
//! `chart-cell-timezone.test.ts` that concern this layer (the rule's resolution and the
//! rows of the zone picker); the cell, feed and renderer cases are not part of this crate
//! layer. Zone offsets come from the `FakeIntl` test double.

mod i18n_common;

use std::collections::HashSet;

use i18n_common::{english, fake, locale, pack, registry, translator};
use vela_core::i18n::{LocaleState, LocalizationOptions, MessageLike, source_text};
use vela_core::timezones::{
    EXCHANGE_TIMEZONE, is_exchange_timezone, normalize_timezone, resolve_timezone,
    timezone_menu_rows, timezones, tz_button_label, tz_menu_label, tz_offset,
};

const OCT_23_2026: f64 = 1_792_713_600_000.0;

#[test]
fn the_exchange_rule_resolves_to_the_markets_zone_and_a_fixed_zone_passes_through() {
    assert_eq!(
        resolve_timezone(EXCHANGE_TIMEZONE, Some("America/Chicago")),
        "America/Chicago"
    );
    // Metadata not landed yet, or a provider that declares none: UTC.
    assert_eq!(resolve_timezone(EXCHANGE_TIMEZONE, None), "Etc/UTC");
    assert_eq!(resolve_timezone(EXCHANGE_TIMEZONE, Some("")), "Etc/UTC");
    assert_eq!(
        resolve_timezone("Europe/Paris", Some("America/Chicago")),
        "Europe/Paris"
    );
    assert_eq!(resolve_timezone("Asia/Tokyo", None), "Asia/Tokyo");
    assert!(is_exchange_timezone("exchange"));
    assert!(!is_exchange_timezone("Etc/UTC"));
}

#[test]
fn every_utc_spelling_folds_onto_the_catalogs_entry() {
    for spelling in ["UTC", "Etc/UTC", "Etc/GMT"] {
        assert_eq!(normalize_timezone(spelling), "Etc/UTC");
    }
    assert_eq!(normalize_timezone("Europe/London"), "Europe/London");
}

#[test]
fn the_catalog_lists_utc_first_and_every_label_is_its_english_name() {
    let catalog = timezones();
    assert_eq!(catalog.len(), 52);
    assert_eq!(
        (catalog[0].value, catalog[0].label.as_str()),
        ("Etc/UTC", "UTC")
    );
    let values: HashSet<&str> = catalog.iter().map(|t| t.value).collect();
    assert_eq!(values.len(), catalog.len(), "zone ids are unique");
    for t in catalog {
        assert_eq!(t.label, source_text(&MessageLike::Message(t.name.clone())));
        assert!(t.name.id.starts_with("timezone."), "{}", t.name.id);
    }
    let by_value = |v: &str| {
        catalog
            .iter()
            .find(|t| t.value == v)
            .map(|t| t.label.clone())
    };
    assert_eq!(by_value("America/St_Johns").as_deref(), Some("St. John's"));
    assert_eq!(by_value("Asia/Kolkata").as_deref(), Some("Mumbai"));
    assert_eq!(by_value("Pacific/Noumea").as_deref(), Some("Nouméa"));
}

#[test]
fn every_catalog_name_is_in_the_english_message_catalog() {
    let catalog: std::collections::HashMap<String, String> =
        serde_json::from_str(include_str!("../locales/en.json")).unwrap_or_default();
    for t in timezones() {
        assert_eq!(
            catalog.get(t.name.id.as_ref()).map(String::as_str),
            Some(t.name.default_message.as_ref()),
            "{}",
            t.name.id
        );
    }
    assert!(catalog.contains_key("timezone.exchange"));
    assert!(catalog.contains_key("timezone.menuLabel"));
}

#[test]
fn the_picker_lists_utc_then_exchange_then_the_rest_and_checks_the_stored_choice() {
    let t = translator(&fake(), LocalizationOptions::default());
    let rows = timezone_menu_rows("exchange", &t, OCT_23_2026);
    assert_eq!(rows.len(), timezones().len() + 1);
    assert_eq!(
        (rows[0].value.as_str(), rows[0].label.as_str()),
        ("Etc/UTC", "UTC")
    );
    assert_eq!(
        (rows[1].value.as_str(), rows[1].label.as_str()),
        ("exchange", "Exchange")
    );
    // The rule shows selected, not the resolved zone.
    assert_eq!(
        rows.iter()
            .filter(|r| r.checked)
            .map(|r| r.value.as_str())
            .collect::<Vec<_>>(),
        ["exchange"]
    );

    let tokyo = timezone_menu_rows("Asia/Tokyo", &t, OCT_23_2026);
    assert_eq!(
        tokyo
            .iter()
            .filter(|r| r.checked)
            .map(|r| r.value.as_str())
            .collect::<Vec<_>>(),
        ["Asia/Tokyo"]
    );
    let utc = timezone_menu_rows("UTC", &t, OCT_23_2026);
    assert_eq!(
        utc.iter()
            .filter(|r| r.checked)
            .map(|r| r.value.as_str())
            .collect::<Vec<_>>(),
        ["Etc/UTC"]
    );

    let label = |value: &str| {
        tokyo
            .iter()
            .find(|r| r.value == value)
            .map(|r| r.label.clone())
    };
    assert_eq!(
        label("America/New_York").as_deref(),
        Some("(UTC-4) New York")
    );
    assert_eq!(label("Asia/Kolkata").as_deref(), Some("(UTC+5:30) Mumbai"));
    assert_eq!(
        label("Pacific/Marquesas").as_deref(),
        Some("(UTC-9:30) Marquesas Islands")
    );
}

#[test]
fn the_built_in_backend_knows_no_region_zones_so_their_rows_read_utc() {
    let t = translator(&english(), LocalizationOptions::default());
    assert_eq!(
        tz_menu_label("America/New_York", "New York", &t, OCT_23_2026),
        "(UTC) New York"
    );
    assert_eq!(
        tz_menu_label("Etc/GMT+5", "Five", &t, OCT_23_2026),
        "(UTC-5) Five"
    );
    assert_eq!(tz_menu_label("Etc/UTC", "UTC", &t, OCT_23_2026), "UTC");
}

#[test]
fn a_language_places_the_offset_the_way_it_writes_it() {
    let intl = fake();
    let registry = registry(&intl);
    registry.register(&pack(
        "de",
        &[
            ("timezone.menuLabel", "{location} ({offset})"),
            ("timezone.America.NewYork", "New York (Ostküste)"),
        ],
    ));
    let t = LocaleState::new(locale("de"), intl, registry);
    let rows = timezone_menu_rows("Etc/UTC", &t, OCT_23_2026);
    let ny = rows
        .iter()
        .find(|r| r.value == "America/New_York")
        .map(|r| r.label.clone());
    assert_eq!(ny.as_deref(), Some("New York (Ostküste) (UTC-4)"));
}

#[test]
fn an_offset_reads_as_utc_plus_or_minus_hours_and_minutes() {
    let intl = fake();
    let off = |zone: &str| tz_offset(intl.as_ref(), zone, OCT_23_2026);
    assert_eq!(off("America/New_York"), "UTC-4");
    assert_eq!(off("Europe/Paris"), "UTC+2");
    assert_eq!(off("Asia/Kolkata"), "UTC+5:30");
    assert_eq!(off("Pacific/Marquesas"), "UTC-9:30");
    assert_eq!(off("Etc/UTC"), "UTC");
    // An unknown zone, and an invalid instant, read UTC.
    assert_eq!(off("Not/AZone"), "UTC");
    assert_eq!(
        tz_offset(intl.as_ref(), "America/New_York", f64::NAN),
        "UTC"
    );
}

#[test]
fn the_bottom_bar_button_shows_just_the_offset() {
    let intl = fake();
    for utc in ["UTC", "Etc/UTC", "Etc/GMT"] {
        assert_eq!(tz_button_label(intl.as_ref(), utc, OCT_23_2026), "UTC");
    }
    assert_eq!(
        tz_button_label(intl.as_ref(), "Asia/Tokyo", OCT_23_2026),
        "UTC+9"
    );
    assert_eq!(
        tz_button_label(intl.as_ref(), "Pacific/Marquesas", OCT_23_2026),
        "UTC-9:30"
    );
}
