//! A cache in front of a [`MarketDataFeed`]: closed bars stay in a shared
//! [`BarStore`], so a re-run fetches only the uncached tail.
// `DataError` is large (it carries a `DataUnavailableError`) and the port fixes it as the
// error type of every bar future; boxing it is a change for `runtime.rs`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use super::provider_registry::{eager, parse_symbol};
use super::{BarStore, series_key, timeframe_to_ms};
use crate::model::Ohlcv;
use crate::ports::market_data::{
    BarRange, BarSink, BatchSink, MarketConfig, MarketDataFeed, SymbolInfo,
};
use crate::runtime::{AbortSignal, DataError, LocalBoxFuture, Platform, Unsubscribe, js_await};

/// Series cache key. The symbol's own grammar carries the venue: a `provider:`
/// prefix keys per venue (the multi-provider feed hands canonical prefixed
/// symbols down); a bare symbol keys venue-less, whatever serves it
/// (single-feed setups). A non-default session keys its own series (regular and
/// extended bars genuinely differ).
fn cache_key(symbol: &str, timeframe: &str, session: Option<&str>) -> String {
    let parsed = parse_symbol(symbol);
    series_key(
        parsed.provider.as_deref().unwrap_or(""),
        &parsed.ticker,
        timeframe,
        session,
    )
}

/// Page size for big ranged fetches (mirrors the orchestrator's history chunks).
const PAGE_BARS: usize = 10_000;
/// At or below this a ranged fetch stays a SINGLE request. One above
/// [`PAGE_BARS`] so the orchestrator's own backfill chunks (`CHUNK_BARS + 1` for
/// the overlap bar) never split.
const SINGLE_FETCH_BARS: usize = PAGE_BARS + 1;

/// What a ranged fetch brought back.
struct Fetched {
    /// Ascending, merged bars.
    bars: Vec<Ohlcv>,
    /// The oldest time the fetch PROVED covered: honest coverage marking, so a
    /// count-satisfied early stop never claims the requested `from`.
    covered_down_to: Option<i64>,
}

/// A `MarketDataFeed` decorator that caches CLOSED bars in a shared in-memory
/// [`BarStore`]. A new indicator run for the same `(provider, symbol,
/// timeframe)` serves the already-fetched bars and re-downloads only the
/// uncached **tail** (newly closed bars plus the forming candle). The forming
/// bar is never cached: the live poll re-fetches it.
///
/// Savings on a re-run are about ⌈N/1000⌉ − 1 requests: dormant for charts of
/// 1000 bars or fewer (the single tail request is the whole fetch either way),
/// paying off with deep history. Needs the inner feed's `load_range`; without it,
/// or for offline `data`, it transparently falls back to a full `load`.
///
/// A cheap handle: clones share the inner feed and the store.
#[derive(Clone)]
pub struct CachingDataFeed {
    inner: Rc<dyn MarketDataFeed>,
    store: Rc<RefCell<BarStore>>,
    platform: Rc<dyn Platform>,
}

impl CachingDataFeed {
    /// `store` is shared with whoever else caches on it (the original keeps one
    /// module-wide store; here the caller owns it).
    pub fn new(
        inner: Rc<dyn MarketDataFeed>,
        store: Rc<RefCell<BarStore>>,
        platform: Rc<dyn Platform>,
    ) -> Self {
        Self {
            inner,
            store,
            platform,
        }
    }

    /// The closing time of the cached series when it is deep enough to cover `n`
    /// bars (`n - 1` accounts for the forming bar that is always dropped).
    fn warm_tail(&self, key: &str, n: usize) -> Option<i64> {
        let store = self.store.borrow();
        let cached = store.get(key)?;
        let last = cached.last()?;
        (cached.len() + 1 >= n).then_some(last.time)
    }

    async fn load_cached(&self, cfg: MarketConfig) -> Result<Vec<Ohlcv>, DataError> {
        // Offline data is already in memory: nothing to cache.
        if has_data(&cfg) {
            return js_await(self.inner.load(&cfg)).await;
        }
        let symbol = symbol_of(&cfg);
        let key = cache_key(symbol, timeframe_of(&cfg), session_of(&cfg));
        self.store.borrow_mut().retain_symbol(symbol); // current-symbol-only purge
        let n = cfg.bars.unwrap_or(500);

        // Tail-only path: enough closed history is cached to cover the requested
        // window. Fetch just the tail from the last cached bar and merge.
        if let Some(from) = self.warm_tail(&key, n) {
            let range = BarRange {
                from: Some(from),
                ..BarRange::default()
            };
            if let Some(fetched) = js_await(self.fetch_range(&cfg, &range)).await? {
                let tail = fetched.bars;
                let closed = self
                    .store
                    .borrow_mut()
                    .merge(&key, drop_forming(&tail))
                    .to_vec();
                let shown = attach_forming(closed, tail.last().copied());
                return Ok(last_n(shown, n));
            }
        }

        // Cold path / requested more than cached: full fetch, cache the closed bars.
        let bars = js_await(self.inner.load(&cfg)).await?;
        self.store.borrow_mut().merge(&key, drop_forming(&bars));
        Ok(bars)
    }

    async fn load_progressive_cached(
        &self,
        cfg: MarketConfig,
        on_batch: BatchSink,
        signal: Option<AbortSignal>,
    ) -> Result<Option<Vec<Ohlcv>>, DataError> {
        if has_data(&cfg) {
            return js_await(self.inner.load(&cfg)).await.map(Some);
        }
        let symbol = symbol_of(&cfg);
        let key = cache_key(symbol, timeframe_of(&cfg), session_of(&cfg));
        let n = cfg.bars.unwrap_or(500);
        if self.warm_tail(&key, n).is_some() {
            return js_await(self.load_cached(cfg)).await.map(Some); // warm: the tail-merge path
        }
        self.store.borrow_mut().retain_symbol(symbol);
        let Some(load) = self.inner.load_progressive(&cfg, on_batch, signal) else {
            return Ok(None);
        };
        let Some(bars) = js_await(load).await? else {
            return Ok(None); // the resolved provider turned out incapable
        };
        self.store.borrow_mut().merge(&key, drop_forming(&bars));
        Ok(Some(bars))
    }

