//! Test-only support: an [`Intl`] that knows a handful of real time zones (with their DST
//! rules) on top of the built-in English backend, which resolves only UTC and fixed
//! offsets. The widget's session and market-status logic is zone-aware, so its tests need
//! a few zones to be true to the original's `Intl.DateTimeFormat`.

use std::rc::Rc;
use std::sync::Arc;

use super::contributions::{
    ContextMenuPointer, DockEdge, DockStripOptions, DockedPanelInfo, DockedStrip, ElementHandle,
    ExternalIndicatorEntry, PanelDockView, SidebarBadge, SidebarControl, ToastKind, TopbarItemId,
    TopbarItems, WidgetContext,
};
use crate::i18n::Translator;
use crate::runtime::Unsubscribe;
use crate::vela::Vela;

use crate::intl::{
    DateTimeFormatOptions, EnglishIntl, FormatPart, Granularity, Intl, LocaleInfo,
    NumberFormatOptions, PluralCategory, PluralKind, Segment, TextDirection,
};

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn year_of(ms: f64) -> i64 {
    let days = (ms / 86_400_000.0).floor() as i64;
    // civil_from_days, year only
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 { y + 1 } else { y }
}

/// Epoch ms of the n-th (1-based) Sunday of a month at `hour` UTC; `n = 0` is the last.
fn nth_sunday_ms(year: i64, month: i64, n: i64, hour: i64) -> f64 {
    let weekday_of = |days: i64| (days + 4).rem_euclid(7); // 0 = Sunday (1970-01-01 was Thursday)
    let day = if n == 0 {
        let next = if month == 12 {
            days_from_civil(year + 1, 1, 1)
        } else {
            days_from_civil(year, month + 1, 1)
        };
        let last = next - 1;
        last - weekday_of(last)
    } else {
        let first = days_from_civil(year, month, 1);
        first + (7 - weekday_of(first)) % 7 + 7 * (n - 1)
    };
    (day * 86_400_000 + hour * 3_600_000) as f64
}

/// The zone's UTC offset in minutes at `ms`, for the zones the tests use.
fn offset_minutes(zone: &str, ms: f64) -> Option<i64> {
    let y = year_of(ms);
    Some(match zone {
        "America/New_York" | "America/Chicago" => {
            let std = if zone == "America/New_York" { -5 } else { -6 };
            // DST: second Sunday of March 02:00 standard, to first Sunday of November
            // 02:00 daylight.
            let start = nth_sunday_ms(y, 3, 2, 2 - std);
            let end = nth_sunday_ms(y, 11, 1, 2 - (std + 1));
            (if ms >= start && ms < end {
                std + 1
            } else {
                std
            }) * 60
        }
        "Europe/Paris" => {
            let start = nth_sunday_ms(y, 3, 0, 1);
            let end = nth_sunday_ms(y, 10, 0, 1);
            (if ms >= start && ms < end { 2 } else { 1 }) * 60
        }
        "Asia/Tokyo" => 9 * 60,
        _ => return None,
    })
}

fn offset_id(minutes: i64) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let m = minutes.abs();
    format!("{sign}{:02}:{:02}", m / 60, m % 60)
}

/// The English backend plus real offsets for New York, Chicago, Paris and Tokyo.
#[derive(Clone, Copy, Debug, Default)]
pub struct ZoneIntl;

impl ZoneIntl {
    pub fn arc() -> Arc<dyn Intl> {
        Arc::new(ZoneIntl)
    }

    fn rewrite(options: &DateTimeFormatOptions, ms: f64) -> DateTimeFormatOptions {
        let mut o = options.clone();
        if let Some(zone) = &options.time_zone
            && let Some(minutes) = offset_minutes(zone, ms)
        {
            o.time_zone = Some(offset_id(minutes));
        }
        o
    }
}

