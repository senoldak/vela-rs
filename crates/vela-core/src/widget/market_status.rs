//! Market-status derivation + tracker behind the statusline's session badge: turns a
//! provider's RESOLVED calendar windows (`DataProvider::get_calendar`: holidays and DST
//! already applied by the source) into the badge vocabulary and keeps it current across
//! session boundaries with a self-arming timer. Symbols with no calendar (crypto, a
//! provider without the capability) keep the constructor's permanent `open`: exactly
//! the pre-calendar behavior.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use super::session_shading::parse_session_spec;
use super::statusline::MarketStatus;
use crate::controls::data_control::DataControl;
use crate::intl::{DateTimeFormatOptions, Intl, MonthStyle, NumericStyle, PartType, WeekdayStyle};
use crate::ports::data_provider::{CalendarRange, CalendarWindows};
use crate::runtime::{Platform, TimerId};

/// One `[start, end)` open window, epoch ms: the `get_calendar` wire shape.
pub type CalendarWindow = (i64, i64);

/// The two window sets a status derivation reads: RTH and the full tape.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarketWindows {
    pub regular: Vec<CalendarWindow>,
    pub extended: Vec<CalendarWindow>,
}

/// The civil date (`YYYY-MM-DD`) of `ms` in `tz`; empty string when the tz is bogus.
/// Its ISO-shaped output is compared as a key, never shown.
fn civil_date(intl: &dyn Intl, ms: i64, tz: &str) -> String {
    let options = DateTimeFormatOptions {
        time_zone: Some(tz.to_string()),
        year: Some(NumericStyle::Numeric),
        month: Some(MonthStyle::TwoDigit),
        day: Some(NumericStyle::TwoDigit),
        ..DateTimeFormatOptions::default()
    };
    let Some(parts) = intl.format_date_time_to_parts("en-US", &options, ms as f64) else {
        return String::new();
    };
    let get = |kind: PartType| {
        parts
            .iter()
            .find(|p| p.kind == kind)
            .map(|p| p.value.clone())
            .unwrap_or_default()
    };
    format!(
        "{}-{}-{}",
        get(PartType::Year),
        get(PartType::Month),
        get(PartType::Day)
    )
}

/// Mon-Fri in `tz`? A bogus tz answers false (never claim a holiday on bad data).
/// Pinned to en-US whatever the chart's locale: the weekday is compared to `Sat` and
/// `Sun`, never shown.
fn is_weekday(intl: &dyn Intl, ms: i64, tz: &str) -> bool {
    let options = DateTimeFormatOptions {
        time_zone: Some(tz.to_string()),
        weekday: Some(WeekdayStyle::Short),
        ..DateTimeFormatOptions::default()
    };
    match intl.format_date_time("en-US", &options, ms as f64) {
        Some(wd) => wd != "Sat" && wd != "Sun",
        None => false,
    }
}

/// The market status at `now` given resolved windows:
///  - inside a regular window: `Open`;
///  - inside the extended tape but outside regular hours: `Extended` on overnight roll
///    tapes (no same-day pre/post split exists); otherwise `Pre` when the day's regular
///    open still lies ahead within the SAME extended window, `Post` otherwise;
///  - otherwise closed, spelled `Holiday` when a WEEKDAY (market tz) carries no extended
///    window at all (a skipped trading day), plain `Closed` for nights and weekends. A
///    window "belongs" to the civil day its start falls on.
pub fn derive_market_status(
    now: i64,
    w: &MarketWindows,
    tz: &str,
    overnight: bool,
    intl: &dyn Intl,
) -> MarketStatus {
    let within = |ws: &[CalendarWindow]| ws.iter().find(|(s, e)| now >= *s && now < *e).copied();
    if within(&w.regular).is_some() {
        return MarketStatus::Open;
    }
    if let Some(ext) = within(&w.extended) {
        if overnight {
            return MarketStatus::Extended;
        }
        return if w.regular.iter().any(|(s, _)| *s >= now && *s < ext.1) {
            MarketStatus::Pre
        } else {
            MarketStatus::Post
        };
    }
    if is_weekday(intl, now, tz) {
        let today = civil_date(intl, now, tz);
        if !today.is_empty()
            && !w
                .extended
                .iter()
                .any(|(s, _)| civil_date(intl, *s, tz) == today)
        {
            return MarketStatus::Holiday;
        }
    }
    MarketStatus::Closed
}

/// The next window edge (either set) strictly after `now`, or `None` when none is known.
pub fn next_status_boundary(now: i64, w: &MarketWindows) -> Option<i64> {
    w.regular
        .iter()
        .chain(w.extended.iter())
        .flat_map(|(s, e)| [*s, *e])
        .filter(|t| *t > now)
        .min()
}

