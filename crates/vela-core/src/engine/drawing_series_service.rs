//! The core implementation of [`DrawingSeriesGateway`]: a synchronous window
//! cache over the feed's ranged fetch (`DrawingSeriesService.ts`).
//!
//! A read either serves a covering cached window, or kicks ONE background fetch
//! for the (padded) window and answers `Loading`; completion fires the
//! [`on_update`](DrawingSeriesService::on_update) listeners and the next paint
//! finds the bars. Windows whose right edge reaches the live bar refresh at most
//! once per bar interval: the refetch rides the feed's own closed-bar cache, so
//! it stays cheap.
//!
//! The service is a cheap handle: clones share one cache. A fetch in flight
//! keeps the shared state alive until it lands, so [`DrawingSeriesDeps`] must
//! hold the orchestrator weakly.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::data::timeframe_to_ms;
use crate::drawings::{DrawingSeriesGateway, DrawingSeriesState, SeriesUnavailable};
use crate::indicators::classics::math::{js_max, js_min};
use crate::model::Ohlcv;
use crate::ports::market_data::BarRange;
use crate::runtime::{DataError, LocalBoxFuture, Platform, Unsubscribe};

/// Hard cap on the bars one request may demand: a huge area at a tiny timeframe
/// must degrade to `TooWide`, not melt the provider.
const MAX_BARS: f64 = 5000.0;
/// Fetch padding as a fraction of the requested span, each side: small anchor
/// drags stay cache hits.
const PAD_FRAC: f64 = 0.25;
/// Bounded memory: total cached windows across all (market, timeframe) keys.
const MAX_ENTRIES: usize = 16;
/// A failed fetch parks its window this long before a paint may retry it.
const RETRY_MS: f64 = 15_000.0;
/// The steps `auto` resolves against: the largest one at least 4x finer than the chart.
const AUTO_STEPS: [&str; 6] = ["240", "60", "30", "15", "5", "1"];

/// What the service needs from its owner (the orchestrator): all read live per call.
pub trait DrawingSeriesDeps {
    /// Ranged fetch of the chart's own symbol at `timeframe` (the cache-backed feed path).
    fn fetch_bars(
        &self,
        timeframe: &str,
        range: BarRange,
    ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>>;
    /// False when the market has no ranged source (inline `data` series, no `load_range`).
    fn can_fetch(&self) -> bool;
    /// The chart's own timeframe (canonical string): what `auto` and `NotLower` resolve against.
    fn chart_timeframe(&self) -> String;
    /// Cache identity of the current market (symbol + session): a market switch
    /// simply misses.
    fn market_key(&self) -> String;
}

/// One fetched (or in-flight) time window of one (market, timeframe) series.
struct RangeEntry {
    from: f64,
    to: f64,
    bars: Vec<Ohlcv>,
    /// 0 while the first fetch is in flight.
    fetched_at: f64,
    pending: bool,
    /// Set when the last fetch failed: the window answers `Loading` until
    /// [`RETRY_MS`] passes.
    failed_at: f64,
}

type EntryRef = Rc<RefCell<RangeEntry>>;

/// Cached windows per `market|timeframe` key, in key insertion order.
#[derive(Default)]
struct Cache {
    keys: Vec<(String, Vec<EntryRef>)>,
}

impl Cache {
    fn entries_of(&self, key: &str) -> Vec<EntryRef> {
        self.keys
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, entries)| entries.clone())
            .unwrap_or_default()
    }

    fn slot(&mut self, key: &str) -> &mut Vec<EntryRef> {
        let i = match self.keys.iter().position(|(k, _)| k == key) {
            Some(i) => i,
            None => {
                self.keys.push((key.to_string(), Vec::new()));
                self.keys.len() - 1
            }
        };
        &mut self.keys[i].1
    }

    fn remove_entry(&mut self, key: &str, entry: &EntryRef) {
        if let Some((_, entries)) = self.keys.iter_mut().find(|(k, _)| k == key) {
            entries.retain(|e| !Rc::ptr_eq(e, entry));
        }
    }

    /// Drop the oldest settled windows once the global count passes [`MAX_ENTRIES`].
    fn evict(&mut self) {
        let mut total: usize = self.keys.iter().map(|(_, entries)| entries.len()).sum();
        while total > MAX_ENTRIES {
            let mut oldest: Option<(usize, usize)> = None;
            let mut oldest_at = f64::INFINITY;
            for (ki, (_, entries)) in self.keys.iter().enumerate() {
                for (ei, entry) in entries.iter().enumerate() {
                    let e = entry.borrow();
                    if e.pending {
                        continue; // never drop an in-flight window
                    }
                    let at = if e.fetched_at != 0.0 {
                        e.fetched_at
                    } else {
                        e.failed_at
                    };
                    if at < oldest_at {
                        oldest = Some((ki, ei));
                        oldest_at = at;
                    }
                }
            }
            let Some((ki, ei)) = oldest else {
                return; // everything in flight: nothing evictable
            };
            self.keys[ki].1.remove(ei);
            if self.keys[ki].1.is_empty() {
                self.keys.remove(ki);
            }
            total -= 1;
        }
    }

    /// Merge windows that overlap `entry` into it (dedupe by bar time) so a
    /// key's list converges instead of accumulating slivers.
    fn absorb_overlaps(&mut self, key: &str, entry: &EntryRef) {
        let Some((_, entries)) = self.keys.iter_mut().find(|(k, _)| k == key) else {
            return;
        };
        for i in (0..entries.len()).rev() {
            let other = entries[i].clone();
            if Rc::ptr_eq(&other, entry) {
                continue;
            }
            let mut me = entry.borrow_mut();
            let theirs = other.borrow();
            if theirs.pending || theirs.failed_at > 0.0 {
                continue;
            }
            if theirs.to < me.from || theirs.from > me.to {
                continue;
            }
            let mut by_time: BTreeMap<i64, Ohlcv> = BTreeMap::new();
            for b in &theirs.bars {
                by_time.insert(b.time, *b);
            }
            for b in &me.bars {
                by_time.insert(b.time, *b); // the fresh fetch wins ties
            }
            me.bars = by_time.into_values().collect();
            me.from = js_min(me.from, theirs.from);
            me.to = js_max(me.to, theirs.to);
            let theirs_fetched = if theirs.fetched_at != 0.0 {
                theirs.fetched_at
            } else {
                me.fetched_at
            };
            me.fetched_at = js_min(me.fetched_at, theirs_fetched);
            drop(theirs);
            drop(me);
            entries.remove(i);
        }
    }
}