    async fn load_range_cached(
        &self,
        cfg: MarketConfig,
        range: BarRange,
    ) -> Result<Vec<Ohlcv>, DataError> {
        let key = cache_key(symbol_of(&cfg), timeframe_of(&cfg), session_of(&cfg));
        let mut cached = self.cached(&key);
        let mut newest = cached.last().copied();

        // Historical request: strictly older than the cached tip. The tail path
        // can never serve it (it only extends the NEWEST edge; before this
        // branch existed, a count-bounded `{to, limit}` walk would read
        // "covered" off the bar COUNT and return the whole cache unfiltered, so
        // a backward chunk loop never progressed).
        if let (Some(to), Some(tip)) = (range.to, newest)
            && to < tip.time
        {
            let older: Vec<Ohlcv> = cached.iter().filter(|b| b.time <= to).copied().collect();
            let covered_from = self.store.borrow().covered_from_of(&key);
            let reaches_from = |from: i64| covered_from.is_some_and(|c| c <= from);
            let covered = match range.limit {
                Some(limit) => older.len() >= limit && range.from.is_none_or(reaches_from),
                None => range.from.is_some_and(reaches_from),
            };
            if covered {
                let mut out: Vec<Ohlcv> = match range.from {
                    Some(from) => older.into_iter().filter(|b| b.time >= from).collect(),
                    None => older,
                };
                if let Some(limit) = range.limit
                    && out.len() > limit
                {
                    out.drain(..out.len() - limit);
                }
                return Ok(out);
            }
            // Every bar at/before `to` predates the cached tip, so it is closed:
            // cache it whole. The `to` filter guards against providers that
            // over-serve past the bound.
            let Some(fetched) = js_await(self.fetch_range(&cfg, &range)).await? else {
                return js_await(self.inner.load(&cfg)).await;
            };
            let bars: Vec<Ohlcv> = fetched.bars.into_iter().filter(|b| b.time <= to).collect();
            if let Some(first) = bars.first() {
                let mut store = self.store.borrow_mut();
                store.merge(&key, &bars);
                store.mark_covered(&key, fetched.covered_down_to.unwrap_or(first.time));
            }
            return Ok(bars);
        }

        // Backward extension: the request reaches OLDER than coverage. Fetch only
        // the missing head `[from, covered_from]`, merge, and serve the rest from
        // cache below (a re-run over deepened history must never re-download the
        // covered middle).
        if newest.is_some()
            && let Some(from) = range.from
        {
            let covered_from = self.store.borrow().covered_from_of(&key);
            if let Some(covered_from) = covered_from
                && covered_from > from
            {
                let head_range = BarRange {
                    from: Some(from),
                    to: Some(covered_from),
                    ..BarRange::default()
                };
                let Some(fetched) = js_await(self.fetch_range(&cfg, &head_range)).await? else {
                    return js_await(self.inner.load(&cfg)).await;
                };
                let head: Vec<Ohlcv> = fetched
                    .bars
                    .into_iter()
                    .filter(|b| b.time <= covered_from)
                    .collect(); // capped at the covered edge: closed
                {
                    let mut store = self.store.borrow_mut();
                    store.merge(&key, &head);
                    store.mark_covered(&key, fetched.covered_down_to.unwrap_or(from));
                }
                cached = self.cached(&key);
                newest = cached.last().copied();
            }
        }

        let covered_from = self.store.borrow().covered_from_of(&key);
        // Tail-only path: the cache was fetched from at/before the requested
        // `from` (or holds enough bars when the request is count-bounded with no
        // `from`). Uses the recorded covered-from, NOT the first cached bar's
        // time: that is always at/after `from`, so it can never prove coverage
        // (the HTF-warmup bug).
        let covers = !cached.is_empty()
            && newest.is_some()
            && match range.from {
                Some(from) => covered_from.is_some_and(|c| c <= from),
                None => range.limit.is_none_or(|limit| cached.len() >= limit),
            };
        if covers && let Some(tip) = newest {
            let tail_range = BarRange {
                from: Some(tip.time),
                to: range.to,
                limit: range.limit,
                session: None,
            };
            let Some(fetched) = js_await(self.fetch_range(&cfg, &tail_range)).await? else {
                return js_await(self.inner.load(&cfg)).await;
            };
            let tail = fetched.bars;
            let closed = self
                .store
                .borrow_mut()
                .merge(&key, drop_forming(&tail))
                .to_vec();
            let full = attach_forming(closed, tail.last().copied());
            let mut out: Vec<Ohlcv> = match range.from {
                Some(from) => full.into_iter().filter(|b| b.time >= from).collect(),
                None => full,
            };
            if let Some(to) = range.to {
                out.retain(|b| b.time <= to);
            }
            return Ok(out);
        }

        // Miss: fetch the requested range, cache its closed bars, and record coverage.
        let Some(fetched) = js_await(self.fetch_range(&cfg, &range)).await? else {
            return js_await(self.inner.load(&cfg)).await;
        };
        let bars = fetched.bars;
        let mut store = self.store.borrow_mut();
        store.merge(&key, drop_forming(&bars));
        store.mark_covered(
            &key,
            fetched
                .covered_down_to
                .or(range.from)
                .or(bars.first().map(|b| b.time))
                .unwrap_or(0),
        );
        drop(store);
        Ok(bars)
    }

    fn cached(&self, key: &str) -> Vec<Ohlcv> {
        self.store
            .borrow()
            .get(key)
            .map(<[Ohlcv]>::to_vec)
            .unwrap_or_default()
    }

