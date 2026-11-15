//! Session-zone expansion + tracker behind the session chart shading (pre/post-market on
//! day-split tapes, one extended-hours phase on overnight roll tapes). The bands are
//! computed LOCALLY from the symbol's declared session vocabulary (`syminfo.session`,
//! e.g. `0930-1600`, optional `session_extended`, e.g. `0400-2000` or overnight
//! `1700-1600`, both in the market's `timezone`): the same timestamps the time axis
//! already understands. Shading is therefore synchronous: it paints the moment symbol
//! info is known and follows any pan/zoom depth with zero network round trips. Holiday
//! precision is deliberately NOT this feature's job: a non-trading day has no bars, so its
//! (unshaded) hours collapse out of the bar axis anyway. The resolved calendar
//! (`DataProvider::get_calendar`) stays the truth for consumers that genuinely need
//! holidays (market-status badges, session profiles). Symbols with no session vocabulary
//! (crypto `24x7`) report `None`: nothing shaded.
//!
//! Zone arithmetic goes through the host's [`Intl`] backend, as the original goes through
//! `Intl.DateTimeFormat`.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use super::market_status::CalendarWindow;
use super::timeframe::timeframe_ms;
use crate::controls::data_control::DataControl;
use crate::indicators::VisibleRange;
use crate::intl::{
    DateTimeFormatOptions, HourCycle, Intl, MonthStyle, NumericStyle, PartType, WeekdayStyle,
};
use crate::ports::market_data::{MarketSession, SymbolInfo};
use crate::runtime::Platform;

/// The `sessionZones` feature payload: `[start, end)` epoch-ms bands per phase.
/// Day-split tapes populate `pre`/`post`; overnight roll tapes (the extended window
/// wraps midnight) have no same-day split and populate the single `extended` phase
/// instead.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionZonesUpdate {
    pub pre: Vec<CalendarWindow>,
    pub post: Vec<CalendarWindow>,
    pub extended: Vec<CalendarWindow>,
}

/// One `[start, end)` window as minutes into the market's civil day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DayWindow {
    pub start: i64,
    pub end: i64,
}

/// A symbol's session structure, as declared by its metadata: enough to expand session
/// bands over any time range without asking the provider anything else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSpec {
    pub regular: DayWindow,
    /// With `overnight`, `end < start`: the tape wraps midnight (evening to next day).
    pub extended: DayWindow,
    /// Roll markets (`1700-1600`): one continuous extended phase, no same-day pre/post
    /// split exists.
    pub overnight: bool,
    pub timezone: String,
}

const DAY_MS: i64 = 86_400_000;
const MINUTE_MS: i64 = 60_000;

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `Date.UTC(y, m - 1, d, h, min)` for a month in `1..=12`.
pub(crate) fn utc_ms(y: i64, m: i64, d: i64, h: i64, min: i64) -> i64 {
    days_from_civil(y, m, d) * DAY_MS + h * 3_600_000 + min * MINUTE_MS
}

/// Parse an `HHMM-HHMM` vocabulary into civil-day minutes. Overnight windows
/// (`start >= end`: a tape that wraps midnight) only make sense for the extended
/// spelling on roll markets; callers opt in via `allow_overnight`.
fn parse_window(text: Option<&str>, allow_overnight: bool) -> Option<DayWindow> {
    let b = text?.as_bytes();
    if b.len() != 9 || b[4] != b'-' {
        return None;
    }
    let digits = |s: &[u8]| -> Option<i64> {
        if s.iter().all(u8::is_ascii_digit) {
            Some(s.iter().fold(0, |acc, d| acc * 10 + i64::from(d - b'0')))
        } else {
            None
        }
    };
    let start = digits(&b[0..2])? * 60 + digits(&b[2..4])?;
    let end = digits(&b[5..7])? * 60 + digits(&b[7..9])?; // `2400` = end of day
    if end > 1440 || start >= 1440 {
        return None;
    }
    if start >= end && !allow_overnight {
        return None;
    }
    Some(DayWindow { start, end })
}

