//! Timezone catalog and labels. The canonical data lives in the crate's core
//! (`crate::timezones`) so the renderer's settings dialog shares the exact same list;
//! re-exported here to keep the widget's surface the original's.

pub use crate::timezones::{
    EXCHANGE_TIMEZONE, TimezoneEntry, TimezoneRow, normalize_timezone, resolve_timezone,
    timezone_menu_rows, timezones, tz_button_label, tz_menu_label, tz_offset,
};

/// `TIMEZONES`: the catalog, UTC first.
pub fn catalog() -> &'static [TimezoneEntry] {
    timezones()
}