impl Intl for ZoneIntl {
    fn canonicalize_locale(&self, tag: &str) -> Option<String> {
        EnglishIntl.canonicalize_locale(tag)
    }
    fn parse_locale(&self, tag: &str) -> Option<LocaleInfo> {
        EnglishIntl.parse_locale(tag)
    }
    fn maximize_locale(&self, tag: &str) -> Option<LocaleInfo> {
        EnglishIntl.maximize_locale(tag)
    }
    fn locale_text_direction(&self, tag: &str) -> Option<TextDirection> {
        EnglishIntl.locale_text_direction(tag)
    }
    fn locale_week_first_day(&self, tag: &str) -> Option<u32> {
        EnglishIntl.locale_week_first_day(tag)
    }
    fn plural_category(&self, locale: &str, kind: PluralKind, n: f64) -> Option<PluralCategory> {
        EnglishIntl.plural_category(locale, kind, n)
    }
    fn format_number(
        &self,
        locale: &str,
        options: &NumberFormatOptions,
        value: f64,
    ) -> Option<String> {
        EnglishIntl.format_number(locale, options, value)
    }
    fn format_number_to_parts(
        &self,
        locale: &str,
        options: &NumberFormatOptions,
        value: f64,
    ) -> Option<Vec<FormatPart>> {
        EnglishIntl.format_number_to_parts(locale, options, value)
    }
    fn resolved_numbering_system(&self, locale: &str, requested: Option<&str>) -> Option<String> {
        EnglishIntl.resolved_numbering_system(locale, requested)
    }
    fn format_date_time(
        &self,
        locale: &str,
        options: &DateTimeFormatOptions,
        ms: f64,
    ) -> Option<String> {
        EnglishIntl.format_date_time(locale, &Self::rewrite(options, ms), ms)
    }
    fn format_date_time_to_parts(
        &self,
        locale: &str,
        options: &DateTimeFormatOptions,
        ms: f64,
    ) -> Option<Vec<FormatPart>> {
        EnglishIntl.format_date_time_to_parts(locale, &Self::rewrite(options, ms), ms)
    }
    fn default_time_zone(&self) -> Option<String> {
        EnglishIntl.default_time_zone()
    }
    fn format_default_time(&self, ms: f64) -> String {
        EnglishIntl.format_default_time(ms)
    }
    fn segment(
        &self,
        locale: Option<&str>,
        granularity: Granularity,
        text: &str,
    ) -> Option<Vec<Segment>> {
        EnglishIntl.segment(locale, granularity, text)
    }
    fn to_upper_case(&self, text: &str, locale: Option<&str>) -> Option<String> {
        EnglishIntl.to_upper_case(text, locale)
    }
}

/// A context for logic that never touches the chart, the DOM or the translator.
pub(crate) struct MockContext {
    pub symbol: std::cell::RefCell<String>,
    pub pointer: Option<ContextMenuPointer>,
    nop: Nop,
}

struct Nop;

impl TopbarItems for Nop {
    fn is_visible(&self, _id: TopbarItemId) -> bool {
        true
    }
    fn set_visible(&self, _id: TopbarItemId, _visible: bool) {}
}

impl PanelDockView for Nop {
    fn list(&self) -> Vec<DockedPanelInfo> {
        Vec::new()
    }
    fn open_id(&self) -> Option<String> {
        None
    }
    fn is_maximized(&self, _id: &str) -> bool {
        false
    }
    fn on_did_change(&self, _cb: Box<dyn Fn()>) -> Unsubscribe {
        Box::new(|| {})
    }
}

impl SidebarControl for Nop {
    fn enabled(&self) -> bool {
        false
    }
    fn set_badge(&self, _id: &str, _badge: Option<SidebarBadge>) {}
    fn refresh(&self) {}
    fn is_visible(&self, _id: &str) -> bool {
        false
    }
    fn set_visible(&self, _id: &str, _visible: bool) {}
}