/// The session structure on a symbol's metadata, or `None` when it has none (continuous
/// markets, missing metadata). Recognized keys: `session` (regular hours, required),
/// `session_extended` (the full tape's bounds, which must contain the regular window to
/// mean anything), `timezone`. An OVERNIGHT extended spelling (`1700-1600`: roll
/// markets, the trading day runs evening-to-evening) marks the spec `overnight`: one
/// continuous extended phase instead of a pre/post split. Without an extended vocabulary
/// the full civil day bounds the bands; hours no bar ever occupies collapse out of the bar
/// axis.
pub fn parse_session_spec(si: Option<&SymbolInfo>) -> Option<SessionSpec> {
    let si = si?;
    let session = si.session.as_deref()?;
    if session.is_empty() || session == "24x7" {
        return None;
    }
    let regular = parse_window(Some(session), false)?;
    let timezone = match si.timezone.as_deref() {
        Some(tz) if !tz.is_empty() => tz.to_string(),
        _ => "Etc/UTC".to_string(),
    };
    let full_day = DayWindow {
        start: 0,
        end: 1440,
    };
    let ext = parse_window(si.session_extended.as_deref(), true);
    if let Some(ext) = ext.filter(|e| e.start >= e.end) {
        // Overnight tape: valid when the evening opens after the regular close and the
        // same-day tail reaches at least that close: the regular window then sits wholly
        // inside the wrapped span.
        if ext.start >= regular.end && ext.end >= regular.end {
            return Some(SessionSpec {
                regular,
                extended: ext,
                overnight: true,
                timezone,
            });
        }
        return Some(SessionSpec {
            regular,
            extended: full_day,
            overnight: false,
            timezone,
        });
    }
    let extended = match ext {
        Some(e) if e.start <= regular.start && e.end >= regular.end => e,
        _ => full_day,
    };
    Some(SessionSpec {
        regular,
        extended,
        overnight: false,
        timezone,
    })
}

#[derive(Clone, Debug)]
pub(crate) struct CivilParts {
    pub weekday: String,
    pub year: i64,
    pub month: i64,
    pub day: i64,
    pub hour: i64,
    pub minute: i64,
}

/// The civil time of `ms` in `tz`, or `None` when the backend rejects the zone (no
/// shading beats wrong shading). Pinned to en-US whatever the chart's locale: its parts
/// are parsed and its weekday compared to `Sun`, `Sat` and `Fri`, never shown.
pub(crate) fn civil_parts(intl: &dyn Intl, tz: &str, ms: i64) -> Option<CivilParts> {
    let options = DateTimeFormatOptions {
        time_zone: Some(tz.to_string()),
        hour_cycle: Some(HourCycle::H23),
        weekday: Some(WeekdayStyle::Short),
        year: Some(NumericStyle::Numeric),
        month: Some(MonthStyle::TwoDigit),
        day: Some(NumericStyle::TwoDigit),
        hour: Some(NumericStyle::TwoDigit),
        minute: Some(NumericStyle::TwoDigit),
        ..DateTimeFormatOptions::default()
    };
    let parts = intl.format_date_time_to_parts("en-US", &options, ms as f64)?;
    let get = |kind: PartType| -> String {
        parts
            .iter()
            .find(|p| p.kind == kind)
            .map(|p| p.value.clone())
            .unwrap_or_default()
    };
    let num = |kind: PartType| get(kind).trim().parse::<i64>().unwrap_or(0);
    Some(CivilParts {
        weekday: get(PartType::Weekday),
        year: num(PartType::Year),
        month: num(PartType::Month),
        day: num(PartType::Day),
        hour: num(PartType::Hour) % 24,
        minute: num(PartType::Minute),
    })
}