    /// THE provider round-trip chokepoint for ranged fetches. Two jobs:
    /// - Every request goes out with an **explicit `limit`**: some sources clip
    ///   a limit-less request to a small default window and tail-slice, silently
    ///   truncating wide date-bounded fetches (the `request.security` bug).
    /// - A request bigger than one page walks **backward in bounded pages**, so
    ///   a multi-hundred-thousand-bar series arrives as digestible responses
    ///   instead of one giant clip-prone payload.
    ///
    /// `None` means the inner feed has no ranged support.
    async fn fetch_range(
        &self,
        cfg: &MarketConfig,
        range: &BarRange,
    ) -> Result<Option<Fetched>, DataError> {
        let tf_ms = timeframe_to_ms(timeframe_of(cfg));
        let estimate = self.estimate_bars(range, tf_ms);
        if estimate <= SINGLE_FETCH_BARS as f64 {
            let sized = BarRange {
                limit: Some(range.limit.unwrap_or(estimate as usize)),
                ..range.clone()
            };
            let Some(load) = self.inner.load_range(cfg, &sized) else {
                return Ok(None);
            };
            let bars = js_await(load).await?;
            let covered_down_to = range.from.or(bars.first().map(|b| b.time));
            return Ok(Some(Fetched {
                bars,
                covered_down_to,
            }));
        }
        // Backward count-paged walk from the newest edge.
        let mut by_time: BTreeMap<i64, Ohlcv> = BTreeMap::new();
        let mut cursor = range.to.unwrap_or(self.platform.now_ms() as i64);
        let mut first_request = true;
        loop {
            if range.from.is_some_and(|from| cursor < from) {
                break;
            }
            let want = match range.limit {
                Some(limit) => limit.saturating_sub(by_time.len()),
                None => PAGE_BARS,
            };
            if want == 0 {
                break;
            }
            let page_range = BarRange {
                to: Some(cursor),
                limit: Some(PAGE_BARS.min(want)),
                ..BarRange::default()
            };
            let Some(load) = self.inner.load_range(cfg, &page_range) else {
                if first_request {
                    return Ok(None);
                }
                break;
            };
            first_request = false;
            let page = js_await(load).await?;
            let oldest = page.iter().map(|b| b.time).min();
            for b in &page {
                if b.time <= cursor && range.from.is_none_or(|from| b.time >= from) {
                    by_time.insert(b.time, *b);
                }
            }
            let Some(oldest) = oldest else {
                // Empty-page semantics differ by source: a count-based provider
                // spans quiet gaps, so empty means nothing older exists (stop:
                // the genesis signal). A time-window source can return an empty
                // WINDOW inside a legit gap: with a `from` bound we keep stepping
                // the window down (a genesis-aware server answers pre-genesis
                // steps locally, so this stays cheap).
                if range.from.is_none() || tf_ms <= 0 {
                    break;
                }
                cursor = cursor.saturating_sub((PAGE_BARS as i64).saturating_mul(tf_ms));
                continue;
            };
            if oldest > cursor {
                break; // defensive: no backward progress
            }
            cursor = oldest - 1;
        }
        let bars: Vec<Ohlcv> = by_time.into_values().collect();
        // Proven floor: a from-bounded walk that ran out the bottom covered down
        // to `from`; an early stop only proves down to the oldest bar actually seen.
        let walked_out = range.from.is_some_and(|from| cursor < from);
        let covered_down_to = if walked_out {
            range.from
        } else {
            bars.first().map(|b| b.time)
        };
        Ok(Some(Fetched {
            bars,
            covered_down_to,
        }))
    }

    /// Estimated bar count of a range: drives single-vs-paged and the explicit
    /// limit put on date-bounded requests. The 1.1x + 2 slack absorbs
    /// variable-length periods (calendar months on `M`) and the forming bar, so
    /// a computed limit can never under-serve.
    fn estimate_bars(&self, range: &BarRange, tf_ms: i64) -> f64 {
        if let Some(limit) = range.limit {
            return limit as f64;
        }
        let Some(from) = range.from else {
            return PAGE_BARS as f64; // unbounded and unlimited: page-sized default
        };
        let to = range.to.unwrap_or(self.platform.now_ms() as i64);
        let span = (to - from).max(0) as f64;
        (span / tf_ms as f64 * 1.1).ceil() + 2.0
    }
}

impl MarketDataFeed for CachingDataFeed {
    fn supports_load_range(&self) -> bool {
        true
    }

    fn supports_load_progressive(&self) -> bool {
        true
    }

    fn load(&self, cfg: &MarketConfig) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
        let this = self.clone();
        let cfg = cfg.clone();
        eager(Box::pin(async move { this.load_cached(cfg).await }))
    }

    /// Progressive twin of `load`: a COLD load streams through the inner feed's
    /// progressive path (batches forwarded verbatim, the FINAL answer cached
    /// exactly as `load` caches), while a cache-covered load answers once
    /// through `load` itself (a warm chart has nothing to stream). Resolves to
    /// `None` when the inner feed lacks the capability, so callers may prefer
    /// this method unconditionally.
    fn load_progressive(
        &self,
        cfg: &MarketConfig,
        on_batch: BatchSink,
        signal: Option<AbortSignal>,
    ) -> Option<LocalBoxFuture<'static, Result<Option<Vec<Ohlcv>>, DataError>>> {
        let this = self.clone();
        let cfg = cfg.clone();
        Some(eager(Box::pin(async move {
            this.load_progressive_cached(cfg, on_batch, signal).await
        })))
    }

    /// Cache-backed ranged fetch: the gateway engines use for secondary series
    /// (`request.security` HTF/LTF/cross-symbol) and the orchestrator's backward
    /// history chunks. Caches per `(provider, symbol, timeframe)` in the shared
    /// store. Three cache-friendly shapes:
    /// - **historical** (`to` before the cached tip): served straight from cache
    ///   when it provably holds the window, else fetched whole (and cached: no
    ///   forming bar can exist there);
    /// - **backward extension** (`from` older than coverage): fetch ONLY the
    ///   missing head, never the already-covered remainder again;
    /// - **tail** (everything else covered): refresh only from the last cached bar.
    ///
    /// Falls back to a full `load` when the inner feed has no ranged support.
    fn load_range(
        &self,
        cfg: &MarketConfig,
        range: &BarRange,
    ) -> Option<LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>>> {
        let this = self.clone();
        let cfg = cfg.clone();
        let range = range.clone();
        Some(eager(Box::pin(async move {
            this.load_range_cached(cfg, range).await
        })))
    }

    fn subscribe(&self, cfg: &MarketConfig, on_bar: BarSink) -> Unsubscribe {
        self.inner.subscribe(cfg, on_bar)
    }

    fn symbol_info(&self, cfg: &MarketConfig) -> Option<SymbolInfo> {
        self.inner.symbol_info(cfg)
    }
}

fn has_data(cfg: &MarketConfig) -> bool {
    cfg.data.as_ref().is_some_and(|d| !d.is_empty())
}