/// How a started fetch settles.
#[derive(Clone, Copy)]
enum FetchKind {
    /// A new window: absorbs overlaps and notifies on success AND on failure.
    Window,
    /// A refresh of a settled window: keeps serving the stale window on failure.
    Refresh,
}

struct FetchJob {
    kind: FetchKind,
    key: String,
    entry: EntryRef,
    timeframe: String,
    from: f64,
    to: f64,
}

/// A callback fired when a background fetch lands.
type UpdateListener = Rc<dyn Fn()>;

struct Inner {
    deps: Rc<dyn DrawingSeriesDeps>,
    platform: Rc<dyn Platform>,
    cache: RefCell<Cache>,
    listeners: RefCell<Vec<(u64, UpdateListener)>>,
    next_listener: Cell<u64>,
}

impl Inner {
    fn fire(&self) {
        let listeners: Vec<UpdateListener> = self
            .listeners
            .borrow()
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        for listener in listeners {
            listener();
        }
    }

    fn settle(&self, job: &FetchJob, result: Result<Vec<Ohlcv>, DataError>) {
        let now = self.platform.now_ms();
        match (job.kind, result) {
            (FetchKind::Window, Ok(bars)) => {
                {
                    let mut e = job.entry.borrow_mut();
                    e.bars = bars;
                    e.fetched_at = now;
                    e.pending = false;
                }
                self.cache
                    .borrow_mut()
                    .absorb_overlaps(&job.key, &job.entry);
                self.fire();
            }
            // Environmental (network/provider): park the window so paints do not
            // hot-loop retries; RETRY_MS later a paint drops it and tries again.
            (FetchKind::Window, Err(_)) => {
                {
                    let mut e = job.entry.borrow_mut();
                    e.pending = false;
                    e.failed_at = now;
                }
                self.fire();
            }
            (FetchKind::Refresh, Ok(bars)) => {
                {
                    let mut e = job.entry.borrow_mut();
                    e.bars = bars;
                    e.fetched_at = now;
                    e.pending = false;
                }
                self.fire();
            }
            // Keep serving the stale window; the next eligible paint retries.
            (FetchKind::Refresh, Err(_)) => {
                let mut e = job.entry.borrow_mut();
                e.pending = false;
                e.fetched_at = now;
            }
        }
    }
}

/// The drawing-series window cache. See the module docs.
#[derive(Clone)]
pub struct DrawingSeriesService {
    inner: Rc<Inner>,
}

impl DrawingSeriesService {
    /// `platform` supplies the clock and runs the background fetches.
    pub fn new(deps: Rc<dyn DrawingSeriesDeps>, platform: Rc<dyn Platform>) -> Self {
        Self {
            inner: Rc::new(Inner {
                deps,
                platform,
                cache: RefCell::new(Cache::default()),
                listeners: RefCell::new(Vec::new()),
                next_listener: Cell::new(0),
            }),
        }
    }