/// Sort + join touching/overlapping bands (the evening head and the next civil day's
/// continuation meet at midnight) so an overnight session paints as one seamless wash.
fn merge_abutting(bands: &[CalendarWindow]) -> Vec<CalendarWindow> {
    let mut sorted: Vec<CalendarWindow> = bands.to_vec();
    sorted.sort_by_key(|b| b.0);
    let mut out: Vec<CalendarWindow> = Vec::new();
    for (s, e) in sorted {
        if e <= s {
            continue;
        }
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Expand a session spec into concrete session bands over `[from, to]` in the market's
/// timezone. Day-split tapes emit pre/post per weekday; overnight roll tapes emit the
/// single `extended` phase: Sun-Thu evening heads (Friday has none), each trading day's
/// continuation up to the regular open, and the post-close tail, merged across midnight
/// into seamless bands. Per civil day, one offset sample at NOON anchors the minute
/// marks: DST transitions land in the small hours, so the noon offset is the day's
/// prevailing one for every session edge. Iterating in 24 h steps and de-duplicating by
/// civil date absorbs the 23/25 h transition days.
pub fn expand_session_zones(
    spec: &SessionSpec,
    from: i64,
    to: i64,
    intl: &dyn Intl,
) -> SessionZonesUpdate {
    let mut pre: Vec<CalendarWindow> = Vec::new();
    let mut post: Vec<CalendarWindow> = Vec::new();
    let mut extended: Vec<CalendarWindow> = Vec::new();
    // A zone the backend rejects shades nothing rather than misplacing bands.
    if civil_parts(intl, &spec.timezone, from).is_none() {
        return SessionZonesUpdate {
            pre,
            post,
            extended,
        };
    }
    let mut seen: HashSet<i64> = HashSet::new();
    let end = to.saturating_add(DAY_MS);
    let mut cursor = from.saturating_sub(DAY_MS);
    while cursor < end {
        let step = cursor;
        cursor = cursor.saturating_add(DAY_MS);
        let Some(civil) = civil_parts(intl, &spec.timezone, step) else {
            continue;
        };
        let key = civil.year * 10_000 + civil.month * 100 + civil.day;
        if !seen.insert(key) {
            continue;
        }
        let sunday = civil.weekday == "Sun";
        if civil.weekday == "Sat" || (sunday && !spec.overnight) {
            continue;
        }
        // Civil midnight spelled as UTC; the noon offset turns civil minutes into epochs.
        let naive = utc_ms(civil.year, civil.month, civil.day, 0, 0);
        let noon_guess = naive + 720 * MINUTE_MS;
        let Some(at_noon) = civil_parts(intl, &spec.timezone, noon_guess) else {
            continue;
        };
        let offset = utc_ms(
            at_noon.year,
            at_noon.month,
            at_noon.day,
            at_noon.hour,
            at_noon.minute,
        ) - noon_guess;
        let at = |minutes: i64| naive + minutes * MINUTE_MS - offset;
        if spec.overnight {
            if !sunday {
                extended.push((at(0), at(spec.regular.start)));
                if spec.regular.end < spec.extended.end {
                    extended.push((at(spec.regular.end), at(spec.extended.end)));
                }
            }
            if civil.weekday != "Fri" {
                extended.push((at(spec.extended.start), at(1440)));
            }
            continue;
        }
        if spec.extended.start < spec.regular.start {
            pre.push((at(spec.extended.start), at(spec.regular.start)));
        }
        if spec.regular.end < spec.extended.end {
            post.push((at(spec.regular.end), at(spec.extended.end)));
        }
    }
    SessionZonesUpdate {
        pre,
        post,
        extended: merge_abutting(&extended),
    }
}

/// Recompute coverage reaches this far beyond the visible span on each side, so
/// ordinary panning keeps landing inside the last expansion.
const COVER_PAD_MIN_MS: i64 = 2 * DAY_MS;

/// What [`SessionShadingTracker::track`] binds to.
#[derive(Clone, Debug)]
pub struct TrackOptions {
    pub session: MarketSession,
    pub timeframe: String,
    pub range: VisibleRange,
}

struct State {
    /// Invalidates the one async step (metadata resolution): bumped by track()/stop().
    epoch: u64,
    spec: Option<SessionSpec>,
    ready: bool,
    session: MarketSession,
    /// Whether one bar is shorter than a civil day: only then do pre/post bars exist.
    intraday: bool,
    covered: Option<VisibleRange>,
    /// The newest range seen: viewport moves during metadata resolution (a load's fit
    /// animation) must not be lost, so the resolution always expands the LATEST range.
    last_range: Option<VisibleRange>,
}

struct Inner {
    platform: Rc<dyn Platform>,
    intl: Arc<dyn Intl>,
    on_zones: Rc<dyn Fn(Option<SessionZonesUpdate>)>,
    state: RefCell<State>,
}

impl Inner {
    fn emit(&self, range: VisibleRange, force: bool) {
        let outcome = {
            let mut st = self.state.borrow_mut();
            let Some(spec) = st.spec.clone() else {
                drop(st);
                if force {
                    (self.on_zones)(None);
                }
                return;
            };
            if st.session != MarketSession::Extended || !st.intraday {
                // No extended-hours bars on screen: the regular tape collapses those hours
                // out of the bar axis, and a daily+ bar aggregates its whole day. Either way
                // a band would land on the wrong pixels: keep the structure known, shade
                // nothing.
                drop(st);
                if force {
                    (self.on_zones)(Some(SessionZonesUpdate::default()));
                }
                return;
            }
            let from = range.from.min(range.to);
            let to = range.from.max(range.to);
            if !force && st.covered.is_some_and(|c| from >= c.from && to <= c.to) {
                return;
            }
            let pad = (to - from).max(COVER_PAD_MIN_MS);
            let covered = VisibleRange {
                from: from.saturating_sub(pad),
                to: to.saturating_add(pad),
            };
            st.covered = Some(covered);
            (spec, covered)
        };
        let (spec, covered) = outcome;
        (self.on_zones)(Some(expand_session_zones(
            &spec,
            covered.from,
            covered.to,
            self.intl.as_ref(),
        )));
    }
}

/// Keeps ONE chart's session shading honest and INSTANT: `track` resolves the symbol's
/// session spec once (symbol metadata, memoized by providers), then every range update is
/// a synchronous local expansion. Reports:
///  - `None`: no session structure at all (continuous market / no metadata);
///  - empty bands: sessions exist but there are no pre/post bar slots to shade: the
///    REGULAR tape is shown, or the timeframe is daily+ (each bar aggregates whole
///    sessions, so a band would just tint full-day candles);
///  - the expanded bands: the extended tape is shown. The renderer clips them to the
///    loaded bar slots, so nothing paints before history or ahead of the current bar.
pub struct SessionShadingTracker {
    inner: Rc<Inner>,
}

impl SessionShadingTracker {
    pub fn new(
        platform: Rc<dyn Platform>,
        intl: Arc<dyn Intl>,
        on_zones: impl Fn(Option<SessionZonesUpdate>) + 'static,
    ) -> Self {
        Self {
            inner: Rc::new(Inner {
                platform,
                intl,
                on_zones: Rc::new(on_zones),
                state: RefCell::new(State {
                    epoch: 0,
                    spec: None,
                    ready: false,
                    session: MarketSession::Regular,
                    intraday: false,
                    covered: None,
                    last_range: None,
                }),
            }),
        }
    }

    /// (Re)bind to a chart's data surface + market and expand once metadata lands.
    pub fn track(&self, data: &DataControl, symbol: &str, opts: TrackOptions) {
        let my = {
            let mut st = self.inner.state.borrow_mut();
            st.epoch += 1;
            st.ready = false;
            st.spec = None;
            st.covered = None;
            st.session = opts.session;
            let tf_ms = timeframe_ms(&opts.timeframe);
            st.intraday = tf_ms.is_finite() && tf_ms < DAY_MS as f64;
            st.last_range = Some(opts.range);
            st.epoch
        };
        let info = data.symbol_info(symbol);
        let inner = Rc::clone(&self.inner);
        let fallback = opts.range;
        self.inner.platform.spawn(Box::pin(async move {
            let si = info.await.ok().flatten();
            let range = {
                let mut st = inner.state.borrow_mut();
                if my != st.epoch {
                    return;
                }
                st.ready = true;
                st.spec = parse_session_spec(si.as_ref());
                st.last_range.unwrap_or(fallback)
            };
            inner.emit(range, true);
        }));
    }

    /// Follow a pan/zoom synchronously: bands are epoch-anchored, so only a range that
    /// leaves the last expansion's coverage needs a recompute: no fetch, no debounce.
    pub fn update_range(&self, range: VisibleRange) {
        let ready = {
            let mut st = self.inner.state.borrow_mut();
            st.last_range = Some(range);
            st.ready
        };
        if ready {
            self.inner.emit(range, false);
        }
    }

    pub fn stop(&self) {
        let mut st = self.inner.state.borrow_mut();
        st.epoch += 1;
        st.ready = false;
        st.spec = None;
        st.covered = None;
        st.last_range = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::testing::ManualPlatform;
    use crate::widget::test_support::ZoneIntl;
    use serde_json::Map;

    const H: i64 = 3_600_000;
    const DAY: i64 = 24 * H;

    fn si(session: &str, extended: Option<&str>, tz: &str) -> SymbolInfo {
        SymbolInfo {
            ticker: "T".into(),
            timezone: Some(tz.into()),
            session: Some(session.into()),
            session_extended: extended.map(str::to_string),
            timeframes: None,
            volume: None,
            ohlc: None,
            extra: Map::new(),
        }
    }

    /// The US-equities shape.
    fn us_equities() -> SymbolInfo {
        si("0930-1600", Some("0400-2000"), "America/New_York")
    }

    /// An overnight roll market: the trading day ROLLS at the 17:00 evening open, so the
    /// extended tape wraps midnight.
    fn overnight_market() -> SymbolInfo {
        si("0830-1515", Some("1700-1600"), "America/Chicago")
    }

    fn utc(y: i64, m: i64, d: i64, h: i64, min: i64) -> i64 {
        utc_ms(y, m, d, h, min)
    }

    #[test]
    fn parse_reads_the_regular_extended_vocabulary_and_the_market_timezone() {
        assert_eq!(
            parse_session_spec(Some(&us_equities())),
            Some(SessionSpec {
                regular: DayWindow {
                    start: 9 * 60 + 30,
                    end: 16 * 60
                },
                extended: DayWindow {
                    start: 4 * 60,
                    end: 20 * 60
                },
                overnight: false,
                timezone: "America/New_York".into(),
            })
        );
    }

    #[test]
    fn parse_reads_an_overnight_extended_vocabulary_as_a_wrap_around_roll_tape() {
        assert_eq!(
            parse_session_spec(Some(&overnight_market())),
            Some(SessionSpec {
                regular: DayWindow {
                    start: 8 * 60 + 30,
                    end: 15 * 60 + 15
                },
                extended: DayWindow {
                    start: 17 * 60,
                    end: 16 * 60
                },
                overnight: true,
                timezone: "America/Chicago".into(),
            })
        );
    }

    #[test]
    fn parse_ignores_an_overnight_window_whose_tail_does_not_reach_the_regular_close() {
        let spec = parse_session_spec(Some(&si("0830-1515", Some("1700-1200"), "America/Chicago")))
            .expect("spec");
        assert_eq!(
            spec.extended,
            DayWindow {
                start: 0,
                end: 1440
            }
        );
        assert!(!spec.overnight);
    }

    #[test]
    fn parse_reports_no_structure_for_continuous_markets_and_missing_metadata() {
        assert_eq!(parse_session_spec(None), None);
        assert_eq!(parse_session_spec(Some(&si("24x7", None, "Etc/UTC"))), None);
        assert_eq!(parse_session_spec(Some(&si("", None, "Etc/UTC"))), None);
        let mut no_session = si("x", None, "Etc/UTC");
        no_session.session = None;
        assert_eq!(parse_session_spec(Some(&no_session)), None);
    }

    #[test]
    fn parse_bounds_the_bands_by_the_full_civil_day_without_an_extended_vocabulary() {
        let spec =
            parse_session_spec(Some(&si("0930-1600", None, "America/New_York"))).expect("spec");
        assert_eq!(
            spec.extended,
            DayWindow {
                start: 0,
                end: 1440
            }
        );
        let defaulted = SymbolInfo {
            timezone: None,
            ..si("0930-1600", None, "")
        };
        assert_eq!(
            parse_session_spec(Some(&defaulted)).expect("spec").timezone,
            "Etc/UTC"
        );
    }

    #[test]
    fn parse_ignores_an_extended_vocabulary_that_does_not_contain_the_regular_window() {
        let spec = parse_session_spec(Some(&si(
            "0930-1600",
            Some("1000-1200"),
            "America/New_York",
        )))
        .expect("spec");
        assert_eq!(
            spec.extended,
            DayWindow {
                start: 0,
                end: 1440
            }
        );
    }

    #[test]
    fn parse_rejects_overnight_regular_windows_and_malformed_text() {
        assert_eq!(
            parse_session_spec(Some(&si("1700-1600", None, "Etc/UTC"))),
            None
        );
        for bad in [
            "0930-160",
            "09301600",
            "09:30-16:00",
            "0930-2500",
            "2400-2500",
            "abcd-efgh",
        ] {
            assert_eq!(
                parse_session_spec(Some(&si(bad, None, "Etc/UTC"))),
                None,
                "{bad}"
            );
        }
        // `2400` is a valid end of day.
        assert!(parse_session_spec(Some(&si("0000-2400", None, "Etc/UTC"))).is_some());
    }

    fn spec_us() -> SessionSpec {
        parse_session_spec(Some(&us_equities())).expect("spec")
    }

    #[test]
    fn it_emits_one_pre_and_one_post_band_per_weekday_at_the_exact_market_time_edges() {
        // Mon 2024-03-04, EST (UTC-5): 04:00 ET = 09:00 UTC.
        let monday0400 = utc(2024, 3, 4, 9, 0);
        let zones = expand_session_zones(&spec_us(), monday0400, monday0400 + 12 * H, &ZoneIntl);
        assert!(
            zones
                .pre
                .contains(&(monday0400, monday0400 + 5 * H + H / 2))
        ); // 04:00-09:30
        assert!(
            zones
                .post
                .contains(&(monday0400 + 12 * H, monday0400 + 16 * H))
        ); // 16:00-20:00
    }

    #[test]
    fn it_skips_weekends() {
        let zones = expand_session_zones(
            &spec_us(),
            utc(2024, 3, 9, 0, 0),
            utc(2024, 3, 11, 0, 0),
            &ZoneIntl,
        );
        let saturday = utc(2024, 3, 9, 0, 0);
        let sunday = utc(2024, 3, 10, 0, 0);
        let inside_weekend = |b: &CalendarWindow| b.0 >= saturday && b.1 <= sunday + DAY;
        assert!(!zones.pre.iter().any(inside_weekend));
        assert!(!zones.post.iter().any(inside_weekend));
    }

    #[test]
    fn it_tracks_the_dst_offset_change_across_a_transition_week() {
        // Fri 2024-03-08 is EST (UTC-5), Mon 2024-03-11 is EDT (UTC-4).
        let friday = utc(2024, 3, 8, 12, 0);
        let monday = utc(2024, 3, 11, 12, 0);
        let zones = expand_session_zones(&spec_us(), friday - DAY, monday + DAY, &ZoneIntl);
        assert!(
            zones
                .pre
                .contains(&(utc(2024, 3, 8, 9, 0), utc(2024, 3, 8, 14, 30)))
        );
        assert!(
            zones
                .pre
                .contains(&(utc(2024, 3, 11, 8, 0), utc(2024, 3, 11, 13, 30)))
        );
    }

    #[test]
    fn it_covers_arbitrarily_deep_ranges_without_a_lookback_cutoff() {
        let to = utc(2024, 1, 5, 0, 0);
        let from = to - 1_100 * DAY; // ~3 years
        let zones = expand_session_zones(&spec_us(), from, to, &ZoneIntl);
        assert!(zones.pre.len() > 700);
        assert!(zones.pre[0].0 < from + 4 * DAY);
        assert!(zones.pre[zones.pre.len() - 1].1 > to - 4 * DAY);
        for pair in zones.pre.windows(2) {
            assert!(pair[1].0 > pair[0].1);
        }
    }

    #[test]
    fn it_emits_nothing_for_a_bogus_timezone_rather_than_misplacing_bands() {
        let spec = SessionSpec {
            timezone: "Not/AZone".into(),
            ..spec_us()
        };
        assert_eq!(
            expand_session_zones(&spec, 0, 7 * DAY, &ZoneIntl),
            SessionZonesUpdate::default()
        );
    }

    #[test]
    fn it_handles_an_empty_or_inverted_range() {
        let zones = expand_session_zones(&spec_us(), 10 * DAY, 10 * DAY, &ZoneIntl);
        assert!(zones.pre.len() <= 3);
        let inverted = expand_session_zones(&spec_us(), 20 * DAY, 5 * DAY, &ZoneIntl);
        assert_eq!(inverted, SessionZonesUpdate::default());
    }

    fn spec_roll() -> SessionSpec {
        parse_session_spec(Some(&overnight_market())).expect("spec")
    }

    #[test]
    fn overnight_tapes_shade_the_whole_overnight_as_one_band_seamless_across_midnight() {
        // Mon 2024-03-04, CST (UTC-6). The Sunday 17:00 CT evening open runs unbroken into
        // Monday's 08:30 CT regular open: 23:00 UTC Sun 3rd to 14:30 UTC Mon 4th.
        let zones = expand_session_zones(
            &spec_roll(),
            utc(2024, 3, 3, 12, 0),
            utc(2024, 3, 5, 23, 0),
            &ZoneIntl,
        );
        assert!(zones.pre.is_empty());
        assert!(zones.post.is_empty());
        assert!(
            zones
                .extended
                .contains(&(utc(2024, 3, 3, 23, 0), utc(2024, 3, 4, 14, 30)))
        );
        // Monday's own evening rolls into Tuesday the same way.
        assert!(
            zones
                .extended
                .contains(&(utc(2024, 3, 4, 23, 0), utc(2024, 3, 5, 14, 30)))
        );
    }

    #[test]
    fn overnight_tapes_shade_the_post_close_tail_up_to_the_tape_end() {
        let zones = expand_session_zones(
            &spec_roll(),
            utc(2024, 3, 4, 0, 0),
            utc(2024, 3, 5, 0, 0),
            &ZoneIntl,
        );
        assert!(
            zones
                .extended
                .contains(&(utc(2024, 3, 4, 21, 15), utc(2024, 3, 4, 22, 0)))
        );
    }

    #[test]
    fn overnight_tapes_emit_no_friday_evening_and_nothing_on_saturday() {
        let zones = expand_session_zones(
            &spec_roll(),
            utc(2024, 2, 29, 0, 0),
            utc(2024, 3, 4, 0, 0),
            &ZoneIntl,
        );
        let gap_from = utc(2024, 3, 1, 22, 0);
        let gap_to = utc(2024, 3, 3, 23, 0);
        assert!(
            !zones
                .extended
                .iter()
                .any(|(s, e)| *s < gap_to && *e > gap_from)
        );
    }

    #[test]
    fn merge_abutting_joins_touching_bands_and_drops_empty_ones() {
        assert_eq!(
            merge_abutting(&[(10, 20), (0, 10), (30, 30), (25, 40), (35, 38)]),
            [(0, 20), (25, 40)]
        );
        assert!(merge_abutting(&[]).is_empty());
    }

    // ── the tracker ──

    use crate::data::BarStore;
    use crate::data::multi_provider_feed::MultiProviderFeed;
    use crate::model::Ohlcv;
    use crate::ports::data_provider::DataProvider;
    use crate::ports::market_data::BarRange;
    use crate::runtime::{DataError, LocalBoxFuture};
    use std::cell::Cell;

    struct SiProvider {
        value: Option<SymbolInfo>,
    }

    impl DataProvider for SiProvider {
        fn get_bars(
            &self,
            _t: &str,
            _tf: &str,
            _r: &BarRange,
        ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
            Box::pin(std::future::ready(Ok(Vec::new())))
        }
        fn get_symbol_info(
            &self,
            _ticker: &str,
        ) -> Option<LocalBoxFuture<'static, Result<Option<SymbolInfo>, DataError>>> {
            let v = self.value.clone();
            Some(Box::pin(std::future::ready(Ok(v))))
        }
    }

    fn rig(value: Option<SymbolInfo>) -> (ManualPlatform, DataControl) {
        let platform = ManualPlatform::new(0.0);
        let store = Rc::new(RefCell::new(BarStore::new()));
        let feed = MultiProviderFeed::new(store, platform.handle());
        let data = DataControl::new(Some(feed), platform.handle());
        data.register_provider("x", Rc::new(SiProvider { value }));
        platform.run();
        (platform, data)
    }

    type Updates = Rc<RefCell<Vec<Option<SessionZonesUpdate>>>>;

    fn tracker(platform: &ManualPlatform) -> (SessionShadingTracker, Updates) {
        let updates: Updates = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&updates);
        let t = SessionShadingTracker::new(platform.handle(), ZoneIntl::arc(), move |z| {
            sink.borrow_mut().push(z)
        });
        (t, updates)
    }

    fn opts(session: MarketSession, timeframe: &str, from: i64, to: i64) -> TrackOptions {
        TrackOptions {
            session,
            timeframe: timeframe.into(),
            range: VisibleRange { from, to },
        }
    }

    #[test]
    fn the_tracker_shades_nothing_on_daily_and_higher_timeframes() {
        let monday = utc(2024, 3, 4, 0, 0);
        for tf in ["1440", "D", "W", "43200"] {
            let (platform, data) = rig(Some(us_equities()));
            let (t, updates) = tracker(&platform);
            t.track(
                &data,
                "x:AAPL",
                opts(MarketSession::Extended, tf, monday, monday + 30 * DAY),
            );
            platform.run();
            assert_eq!(
                updates.borrow().last().cloned(),
                Some(Some(SessionZonesUpdate::default())),
                "{tf}"
            );
            t.stop();
        }
    }

    #[test]
    fn the_tracker_keeps_shading_intraday_extended_session_timeframes() {
        let monday = utc(2024, 3, 4, 0, 0);
        for tf in ["1", "60", "240"] {
            let (platform, data) = rig(Some(us_equities()));
            let (t, updates) = tracker(&platform);
            t.track(
                &data,
                "x:AAPL",
                opts(MarketSession::Extended, tf, monday, monday + DAY),
            );
            platform.run();
            let zones = updates.borrow().last().cloned().flatten().expect("zones");
            assert!(!zones.pre.is_empty(), "{tf}");
        }
    }

    #[test]
    fn the_tracker_shades_nothing_on_the_regular_tape() {
        let monday = utc(2024, 3, 4, 0, 0);
        let (platform, data) = rig(Some(us_equities()));
        let (t, updates) = tracker(&platform);
        t.track(
            &data,
            "x:AAPL",
            opts(MarketSession::Regular, "60", monday, monday + DAY),
        );
        platform.run();
        assert_eq!(
            updates.borrow().last().cloned(),
            Some(Some(SessionZonesUpdate::default()))
        );
    }

    #[test]
    fn the_tracker_reports_the_single_extended_phase_for_overnight_tapes() {
        let monday = utc(2024, 3, 4, 0, 0);
        let (platform, data) = rig(Some(overnight_market()));
        let (t, updates) = tracker(&platform);
        t.track(
            &data,
            "x:ROLL",
            opts(MarketSession::Extended, "15", monday, monday + DAY),
        );
        platform.run();
        let zones = updates.borrow().last().cloned().flatten().expect("zones");
        assert!(!zones.extended.is_empty());
        assert!(zones.pre.is_empty() && zones.post.is_empty());
    }

    #[test]
    fn the_tracker_reports_none_for_a_continuous_market_or_missing_metadata() {
        for value in [Some(si("24x7", None, "Etc/UTC")), None] {
            let (platform, data) = rig(value);
            let (t, updates) = tracker(&platform);
            t.track(&data, "x:BTC", opts(MarketSession::Extended, "60", 0, DAY));
            platform.run();
            assert_eq!(updates.borrow().as_slice(), [None]);
        }
    }

    #[test]
    fn the_tracker_expands_the_newest_range_when_the_viewport_moves_while_metadata_resolves() {
        let monday = utc(2024, 3, 4, 0, 0);
        let (platform, data) = rig(Some(us_equities()));
        let (t, updates) = tracker(&platform);
        t.track(
            &data,
            "x:AAPL",
            opts(MarketSession::Extended, "60", monday, monday + DAY),
        );
        // A load's fit animation settles the viewport two weeks earlier before metadata lands.
        t.update_range(VisibleRange {
            from: monday - 14 * DAY,
            to: monday - 13 * DAY,
        });
        assert!(
            updates.borrow().is_empty(),
            "nothing is emitted before the metadata lands"
        );
        platform.run();
        let zones = updates.borrow().last().cloned().flatten().expect("zones");
        assert!(
            zones
                .pre
                .iter()
                .any(|(start, _)| *start < monday - 13 * DAY)
        );
    }

    #[test]
    fn panning_inside_the_covered_span_recomputes_nothing() {
        let monday = utc(2024, 3, 4, 0, 0);
        let (platform, data) = rig(Some(us_equities()));
        let (t, updates) = tracker(&platform);
        t.track(
            &data,
            "x:AAPL",
            opts(MarketSession::Extended, "60", monday, monday + DAY),
        );
        platform.run();
        assert_eq!(updates.borrow().len(), 1);
        t.update_range(VisibleRange {
            from: monday + H,
            to: monday + DAY + H,
        });
        assert_eq!(
            updates.borrow().len(),
            1,
            "still inside the padded coverage"
        );
        t.update_range(VisibleRange {
            from: monday + 30 * DAY,
            to: monday + 31 * DAY,
        });
        assert_eq!(updates.borrow().len(), 2, "left the coverage: recomputed");
    }

    #[test]
    fn a_rebind_discards_the_in_flight_resolution() {
        let monday = utc(2024, 3, 4, 0, 0);
        let (platform, data) = rig(Some(us_equities()));
        let (t, updates) = tracker(&platform);
        let seen = Cell::new(0);
        t.track(
            &data,
            "x:AAPL",
            opts(MarketSession::Extended, "60", monday, monday + DAY),
        );
        t.stop(); // before the metadata future ran
        platform.run();
        seen.set(updates.borrow().len());
        assert_eq!(seen.get(), 0, "a stopped tracker emits nothing");
    }
}