/// How far around `now` one evaluation fetches windows: enough for the longest holiday
/// cluster behind and a re-arm horizon ahead.
const FETCH_BACK_MS: i64 = 4 * 86_400_000;
const FETCH_AHEAD_MS: i64 = 10 * 86_400_000;
/// Timer clamp: at least 15 s (boundary jitter), at most an hour; the hourly
/// re-evaluation also rolls the civil day for the holiday verdict.
const MIN_TIMER_MS: i64 = 15_000;
const MAX_TIMER_MS: i64 = 3_600_000;
/// Retry cadence when the calendar fetch fails (the badge keeps its last state).
const RETRY_MS: i64 = 60_000;

type Rerun = Rc<dyn Fn(u64)>;

struct Inner {
    platform: Rc<dyn Platform>,
    intl: Arc<dyn Intl>,
    on_status: Rc<dyn Fn(MarketStatus)>,
    /// Invalidates detached async work: bumped by every track()/stop().
    epoch: Cell<u64>,
    timer: Cell<Option<TimerId>>,
    /// The calendar-fetching closure of the current binding, re-run by the timer.
    rerun: RefCell<Option<Rerun>>,
}

impl Inner {
    fn clear_timer(&self) {
        if let Some(id) = self.timer.take() {
            self.platform.clear_timeout(id);
        }
    }

    fn arm(self: &Rc<Self>, my: u64, delay: i64) {
        self.clear_timer();
        let inner = Rc::downgrade(self);
        let id = self.platform.set_timeout(
            delay as f64,
            Box::new(move || {
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                inner.timer.set(None);
                if my != inner.epoch.get() {
                    return;
                }
                let rerun = inner.rerun.borrow().clone();
                if let Some(rerun) = rerun {
                    rerun(my);
                }
            }),
        );
        self.timer.set(Some(id));
    }
}

/// Keeps ONE statusline's market badge honest: resolves the symbol's provider, reads its
/// calendar when it has one (`get_calendar` + a real session vocabulary on the symbol's
/// info), derives the status, and re-derives at every window boundary. `track` rebinds on
/// any symbol/chart change; `stop` ends the timers. Symbols without a calendar report a
/// permanent `Open`: continuous markets, and the exact legacy posture.
pub struct MarketStatusTracker {
    inner: Rc<Inner>,
}

impl MarketStatusTracker {
    pub fn new(
        platform: Rc<dyn Platform>,
        intl: Arc<dyn Intl>,
        on_status: impl Fn(MarketStatus) + 'static,
    ) -> Self {
        Self {
            inner: Rc::new(Inner {
                platform,
                intl,
                on_status: Rc::new(on_status),
                epoch: Cell::new(0),
                timer: Cell::new(None),
                rerun: RefCell::new(None),
            }),
        }
    }

    /// (Re)bind to a chart's data surface + symbol and start evaluating.
    pub fn track(&self, data: &DataControl, symbol: &str) {
        let my = self.inner.epoch.get() + 1;
        self.inner.epoch.set(my);
        self.inner.clear_timer();
        let inner = Rc::downgrade(&self.inner);
        let (data, symbol) = (data.clone(), symbol.to_string());
        let evaluate: Rc<dyn Fn(u64)> = Rc::new(move |my| {
            let Some(inner) = inner.upgrade() else {
                return;
            };
            let platform = Rc::clone(&inner.platform);
            platform.spawn(Box::pin(evaluate(inner, my, data.clone(), symbol.clone())));
        });
        *self.inner.rerun.borrow_mut() = Some(Rc::clone(&evaluate));
        evaluate(my);
    }

    pub fn stop(&self) {
        self.inner.epoch.set(self.inner.epoch.get() + 1);
        self.inner.clear_timer();
        *self.inner.rerun.borrow_mut() = None;
    }
}