    /// Run `listener` whenever a background fetch lands (the host repaints). The
    /// returned function detaches it.
    pub fn on_update(&self, listener: impl Fn() + 'static) -> Unsubscribe {
        let id = self.inner.next_listener.get();
        self.inner.next_listener.set(id + 1);
        self.inner
            .listeners
            .borrow_mut()
            .push((id, Rc::new(listener)));
        let inner = Rc::downgrade(&self.inner);
        Box::new(move || {
            if let Some(inner) = inner.upgrade() {
                inner.listeners.borrow_mut().retain(|(i, _)| *i != id);
            }
        })
    }

    /// The synchronous read: bars of the chart's own symbol at `timeframe` over
    /// `[from, to]` (epoch ms, either order).
    pub fn series_in_range(&self, timeframe: &str, from: f64, to: f64) -> DrawingSeriesState {
        let deps = &self.inner.deps;
        if !deps.can_fetch() {
            return unavailable(SeriesUnavailable::NoSource);
        }
        let resolved = match self.resolve_timeframe(timeframe) {
            Ok(resolved) => resolved,
            Err(reason) => return unavailable(reason),
        };
        let bar_ms = timeframe_to_ms(&resolved) as f64;
        let lo = js_min(from, to);
        let hi = js_max(from, to);
        if !(hi > lo && bar_ms > 0.0) {
            return unavailable(SeriesUnavailable::NotLower);
        }
        if (hi - lo) / bar_ms > MAX_BARS {
            return unavailable(SeriesUnavailable::TooWide);
        }

        let key = format!("{}|{}", deps.market_key(), resolved);
        let now = self.inner.platform.now_ms();
        let (answer, job) = self.read(&key, &resolved, bar_ms, lo, hi, now);
        if let Some(job) = job {
            self.start(job);
        }
        answer
    }

    /// `auto` resolves to the largest standard step at least 4x finer than the
    /// chart (else the finest step still below it); an explicit timeframe passes
    /// only when strictly finer. Failures distinguish "this pick is not lower"
    /// from "NOTHING lower exists" (the chart is already at the finest offered
    /// step) so the consumer can word its notice honestly.
    fn resolve_timeframe(&self, timeframe: &str) -> Result<String, SeriesUnavailable> {
        let chart_ms = timeframe_to_ms(&self.inner.deps.chart_timeframe()) as f64;
        let finest = AUTO_STEPS[AUTO_STEPS.len() - 1];
        if timeframe_to_ms(finest) as f64 >= chart_ms {
            return Err(SeriesUnavailable::NoneLower);
        }
        let tf = match timeframe.trim() {
            "" => "auto",
            trimmed => trimmed,
        };
        if tf == "auto" {
            for step in AUTO_STEPS {
                if timeframe_to_ms(step) as f64 <= chart_ms / 4.0 {
                    return Ok(step.to_string());
                }
            }
            // No step subdivides 4x: the finest one below the chart still magnifies.
            return Ok(finest.to_string());
        }
        if (timeframe_to_ms(tf) as f64) < chart_ms {
            Ok(tf.to_string())
        } else {
            Err(SeriesUnavailable::NotLower)
        }
    }

    /// The cache decision of one read, under one borrow; the fetch it asks for
    /// is started by the caller, after the borrow ends.
    fn read(
        &self,
        key: &str,
        timeframe: &str,
        bar_ms: f64,
        lo: f64,
        hi: f64,
        now: f64,
    ) -> (DrawingSeriesState, Option<FetchJob>) {
        let mut cache = self.inner.cache.borrow_mut();
        let entries = cache.entries_of(key);
        let covering = entries.iter().find(|e| {
            let e = e.borrow();
            e.from <= lo && e.to >= hi
        });
        if let Some(covering) = covering {
            let (pending, failed_at) = {
                let c = covering.borrow();
                (c.pending, c.failed_at)
            };
            if pending {
                return (loading(&entries, timeframe, bar_ms, lo, hi), None);
            }
            if failed_at > 0.0 {
                if now - failed_at < RETRY_MS {
                    return (loading(&entries, timeframe, bar_ms, lo, hi), None);
                }
                cache.remove_entry(key, covering); // parked long enough: refetch below
            } else {
                let job = maybe_refresh(key, covering, timeframe, bar_ms, hi, now);
                let bars = slice(&covering.borrow().bars, lo, hi);
                return (
                    DrawingSeriesState::Ready {
                        bars,
                        timeframe: timeframe.to_string(),
                        bar_ms,
                    },
                    job,
                );
            }
        }
        let job = fetch_window(&mut cache, key, timeframe, lo, hi);
        let entries = cache.entries_of(key);
        (loading(&entries, timeframe, bar_ms, lo, hi), job)
    }