fn symbol_of(cfg: &MarketConfig) -> &str {
    cfg.symbol.as_deref().unwrap_or("TEST")
}

fn timeframe_of(cfg: &MarketConfig) -> &str {
    cfg.timeframe.as_deref().unwrap_or("60")
}

fn session_of(cfg: &MarketConfig) -> Option<&str> {
    cfg.session.map(|s| s.as_str())
}

/// `bars.slice(-n)`; a zero count reads as `slice(-0)`, which keeps everything.
fn last_n(mut bars: Vec<Ohlcv>, n: usize) -> Vec<Ohlcv> {
    if n > 0 && bars.len() > n {
        bars.drain(..bars.len() - n);
    }
    bars
}

/// Drop the last bar: treated as the forming/in-progress candle, never cached.
fn drop_forming(bars: &[Ohlcv]) -> &[Ohlcv] {
    bars.split_last().map_or(bars, |(_, closed)| closed)
}

/// Re-attach the live forming bar to the closed set for display (if it is genuinely newer).
fn attach_forming(mut closed: Vec<Ohlcv>, forming: Option<Ohlcv>) -> Vec<Ohlcv> {
    if let Some(forming) = forming
        && closed.last().is_none_or(|last| forming.time > last.time)
    {
        closed.push(forming);
    }
    closed
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::data::DEFAULT_RETAIN_OWNER;
    use crate::data_unavailable::{DataUnavailableDetails, DataUnavailableError};
    use crate::ports::market_data::MarketSession;
    use crate::runtime::testing::ManualPlatform;

    const T0: i64 = 1_700_000_000_000;
    const HOUR: i64 = 3_600_000;

    fn bar_at(start: i64, step: i64, i: i64) -> Ohlcv {
        let price = 100.0 + (i as f64 / 5.0).sin() * 10.0;
        Ohlcv::new(
            start + i * step,
            price,
            price + 1.0,
            price - 1.0,
            price,
            Some(1.0),
        )
    }

    /// A feed over a synthetic, growing timeline. `load` returns the most recent
    /// N; `load_range` the requested slice.
    struct FakeFeed {
        start: i64,
        step: i64,
        all: RefCell<Vec<Ohlcv>>,
        load_calls: Cell<usize>,
        range_calls: Cell<usize>,
        last_range_from: Cell<Option<i64>>,
        ranges: RefCell<Vec<BarRange>>,
        progressive_calls: Cell<usize>,
        batches: RefCell<Vec<usize>>,
        ranged: bool,
        progressive: bool,
        range_error: RefCell<Option<DataError>>,
    }

    impl FakeFeed {
        fn with(count: i64, start: i64, step: i64) -> Self {
            Self {
                start,
                step,
                all: RefCell::new((0..count).map(|i| bar_at(start, step, i)).collect()),
                load_calls: Cell::new(0),
                range_calls: Cell::new(0),
                last_range_from: Cell::new(None),
                ranges: RefCell::default(),
                progressive_calls: Cell::new(0),
                batches: RefCell::default(),
                ranged: true,
                progressive: false,
                range_error: RefCell::new(None),
            }
        }

        fn new(count: i64) -> Self {
            Self::with(count, T0, HOUR)
        }

        fn without_range(mut self) -> Self {
            self.ranged = false;
            self
        }

        fn with_progressive(mut self) -> Self {
            self.progressive = true;
            self
        }

        /// Append n new bars; the last is the forming candle.
        fn advance(&self, n: i64) {
            let base = self.all.borrow().len() as i64;
            for i in 0..n {
                self.all
                    .borrow_mut()
                    .push(bar_at(self.start, self.step, base + i));
            }
        }

        fn expected(&self, n: usize) -> Vec<Ohlcv> {
            let all = self.all.borrow();
            all[all.len().saturating_sub(n)..].to_vec()
        }

        fn limits(&self) -> Vec<Option<usize>> {
            self.ranges.borrow().iter().map(|r| r.limit).collect()
        }
    }

    impl MarketDataFeed for FakeFeed {
        fn supports_load_range(&self) -> bool {
            self.ranged
        }

        fn supports_load_progressive(&self) -> bool {
            self.progressive
        }

        fn load(
            &self,
            cfg: &MarketConfig,
        ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
            self.load_calls.set(self.load_calls.get() + 1);
            let out = match &cfg.data {
                Some(data) if !data.is_empty() => data.clone(),
                _ => self.expected(cfg.bars.unwrap_or(500)),
            };
            Box::pin(async move { Ok(out) })
        }

        fn load_progressive(
            &self,
            cfg: &MarketConfig,
            mut on_batch: BatchSink,
            _signal: Option<AbortSignal>,
        ) -> Option<LocalBoxFuture<'static, Result<Option<Vec<Ohlcv>>, DataError>>> {
            if !self.progressive {
                return None;
            }
            self.progressive_calls.set(self.progressive_calls.get() + 1);
            let full = self.expected(cfg.bars.unwrap_or(500));
            let half = full[full.len() / 2..].to_vec();
            self.batches.borrow_mut().push(half.len());
            on_batch(half);
            Some(Box::pin(async move { Ok(Some(full)) }))
        }

        fn load_range(
            &self,
            _cfg: &MarketConfig,
            range: &BarRange,
        ) -> Option<LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>>> {
            if !self.ranged {
                return None;
            }
            self.range_calls.set(self.range_calls.get() + 1);
            self.last_range_from.set(range.from);
            self.ranges.borrow_mut().push(range.clone());
            if let Some(error) = self.range_error.borrow().clone() {
                return Some(Box::pin(async move { Err(error) }));
            }
            let from = range.from.unwrap_or(i64::MIN);
            let to = range.to.unwrap_or(i64::MAX);
            let mut out: Vec<Ohlcv> = self
                .all
                .borrow()
                .iter()
                .filter(|b| b.time >= from && b.time <= to)
                .copied()
                .collect();
            if let Some(limit) = range.limit
                && out.len() > limit
            {
                out.drain(..out.len() - limit);
            }
            Some(Box::pin(async move { Ok(out) }))
        }

        fn subscribe(&self, _cfg: &MarketConfig, _on_bar: BarSink) -> Unsubscribe {
            Box::new(|| {})
        }
    }

    fn cfg(symbol: &str, timeframe: &str, bars: Option<usize>) -> MarketConfig {
        MarketConfig {
            symbol: Some(symbol.into()),
            timeframe: Some(timeframe.into()),
            bars,
            ..MarketConfig::default()
        }
    }

    fn main_cfg() -> MarketConfig {
        cfg("binance:BTCUSDT", "60", Some(500))
    }

    fn key() -> String {
        series_key("binance", "BTCUSDT", "60", None)
    }

    fn times(bars: &[Ohlcv]) -> Vec<i64> {
        bars.iter().map(|b| b.time).collect()
    }

    struct Rig {
        platform: ManualPlatform,
        feed: Rc<FakeFeed>,
        store: Rc<RefCell<BarStore>>,
        cache: CachingDataFeed,
    }

    impl Rig {
        fn over(feed: FakeFeed) -> Self {
            // "Now" sits just past the fake timeline, as the original's wall clock
            // sat just past its test data.
            let platform = ManualPlatform::new((T0 + 1_000 * HOUR) as f64);
            let feed = Rc::new(feed);
            let store = Rc::new(RefCell::new(BarStore::new()));
            let cache = CachingDataFeed::new(feed.clone(), store.clone(), platform.handle());
            Self {
                platform,
                feed,
                store,
                cache,
            }
        }

        fn new(count: i64) -> Self {
            Self::over(FakeFeed::new(count))
        }

        fn load(&self, cfg: &MarketConfig) -> Vec<Ohlcv> {
            self.platform.block_on(self.cache.load(cfg)).unwrap()
        }

        fn range(&self, cfg: &MarketConfig, range: BarRange) -> Vec<Ohlcv> {
            self.range_result(cfg, range).unwrap()
        }

        fn range_result(
            &self,
            cfg: &MarketConfig,
            range: BarRange,
        ) -> Result<Vec<Ohlcv>, DataError> {
            self.platform
                .block_on(self.cache.load_range(cfg, &range).unwrap())
        }

        fn cached(&self, key: &str) -> Option<Vec<Ohlcv>> {
            self.store.borrow().get(key).map(<[Ohlcv]>::to_vec)
        }
    }

    fn from(from: i64) -> BarRange {
        BarRange {
            from: Some(from),
            ..BarRange::default()
        }
    }

    fn window(to: i64, limit: usize) -> BarRange {
        BarRange {
            to: Some(to),
            limit: Some(limit),
            ..BarRange::default()
        }
    }

    // ---- caching-data-feed.test.ts ----

    #[test]
    fn cold_load_fetches_in_full_caches_the_closed_bars_and_excludes_the_forming_bar() {
        let rig = Rig::new(600);
        let r1 = rig.load(&main_cfg());
        assert_eq!(r1.len(), 500);
        assert_eq!(rig.feed.load_calls.get(), 1);
        assert_eq!(rig.feed.range_calls.get(), 0);
        let cached = rig.cached(&key()).unwrap();
        assert_eq!(cached.len(), 499); // forming bar dropped
        assert!(cached.last().unwrap().time < r1.last().unwrap().time);
    }

    #[test]
    fn a_re_run_fetches_only_the_tail_through_load_range() {
        let rig = Rig::new(600);
        rig.load(&main_cfg());
        let r2 = rig.load(&main_cfg());
        assert_eq!(rig.feed.load_calls.get(), 1); // not re-fetched in full
        assert_eq!(rig.feed.range_calls.get(), 1); // tail-only
        assert_eq!(r2.len(), 500);
        assert_eq!(times(&r2), times(&rig.feed.expected(500)));
    }

    #[test]
    fn after_new_bars_form_the_re_run_still_tails_and_returns_the_shifted_window() {
        let rig = Rig::new(600);
        rig.load(&main_cfg());
        rig.feed.advance(3);
        let r3 = rig.load(&main_cfg());
        assert_eq!(rig.feed.load_calls.get(), 1);
        assert_eq!(rig.feed.range_calls.get(), 1);
        assert_eq!(times(&r3), times(&rig.feed.expected(500)));
    }

    #[test]
    fn the_cache_stays_contiguous_sorted_and_deduplicated() {
        let rig = Rig::new(600);
        rig.load(&main_cfg());
        rig.feed.advance(2);
        rig.load(&main_cfg());
        let t = times(&rig.cached(&key()).unwrap());
        let mut sorted = t.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(t, sorted);
    }

    #[test]
    fn requesting_more_bars_than_cached_falls_back_to_a_full_fetch() {
        let rig = Rig::new(3000);
        rig.load(&main_cfg()); // caches ~499
        let big = rig.load(&cfg("binance:BTCUSDT", "60", Some(2000)));
        assert_eq!(rig.feed.load_calls.get(), 2); // 499 < 1999: full fetch, not tail
        assert_eq!(big.len(), 2000);
    }

    #[test]
    fn changing_symbol_purges_the_previous_one() {
        let rig = Rig::new(600);
        rig.load(&main_cfg());
        assert!(rig.cached(&key()).is_some());
        rig.load(&cfg("binance:ETHUSDT", "60", Some(500)));
        assert!(rig.cached(&key()).is_none()); // BTCUSDT evicted
        assert!(
            rig.cached(&series_key("binance", "ETHUSDT", "60", None))
                .is_some()
        );
        // And re-loading the original is a cold fetch again.
        let before = rig.feed.load_calls.get();
        rig.load(&main_cfg());
        assert_eq!(rig.feed.load_calls.get(), before + 1);
    }

    #[test]
    fn offline_data_bypasses_the_cache_entirely() {
        let rig = Rig::new(10);
        let offline = rig.feed.expected(5);
        let loaded = rig.load(&MarketConfig {
            data: Some(offline.clone()),
            ..MarketConfig::default()
        });
        assert_eq!(loaded, offline);
        assert!(rig.cached(&series_key("", "TEST", "60", None)).is_none());
        assert!(
            rig.store
                .borrow()
                .get(&series_key("binance", "TEST", "60", None))
                .is_none()
        );
    }

    #[test]
    fn falls_back_to_a_full_load_when_the_inner_feed_has_no_load_range() {
        let rig = Rig::over(FakeFeed::new(600).without_range());
        let a = rig.load(&main_cfg());
        let b = rig.load(&main_cfg());
        assert_eq!((a.len(), b.len()), (500, 500)); // correct, just not tail-optimized
        assert_eq!(rig.feed.load_calls.get(), 2);
        // The ranged gateway answers from a full load too.
        let c = rig.range(&cfg("binance:ETHUSDT", "60", Some(100)), from(0));
        assert_eq!(c.len(), 100);
    }

    #[test]
    fn load_range_caches_a_secondary_series_and_refetches_only_the_tail() {
        let rig = Rig::new(600);
        let secondary = cfg("binance:ETHUSDT", "240", None);
        let earliest = rig.feed.expected(600)[0].time;
        rig.range(&secondary, from(earliest));
        assert!(
            rig.cached(&series_key("binance", "ETHUSDT", "240", None))
                .is_some()
        );
        let first_from = rig.feed.last_range_from.get().unwrap();
        rig.range(&secondary, from(earliest));
        // The second call fetched only the tail (from the last cached bar).
        assert!(rig.feed.last_range_from.get().unwrap() > first_from);
    }

    #[test]
    fn load_range_caches_even_when_the_fetch_start_precedes_the_first_bar() {
        let rig = Rig::new(600);
        let secondary = cfg("binance:ETHUSDT", "240", None);
        // `from` earlier than any available bar: the cached first bar is AFTER
        // it, so an "oldest.time <= from" check could never prove coverage.
        let buffered = rig.feed.expected(600)[0].time - 5_000_000;
        rig.range(&secondary, from(buffered));
        let first_from = rig.feed.last_range_from.get().unwrap();
        rig.range(&secondary, from(buffered));
        assert!(rig.feed.last_range_from.get().unwrap() > first_from);
    }

    #[test]
    fn backward_chunks_walk_older_history_and_never_re_serve_the_cache() {
        let rig = Rig::new(3000);
        rig.load(&main_cfg()); // caches ~499 recent bars
        let oldest = rig.load(&main_cfg())[0].time;

        // Chunk 1: strictly older window ending AT the current oldest (overlap by one).
        let c1 = rig.range(&main_cfg(), window(oldest, 100));
        assert_eq!(c1.len(), 100);
        assert_eq!(c1.last().unwrap().time, oldest); // overlap bar included
        assert!(c1[0].time < oldest); // genuinely older data: the loop progresses
        let after_c1 = rig.feed.range_calls.get();

        // Chunk 2 walks further back from chunk 1's oldest bar.
        let c2 = rig.range(&main_cfg(), window(c1[0].time, 100));
        assert!(c2[0].time < c1[0].time);
        assert_eq!(rig.feed.range_calls.get(), after_c1 + 1);

        // Re-requesting chunk 1's window is now served from cache: no new fetch.
        let again = rig.range(&main_cfg(), window(oldest, 100));
        assert_eq!(times(&again), times(&c1));
        assert_eq!(rig.feed.range_calls.get(), after_c1 + 1);
    }

    #[test]
    fn a_backward_chunk_at_genesis_returns_empty() {
        let rig = Rig::new(300);
        rig.load(&main_cfg());
        let first = rig.feed.expected(300)[0].time; // the very first bar that exists
        let partial = rig.range(&main_cfg(), window(first, 100));
        assert_eq!(partial.len(), 1); // only the boundary bar: a partial chunk
        let empty = rig.range(&main_cfg(), window(first - HOUR, 100));
        assert!(empty.is_empty()); // strictly before genesis
    }

    #[test]
    fn a_from_request_older_than_coverage_fetches_only_the_missing_head() {
        let rig = Rig::new(600);
        let secondary = cfg("binance:ETHUSDT", "60", None);
        let all = rig.feed.expected(600);
        let (mid, earlier) = (all[300].time, all[100].time);

        rig.range(&secondary, from(mid)); // coverage recorded at mid
        rig.feed.ranges.borrow_mut().clear();

        let r = rig.range(&secondary, from(earlier));
        // Exactly one head fetch [earlier, mid] + one tail refresh: never a full re-fetch.
        let ranges = rig.feed.ranges.borrow();
        assert_eq!((ranges[0].from, ranges[0].to), (Some(earlier), Some(mid)));
        assert_eq!(ranges.len(), 2);
        assert_eq!(r[0].time, earlier); // the served window spans the full request
        let expected: Vec<i64> = all
            .iter()
            .filter(|b| b.time >= earlier)
            .map(|b| b.time)
            .collect();
        assert_eq!(times(&r), expected);
    }

    #[test]
    fn a_covered_window_is_clipped_at_to() {
        let rig = Rig::new(600);
        let secondary = cfg("binance:ETHUSDT", "60", None);
        let all = rig.feed.expected(600);
        let (start, to) = (all[0].time, all[599].time); // `to` is the current tip
        rig.range(&secondary, from(start));
        rig.feed.advance(5); // newer bars exist beyond `to` now
        let r = rig.range(
            &secondary,
            BarRange {
                from: Some(start),
                to: Some(to),
                ..BarRange::default()
            },
        );
        assert!(r.last().unwrap().time <= to);
    }

    #[test]
    fn a_giant_count_bounded_miss_pages_backward_with_explicit_limits() {
        let rig = Rig::new(30_000);
        let tip = rig.feed.expected(1)[0].time;
        rig.feed.ranges.borrow_mut().clear();
        let r = rig.range(&main_cfg(), window(tip, 25_000));
        assert_eq!(r.len(), 25_000);
        let t = times(&r);
        assert!(t.windows(2).all(|w| w[0] < w[1])); // ascending, deduped merge
        // Three bounded pages instead of one 25k request; every page carries an
        // explicit limit.
        assert_eq!(
            rig.feed.limits(),
            vec![Some(10_000), Some(10_000), Some(5_000)]
        );
        assert!(rig.feed.ranges.borrow().iter().all(|r| r.to.is_some()));
    }

    #[test]
    fn a_paged_walk_that_runs_out_of_history_marks_only_what_it_proved() {
        let rig = Rig::new(5_000); // less exists than requested
        let tip = rig.feed.expected(1)[0].time;
        let oldest = rig.feed.expected(5_000)[0].time;
        let r = rig.range(&main_cfg(), window(tip, 20_000));
        assert_eq!(r.len(), 5_000); // everything that exists
        // Coverage claims only down to the oldest bar actually seen.
        assert_eq!(rig.store.borrow().covered_from_of(&key()), Some(oldest));
    }

    /// A lux-server-shaped feed: `{to, limit}` means "the WINDOW of `limit`
    /// bar-slots ending at `to`": an empty window mid-history is a quiet gap,
    /// not genesis.
    struct WindowedFeed {
        all: Vec<Ohlcv>,
        ranges: RefCell<Vec<BarRange>>,
    }

    impl MarketDataFeed for WindowedFeed {
        fn supports_load_range(&self) -> bool {
            true
        }

        fn load(
            &self,
            _cfg: &MarketConfig,
        ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
            let out = self.all[self.all.len().saturating_sub(500)..].to_vec();
            Box::pin(async move { Ok(out) })
        }

        fn subscribe(&self, _cfg: &MarketConfig, _on_bar: BarSink) -> Unsubscribe {
            Box::new(|| {})
        }

        fn load_range(
            &self,
            _cfg: &MarketConfig,
            range: &BarRange,
        ) -> Option<LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>>> {
            self.ranges.borrow_mut().push(range.clone());
            let to = range.to.unwrap_or(i64::MAX);
            let from = match range.limit {
                Some(limit) => to.saturating_sub(limit as i64 * HOUR),
                None => range.from.unwrap_or(i64::MIN),
            };
            let out: Vec<Ohlcv> = self
                .all
                .iter()
                .filter(|b| b.time >= from && b.time <= to)
                .copied()
                .collect();
            Some(Box::pin(async move { Ok(out) }))
        }
    }

    #[test]
    fn a_from_bounded_walk_steps_across_an_empty_time_window() {
        let mk = |i: i64| Ohlcv::new(T0 + i * HOUR, 1.0, 2.0, 0.0, 1.0, Some(1.0));
        // Two clusters separated by a more-than-one-page hole: 0..99 and 20_000..20_099.
        let all: Vec<Ohlcv> = (0..100).map(mk).chain((20_000..20_100).map(mk)).collect();
        let feed = Rc::new(WindowedFeed {
            all: all.clone(),
            ranges: RefCell::default(),
        });
        let platform = ManualPlatform::new((T0 + 30_000 * HOUR) as f64);
        let cache = CachingDataFeed::new(
            feed.clone(),
            Rc::new(RefCell::new(BarStore::new())),
            platform.handle(),
        );
        let range = BarRange {
            from: Some(all[0].time),
            to: Some(all[all.len() - 1].time),
            ..BarRange::default()
        };
        let r = platform
            .block_on(cache.load_range(&main_cfg(), &range).unwrap())
            .unwrap();
        // BOTH clusters arrive: the walk stepped through the empty middle
        // window(s) instead of treating them as genesis.
        assert_eq!(r.len(), 200);
        assert_eq!(r[0].time, all[0].time);
        assert!(feed.ranges.borrow().len() > 2); // paged, incl. at least one empty-window step
    }

    #[test]
    fn every_outgoing_ranged_request_carries_an_explicit_limit() {
        let rig = Rig::new(600);
        rig.load(&main_cfg()); // cold
        rig.feed.advance(3);
        rig.load(&main_cfg()); // tail refresh
        let earliest = rig.feed.expected(600)[0].time;
        rig.range(&cfg("binance:ETHUSDT", "60", None), from(earliest)); // secondary miss
        assert!(!rig.feed.ranges.borrow().is_empty());
        assert!(rig.feed.ranges.borrow().iter().all(|r| r.limit.is_some()));
    }

    #[test]
    fn a_counted_month_secondary_series_is_sized_in_months() {
        const QUARTER: i64 = 90 * 86_400_000;
        let first = 1_100_000_000_000;
        let rig = Rig::over(FakeFeed::with(80, first, QUARTER)); // 20 years of quarterly bars
        let r = rig.range(
            &cfg("binance:GDP", "3M", None),
            BarRange {
                from: Some(first),
                to: Some(first + 79 * QUARTER),
                ..BarRange::default()
            },
        );
        assert_eq!(r.len(), 80);
        // Read as 3 MINUTES, the 20-year span looked like millions of bars and paged.
        assert_eq!(rig.feed.ranges.borrow().len(), 1);
        assert!(rig.feed.ranges.borrow()[0].limit.unwrap() < 200);
    }

    #[test]
    fn secondary_series_survive_a_same_symbol_re_run_and_drop_on_symbol_change() {
        let rig = Rig::new(600);
        let secondary = cfg("binance:ETHUSDT", "240", None);
        let sec_key = series_key("binance", "ETHUSDT", "240", None);
        let earliest = rig.feed.expected(600)[0].time;

        rig.load(&main_cfg()); // chart symbol = BTC
        rig.range(&secondary, from(earliest)); // cache an ETH secondary
        assert!(rig.cached(&sec_key).is_some());
        rig.load(&main_cfg()); // re-run, same chart symbol
        assert!(rig.cached(&sec_key).is_some());
        rig.load(&cfg("SOLUSDT", "60", Some(500))); // chart symbol changes
        assert!(rig.cached(&sec_key).is_none());
    }

    #[test]
    fn the_two_sessions_never_share_cached_bars() {
        let rig = Rig::new(600);
        let regular = rig.load(&main_cfg());
        // Extended is a COLD series of its own: a fresh full load.
        let after_regular = rig.feed.load_calls.get();
        let extended = rig.load(&MarketConfig {
            session: Some(MarketSession::Extended),
            ..main_cfg()
        });
        assert_eq!(rig.feed.load_calls.get(), after_regular + 1);
        assert_eq!(times(&extended), times(&regular)); // same fake universe...
        assert!(rig.cached(&key()).is_some());
        assert!(
            rig.cached(&series_key("binance", "BTCUSDT", "60", Some("extended")))
                .is_some()
        ); // ...two series
    }

    // ---- bar-store-retention.test.ts: the CachingDataFeed cases ----

    #[test]
    fn without_retention_alternating_loads_re_download_in_full() {
        let rig = Rig::new(600);
        rig.load(&cfg("binance:BTCUSDT", "60", Some(500)));
        rig.load(&cfg("binance:ETHUSDT", "60", Some(500)));
        rig.load(&cfg("binance:BTCUSDT", "60", Some(500))); // BTC was purged by the ETH load
        assert_eq!(rig.feed.load_calls.get(), 3);
    }

    #[test]
    fn with_both_symbols_retained_the_second_load_of_each_serves_from_cache() {
        let rig = Rig::new(600);
        rig.store
            .borrow_mut()
            .retain(["BTCUSDT", "ETHUSDT"], DEFAULT_RETAIN_OWNER);
        rig.load(&cfg("binance:BTCUSDT", "60", Some(500)));
        rig.load(&cfg("binance:ETHUSDT", "60", Some(500)));
        let again = rig.load(&cfg("binance:BTCUSDT", "60", Some(500)));
        assert_eq!(rig.feed.load_calls.get(), 2); // no third full download
        assert_eq!(rig.feed.range_calls.get(), 1); // just the uncached tail
        assert_eq!(again.len(), 500); // the served window is still complete
    }

    // ---- additions ----

    #[test]
    fn a_refused_series_is_never_cached_and_the_next_ask_reaches_the_feed_again() {
        let rig = Rig::new(600);
        let refusal = DataError::from(DataUnavailableError::new(
            DataUnavailableDetails::new("timeframe".into()),
            None,
        ));
        *rig.feed.range_error.borrow_mut() = Some(refusal.clone());
        let secondary = cfg("binance:GDP", "60", None);
        let range = from(T0);
        assert_eq!(
            rig.range_result(&secondary, range.clone()),
            Err(refusal.clone())
        );
        let k = series_key("binance", "GDP", "60", None);
        assert!(rig.cached(&k).is_none());
        assert_eq!(rig.store.borrow().covered_from_of(&k), None);
        assert_eq!(rig.range_result(&secondary, range), Err(refusal));
        assert_eq!(rig.feed.range_calls.get(), 2);
    }

    #[test]
    fn a_failed_tail_refresh_leaves_the_cache_as_it_was() {
        let rig = Rig::new(600);
        rig.load(&main_cfg());
        let before = rig.cached(&key()).unwrap();
        *rig.feed.range_error.borrow_mut() = Some(DataError::other("network down"));
        let result = rig.platform.block_on(rig.cache.load(&main_cfg()));
        assert_eq!(result, Err(DataError::other("network down")));
        assert_eq!(rig.cached(&key()).unwrap(), before);
    }

    #[test]
    fn progressive_cold_load_forwards_batches_and_caches_the_final_answer() {
        let rig = Rig::over(FakeFeed::new(600).with_progressive());
        let batches = Rc::new(RefCell::new(Vec::new()));
        let sink = batches.clone();
        let load = rig
            .cache
            .load_progressive(
                &main_cfg(),
                Box::new(move |b| sink.borrow_mut().push(b.len())),
                None,
            )
            .unwrap();
        let bars = rig.platform.block_on(load).unwrap().unwrap();
        assert_eq!(bars.len(), 500);
        assert_eq!(*batches.borrow(), vec![250]);
        assert_eq!(rig.cached(&key()).unwrap().len(), 499); // forming bar dropped, as in `load`
        assert_eq!(rig.feed.progressive_calls.get(), 1);
    }

    #[test]
    fn progressive_warm_load_answers_through_the_tail_path_without_streaming() {
        let rig = Rig::over(FakeFeed::new(600).with_progressive());
        rig.load(&main_cfg());
        let load = rig
            .cache
            .load_progressive(&main_cfg(), Box::new(|_| {}), None)
            .unwrap();
        let bars = rig.platform.block_on(load).unwrap().unwrap();
        assert_eq!(bars.len(), 500);
        assert_eq!(rig.feed.progressive_calls.get(), 0);
        assert_eq!(rig.feed.range_calls.get(), 1);
    }

    #[test]
    fn progressive_resolves_to_none_when_the_inner_feed_cannot_stream() {
        let rig = Rig::new(600);
        let load = rig
            .cache
            .load_progressive(&main_cfg(), Box::new(|_| {}), None)
            .unwrap();
        assert_eq!(rig.platform.block_on(load), Ok(None));
        assert!(rig.cached(&key()).is_none());
    }

    #[test]
    fn progressive_offline_data_goes_straight_to_the_inner_load() {
        let rig = Rig::over(FakeFeed::new(10).with_progressive());
        let offline = rig.feed.expected(4);
        let load = rig
            .cache
            .load_progressive(
                &MarketConfig {
                    data: Some(offline.clone()),
                    ..MarketConfig::default()
                },
                Box::new(|_| {}),
                None,
            )
            .unwrap();
        assert_eq!(rig.platform.block_on(load), Ok(Some(offline)));
        assert_eq!(rig.feed.progressive_calls.get(), 0);
    }

    #[test]
    fn a_zero_count_keeps_the_whole_window() {
        let bars: Vec<Ohlcv> = (0..5).map(|i| bar_at(T0, HOUR, i)).collect();
        assert_eq!(last_n(bars.clone(), 0).len(), 5); // `slice(-0)` is `slice(0)`
        assert_eq!(last_n(bars.clone(), 3).len(), 3);
        assert_eq!(last_n(bars, 9).len(), 5);
    }

    #[test]
    fn helpers_drop_and_reattach_the_forming_bar() {
        let bars: Vec<Ohlcv> = (0..3).map(|i| bar_at(T0, HOUR, i)).collect();
        assert_eq!(drop_forming(&bars).len(), 2);
        assert!(drop_forming(&[]).is_empty());
        let closed = bars[..2].to_vec();
        assert_eq!(attach_forming(closed.clone(), Some(bars[2])).len(), 3);
        assert_eq!(attach_forming(closed.clone(), Some(bars[1])).len(), 2); // not newer
        assert_eq!(attach_forming(closed, None).len(), 2);
        assert_eq!(attach_forming(Vec::new(), Some(bars[0])).len(), 1);
    }

    #[test]
    fn the_cache_key_follows_the_symbol_grammar() {
        assert_eq!(
            cache_key("binance:BTCUSDT", "60", None),
            "binance|BTCUSDT|60"
        );
        assert_eq!(cache_key("BTCUSDT", "60", None), "|BTCUSDT|60");
        assert_eq!(
            cache_key("Binance.US:BTCUSDT.P", "D", Some("extended")),
            "binance.us|BTCUSDT.P|D|extended"
        );
    }
}