async fn evaluate(inner: Rc<Inner>, my: u64, data: DataControl, symbol: String) {
    let resolved = data.resolve(&symbol);
    let provider = resolved
        .as_ref()
        .and_then(|r| data.provider_instance(&r.provider));
    let si = data.symbol_info(&symbol).await.ok().flatten();
    if my != inner.epoch.get() {
        return;
    }
    let has_sessions = si
        .as_ref()
        .and_then(|s| s.session.as_deref())
        .is_some_and(|s| !s.is_empty() && s != "24x7");
    let (Some(provider), Some(resolved), true) = (provider, resolved, has_sessions) else {
        (inner.on_status)(MarketStatus::Open); // continuous market / no calendar: the legacy badge
        return;
    };
    let tz = si
        .as_ref()
        .and_then(|s| s.timezone.clone())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Etc/UTC".to_string());
    let now = inner.platform.now_ms() as i64;
    let range = |session: &str| CalendarRange {
        from: now - FETCH_BACK_MS,
        to: now + FETCH_AHEAD_MS,
        session: Some(session.to_string()),
    };
    // A provider without the capability answers `None`: nothing to track.
    let (Some(regular_future), Some(extended_future)) = (
        provider.get_calendar(&resolved.ticker, &range("regular")),
        provider.get_calendar(&resolved.ticker, &range("extended")),
    ) else {
        (inner.on_status)(MarketStatus::Open);
        return;
    };
    let regular: Option<CalendarWindows> = regular_future.await.ok();
    let extended: Option<CalendarWindows> = extended_future.await.ok();
    if my != inner.epoch.get() {
        return;
    }
    let (Some(regular), Some(extended)) = (regular, extended) else {
        // Transient fetch failure: keep the badge as it stands and retry.
        inner.arm(my, RETRY_MS);
        return;
    };
    let w = MarketWindows { regular, extended };
    // Roll tapes (overnight `session_extended`) badge the single extended-hours state:
    // pre/post is a day-split vocabulary they don't have.
    let overnight = parse_session_spec(si.as_ref()).is_some_and(|s| s.overnight);
    (inner.on_status)(derive_market_status(
        now,
        &w,
        &tz,
        overnight,
        inner.intl.as_ref(),
    ));
    let delay = match next_status_boundary(now, &w) {
        Some(boundary) => (boundary - now).min(MAX_TIMER_MS),
        None => MAX_TIMER_MS,
    };
    inner.arm(my, delay.max(MIN_TIMER_MS));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::BarStore;
    use crate::data::multi_provider_feed::MultiProviderFeed;
    use crate::model::Ohlcv;
    use crate::ports::data_provider::DataProvider;
    use crate::ports::market_data::BarRange;
    use crate::ports::market_data::SymbolInfo;
    use crate::runtime::testing::ManualPlatform;
    use crate::runtime::{DataError, LocalBoxFuture};
    use crate::widget::session_shading::utc_ms;
    use crate::widget::test_support::ZoneIntl;
    use serde_json::Map;

    const NY: &str = "America/New_York";

    /// A plain August 2026 week (EDT = UTC-4): Mon 10th to Fri 14th. Regular 09:30-16:00 ET,
    /// extended 04:00-20:00 ET.
    fn d(day: i64, h: i64, m: i64) -> i64 {
        utc_ms(2026, 8, day, h, m)
    }

    fn week(days: &[i64]) -> MarketWindows {
        MarketWindows {
            regular: days.iter().map(|&x| (d(x, 13, 30), d(x, 20, 0))).collect(),
            extended: days.iter().map(|&x| (d(x, 8, 0), d(x, 24, 0))).collect(),
        }
    }

    fn full_week() -> MarketWindows {
        week(&[10, 11, 12, 13, 14])
    }

    fn derive(now: i64, w: &MarketWindows, tz: &str, overnight: bool) -> MarketStatus {
        derive_market_status(now, w, tz, overnight, &ZoneIntl)
    }

    #[test]
    fn inside_a_regular_window_is_open() {
        assert_eq!(
            derive(d(11, 18, 0), &full_week(), NY, false),
            MarketStatus::Open
        );
    }

    #[test]
    fn extended_before_the_regular_open_is_pre_and_after_the_close_post() {
        assert_eq!(
            derive(d(11, 9, 0), &full_week(), NY, false),
            MarketStatus::Pre
        ); // 05:00 ET
        assert_eq!(
            derive(d(11, 21, 0), &full_week(), NY, false),
            MarketStatus::Post
        ); // 17:00 ET
    }

    #[test]
    fn an_overnight_gap_on_a_trading_weekday_is_closed() {
        // Tue 22:00 ET = Wed 02:00 UTC: Tuesday's tape is done, Tuesday traded.
        assert_eq!(
            derive(d(12, 2, 0), &full_week(), NY, false),
            MarketStatus::Closed
        );
    }

    #[test]
    fn a_weekend_is_closed_never_holiday() {
        assert_eq!(
            derive(d(15, 16, 0), &full_week(), NY, false),
            MarketStatus::Closed
        ); // Sat noon ET
    }

    #[test]
    fn a_weekday_with_no_window_at_all_is_a_holiday() {
        let no_monday = week(&[11, 12, 13, 14]);
        assert_eq!(
            derive(d(10, 16, 0), &no_monday, NY, false),
            MarketStatus::Holiday
        ); // Mon noon ET
    }

    #[test]
    fn a_bogus_timezone_never_claims_a_holiday() {
        let no_monday = week(&[11, 12, 13, 14]);
        assert_eq!(
            derive(d(10, 16, 0), &no_monday, "Not/AZone", false),
            MarketStatus::Closed
        );
    }

    #[test]
    fn an_overnight_roll_tape_reports_extended_never_pre_or_post() {
        assert_eq!(
            derive(d(11, 9, 0), &full_week(), NY, true),
            MarketStatus::Extended
        );
        assert_eq!(
            derive(d(11, 21, 0), &full_week(), NY, true),
            MarketStatus::Extended
        );
        assert_eq!(
            derive(d(11, 18, 0), &full_week(), NY, true),
            MarketStatus::Open
        );
    }

    #[test]
    fn the_next_boundary_is_the_nearest_future_edge_across_both_window_sets() {
        // Tue 05:00 ET (pre-market): the next edge is Tuesday's regular open.
        assert_eq!(
            next_status_boundary(d(11, 9, 0), &full_week()),
            Some(d(11, 13, 30))
        );
        // Tue 14:00 ET (open): the next edge is the regular close.
        assert_eq!(
            next_status_boundary(d(11, 18, 0), &full_week()),
            Some(d(11, 20, 0))
        );
    }

    #[test]
    fn there_is_no_boundary_when_no_edge_lies_ahead() {
        assert_eq!(next_status_boundary(d(20, 0, 0), &full_week()), None);
        assert_eq!(next_status_boundary(0, &MarketWindows::default()), None);
    }

    // ── the tracker ──

    type Calendar =
        Rc<dyn Fn(&str) -> Option<LocalBoxFuture<'static, Result<CalendarWindows, DataError>>>>;

    struct CalProvider {
        si: Option<SymbolInfo>,
        calendar: Option<Calendar>,
    }

    impl DataProvider for CalProvider {
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
            let v = self.si.clone();
            Some(Box::pin(std::future::ready(Ok(v))))
        }
        fn get_calendar(
            &self,
            _ticker: &str,
            range: &CalendarRange,
        ) -> Option<LocalBoxFuture<'static, Result<CalendarWindows, DataError>>> {
            let calendar = self.calendar.as_ref()?;
            calendar(range.session.as_deref().unwrap_or(""))
        }
    }

    fn syminfo(session: &str, extended: Option<&str>) -> SymbolInfo {
        SymbolInfo {
            ticker: "T".into(),
            timezone: Some(NY.into()),
            session: Some(session.into()),
            session_extended: extended.map(str::to_string),
            timeframes: None,
            volume: None,
            ohlc: None,
            extra: Map::new(),
        }
    }

    fn rig(
        now: i64,
        si: Option<SymbolInfo>,
        calendar: Option<Calendar>,
    ) -> (ManualPlatform, DataControl) {
        let platform = ManualPlatform::new(now as f64);
        let store = Rc::new(RefCell::new(BarStore::new()));
        let feed = MultiProviderFeed::new(store, platform.handle());
        let data = DataControl::new(Some(feed), platform.handle());
        data.register_provider("edgx", Rc::new(CalProvider { si, calendar }));
        platform.run();
        (platform, data)
    }

    fn windows_for(w: MarketWindows, log: Rc<RefCell<Vec<String>>>) -> Calendar {
        Rc::new(move |session| {
            log.borrow_mut().push(session.to_string());
            let list = if session == "regular" {
                w.regular.clone()
            } else {
                w.extended.clone()
            };
            Some(Box::pin(std::future::ready(Ok(list))))
        })
    }

    type Statuses = Rc<RefCell<Vec<MarketStatus>>>;

    fn tracker(platform: &ManualPlatform) -> (MarketStatusTracker, Statuses) {
        let statuses: Statuses = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&statuses);
        let t = MarketStatusTracker::new(platform.handle(), ZoneIntl::arc(), move |s| {
            sink.borrow_mut().push(s)
        });
        (t, statuses)
    }

    #[test]
    fn the_tracker_derives_the_status_from_the_calendar_and_re_derives_at_the_boundary() {
        let log = Rc::new(RefCell::new(Vec::new()));
        // Tue 15:59 ET: one minute before the close.
        let (platform, data) = rig(
            d(11, 19, 59),
            Some(syminfo("0930-1600", None)),
            Some(windows_for(full_week(), Rc::clone(&log))),
        );
        let (t, statuses) = tracker(&platform);
        t.track(&data, "edgx:AAPL");
        platform.advance(1.0);
        assert_eq!(*statuses.borrow(), [MarketStatus::Open]);
        let mut sessions = log.borrow().clone();
        sessions.sort();
        assert_eq!(sessions, ["extended", "regular"]); // both sets fetched
        // The close passes: the armed boundary timer re-derives into post-market.
        platform.advance(120_000.0);
        assert_eq!(*statuses.borrow(), [MarketStatus::Open, MarketStatus::Post]);
        t.stop();
    }

    #[test]
    fn an_overnight_extended_vocabulary_badges_extended_instead_of_pre() {
        // Tue 05:00 ET: inside the extended tape, open ahead.
        let (platform, data) = rig(
            d(11, 9, 0),
            Some(syminfo("0930-1600", Some("2000-1900"))),
            Some(windows_for(full_week(), Rc::new(RefCell::new(Vec::new())))),
        );
        let (t, statuses) = tracker(&platform);
        t.track(&data, "edgx:ROLL");
        platform.advance(1.0);
        assert_eq!(*statuses.borrow(), [MarketStatus::Extended]);
        t.stop();
    }

    #[test]
    fn no_calendar_capability_or_a_continuous_market_reports_the_legacy_permanent_open() {
        // A provider without get_calendar.
        let (platform, data) = rig(d(15, 12, 0), Some(syminfo("0930-1600", None)), None);
        let (t, statuses) = tracker(&platform);
        t.track(&data, "edgx:AAPL");
        platform.advance(1.0);
        // A continuous market: the calendar is never consulted.
        let never: Calendar = Rc::new(|_| panic!("never called"));
        let (platform2, data2) = rig(d(15, 12, 0), Some(syminfo("24x7", None)), Some(never));
        let (t2, statuses2) = tracker(&platform2);
        t2.track(&data2, "edgx:BTCUSDT");
        platform2.advance(1.0);
        assert_eq!(*statuses.borrow(), [MarketStatus::Open]);
        assert_eq!(*statuses2.borrow(), [MarketStatus::Open]);
        t.stop();
        t2.stop();
    }

    #[test]
    fn a_failed_calendar_fetch_keeps_the_badge_and_retries_and_stop_ends_the_loop() {
        let calls = Rc::new(std::cell::Cell::new(0));
        let counter = Rc::clone(&calls);
        let failing: Calendar = Rc::new(move |_| {
            counter.set(counter.get() + 1);
            Some(Box::pin(std::future::ready(Err(DataError::other("down")))))
        });
        let (platform, data) = rig(
            d(11, 18, 0),
            Some(syminfo("0930-1600", None)),
            Some(failing),
        );
        let (t, statuses) = tracker(&platform);
        t.track(&data, "edgx:AAPL");
        platform.advance(1.0);
        assert!(statuses.borrow().is_empty(), "no verdict on bad data");
        let before = calls.get();
        platform.advance(61_000.0); // the retry fires
        assert!(calls.get() > before);
        t.stop();
        let at_stop = calls.get();
        platform.advance(600_000.0);
        assert_eq!(calls.get(), at_stop, "stopped means stopped");
    }

    #[test]
    fn a_rebind_invalidates_the_in_flight_evaluation() {
        use crate::runtime::{OneshotReceiver, oneshot};
        type Slot =
            Rc<RefCell<Option<crate::runtime::OneshotSender<Result<CalendarWindows, DataError>>>>>;
        let slot: Slot = Rc::new(RefCell::new(None));
        let hold = Rc::clone(&slot);
        let slow: Calendar = Rc::new(move |_| {
            let (tx, rx): (_, OneshotReceiver<Result<CalendarWindows, DataError>>) = oneshot();
            *hold.borrow_mut() = Some(tx);
            Some(Box::pin(rx))
        });
        let (platform, data) = rig(d(11, 18, 0), Some(syminfo("0930-1600", None)), Some(slow));
        let (t, statuses) = tracker(&platform);
        t.track(&data, "edgx:AAPL");
        platform.advance(1.0); // symbol info settles; the calendar hangs
        // Rebind to a continuous market.
        data.register_provider(
            "crypto",
            Rc::new(CalProvider {
                si: Some(syminfo("24x7", None)),
                calendar: None,
            }),
        );
        platform.run();
        t.track(&data, "crypto:BTCUSDT");
        platform.advance(1.0);
        // The OLD evaluation finally resolves: it must be discarded.
        if let Some(tx) = slot.borrow_mut().take() {
            tx.send(Ok(full_week().regular));
        }
        platform.advance(1.0);
        assert_eq!(*statuses.borrow(), [MarketStatus::Open]); // only the rebind's verdict
        t.stop();
    }
}