impl WidgetContext for MockContext {
    fn chart(&self) -> Vela {
        unimplemented!("not used by the registries")
    }
    fn symbol(&self) -> String {
        self.symbol.borrow().clone()
    }
    fn pointer(&self) -> Option<ContextMenuPointer> {
        self.pointer.clone()
    }
    fn timeframe(&self) -> String {
        "60".into()
    }
    fn price_style(&self) -> String {
        "candles".into()
    }
    fn set_symbol(&self, _symbol: &str) {}
    fn set_timeframe(&self, _tf: &str) {}
    fn set_price_style(&self, _style: &str) {}
    fn open_symbol_search(&self, _query: Option<&str>) {}
    fn toggle_panel(&self, _id: &str, _open: Option<bool>) {}
    fn dock_strip(&self, _el: ElementHandle, _opts: DockStripOptions) -> Box<dyn DockedStrip> {
        unimplemented!("not used by the registries")
    }
    fn dock_edge(&self, _el: ElementHandle, _side: DockEdge) -> Unsubscribe {
        Box::new(|| {})
    }
    fn host(&self) -> ElementHandle {
        Rc::new(())
    }
    fn i18n(&self) -> Rc<dyn Translator> {
        unimplemented!("not used by the registries")
    }
    fn toast(&self, _message: &str, _kind: ToastKind) {}
    fn add_indicator(&self, _entry: ExternalIndicatorEntry) {}
    fn add_native_indicator(&self, _indicator_type: &str) {}
    fn state_changed(&self) {}
    fn topbar(&self) -> &dyn TopbarItems {
        &self.nop
    }
    fn panels(&self) -> &dyn PanelDockView {
        &self.nop
    }
    fn sidebar(&self) -> &dyn SidebarControl {
        &self.nop
    }
}

/// A context; the number only tells call sites apart.
pub(crate) fn ctx(_n: u32) -> MockContext {
    MockContext {
        symbol: std::cell::RefCell::new("BTCUSDT".to_string()),
        pointer: None,
        nop: Nop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i64, m: i64, d: i64, h: i64) -> f64 {
        (days_from_civil(y, m, d) * 86_400_000 + h * 3_600_000) as f64
    }

    #[test]
    fn new_york_follows_the_us_dst_rules() {
        assert_eq!(
            offset_minutes("America/New_York", utc(2024, 3, 8, 12)),
            Some(-300)
        );
        assert_eq!(
            offset_minutes("America/New_York", utc(2024, 3, 11, 12)),
            Some(-240)
        );
        // 2024-03-10 07:00 UTC is the spring-forward instant.
        assert_eq!(
            offset_minutes("America/New_York", utc(2024, 3, 10, 6)),
            Some(-300)
        );
        assert_eq!(
            offset_minutes("America/New_York", utc(2024, 3, 10, 7)),
            Some(-240)
        );
        assert_eq!(
            offset_minutes("America/New_York", utc(2024, 11, 3, 5)),
            Some(-240)
        );
        assert_eq!(
            offset_minutes("America/New_York", utc(2024, 11, 3, 6)),
            Some(-300)
        );
        assert_eq!(
            offset_minutes("America/Chicago", utc(2024, 3, 4, 12)),
            Some(-360)
        );
        assert_eq!(
            offset_minutes("Europe/Paris", utc(2024, 7, 1, 12)),
            Some(120)
        );
        assert_eq!(
            offset_minutes("Europe/Paris", utc(2024, 1, 1, 12)),
            Some(60)
        );
        assert_eq!(offset_minutes("Not/AZone", 0.0), None);
    }

    #[test]
    fn an_unknown_zone_is_left_to_the_backend() {
        let intl = ZoneIntl;
        let options = DateTimeFormatOptions {
            time_zone: Some("Not/AZone".into()),
            ..DateTimeFormatOptions::default()
        };
        assert!(intl.format_date_time("en-US", &options, 0.0).is_none());
    }
}