    /// Call the feed now (like the original, synchronously) and settle the entry
    /// when the answer lands.
    fn start(&self, job: FetchJob) {
        let range = BarRange {
            from: Some(job.from.floor() as i64),
            to: Some(job.to.ceil() as i64),
            ..BarRange::default()
        };
        let pending = self.inner.deps.fetch_bars(&job.timeframe, range);
        let inner = Rc::clone(&self.inner);
        self.inner.platform.spawn(Box::pin(async move {
            let result = pending.await;
            inner.settle(&job, result);
        }));
    }
}

impl DrawingSeriesGateway for DrawingSeriesService {
    fn series_in_range(&self, timeframe: &str, from: f64, to: f64) -> DrawingSeriesState {
        DrawingSeriesService::series_in_range(self, timeframe, from, to)
    }
}

fn unavailable(reason: SeriesUnavailable) -> DrawingSeriesState {
    DrawingSeriesState::Unavailable { reason }
}

/// The `Loading` answer, carrying best-effort PARTIAL bars from settled
/// overlapping windows: a widened window keeps painting what it already has
/// while it fetches.
fn loading(
    entries: &[EntryRef],
    timeframe: &str,
    bar_ms: f64,
    lo: f64,
    hi: f64,
) -> DrawingSeriesState {
    let mut partial: BTreeMap<i64, Ohlcv> = BTreeMap::new();
    for entry in entries {
        let e = entry.borrow();
        if e.pending || e.failed_at > 0.0 {
            continue;
        }
        if e.to < lo || e.from > hi {
            continue;
        }
        for b in slice(&e.bars, lo, hi) {
            partial.insert(b.time, b);
        }
    }
    let bars = if partial.is_empty() {
        None
    } else {
        Some(partial.into_values().collect())
    };
    DrawingSeriesState::Loading {
        timeframe: timeframe.to_string(),
        bar_ms,
        bars,
    }
}

/// Kick ONE background fetch for the padded window. Any OVERLAPPING in-flight
/// fetch defers this one (a corner drag repaints per pointer move: kicking a
/// window per frame would spam the provider); when it lands, the next paint
/// re-evaluates.
fn fetch_window(
    cache: &mut Cache,
    key: &str,
    timeframe: &str,
    lo: f64,
    hi: f64,
) -> Option<FetchJob> {
    let overlapping_pending = cache.entries_of(key).iter().any(|e| {
        let e = e.borrow();
        e.pending && e.to >= lo && e.from <= hi
    });
    if overlapping_pending {
        return None;
    }
    let pad = (hi - lo) * PAD_FRAC;
    let entry = Rc::new(RefCell::new(RangeEntry {
        from: lo - pad,
        to: hi + pad,
        bars: Vec::new(),
        fetched_at: 0.0,
        pending: true,
        failed_at: 0.0,
    }));
    cache.slot(key).push(entry.clone());
    cache.evict();
    let (from, to) = {
        let e = entry.borrow();
        (e.from, e.to)
    };
    Some(FetchJob {
        kind: FetchKind::Window,
        key: key.to_string(),
        entry,
        timeframe: timeframe.to_string(),
        from,
        to,
    })
}

/// A window whose right edge reaches the newest fetched bar refreshes at most
/// once per bar interval: new closed bars ride the feed's cache, only the live
/// tail re-fetches.
fn maybe_refresh(
    key: &str,
    entry: &EntryRef,
    timeframe: &str,
    bar_ms: f64,
    hi: f64,
    now: f64,
) -> Option<FetchJob> {
    let mut e = entry.borrow_mut();
    if e.pending {
        return None;
    }
    let last_bar = e.bars.last().map_or(e.from, |b| b.time as f64);
    if hi < last_bar {
        return None; // fully historical: closed bars do not change
    }
    if now - e.fetched_at < bar_ms {
        return None;
    }
    e.pending = true;
    let (from, to) = (e.from, e.to);
    drop(e);
    Some(FetchJob {
        kind: FetchKind::Refresh,
        key: key.to_string(),
        entry: entry.clone(),
        timeframe: timeframe.to_string(),
        from,
        to,
    })
}

