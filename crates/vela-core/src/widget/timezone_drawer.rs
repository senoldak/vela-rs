//! Timezone drawer: the mobile bottom sheet of display time zones.
//!
//! The pure half of the original `TimezoneDrawer`: the title and the rows
//! ([`timezone_drawer_rows`]; the same rows as the desktop dropdown). Left for the browser
//! crate: the sheet, the row elements with the check icon and CSS, and hiding the sheet
//! after a pick (which reports the zone value).

use super::timezones::timezone_menu_rows;
use crate::i18n::{MessageLike, Translator, english_translator, msg};

pub fn title_message() -> MessageLike {
    msg("widget.timezoneDrawer.title", "Time zone")
}

/// One row: the value a pick stores, its label, and whether it is the stored choice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimezoneDrawerRow {
    pub value: String,
    pub label: String,
    pub checked: bool,
}

/// The rows for the stored choice (a zone, or the exchange rule): UTC, the exchange rule,
/// then the catalog, with one check. `at_ms` is the instant the offsets are read at.
pub fn timezone_drawer_rows(
    timezone: &str,
    i18n: Option<&dyn Translator>,
    at_ms: f64,
) -> Vec<TimezoneDrawerRow> {
    let translator: &dyn Translator = i18n.unwrap_or_else(|| english_translator());
    timezone_menu_rows(timezone, translator, at_ms)
        .into_iter()
        .map(|r| TimezoneDrawerRow {
            value: r.value,
            label: r.label,
            checked: r.checked,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::text_of;

    #[test]
    fn the_rows_check_exactly_the_stored_choice() {
        let rows = timezone_drawer_rows("exchange", None, 0.0);
        assert_eq!(rows[0].value, "Etc/UTC");
        assert_eq!(
            rows[1],
            TimezoneDrawerRow {
                value: "exchange".into(),
                label: "Exchange".into(),
                checked: true
            }
        );
        assert_eq!(rows.iter().filter(|r| r.checked).count(), 1);
        let fixed = timezone_drawer_rows("UTC", None, 0.0);
        assert!(!fixed[1].checked);
        let checked: Vec<&str> = fixed
            .iter()
            .filter(|r| r.checked)
            .map(|r| r.value.as_str())
            .collect();
        assert_eq!(checked, ["Etc/UTC"]);
        assert_eq!(text_of(None, &title_message(), &[]), "Time zone");
    }
}