/// Bars whose open time falls within `[lo, hi]` (ascending input: a linear scan is fine).
fn slice(bars: &[Ohlcv], lo: f64, hi: f64) -> Vec<Ohlcv> {
    let mut out = Vec::new();
    for b in bars {
        let t = b.time as f64;
        if t < lo {
            continue;
        }
        if t > hi {
            break;
        }
        out.push(*b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::testing::ManualPlatform;
    use crate::runtime::{OneshotSender, oneshot};

    const MIN: f64 = 60_000.0;

    /// Flat 5m bars covering `[from, to]` (open time inclusive).
    fn bars_5m(from: i64, to: i64) -> Vec<Ohlcv> {
        let step = 5 * 60_000;
        let mut out = Vec::new();
        let mut t = (from as f64 / step as f64).ceil() as i64 * step;
        while t <= to {
            out.push(Ohlcv::new(t, 1.0, 2.0, 0.5, 1.5, Some(10.0)));
            t += step;
        }
        out
    }

    type FetchSender = OneshotSender<Result<Vec<Ohlcv>, DataError>>;

    #[derive(Default)]
    struct State {
        chart_timeframe: RefCell<String>,
        market_key: RefCell<String>,
        can_fetch: Cell<bool>,
        fetches: RefCell<Vec<(String, BarRange)>>,
        pending: RefCell<Vec<(BarRange, FetchSender)>>,
        fail: Cell<bool>,
    }

    struct Deps(Rc<State>);

    impl DrawingSeriesDeps for Deps {
        fn fetch_bars(
            &self,
            timeframe: &str,
            range: BarRange,
        ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
            self.0
                .fetches
                .borrow_mut()
                .push((timeframe.to_string(), range.clone()));
            if self.0.fail.get() {
                return Box::pin(std::future::ready(Err(DataError::other("offline"))));
            }
            let (tx, rx) = oneshot();
            self.0.pending.borrow_mut().push((range, tx));
            Box::pin(rx)
        }
        fn can_fetch(&self) -> bool {
            self.0.can_fetch.get()
        }
        fn chart_timeframe(&self) -> String {
            self.0.chart_timeframe.borrow().clone()
        }
        fn market_key(&self) -> String {
            self.0.market_key.borrow().clone()
        }
    }

    struct Harness {
        platform: ManualPlatform,
        svc: DrawingSeriesService,
        state: Rc<State>,
    }

    impl Harness {
        fn new() -> Self {
            let state = Rc::new(State::default());
            *state.chart_timeframe.borrow_mut() = "60".into();
            *state.market_key.borrow_mut() = "BTCUSDT|".into();
            state.can_fetch.set(true);
            let platform = ManualPlatform::new(1_000_000.0);
            let svc = DrawingSeriesService::new(Rc::new(Deps(state.clone())), platform.handle());
            Self {
                platform,
                svc,
                state,
            }
        }

        fn fetches(&self) -> Vec<(String, BarRange)> {
            self.state.fetches.borrow().clone()
        }

        /// Resolve the oldest unresolved fetch with bars covering its requested range.
        fn flush(&self) {
            let next = {
                let mut pending = self.state.pending.borrow_mut();
                (!pending.is_empty()).then(|| pending.remove(0))
            };
            if let Some((range, tx)) = next {
                tx.send(Ok(bars_5m(range.from.unwrap_or(0), range.to.unwrap_or(0))));
            }
            self.platform.run();
        }

        fn set_chart_timeframe(&self, tf: &str) {
            *self.state.chart_timeframe.borrow_mut() = tf.into();
        }
    }

    fn is_loading(s: &DrawingSeriesState) -> bool {
        matches!(s, DrawingSeriesState::Loading { .. })
    }

    #[test]
    fn reports_no_source_when_the_market_has_no_ranged_feed() {
        let h = Harness::new();
        h.state.can_fetch.set(false);
        assert_eq!(
            h.svc.series_in_range("5", 0.0, MIN * 60.0),
            unavailable(SeriesUnavailable::NoSource)
        );
        assert!(h.fetches().is_empty());
    }

    #[test]
    fn refuses_timeframes_not_strictly_below_the_chart() {
        let h = Harness::new(); // chart = 60
        assert_eq!(
            h.svc.series_in_range("60", 0.0, MIN * 600.0),
            unavailable(SeriesUnavailable::NotLower)
        );
        assert_eq!(
            h.svc.series_in_range("240", 0.0, MIN * 600.0),
            unavailable(SeriesUnavailable::NotLower)
        );
        assert!(h.fetches().is_empty());
    }

    #[test]
    fn auto_resolves_to_the_largest_standard_step_at_least_4x_finer_than_the_chart() {
        let h = Harness::new(); // chart = 60 -> 15m
        match h.svc.series_in_range("auto", 0.0, MIN * 600.0) {
            DrawingSeriesState::Loading {
                timeframe, bar_ms, ..
            } => assert_eq!((timeframe.as_str(), bar_ms), ("15", 15.0 * MIN)),
            other => panic!("{other:?}"),
        }
        assert_eq!(h.fetches()[0].0, "15");
        h.set_chart_timeframe("D"); // 1440m / 4 = 360m -> '240'
        h.svc.series_in_range("auto", 0.0, MIN * 1440.0 * 3.0);
        assert_eq!(h.fetches()[1].0, "240");
        h.set_chart_timeframe("1"); // nothing lower exists: a distinct reason from a bad pick
        assert_eq!(
            h.svc.series_in_range("auto", 0.0, MIN * 30.0),
            unavailable(SeriesUnavailable::NoneLower)
        );
        assert_eq!(
            h.svc.series_in_range("5", 0.0, MIN * 30.0),
            unavailable(SeriesUnavailable::NoneLower)
        );
        h.set_chart_timeframe("2"); // no step subdivides 4x: auto still magnifies with the finest
        h.svc.series_in_range("auto", 0.0, MIN * 60.0);
        assert_eq!(h.fetches().last().unwrap().0, "1");
    }

    #[test]
    fn a_blank_timeframe_reads_as_auto() {
        let h = Harness::new();
        h.svc.series_in_range("  ", 0.0, MIN * 600.0);
        assert_eq!(h.fetches()[0].0, "15");
    }

    #[test]
    fn caps_a_request_that_would_need_more_bars_than_the_limit() {
        let h = Harness::new();
        assert_eq!(
            h.svc.series_in_range("1", 0.0, MIN * 5001.0),
            unavailable(SeriesUnavailable::TooWide)
        );
        assert!(h.fetches().is_empty());
    }

    #[test]
    fn an_empty_or_reversed_or_nan_span_is_not_lower() {
        let h = Harness::new();
        assert_eq!(
            h.svc.series_in_range("5", 100.0, 100.0),
            unavailable(SeriesUnavailable::NotLower)
        );
        assert_eq!(
            h.svc.series_in_range("5", f64::NAN, 100.0),
            unavailable(SeriesUnavailable::NotLower)
        );
        // Either order reads the same window.
        assert!(is_loading(&h.svc.series_in_range("5", MIN * 600.0, 0.0)));
        assert_eq!(h.fetches().len(), 1);
    }

    #[test]
    fn answers_loading_on_a_miss_fetches_a_padded_window_once_then_serves_from_cache() {
        let h = Harness::new();
        let from = MIN * 1000.0;
        let to = MIN * 1600.0; // 600 min = 120 five-minute bars
        assert!(is_loading(&h.svc.series_in_range("5", from, to)));
        assert!(is_loading(&h.svc.series_in_range("5", from, to))); // no duplicate fetch
        assert_eq!(h.fetches().len(), 1);
        // The fetched window is padded a quarter-span each side.
        let pad = (to - from) * 0.25;
        let range = &h.fetches()[0].1;
        assert_eq!(range.from, Some((from - pad) as i64));
        assert_eq!(range.to, Some((to + pad) as i64));

        let updates = Rc::new(Cell::new(0));
        let u = updates.clone();
        let _keep = h.svc.on_update(move || u.set(u.get() + 1));
        h.flush();
        assert_eq!(updates.get(), 1);

        match h.svc.series_in_range("5", from, to) {
            DrawingSeriesState::Ready {
                bars,
                timeframe,
                bar_ms,
            } => {
                assert_eq!(timeframe, "5");
                assert_eq!(bar_ms, 5.0 * MIN);
                assert!(bars[0].time as f64 >= from);
                assert!(bars[bars.len() - 1].time as f64 <= to);
                assert_eq!(bars.len(), 121); // inclusive bounds
            }
            other => panic!("{other:?}"),
        }
        // A narrower read inside the cached window is a pure cache hit.
        assert!(matches!(
            h.svc
                .series_in_range("5", from + MIN * 50.0, to - MIN * 50.0),
            DrawingSeriesState::Ready { .. }
        ));
        assert_eq!(h.fetches().len(), 1);
    }

    #[test]
    fn misses_the_cache_when_the_market_identity_changes() {
        let h = Harness::new();
        h.svc.series_in_range("5", 0.0, MIN * 600.0);
        h.flush();
        assert!(matches!(
            h.svc.series_in_range("5", 0.0, MIN * 600.0),
            DrawingSeriesState::Ready { .. }
        ));
        *h.state.market_key.borrow_mut() = "ETHUSDT|".into();
        assert!(is_loading(&h.svc.series_in_range("5", 0.0, MIN * 600.0)));
        assert_eq!(h.fetches().len(), 2);
    }

    #[test]
    fn serves_best_effort_partial_bars_while_a_widened_window_fetches() {
        let h = Harness::new();
        let from = MIN * 1000.0;
        let to = MIN * 1600.0;
        h.svc.series_in_range("5", from, to);
        h.flush(); // the narrow window is now settled
        // Widen the read past the cached window: it goes loading (a fetch kicks)
        // but still carries the bars the settled window holds for the overlap.
        match h.svc.series_in_range("5", from, to + MIN * 1000.0) {
            DrawingSeriesState::Loading {
                timeframe, bars, ..
            } => {
                assert_eq!(timeframe, "5");
                let bars = bars.expect("partial bars");
                assert!(bars.len() > 100); // the overlap, not nothing
                assert!(bars[0].time as f64 >= from);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(h.fetches().len(), 2);
    }

    #[test]
    fn defers_a_widened_request_while_an_overlapping_fetch_is_in_flight() {
        let h = Harness::new();
        h.svc.series_in_range("5", MIN * 1000.0, MIN * 1600.0); // fetch #1 in flight
        // Overlapping wider reads (a corner drag repaints per pointer move) must
        // not stack more fetches while #1 runs.
        h.svc.series_in_range("5", MIN * 900.0, MIN * 1700.0);
        h.svc.series_in_range("5", MIN * 800.0, MIN * 1800.0);
        assert_eq!(h.fetches().len(), 1);
    }

    #[test]
    fn a_failed_window_is_parked_instead_of_hot_looping_retries() {
        let h = Harness::new();
        h.state.fail.set(true);
        assert!(is_loading(&h.svc.series_in_range("5", 0.0, MIN * 600.0)));
        h.platform.run();
        // The failure parks the window: further reads stay loading without new fetches.
        assert!(is_loading(&h.svc.series_in_range("5", 0.0, MIN * 600.0)));
        assert!(is_loading(&h.svc.series_in_range("5", 0.0, MIN * 600.0)));
        assert_eq!(h.fetches().len(), 1);
        // A failure still notifies, so the host repaints its loading state.
    }

    #[test]
    fn a_parked_window_is_retried_once_the_penalty_passed() {
        let h = Harness::new();
        h.state.fail.set(true);
        h.svc.series_in_range("5", 0.0, MIN * 600.0);
        h.platform.run();
        h.platform.advance(RETRY_MS - 1.0);
        assert!(is_loading(&h.svc.series_in_range("5", 0.0, MIN * 600.0)));
        assert_eq!(h.fetches().len(), 1);
        h.platform.advance(1.0);
        h.state.fail.set(false);
        assert!(is_loading(&h.svc.series_in_range("5", 0.0, MIN * 600.0)));
        assert_eq!(h.fetches().len(), 2);
        h.flush();
        assert!(matches!(
            h.svc.series_in_range("5", 0.0, MIN * 600.0),
            DrawingSeriesState::Ready { .. }
        ));
    }

    #[test]
    fn a_failure_notifies_the_listeners_too() {
        let h = Harness::new();
        h.state.fail.set(true);
        let updates = Rc::new(Cell::new(0));
        let u = updates.clone();
        let _keep = h.svc.on_update(move || u.set(u.get() + 1));
        h.svc.series_in_range("5", 0.0, MIN * 600.0);
        h.platform.run();
        assert_eq!(updates.get(), 1);
    }

    #[test]
    fn unsubscribing_an_update_listener_stops_its_notifications() {
        let h = Harness::new();
        let updates = Rc::new(Cell::new(0));
        let u = updates.clone();
        let off = h.svc.on_update(move || u.set(u.get() + 1));
        off();
        h.svc.series_in_range("5", 0.0, MIN * 600.0);
        h.flush();
        assert_eq!(updates.get(), 0);
    }

    /// A settled window over [1000, 1600] minutes, padded to [850, 1750]; reads
    /// up to its right edge (1750) reach the newest fetched bar.
    fn live_window(h: &Harness) -> (f64, f64) {
        h.svc.series_in_range("5", MIN * 1000.0, MIN * 1600.0);
        h.flush();
        (MIN * 1000.0, MIN * 1750.0)
    }

    fn ready_bars(state: DrawingSeriesState) -> usize {
        match state {
            DrawingSeriesState::Ready { bars, .. } => bars.len(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_window_reaching_the_live_bar_refreshes_at_most_once_per_bar_interval() {
        let h = Harness::new();
        let (from, edge) = live_window(&h);
        // A fully historical read never refreshes, however stale the window.
        h.platform.advance(10.0 * MIN);
        assert!(ready_bars(h.svc.series_in_range("5", from, MIN * 1600.0)) > 0);
        assert_eq!(h.fetches().len(), 1);
        // A read reaching the newest bar refreshes the stale window, served stale meanwhile.
        assert!(ready_bars(h.svc.series_in_range("5", from, edge)) > 0);
        assert_eq!(h.fetches().len(), 2);
        // The refresh is in flight: reads do not stack another.
        let _ = h.svc.series_in_range("5", from, edge);
        assert_eq!(h.fetches().len(), 2);
        let updates = Rc::new(Cell::new(0));
        let u = updates.clone();
        let _keep = h.svc.on_update(move || u.set(u.get() + 1));
        h.flush();
        assert_eq!(updates.get(), 1);
        // Fresh again: within one bar interval of the last fetch there is no refresh.
        let _ = h.svc.series_in_range("5", from, edge);
        assert_eq!(h.fetches().len(), 2);
        h.platform.advance(5.0 * MIN);
        let _ = h.svc.series_in_range("5", from, edge);
        assert_eq!(h.fetches().len(), 3);
    }

    #[test]
    fn a_refresh_that_fails_keeps_serving_the_stale_window_and_retries_a_bar_later() {
        let h = Harness::new();
        let (from, edge) = live_window(&h);
        let settled = ready_bars(h.svc.series_in_range("5", from, edge));
        h.platform.advance(5.0 * MIN);
        h.state.fail.set(true);
        assert_eq!(ready_bars(h.svc.series_in_range("5", from, edge)), settled);
        h.platform.run();
        assert_eq!(h.fetches().len(), 2);
        // The failure restarts the interval and drops no bars.
        assert_eq!(ready_bars(h.svc.series_in_range("5", from, edge)), settled);
        assert_eq!(h.fetches().len(), 2);
        h.platform.advance(5.0 * MIN);
        let _ = h.svc.series_in_range("5", from, edge);
        assert_eq!(h.fetches().len(), 3);
    }

    #[test]
    fn overlapping_windows_merge_into_one_and_the_cache_stays_bounded() {
        let h = Harness::new();
        // 20 disjoint windows exceed the 16-entry bound: the oldest settled go.
        for i in 0..20 {
            let from = MIN * 100_000.0 * f64::from(i);
            h.svc.series_in_range("5", from, from + MIN * 600.0);
            h.flush();
            h.platform.advance(1.0);
        }
        let total: usize = h
            .svc
            .inner
            .cache
            .borrow()
            .keys
            .iter()
            .map(|(_, e)| e.len())
            .sum();
        assert_eq!(total, MAX_ENTRIES);
        // The newest window is still cached; the oldest was evicted.
        let newest = MIN * 100_000.0 * 19.0;
        assert!(matches!(
            h.svc.series_in_range("5", newest, newest + MIN * 600.0),
            DrawingSeriesState::Ready { .. }
        ));
        assert!(is_loading(&h.svc.series_in_range("5", 0.0, MIN * 600.0)));
    }

    #[test]
    fn two_adjacent_windows_are_absorbed_into_the_fresh_one() {
        let h = Harness::new();
        h.svc.series_in_range("5", MIN * 1000.0, MIN * 1600.0);
        h.flush();
        // Overlaps the first window's padded edge: a second window is fetched,
        // and on landing it absorbs the first.
        h.svc.series_in_range("5", MIN * 1500.0, MIN * 2100.0);
        h.flush();
        let entries: Vec<usize> = h
            .svc
            .inner
            .cache
            .borrow()
            .keys
            .iter()
            .map(|(_, e)| e.len())
            .collect();
        assert_eq!(entries, [1]);
        // The merged window serves the whole span without another fetch.
        let fetches = h.fetches().len();
        assert!(matches!(
            h.svc.series_in_range("5", MIN * 1000.0, MIN * 2100.0),
            DrawingSeriesState::Ready { .. }
        ));
        assert_eq!(h.fetches().len(), fetches);
    }

    #[test]
    fn works_as_a_gateway_trait_object() {
        let h = Harness::new();
        let gateway: Rc<dyn DrawingSeriesGateway> = Rc::new(h.svc.clone());
        assert!(matches!(
            gateway.series_in_range("5", 0.0, MIN * 600.0),
            DrawingSeriesState::Loading { .. }
        ));
        assert_eq!(h.fetches().len(), 1);
    }

    #[test]
    fn a_response_landing_after_the_service_was_dropped_is_harmless() {
        let h = Harness::new();
        h.svc.series_in_range("5", 0.0, MIN * 600.0);
        let Harness {
            platform,
            svc,
            state,
        } = h;
        drop(svc);
        let (range, tx) = state.pending.borrow_mut().remove(0);
        tx.send(Ok(bars_5m(range.from.unwrap_or(0), range.to.unwrap_or(0))));
        platform.run();
    }
}
