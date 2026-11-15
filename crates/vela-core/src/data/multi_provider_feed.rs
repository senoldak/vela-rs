//! The default [`MarketDataFeed`]: a router over a registry of named providers,
//! with a cache in front.
// `DataError` is large (it carries a `DataUnavailableError`) and the port fixes it as the
// error type of every bar future; boxing it is a change for `runtime.rs`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use super::BarStore;
use super::caching_feed::CachingDataFeed;
use super::provider_registry::{ProviderRegistry, ResolveOptions, Resolved, eager, parse_symbol};
use crate::data_unavailable::{
    DataUnavailableDetails, DataUnavailableError, data_unavailable_message,
};
use crate::model::Ohlcv;
use crate::ports::data_provider::{
    DataProvider, ProviderCapabilities, ProviderInfo, SymbolDescriptor,
};
use crate::ports::market_data::{
    BarRange, BarSink, BatchSink, MarketConfig, MarketDataFeed, SymbolInfo, UnresolvedSymbol,
};
use crate::runtime::{
    AbortSignal, DataError, LocalBoxFuture, OneshotSender, Platform, TimerId, Unsubscribe,
    js_await, noop_unsubscribe, oneshot,
};

/// How long a symbol-metadata answer stays fresh (see [`MultiProviderFeed::symbol_info_for`]).
const SYMBOL_INFO_TTL_MS: f64 = 10.0 * 60_000.0;

/// Cadence of the poll that stands in for a stream on a provider without `subscribe`.
const POLL_MS: f64 = 3_000.0;

type InfoResult = Result<Option<SymbolInfo>, DataError>;

/// Waiters sharing one in-flight metadata request.
#[derive(Default)]
struct Inflight {
    waiters: RefCell<Vec<OneshotSender<InfoResult>>>,
}

struct Inner {
    platform: Rc<dyn Platform>,
    registry: ProviderRegistry,
    cache: CachingDataFeed,
    /// The chart symbol's resolved provider: the default for bare secondary symbols.
    primary_provider: RefCell<Option<String>>,
    /// Sync-accessible symbol metadata, warmed by `load` / `symbol_info_for`
    /// (the engine reads it synchronously).
    sym_info_cache: RefCell<HashMap<String, (SymbolInfo, f64)>>,
    /// Requests in flight, keyed like the cache: every concurrent asker shares one round trip.
    sym_info_inflight: RefCell<HashMap<String, Rc<Inflight>>>,
}

/// The default `MarketDataFeed`: a router over a registry of named
/// [`DataProvider`]s. It parses every symbol (`name:SYMBOL` / bare `SYMBOL`),
/// resolves a provider, rewrites the config to that provider's **canonical**
/// identity, and delegates to an internal [`CachingDataFeed`], so `BTCUSDT` and
/// `BINANCE:BTCUSDT` collapse to one cache entry. Offline `data` bypasses the
/// registry entirely.
///
/// Providers are registered at runtime. Until one that resolves the chart symbol
/// is registered, `load` **parks** (its future stays pending) rather than
/// fetching, so registration drives the first load.
///
/// A cheap handle: clones share one feed.
#[derive(Clone)]
pub struct MultiProviderFeed {
    inner: Rc<Inner>,
}

impl MultiProviderFeed {
    /// `store` is the bar cache this feed shares with whoever else caches on it.
    pub fn new(store: Rc<RefCell<BarStore>>, platform: Rc<dyn Platform>) -> Self {
        let registry = ProviderRegistry::new(platform.clone());
        let fetch = RegistryFetchFeed {
            registry: registry.clone(),
            platform: platform.clone(),
        };
        let cache = CachingDataFeed::new(Rc::new(fetch), store, platform.clone());
        Self {
            inner: Rc::new(Inner {
                platform,
                registry,
                cache,
                primary_provider: RefCell::new(None),
                sym_info_cache: RefCell::default(),
                sym_info_inflight: RefCell::default(),
            }),
        }
    }

    // ── Registry surface (driven by chart.data / DataControl) ──────────────

    /// Register (or replace) a provider; the future settles when its index is built.
    pub fn register_provider(
        &self,
        name: &str,
        provider: Rc<dyn DataProvider>,
    ) -> LocalBoxFuture<'static, ()> {
        self.inner.registry.register(name, provider)
    }

    pub fn unregister_provider(&self, name: &str) {
        self.inner.registry.unregister(name);
    }

    pub fn providers(&self) -> Vec<ProviderInfo> {
        let registry = &self.inner.registry;
        registry
            .names()
            .iter()
            .map(|n| registry.info_of(n))
            .collect()
    }

    pub fn resolve_symbol(&self, raw: &str) -> Option<Resolved> {
        let primary = self.primary();
        self.inner.registry.resolve(
            raw,
            ResolveOptions {
                default: primary.as_deref(),
                lenient: false,
            },
        )
    }

    /// The DISPLAY prefix for `raw`: the descriptor's LISTING prefix when the data
    /// declares one (`NASDAQ` for AAPL), else the resolved provider name. `None`
    /// while nothing resolves the symbol.
    pub fn display_prefix(&self, raw: &str) -> Option<String> {
        let resolved = self.resolve_symbol(raw)?;
        Some(self.inner.registry.display_prefix_of(&resolved))
    }

    /// The canonical `PREFIX:TICKER` form of `raw`, or `None` while unresolvable.
    pub fn canonical_symbol(&self, raw: &str) -> Option<String> {
        let resolved = self.resolve_symbol(raw)?;
        Some(self.inner.registry.canonical_symbol(&resolved))
    }

    /// The registered provider INSTANCE under `name` (`None` if unknown).
    pub fn provider_instance(&self, name: &str) -> Option<Rc<dyn DataProvider>> {
        self.inner.registry.get(name)
    }

    /// The icon URL for a DESCRIPTOR: its owning provider's `resolve_symbol_icon` (picker rows).
    pub fn symbol_icon_of(&self, d: &SymbolDescriptor) -> Option<String> {
        self.inner.registry.symbol_icon_of(d)
    }

    /// The icon URL for a raw SYMBOL string: resolve, then route (statusline, object tree).
    pub fn symbol_icon(&self, raw: &str) -> Option<String> {
        self.inner
            .registry
            .symbol_icon(self.resolve_symbol(raw).as_ref())
    }

    pub fn symbols(&self, name: Option<&str>) -> Vec<SymbolDescriptor> {
        self.inner.registry.symbols_of(name)
    }

    /// Resolves when every registered provider's eager index has settled.
    pub fn ready(&self) -> LocalBoxFuture<'static, ()> {
        self.inner.registry.index_ready()
    }

    /// Async per-symbol metadata (the facade's `chart.data.symbolInfo`); also
    /// warms the sync cache. Resolves to `Ok(None)` when nothing resolves the
    /// symbol or the provider has no metadata; a failed probe is an error.
    pub fn symbol_info_for(&self, raw: &str) -> LocalBoxFuture<'static, InfoResult> {
        match self.resolve_symbol(raw) {
            Some(resolved) => self.fetch_symbol_info(&resolved),
            None => Box::pin(std::future::ready(Ok(None))),
        }
    }

    /// Per-symbol capabilities, resolved through the owning provider's
    /// `capabilities_for` refinement (an instrument may support more than its
    /// provider-wide baseline), falling back to the provider-wide `info()`
    /// capabilities. `None` when nothing resolves the symbol.
    pub fn capabilities_for(&self, raw: &str) -> Option<ProviderCapabilities> {
        let resolved = self.resolve_symbol(raw)?;
        let provider = self.inner.registry.get(&resolved.provider)?;
        provider
            .capabilities_for(&resolved.ticker)
            .or_else(|| Some(self.inner.registry.info_of(&resolved.provider).capabilities))
    }

    fn primary(&self) -> Option<String> {
        self.inner.primary_provider.borrow().clone()
    }

    /// One round trip per symbol, shared. A market switch probes this metadata
    /// from five places at once (the bar load's prefetch, the renderer's tick
    /// size, the market-status badge, the session shading, the session toggle):
    /// without the in-flight map each would open its own request for the same
    /// answer, and the last one to land decided how late the chrome settled.
    ///
    /// Cached for [`SYMBOL_INFO_TTL_MS`]: this metadata is stable within a
    /// session (tick size, session vocabulary, listing prefix), but not
    /// immutable (a continuous futures row rolls its current contract), so the
    /// entry expires rather than pinning the session's first answer forever. A
    /// failed probe is NOT remembered: the next asker retries.
    fn fetch_symbol_info(&self, resolved: &Resolved) -> LocalBoxFuture<'static, InfoResult> {
        let inner = &self.inner;
        let key = sym_key(resolved);
        let now = inner.platform.now_ms();
        if let Some((info, at)) = inner.sym_info_cache.borrow().get(&key)
            && now - at < SYMBOL_INFO_TTL_MS
        {
            return Box::pin(std::future::ready(Ok(Some(info.clone()))));
        }
        let shared = inner.sym_info_inflight.borrow().get(&key).cloned();
        if let Some(inflight) = shared {
            let (tx, rx) = oneshot();
            inflight.waiters.borrow_mut().push(tx);
            return Box::pin(rx);
        }
        let probe = inner
            .registry
            .get(&resolved.provider)
            .and_then(|provider| provider.get_symbol_info(&resolved.ticker));
        let Some(probe) = probe else {
            return Box::pin(std::future::ready(Ok(None)));
        };
        let inflight = Rc::new(Inflight::default());
        let (tx, rx) = oneshot();
        inflight.waiters.borrow_mut().push(tx);
        inner
            .sym_info_inflight
            .borrow_mut()
            .insert(key.clone(), inflight.clone());
        let this = self.inner.clone();
        inner.platform.spawn(Box::pin(async move {
            let result = probe.await;
            if let Ok(Some(info)) = &result {
                let at = this.platform.now_ms();
                this.sym_info_cache
                    .borrow_mut()
                    .insert(key.clone(), (info.clone(), at));
            }
            this.sym_info_inflight.borrow_mut().remove(&key);
            let waiters = std::mem::take(&mut *inflight.waiters.borrow_mut());
            for waiter in waiters {
                waiter.send(result.clone());
            }
        }));
        Box::pin(rx)
    }

    /// Warm symbol metadata for the engine. Fire-and-forget: one small request
    /// that typically lands before the (heavier, paginated) bar load finishes.
    fn prefetch_symbol_info(&self, resolved: &Resolved) {
        drop(self.fetch_symbol_info(resolved));
    }

    /// The chart's provider FOLLOWS its market: after a switch to another venue,
    /// bare secondary symbols and the metadata/capability probes must default to
    /// the venue now on screen: latching the first one made every later probe
    /// answer for the wrong venue.
    fn follow(&self, resolved: &Resolved) {
        *self.inner.primary_provider.borrow_mut() = Some(resolved.provider.clone());
        self.prefetch_symbol_info(resolved);
    }

    /// Resolve `cfg`'s own symbol, with the chart provider as the bare default.
    fn resolve_cfg(&self, cfg: &MarketConfig) -> Option<Resolved> {
        self.resolve_symbol(raw_symbol(cfg))
    }
}

impl MarketDataFeed for MultiProviderFeed {
    fn supports_load_range(&self) -> bool {
        true
    }

    fn supports_load_progressive(&self) -> bool {
        true
    }

    fn load(&self, cfg: &MarketConfig) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
        if let Some(data) = offline_data(cfg) {
            return Box::pin(std::future::ready(Ok(data)));
        }
        let this = self.clone();
        let cfg = cfg.clone();
        let wait = self
            .inner
            .registry
            .when_resolvable(raw_symbol(&cfg), self.primary().as_deref());
        Box::pin(async move {
            let resolved = js_await(wait).await;
            this.follow(&resolved);
            js_await(this.inner.cache.load(&canonical(&cfg, &resolved))).await
        })
    }

    /// Progressive twin of `load`: same resolution, the cache streams the batches.
    fn load_progressive(
        &self,
        cfg: &MarketConfig,
        on_batch: BatchSink,
        signal: Option<AbortSignal>,
    ) -> Option<LocalBoxFuture<'static, Result<Option<Vec<Ohlcv>>, DataError>>> {
        if let Some(data) = offline_data(cfg) {
            return Some(Box::pin(std::future::ready(Ok(Some(data)))));
        }
        let this = self.clone();
        let cfg = cfg.clone();
        let wait = self
            .inner
            .registry
            .when_resolvable(raw_symbol(&cfg), self.primary().as_deref());
        Some(Box::pin(async move {
            let resolved = js_await(wait).await;
            this.follow(&resolved);
            match this
                .inner
                .cache
                .load_progressive(&canonical(&cfg, &resolved), on_batch, signal)
            {
                Some(load) => js_await(load).await,
                None => Ok(None),
            }
        }))
    }

    /// Synchronous per-symbol metadata for engines (Pine `syminfo.*`), served
    /// from the cache warmed by `load`. `None` until the prefetch lands (the
    /// engine then synthesizes a fallback) and real thereafter: the
    /// `MarketDataFeed` port is synchronous by contract.
    fn symbol_info(&self, cfg: &MarketConfig) -> Option<SymbolInfo> {
        if offline_data(cfg).is_some() {
            return None;
        }
        let resolved = self.resolve_cfg(cfg)?;
        // Whatever its age: this is the SYNC door (Pine `syminfo.*`, the price
        // scale's tick size), and the last known answer always beats none while
        // a refresh is in flight.
        self.inner
            .sym_info_cache
            .borrow()
            .get(&sym_key(&resolved))
            .map(|(info, _)| info.clone())
    }

    fn load_range(
        &self,
        cfg: &MarketConfig,
        range: &BarRange,
    ) -> Option<LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>>> {
        if offline_data(cfg).is_some() {
            return self.inner.cache.load_range(cfg, range);
        }
        // Secondary series (request.security): resolve its OWN prefix, with the
        // chart provider as the bare default. Unresolvable means empty (the
        // engine degrades).
        match self.resolve_cfg(cfg) {
            Some(resolved) => self
                .inner
                .cache
                .load_range(&canonical(cfg, &resolved), range),
            None => Some(Box::pin(std::future::ready(Ok(Vec::new())))),
        }
    }

    /// Offline `data` has no source of ticks: the caller's bars are shown
    /// exactly as given, and live updates need a registered provider that
    /// implements `subscribe`.
    fn subscribe(&self, cfg: &MarketConfig, on_bar: BarSink) -> Unsubscribe {
        if offline_data(cfg).is_some() {
            return noop_unsubscribe();
        }
        match self.resolve_cfg(cfg) {
            Some(resolved) => self
                .inner
                .cache
                .subscribe(&canonical(cfg, &resolved), on_bar),
            None => noop_unsubscribe(),
        }
    }

    /// Told when a symbol stays unservable by everything registered (the load is parked).
    fn on_unresolved(&self, callback: Box<dyn FnMut(&UnresolvedSymbol)>) -> Option<Unsubscribe> {
        Some(self.inner.registry.on_unresolved(callback))
    }

    /// Abandon parked waits: a destroyed chart must not keep listeners on the registry.
    fn destroy(&self) {
        self.inner.registry.cancel_waits();
    }
}

fn offline_data(cfg: &MarketConfig) -> Option<Vec<Ohlcv>> {
    cfg.data.as_ref().filter(|d| !d.is_empty()).cloned()
}

/// The raw symbol string the registry parses. Only an EXPLICIT `provider:`
/// prefix written into the symbol survives: that one is a hard requirement (the
/// caller named a venue, so the load parks until it registers). The `provider`
/// OPTION is merely the DEFAULT and travels separately, as the resolve default:
/// welding it in as a prefix made an unregistered default POISON every symbol,
/// and the chart could then never resolve anything, not even after the user
/// switched to a symbol a registered provider serves.
fn raw_symbol(cfg: &MarketConfig) -> &str {
    cfg.symbol.as_deref().unwrap_or("")
}

/// Rewrite the config to the canonical PREFIXED symbol (`provider:TICKER`): the
/// one symbol grammar carries the resolved identity through the cache and the
/// inner feed.
fn canonical(cfg: &MarketConfig, resolved: &Resolved) -> MarketConfig {
    MarketConfig {
        symbol: Some(format!("{}:{}", resolved.provider, resolved.ticker)),
        ..cfg.clone()
    }
}

/// Cache key of a resolved symbol. The TICKER is case-folded because `resolve`
/// keeps the CALLER's spelling (`btcusdt` and `BTCUSDT` both resolve, unchanged)
/// while the registry indexes them as one symbol: without the fold each spelling
/// would fetch its own copy.
fn sym_key(resolved: &Resolved) -> String {
    format!(
        "{}|{}",
        resolved.provider,
        resolved.ticker.trim().to_uppercase()
    )
}

fn timeframe_of(cfg: &MarketConfig) -> &str {
    cfg.timeframe.as_deref().unwrap_or("60")
}

/// A provider's refusal with the refused series filled in where it left them
/// out, so the message a script ends up showing names what was asked. A message
/// of the provider's own is kept as written.
fn refusal_of(e: DataUnavailableError, ticker: &str, timeframe: &str) -> DataUnavailableError {
    if e.symbol.is_some() && e.timeframe.is_some() {
        return e;
    }
    let own = DataUnavailableDetails {
        reason: e.reason.clone(),
        symbol: e.symbol.clone(),
        timeframe: e.timeframe.clone(),
        finest: e.finest.clone(),
        default: e.default.clone(),
    };
    let own_message = e.message() != data_unavailable_message(&own);
    let details = DataUnavailableDetails {
        symbol: Some(own.symbol.unwrap_or_else(|| ticker.to_owned())),
        timeframe: Some(own.timeframe.unwrap_or_else(|| timeframe.to_owned())),
        ..own
    };
    DataUnavailableError::new(details, own_message.then(|| e.message().to_owned()))
}

/// Call a provider's `get_bars` defensively: a failing provider yields an empty
/// result plus a warning, never a failed load. Keeps the initial fetch as
/// fault-tolerant as the live poll (which already swallows transient errors): a
/// bad symbol or network blip empties the chart instead of failing the parked
/// load. The one exception is a PERMANENT refusal ([`DataError::Unavailable`]):
/// it fails, so the chart can stop asking and a script's secondary fetch can
/// report it; read as empty, the series would be retried forever.
async fn safe_bars(
    platform: &dyn Platform,
    provider: Rc<dyn DataProvider>,
    ticker: String,
    timeframe: String,
    range: BarRange,
) -> Result<Vec<Ohlcv>, DataError> {
    match js_await(provider.get_bars(&ticker, &timeframe, &range)).await {
        Ok(bars) => Ok(bars),
        Err(DataError::Unavailable(e)) => Err(DataError::from(refusal_of(*e, &ticker, &timeframe))),
        Err(DataError::Other(message)) => {
            platform.warn(&format!(
                "[vela] data fetch failed for {ticker} {timeframe} \u{2014} {message}"
            ));
            Ok(Vec::new())
        }
    }
}

/// The cache's inner feed: given a CANONICAL config (provider name + ticker
/// already resolved), look the provider up and call its neutral `get_bars` /
/// `subscribe`. Live ticks use the provider's `subscribe` if it has one, else a
/// generic 3s poll.
struct RegistryFetchFeed {
    registry: ProviderRegistry,
    platform: Rc<dyn Platform>,
}

impl RegistryFetchFeed {
    /// The provider and ticker a canonical config names.
    fn target(&self, cfg: &MarketConfig) -> (Option<Rc<dyn DataProvider>>, String) {
        let parsed = parse_symbol(raw_symbol(cfg));
        let provider = self.registry.get(parsed.provider.as_deref().unwrap_or(""));
        (provider, parsed.ticker)
    }
}

impl MarketDataFeed for RegistryFetchFeed {
    fn supports_load_range(&self) -> bool {
        true
    }

    fn supports_load_progressive(&self) -> bool {
        true
    }

    fn load(&self, cfg: &MarketConfig) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
        let (provider, ticker) = self.target(cfg);
        let Some(provider) = provider else {
            return Box::pin(std::future::ready(Ok(Vec::new())));
        };
        let range = BarRange {
            limit: Some(cfg.bars.unwrap_or(500)),
            session: session_name(cfg),
            ..BarRange::default()
        };
        let platform = self.platform.clone();
        let timeframe = timeframe_of(cfg).to_owned();
        eager(Box::pin(async move {
            safe_bars(&*platform, provider, ticker, timeframe, range).await
        }))
    }

    fn load_progressive(
        &self,
        cfg: &MarketConfig,
        on_batch: BatchSink,
        signal: Option<AbortSignal>,
    ) -> Option<LocalBoxFuture<'static, Result<Option<Vec<Ohlcv>>, DataError>>> {
        let (provider, ticker) = self.target(cfg);
        let Some(provider) = provider else {
            return Some(Box::pin(std::future::ready(Ok(Some(Vec::new())))));
        };
        let timeframe = timeframe_of(cfg).to_owned();
        let range = BarRange {
            limit: Some(cfg.bars.unwrap_or(500)),
            session: session_name(cfg),
            ..BarRange::default()
        };
        // Incapable: the caller keeps its own paths.
        let Some(load) =
            provider.get_bars_progressive(&ticker, &timeframe, &range, on_batch, signal)
        else {
            return Some(Box::pin(std::future::ready(Ok(None))));
        };
        let platform = self.platform.clone();
        Some(eager(Box::pin(async move {
            match js_await(load).await {
                Ok(bars) => Ok(Some(bars)),
                // Same fault posture as `safe_bars`: a failing provider empties
                // the chart, never fails the load (batches already painted stay
                // painted). A permanent refusal fails, as there.
                Err(DataError::Unavailable(e)) => {
                    Err(DataError::from(refusal_of(*e, &ticker, &timeframe)))
                }
                Err(DataError::Other(message)) => {
                    platform.warn(&format!(
                        "[vela] progressive fetch failed for {ticker} {timeframe} \u{2014} {message}"
                    ));
                    Ok(Some(Vec::new()))
                }
            }
        })))
    }

    fn load_range(
        &self,
        cfg: &MarketConfig,
        range: &BarRange,
    ) -> Option<LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>>> {
        let (provider, ticker) = self.target(cfg);
        let Some(provider) = provider else {
            return Some(Box::pin(std::future::ready(Ok(Vec::new()))));
        };
        let range = BarRange {
            session: session_name(cfg),
            ..range.clone()
        };
        let platform = self.platform.clone();
        let timeframe = timeframe_of(cfg).to_owned();
        Some(eager(Box::pin(async move {
            safe_bars(&*platform, provider, ticker, timeframe, range).await
        })))
    }

    fn subscribe(&self, cfg: &MarketConfig, on_bar: BarSink) -> Unsubscribe {
        let (provider, ticker) = self.target(cfg);
        let Some(provider) = provider else {
            return noop_unsubscribe();
        };
        let timeframe = timeframe_of(cfg).to_owned();
        let session = session_name(cfg);
        let sink: Sink = Rc::new(RefCell::new(Some(on_bar)));
        let forward: BarSink = {
            let sink = sink.clone();
            Box::new(move |bar| deliver(&sink, bar))
        };
        if let Some(unsubscribe) =
            provider.subscribe(&ticker, &timeframe, forward, session.as_deref())
        {
            return unsubscribe;
        }
        let poller = Rc::new(Poller {
            platform: self.platform.clone(),
            provider,
            ticker,
            timeframe,
            session,
            sink,
            stopped: Cell::new(false),
            timer: Cell::new(None),
        });
        poller.schedule();
        Box::new(move || poller.stop())
    }
}

fn session_name(cfg: &MarketConfig) -> Option<String> {
    cfg.session.map(|s| s.as_str().to_owned())
}

type Sink = Rc<RefCell<Option<BarSink>>>;

/// Hand a bar to the sink without holding a borrow while its callback runs.
fn deliver(sink: &Sink, bar: Ohlcv) {
    let taken = sink.borrow_mut().take();
    if let Some(mut on_bar) = taken {
        on_bar(bar);
        let mut slot = sink.borrow_mut();
        if slot.is_none() {
            *slot = Some(on_bar);
        }
    }
}

/// The generic live feed of a provider that cannot stream: poll its last two bars.
struct Poller {
    platform: Rc<dyn Platform>,
    provider: Rc<dyn DataProvider>,
    ticker: String,
    timeframe: String,
    session: Option<String>,
    sink: Sink,
    stopped: Cell<bool>,
    timer: Cell<Option<TimerId>>,
}

impl Poller {
    fn schedule(self: &Rc<Self>) {
        let poller = self.clone();
        let id = self
            .platform
            .set_timeout(POLL_MS, Box::new(move || poller.start_poll()));
        self.timer.set(Some(id));
    }

    fn start_poll(self: &Rc<Self>) {
        let poller = self.clone();
        self.platform
            .spawn(eager(Box::pin(async move { poller.poll().await })));
    }

    async fn poll(self: Rc<Self>) {
        if self.stopped.get() {
            return;
        }
        // Last two bars: the (possibly just-closed) previous bar and the forming
        // one. The consumer dedupes by time, so the older one is harmless.
        let range = BarRange {
            limit: Some(2),
            session: self.session.clone(),
            ..BarRange::default()
        };
        let result = js_await(
            self.provider
                .get_bars(&self.ticker, &self.timeframe, &range),
        )
        .await;
        // Re-check AFTER the await: an unsubscribe during the fetch (a market
        // switch) must not push the OLD market's bars into the new series. On
        // the same timeframe the forming bar shares its open time, so a stale
        // bar would silently REPLACE the new market's forming candle.
        if self.stopped.get() {
            return;
        }
        match result {
            Ok(bars) => {
                for bar in bars {
                    deliver(&self.sink, bar);
                }
            }
            // A permanent refusal ends the poll: asking again every few seconds
            // can only be refused again. Anything else is transient: keep polling.
            Err(DataError::Unavailable(e)) => {
                self.platform.warn(&format!(
                    "[vela] live updates stopped \u{2014} {}",
                    e.message()
                ));
                self.stopped.set(true);
                return;
            }
            Err(DataError::Other(_)) => {}
        }
        if !self.stopped.get() {
            self.schedule();
        }
    }

    fn stop(&self) {
        self.stopped.set(true);
        if let Some(id) = self.timer.take() {
            self.platform.clear_timeout(id);
        }
    }
}

#[cfg(test)]
#[allow(clippy::let_underscore_future)] // registering needs no awaiting: the index builds on the platform
mod tests {
    use serde_json::json;

    use super::*;
    use crate::data::provider_registry::test_support::{FakeProvider, START, make_bars};
    use crate::data::series_key;
    use crate::data_unavailable::DataUnavailableReason;
    use crate::ports::market_data::MarketSession;
    use crate::runtime::testing::ManualPlatform;
    use crate::runtime::{OneshotReceiver, oneshot};

    const DAY: i64 = 86_400_000;

    struct Rig {
        platform: ManualPlatform,
        store: Rc<RefCell<BarStore>>,
        feed: MultiProviderFeed,
    }

    fn rig() -> Rig {
        let platform = ManualPlatform::new((START + 100 * 3_600_000) as f64);
        let store = Rc::new(RefCell::new(BarStore::new()));
        let feed = MultiProviderFeed::new(store.clone(), platform.handle());
        Rig {
            platform,
            store,
            feed,
        }
    }

    fn cfg(symbol: &str, timeframe: &str, bars: usize) -> MarketConfig {
        MarketConfig {
            symbol: Some(symbol.into()),
            timeframe: Some(timeframe.into()),
            bars: Some(bars),
            ..MarketConfig::default()
        }
    }

    fn secondary(symbol: &str, timeframe: &str) -> MarketConfig {
        MarketConfig {
            symbol: Some(symbol.into()),
            timeframe: Some(timeframe.into()),
            ..MarketConfig::default()
        }
    }

    fn limit(n: usize) -> BarRange {
        BarRange {
            limit: Some(n),
            ..BarRange::default()
        }
    }

    fn from(from: i64) -> BarRange {
        BarRange {
            from: Some(from),
            ..BarRange::default()
        }
    }

    type Slot<T> = Rc<RefCell<Option<T>>>;

    /// Run a future in the background and keep its result.
    fn track<T: 'static>(rig: &Rig, future: LocalBoxFuture<'static, T>) -> Slot<T> {
        let slot: Slot<T> = Rc::default();
        let out = slot.clone();
        rig.platform.spawn(Box::pin(async move {
            *out.borrow_mut() = Some(future.await);
        }));
        slot
    }

    impl Rig {
        fn register(&self, name: &str, provider: Rc<dyn DataProvider>) {
            self.platform
                .block_on(self.feed.register_provider(name, provider));
        }

        fn load(&self, cfg: &MarketConfig) -> Result<Vec<Ohlcv>, DataError> {
            self.platform.block_on(self.feed.load(cfg))
        }

        fn range(&self, cfg: &MarketConfig, range: BarRange) -> Result<Vec<Ohlcv>, DataError> {
            self.platform
                .block_on(self.feed.load_range(cfg, &range).unwrap())
        }

        fn resolve(&self, raw: &str) -> Option<(String, String)> {
            self.feed
                .resolve_symbol(raw)
                .map(|r| (r.provider, r.ticker))
        }
    }

    fn pair(provider: &str, ticker: &str) -> Option<(String, String)> {
        Some((provider.to_owned(), ticker.to_owned()))
    }

    fn up(s: &str) -> String {
        s.to_uppercase()
    }

    /// A provider whose enumeration the test releases by hand (or never).
    struct GatedList {
        rx: RefCell<Option<OneshotReceiver<Vec<SymbolDescriptor>>>>,
    }

    impl GatedList {
        fn new() -> (
            Rc<Self>,
            crate::runtime::OneshotSender<Vec<SymbolDescriptor>>,
        ) {
            let (tx, rx) = oneshot();
            (
                Rc::new(Self {
                    rx: RefCell::new(Some(rx)),
                }),
                tx,
            )
        }
    }

    impl DataProvider for GatedList {
        fn get_bars(
            &self,
            _ticker: &str,
            _timeframe: &str,
            range: &BarRange,
        ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
            let out = make_bars(range.limit.unwrap_or(6));
            Box::pin(async move { Ok(out) })
        }

        fn list_symbols(
            &self,
        ) -> Option<LocalBoxFuture<'static, Result<Vec<SymbolDescriptor>, DataError>>> {
            let rx = self.rx.borrow_mut().take()?;
            Some(Box::pin(async move { Ok(rx.await) }))
        }
    }

    // ---- multi-provider-feed.test.ts: parked load lifecycle ----

    #[test]
    fn parks_load_until_a_provider_that_resolves_the_symbol_is_registered() {
        let rig = rig();
        let parked = track(&rig, rig.feed.load(&cfg("BTCUSDT", "60", 10)));
        rig.platform.run();
        assert!(parked.borrow().is_none()); // no provider yet: parked

        let _ = rig
            .feed
            .register_provider("binance", FakeProvider::new(&["BTCUSDT"]).rc());
        rig.platform.run();
        assert_eq!(
            parked.borrow().as_ref().unwrap().as_ref().unwrap().len(),
            10
        );
    }

    #[test]
    fn a_bare_symbol_renders_via_the_first_supporting_provider() {
        let rig = rig();
        let binance = FakeProvider::new(&["BTCUSDT", "ETHUSDT"]).rc(); // no AAPL
        let fmp = FakeProvider::new(&["AAPL", "MSFT"]).rc(); // has AAPL
        let parked = track(&rig, rig.feed.load(&cfg("AAPL", "60", 5)));
        let _ = rig.feed.register_provider("binance", binance.clone());
        let _ = rig.feed.register_provider("fmp", fmp.clone());
        rig.platform.run();
        assert_eq!(parked.borrow().as_ref().unwrap().as_ref().unwrap().len(), 5);
        assert!(fmp.tickers().iter().any(|t| up(t) == "AAPL")); // fetched from FMP
        assert!(!binance.tickers().iter().any(|t| up(t) == "AAPL")); // never a doomed load on Binance
    }

    #[test]
    fn resolution_is_registration_order_independent() {
        let rig = rig();
        let fmp = FakeProvider::new(&["AAPL"]).rc();
        let binance = FakeProvider::new(&["BTCUSDT"]).rc();
        let parked = track(&rig, rig.feed.load(&cfg("AAPL", "60", 4)));
        let _ = rig.feed.register_provider("fmp", fmp.clone()); // the supporting provider FIRST this time
        let _ = rig.feed.register_provider("binance", binance);
        rig.platform.run();
        assert_eq!(parked.borrow().as_ref().unwrap().as_ref().unwrap().len(), 4);
        assert!(fmp.tickers().iter().any(|t| up(t) == "AAPL"));
    }

    #[test]
    fn stays_parked_and_warns_when_nothing_supports_the_symbol_then_resolves_once_one_does() {
        let rig = rig();
        let parked = track(&rig, rig.feed.load(&cfg("AAPL", "60", 5)));
        rig.register("binance", FakeProvider::new(&["BTCUSDT"]).rc()); // indexed; AAPL absent
        rig.platform.run();
        assert!(parked.borrow().is_none()); // still parked: the lone provider does not serve AAPL
        assert!(rig.platform.warnings().iter().any(|w| w.contains("AAPL")));

        let _ = rig
            .feed
            .register_provider("fmp", FakeProvider::new(&["AAPL"]).rc());
        rig.platform.run();
        assert_eq!(parked.borrow().as_ref().unwrap().as_ref().unwrap().len(), 5);
    }

    #[test]
    fn an_explicit_prefix_resolves_without_waiting_for_the_eager_index() {
        let rig = rig();
        // The enumeration never resolves: the index never settles.
        let (provider, _never) = GatedList::new();
        let parked = track(&rig, rig.feed.load(&cfg("binance:BTCUSDT", "60", 6)));
        let _ = rig.feed.register_provider("binance", provider);
        rig.platform.run();
        assert_eq!(parked.borrow().as_ref().unwrap().as_ref().unwrap().len(), 6);
    }

    #[test]
    fn offline_data_loads_immediately_with_no_provider_registered() {
        let rig = rig();
        let data = make_bars(6);
        let loaded = rig.load(&MarketConfig {
            data: Some(data.clone()),
            ..MarketConfig::default()
        });
        assert_eq!(loaded, Ok(data));
    }

    // ---- a parked load is reported and releasable ----

    fn collect_unresolved(rig: &Rig) -> Rc<RefCell<Vec<UnresolvedSymbol>>> {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = seen.clone();
        let _off = rig
            .feed
            .on_unresolved(Box::new(move |info| log.borrow_mut().push(info.clone())))
            .unwrap();
        seen
    }

    #[test]
    fn reports_the_unservable_symbol_once_the_provider_index_settles() {
        let rig = rig();
        let seen = collect_unresolved(&rig);
        let _parked = track(&rig, rig.feed.load(&cfg("NOPE", "60", 10)));
        rig.register("hyperliquid", FakeProvider::new(&["BTC"]).rc());
        rig.platform.run();
        assert!(!seen.borrow().is_empty());
        assert_eq!(
            seen.borrow()[0],
            UnresolvedSymbol {
                symbol: "NOPE".into(),
                providers: vec!["hyperliquid".into()]
            }
        );
    }

    #[test]
    fn waits_for_every_index_before_declaring_a_symbol_unservable() {
        let rig = rig();
        let seen = collect_unresolved(&rig);
        let (slow, release) = GatedList::new();
        let parked = track(&rig, rig.feed.load(&cfg("BTC", "60", 10)));
        let _ = rig.feed.register_provider("slow", slow);
        rig.register("fast", FakeProvider::new(&["ETHUSDT"]).rc()); // settles first, has no BTC
        rig.platform.run();
        assert!(seen.borrow().is_empty()); // the slow venue may still serve it: no verdict yet

        release.send(vec![SymbolDescriptor::new("BTC")]);
        rig.platform.run();
        assert!(seen.borrow().is_empty()); // and it did serve it: never reported at all
        assert_eq!(
            parked.borrow().as_ref().unwrap().as_ref().unwrap().len(),
            10
        );
    }

    #[test]
    fn still_reports_once_the_last_index_settles_with_nothing_serving_the_symbol() {
        let rig = rig();
        let seen = collect_unresolved(&rig);
        let _parked = track(&rig, rig.feed.load(&cfg("NOPE", "60", 10)));
        rig.register("a", FakeProvider::new(&["BTC"]).rc());
        rig.register("b", FakeProvider::new(&["ETH"]).rc());
        rig.platform.run();
        assert!(seen.borrow().iter().any(|i| i.symbol == "NOPE"));
    }

    #[test]
    fn destroy_drops_the_parked_wait_so_a_rebuilt_chart_leaves_nothing_behind() {
        let rig = rig();
        let seen = collect_unresolved(&rig);
        let parked = track(&rig, rig.feed.load(&cfg("NOPE", "60", 10)));
        rig.register("a", FakeProvider::new(&["BTC"]).rc());
        rig.platform.run();
        let before = seen.borrow().len();
        assert!(before > 0);

        rig.feed.destroy(); // the chart went away: its wait must go with it
        rig.register("b", FakeProvider::new(&["ETH"]).rc());
        rig.platform.run();
        assert_eq!(seen.borrow().len(), before); // no listener left to re-report
        assert!(parked.borrow().is_none());
    }

    #[test]
    fn a_destroyed_wait_never_resumes_even_when_a_capable_provider_registers() {
        let rig = rig();
        let parked = track(&rig, rig.feed.load(&cfg("BTC", "60", 10)));
        rig.platform.run();
        rig.feed.destroy();
        rig.register("hyperliquid", FakeProvider::new(&["BTC"]).rc());
        rig.platform.run();
        assert!(parked.borrow().is_none());
    }

    #[test]
    fn a_parked_load_still_resumes_when_a_capable_provider_registers() {
        let rig = rig();
        let parked = track(&rig, rig.feed.load(&cfg("BTC", "60", 10)));
        rig.platform.run();
        assert!(parked.borrow().is_none());
        rig.register("hyperliquid", FakeProvider::new(&["BTC"]).rc());
        rig.platform.run();
        assert_eq!(
            parked.borrow().as_ref().unwrap().as_ref().unwrap().len(),
            10
        );
    }

    // ---- venue resolution ----

    #[test]
    fn a_bare_symbol_resolves_via_whatever_registered_venue_lists_it() {
        let rig = rig();
        rig.register("hyperliquid", FakeProvider::new(&["BTC"]).rc());
        assert!(!rig.load(&cfg("BTC", "60", 10)).unwrap().is_empty());
        assert_eq!(rig.resolve("BTC"), pair("hyperliquid", "BTC"));
    }

    #[test]
    fn an_ambiguous_bare_symbol_goes_to_the_first_declared_venue_that_lists_it() {
        let rig = rig();
        let (hl, bn) = (
            FakeProvider::new(&["BTC"]).rc(),
            FakeProvider::new(&["BTC"]).rc(),
        );
        rig.register("hyperliquid", hl.clone()); // declared FIRST
        rig.register("binance", bn.clone());
        rig.load(&cfg("BTC", "60", 10)).unwrap();
        assert_eq!(hl.calls.borrow().len(), 1);
        assert_eq!(bn.calls.borrow().len(), 0);
    }

    #[test]
    fn an_exchange_prefix_pins_the_venue_beating_declaration_order() {
        let rig = rig();
        let (hl, bn) = (
            FakeProvider::new(&["BTC"]).rc(),
            FakeProvider::new(&["BTC"]).rc(),
        );
        rig.register("hyperliquid", hl.clone());
        rig.register("binance", bn.clone());
        rig.load(&cfg("binance:BTC", "60", 10)).unwrap();
        assert_eq!(bn.calls.borrow().len(), 1); // the pinned venue served...
        assert_eq!(hl.calls.borrow().len(), 0); // ...and declaration order never entered it
    }

    #[test]
    fn a_first_declared_venue_that_does_not_list_the_symbol_steps_aside() {
        let rig = rig();
        let (bn, hl) = (
            FakeProvider::new(&["BTCUSDT"]).rc(),
            FakeProvider::new(&["BTC"]).rc(),
        );
        rig.register("binance", bn.clone());
        rig.register("hyperliquid", hl.clone());
        assert!(!rig.load(&cfg("BTC", "60", 10)).unwrap().is_empty());
        assert_eq!(hl.calls.borrow().len(), 1); // the venue that LISTS it served it...
        assert_eq!(bn.calls.borrow().len(), 0); // ...first-declared but unlisting was never asked
        assert_eq!(rig.resolve("BTC"), pair("hyperliquid", "BTC"));
    }

    #[test]
    fn an_in_place_switch_to_another_venue_is_not_poisoned_by_the_first_load() {
        let rig = rig();
        let (bn, cb) = (
            FakeProvider::new(&["BTCUSDT"]).rc(),
            FakeProvider::new(&["BTC-USD"]).rc(),
        );
        rig.register("binance", bn.clone());
        rig.register("coinbase", cb.clone());
        rig.load(&cfg("BTCUSDT", "60", 10)).unwrap();
        let after = rig.load(&cfg("BTC-USD", "60", 10)).unwrap();
        assert!(!after.is_empty());
        assert_eq!(cb.tickers(), ["BTC-USD"]);
        assert_eq!(bn.tickers(), ["BTCUSDT"]); // binance served only its own
    }

    #[test]
    fn a_bare_secondary_symbol_follows_the_venue_the_chart_switched_to() {
        let rig = rig();
        let bn = FakeProvider::new(&["BTCUSDT", "ETHUSDT"]).rc();
        let cb = FakeProvider::new(&["BTCUSDT", "ETHUSDT"]).rc(); // both list both tickers
        rig.register("binance", bn.clone());
        rig.register("coinbase", cb.clone());
        rig.load(&cfg("BTCUSDT", "60", 10)).unwrap();
        rig.load(&cfg("coinbase:BTCUSDT", "60", 10)).unwrap(); // venue switch (prefix pins it)

        rig.range(&secondary("ETHUSDT", "60"), limit(5)).unwrap(); // bare secondary
        assert!(cb.tickers().contains(&"ETHUSDT".to_owned())); // the venue on screen serves it
        assert!(!bn.tickers().contains(&"ETHUSDT".to_owned()));
    }

    #[test]
    fn an_explicit_prefix_in_the_symbol_stays_a_hard_requirement() {
        let rig = rig();
        rig.register("hyperliquid", FakeProvider::new(&["BTC"]).rc());
        let parked = track(&rig, rig.feed.load(&cfg("binance:BTC", "60", 10)));
        rig.platform.run();
        assert!(parked.borrow().is_none()); // the caller NAMED binance: waiting for it is correct
        rig.register("binance", FakeProvider::new(&["BTC"]).rc());
        rig.platform.run();
        assert!(parked.borrow().is_some());
    }

    // ---- resolution + cache identity ----

    #[test]
    fn collapses_bare_prefixed_and_legacy_forms_to_one_canonical_identity() {
        let rig = rig();
        rig.register("binance", FakeProvider::new(&["BTCUSDT"]).rc());
        let canon = pair("binance", "BTCUSDT");
        assert_eq!(rig.resolve("BTCUSDT"), canon);
        assert_eq!(rig.resolve("BINANCE:BTCUSDT"), canon);
        assert_eq!(rig.resolve("binance:BTCUSDT"), canon);
        // A load keys the cache on the resolved identity, so the prefixed form reuses it.
        rig.load(&cfg("BTCUSDT", "60", 10)).unwrap();
        assert!(
            rig.store
                .borrow()
                .get(&series_key("binance", "BTCUSDT", "60", None))
                .is_some()
        );
    }

    #[test]
    fn routes_an_explicit_prefix_even_to_a_ticker_the_index_does_not_list() {
        let rig = rig();
        let binance = FakeProvider::new(&["BTCUSDT", "ETHUSDT"])
            .listing(&["BTCUSDT"])
            .rc();
        rig.register("binance", binance.clone());
        let bars = rig.load(&cfg("binance:ETHUSDT", "60", 8)).unwrap();
        assert_eq!(bars.len(), 8);
        assert!(binance.tickers().iter().any(|t| up(t) == "ETHUSDT"));
    }

    #[test]
    fn resolve_symbol_returns_none_for_an_unknown_provider_prefix() {
        let rig = rig();
        rig.register("binance", FakeProvider::new(&["BTCUSDT"]).rc());
        assert_eq!(rig.resolve("NASDAQ:AAPL"), None);
    }

    #[test]
    fn an_exchange_prefixed_symbol_resolves_immediately() {
        let rig = rig();
        rig.register("binance", FakeProvider::new(&["BTCUSDT"]).rc());
        assert_eq!(rig.load(&cfg("binance:BTCUSDT", "60", 9)).unwrap().len(), 9);
    }

    // ---- unregister, secondary series, symbolInfo, resilience ----

    #[test]
    fn unregister_provider_removes_it_and_makes_its_symbols_unresolvable() {
        let rig = rig();
        rig.register("binance", FakeProvider::new(&["BTCUSDT"]).rc());
        assert_eq!(rig.resolve("BTCUSDT"), pair("binance", "BTCUSDT"));
        rig.feed.unregister_provider("binance");
        assert_eq!(rig.resolve("BTCUSDT"), None);
        assert!(rig.feed.providers().is_empty());
    }

    #[test]
    fn load_range_resolves_each_secondary_independently_with_the_primary_as_bare_default() {
        let rig = rig();
        let binance = FakeProvider::new(&["BTCUSDT", "ETHUSDT"]).rc();
        let fmp = FakeProvider::new(&["AAPL"]).rc();
        rig.register("binance", binance.clone());
        rig.register("fmp", fmp.clone());
        // Establish the primary provider via a primary load.
        rig.load(&cfg("BTCUSDT", "60", 5)).unwrap();

        let aapl = rig.range(&secondary("AAPL", "240"), from(1)).unwrap();
        assert!(!aapl.is_empty());
        assert!(fmp.tickers().iter().any(|t| up(t) == "AAPL"));

        let eth = rig
            .range(&secondary("binance:ETHUSDT", "240"), from(1))
            .unwrap();
        assert!(!eth.is_empty());

        // A bare secondary the primary (binance) serves routes to binance, not fmp.
        let bare_eth = rig.range(&secondary("ETHUSDT", "240"), from(1)).unwrap();
        assert!(!bare_eth.is_empty());
        assert!(binance.tickers().iter().any(|t| up(t) == "ETHUSDT"));

        // Unresolvable secondary: empty (the engine degrades), never an error.
        assert_eq!(
            rig.range(&secondary("NASDAQ:AAPL", "240"), from(1)),
            Ok(Vec::new())
        );
    }

    #[test]
    fn warms_sync_symbol_info_from_the_provider_on_load() {
        let rig = rig();
        rig.register(
            "binance",
            FakeProvider::new(&["BTCUSDT"])
                .sym_info(json!({ "mintick": 0.5, "pricescale": 2 }))
                .rc(),
        );
        rig.load(&cfg("BTCUSDT", "60", 5)).unwrap();
        rig.platform.run(); // let the fire-and-forget prefetch land

        let info = rig.feed.symbol_info(&secondary("BTCUSDT", "60")).unwrap();
        assert_eq!(info.ticker, "BTCUSDT");
        assert_eq!(info.extra.get("mintick"), Some(&json!(0.5)));
        let via_facade = rig
            .platform
            .block_on(rig.feed.symbol_info_for("BINANCE:BTCUSDT"))
            .unwrap()
            .unwrap();
        assert_eq!(via_facade.extra.get("mintick"), Some(&json!(0.5)));
        assert_eq!(rig.feed.symbol_info(&secondary("NASDAQ:AAPL", "60")), None);
    }

    #[test]
    fn symbol_info_for_an_unresolvable_symbol_or_a_provider_without_metadata_is_none() {
        let rig = rig();
        rig.register("binance", FakeProvider::new(&["BTCUSDT"]).rc());
        assert_eq!(
            rig.platform.block_on(rig.feed.symbol_info_for("NOPE:X")),
            Ok(None)
        );
        assert_eq!(
            rig.platform.block_on(rig.feed.symbol_info_for("BTCUSDT")),
            Ok(None)
        );
        assert_eq!(
            rig.feed.symbol_info(&MarketConfig {
                data: Some(make_bars(2)),
                ..secondary("BTCUSDT", "60")
            }),
            None
        );
    }

    /// Counts `get_symbol_info` calls and holds the first one on a gate.
    struct InfoProbe {
        calls: Cell<usize>,
        gate: RefCell<Option<OneshotReceiver<()>>>,
        fail_first: bool,
    }

    impl InfoProbe {
        fn new(fail_first: bool) -> (Rc<Self>, Option<crate::runtime::OneshotSender<()>>) {
            let (tx, rx) = oneshot();
            let gated = !fail_first;
            (
                Rc::new(Self {
                    calls: Cell::new(0),
                    gate: RefCell::new(gated.then_some(rx)),
                    fail_first,
                }),
                gated.then_some(tx),
            )
        }
    }

    impl DataProvider for InfoProbe {
        fn supports_symbol_info(&self) -> bool {
            true
        }

        fn get_bars(
            &self,
            _ticker: &str,
            _timeframe: &str,
            _range: &BarRange,
        ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
            let out = make_bars(5);
            Box::pin(async move { Ok(out) })
        }

        fn list_symbols(
            &self,
        ) -> Option<LocalBoxFuture<'static, Result<Vec<SymbolDescriptor>, DataError>>> {
            Some(Box::pin(async {
                Ok(vec![SymbolDescriptor::new("BTCUSDT")])
            }))
        }

        fn get_symbol_info(
            &self,
            ticker: &str,
        ) -> Option<LocalBoxFuture<'static, Result<Option<SymbolInfo>, DataError>>> {
            let call = self.calls.get() + 1;
            self.calls.set(call);
            let gate = self.gate.borrow_mut().take();
            let fail = self.fail_first && call == 1;
            let mut extra = serde_json::Map::new();
            extra.insert("mintick".into(), json!(0.5));
            let info = SymbolInfo {
                ticker: ticker.to_owned(),
                extra,
                ..SymbolInfo::default()
            };
            Some(Box::pin(async move {
                if let Some(gate) = gate {
                    gate.await;
                }
                if fail {
                    return Err(DataError::other("boom"));
                }
                Ok(Some(info))
            }))
        }
    }

    #[test]
    fn fetches_symbol_metadata_once_for_every_concurrent_asker_and_caches_it() {
        let rig = rig();
        let (provider, release) = InfoProbe::new(false);
        rig.register("binance", provider.clone());

        let askers = [
            track(&rig, rig.feed.symbol_info_for("BTCUSDT")),
            track(&rig, rig.feed.symbol_info_for("BINANCE:BTCUSDT")),
            track(&rig, rig.feed.symbol_info_for("btcusdt")),
        ];
        rig.platform.run();
        assert_eq!(provider.calls.get(), 1); // the three askers share that one request
        assert!(askers.iter().all(|a| a.borrow().is_none()));

        release.unwrap().send(());
        rig.platform.run();
        for asker in &askers {
            let info = asker.borrow().clone().unwrap().unwrap().unwrap();
            assert_eq!(
                (info.ticker.as_str(), info.extra.get("mintick")),
                ("BTCUSDT", Some(&json!(0.5)))
            );
        }

        // Cached: a later asker (and the bar load's own prefetch) opens no new request.
        let again = rig
            .platform
            .block_on(rig.feed.symbol_info_for("BTCUSDT"))
            .unwrap()
            .unwrap();
        assert_eq!(again.extra.get("mintick"), Some(&json!(0.5)));
        rig.load(&cfg("BTCUSDT", "60", 5)).unwrap();
        rig.platform.run();
        assert_eq!(provider.calls.get(), 1);
        assert!(rig.feed.symbol_info(&secondary("BTCUSDT", "60")).is_some());
    }

    #[test]
    fn symbol_metadata_expires_after_its_ttl_but_stays_readable_synchronously() {
        let rig = rig();
        let (provider, release) = InfoProbe::new(false);
        rig.register("binance", provider.clone());
        let first = track(&rig, rig.feed.symbol_info_for("BTCUSDT"));
        release.unwrap().send(());
        rig.platform.run();
        assert!(first.borrow().is_some());

        rig.platform.advance(SYMBOL_INFO_TTL_MS - 1.0);
        rig.platform
            .block_on(rig.feed.symbol_info_for("BTCUSDT"))
            .unwrap();
        assert_eq!(provider.calls.get(), 1); // still fresh
        rig.platform.advance(1.0);
        rig.platform
            .block_on(rig.feed.symbol_info_for("BTCUSDT"))
            .unwrap();
        assert_eq!(provider.calls.get(), 2); // expired: refreshed
        assert!(rig.feed.symbol_info(&secondary("BTCUSDT", "60")).is_some());
    }

    #[test]
    fn a_failed_metadata_probe_is_not_remembered() {
        let rig = rig();
        let (provider, _) = InfoProbe::new(true);
        rig.register("binance", provider.clone());
        assert_eq!(
            rig.platform.block_on(rig.feed.symbol_info_for("BTCUSDT")),
            Err(DataError::other("boom"))
        );
        let retry = rig
            .platform
            .block_on(rig.feed.symbol_info_for("BTCUSDT"))
            .unwrap()
            .unwrap();
        assert_eq!(retry.extra.get("mintick"), Some(&json!(0.5)));
        assert_eq!(provider.calls.get(), 2);
    }

    #[test]
    fn a_throwing_provider_yields_an_empty_result_and_a_warning() {
        let rig = rig();
        rig.register("binance", FakeProvider::new(&["BTCUSDT"]).throws().rc());
        assert_eq!(rig.load(&cfg("BTCUSDT", "60", 5)), Ok(Vec::new()));
        assert!(
            rig.platform
                .warnings()
                .contains(&"[vela] data fetch failed for BTCUSDT 60 \u{2014} boom".to_owned())
        );
    }

    #[test]
    fn never_invents_ticks_for_offline_data() {
        let rig = rig();
        let data = make_bars(5);
        let snapshot = data.clone();
        let loaded = rig
            .load(&MarketConfig {
                data: Some(data.clone()),
                ..MarketConfig::default()
            })
            .unwrap();
        let ticks = Rc::new(Cell::new(0));
        let count = ticks.clone();
        let stop = rig.feed.subscribe(
            &MarketConfig {
                data: Some(data.clone()),
                timeframe: Some("60".into()),
                ..MarketConfig::default()
            },
            Box::new(move |_| count.set(count.get() + 1)),
        );
        rig.platform.advance(60_000.0);
        assert_eq!(ticks.get(), 0);
        assert_eq!(rig.platform.pending_timers(), 0);
        assert_eq!(data, snapshot);
        assert_eq!(loaded, snapshot);
        stop();
    }

    // ---- dotted prefixes, capabilities, provider instance ----

    #[test]
    fn routes_a_dotted_qualifier_prefix_and_keeps_the_market_suffix_on_the_ticker() {
        let rig = rig();
        rig.register("binance", FakeProvider::new(&["BTCUSDT", "BTCUSDT.P"]).rc());
        rig.register("binance.us", FakeProvider::new(&["BTCUSDT"]).rc());
        assert_eq!(
            rig.resolve("BINANCE.US:BTCUSDT"),
            pair("binance.us", "BTCUSDT")
        );
        assert_eq!(
            rig.resolve("BINANCE:BTCUSDT.P"),
            pair("binance", "BTCUSDT.P")
        );
        assert_eq!(
            rig.resolve("binance.us:btcusdt").map(|r| r.0),
            Some("binance.us".into())
        );
        assert_eq!(
            rig.resolve("BTCUSDT.P").map(|r| r.0),
            Some("binance".into())
        );
    }

    #[test]
    fn capabilities_expose_the_resolved_per_symbol_record() {
        let rig = rig();
        assert_eq!(rig.feed.capabilities_for("nope:NOPE"), None);
        let info = ProviderInfo {
            name: "x".into(),
            display_name: None,
            requires_api_key: None,
            supported_timeframes: None,
            capabilities: ProviderCapabilities {
                enumerate: true,
                stream: false,
                symbol_info: false,
            },
        };
        rig.register("x", FakeProvider::new(&["TEST"]).with_info(info).rc());
        let caps = rig.feed.capabilities_for("x:TEST").unwrap();
        assert_eq!((caps.enumerate, caps.stream), (true, false));
    }

    struct Refining;

    impl DataProvider for Refining {
        fn get_bars(
            &self,
            _ticker: &str,
            _timeframe: &str,
            _range: &BarRange,
        ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn capabilities_for(&self, ticker: &str) -> Option<ProviderCapabilities> {
            (ticker == "LIVE").then_some(ProviderCapabilities {
                enumerate: false,
                stream: true,
                symbol_info: true,
            })
        }
    }

    #[test]
    fn a_per_instrument_refinement_beats_the_provider_wide_capabilities() {
        let rig = rig();
        rig.register("lux", Rc::new(Refining));
        assert!(rig.feed.capabilities_for("lux:LIVE").unwrap().stream);
        assert!(!rig.feed.capabilities_for("lux:OTHER").unwrap().stream);
    }

    #[test]
    fn provider_instance_returns_the_registered_instance() {
        let rig = rig();
        let lux = FakeProvider::new(&[]).rc();
        rig.register("lux", lux.clone());
        let got = rig.feed.provider_instance("lux").unwrap();
        assert!(std::ptr::eq(
            Rc::as_ptr(&got).cast::<()>(),
            Rc::as_ptr(&lux).cast::<()>()
        ));
        assert!(rig.feed.provider_instance("nope").is_none());
    }

    // ---- poll fallback ----

    /// `get_bars` parks on a gate the test opens by hand.
    struct GatedBars {
        gate: RefCell<Option<OneshotReceiver<Vec<Ohlcv>>>>,
    }

    impl DataProvider for GatedBars {
        fn get_bars(
            &self,
            _ticker: &str,
            _timeframe: &str,
            _range: &BarRange,
        ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
            let gate = self.gate.borrow_mut().take();
            Box::pin(async move {
                match gate {
                    Some(gate) => Ok(gate.await),
                    None => Ok(Vec::new()),
                }
            })
        }

        fn list_symbols(
            &self,
        ) -> Option<LocalBoxFuture<'static, Result<Vec<SymbolDescriptor>, DataError>>> {
            Some(Box::pin(async { Ok(vec![SymbolDescriptor::new("AAA")]) }))
        }
    }

    #[test]
    fn a_poll_in_flight_when_unsubscribe_lands_does_not_deliver_its_stale_bars() {
        // The market-switch race: stale old-market bars arriving after the
        // switch would silently replace the new market's forming candle (same
        // open time).
        let rig = rig();
        let (tx, rx) = oneshot();
        rig.register(
            "p",
            Rc::new(GatedBars {
                gate: RefCell::new(Some(rx)),
            }),
        );
        let ticks = Rc::new(Cell::new(0));
        let count = ticks.clone();
        let stop = rig.feed.subscribe(
            &secondary("AAA", "60"),
            Box::new(move |_| count.set(count.get() + 1)),
        );
        rig.platform.advance(3_100.0); // the first poll fires and parks on the gate
        stop(); // the market switches away while the fetch is in flight
        tx.send(make_bars(2));
        rig.platform.advance(10.0);
        assert_eq!(ticks.get(), 0);
        assert_eq!(rig.platform.pending_timers(), 0); // and it never reschedules
    }

    #[test]
    fn a_provider_without_subscribe_is_polled_every_three_seconds_for_its_last_two_bars() {
        let rig = rig();
        let provider = FakeProvider::new(&["AAA"]).rc();
        rig.register("p", provider.clone());
        let times = Rc::new(RefCell::new(Vec::new()));
        let log = times.clone();
        let stop = rig.feed.subscribe(
            &MarketConfig {
                session: Some(MarketSession::Extended),
                ..secondary("AAA", "60")
            },
            Box::new(move |bar| log.borrow_mut().push(bar.time)),
        );
        rig.platform.advance(2_999.0);
        assert!(provider.calls.borrow().is_empty());
        rig.platform.advance(1.0);
        assert_eq!(provider.calls.borrow().len(), 1);
        assert_eq!(times.borrow().len(), 2); // the previous and the forming bar
        let range = provider.ranges.borrow()[0].clone();
        assert_eq!(
            (range.limit, range.session.as_deref()),
            (Some(2), Some("extended"))
        );
        rig.platform.advance(6_000.0);
        assert_eq!(provider.calls.borrow().len(), 3);
        stop();
        assert_eq!(rig.platform.pending_timers(), 0);
        rig.platform.advance(30_000.0);
        assert_eq!(provider.calls.borrow().len(), 3);
    }

    #[test]
    fn a_tick_callback_that_unsubscribes_ends_the_poll_loop() {
        let rig = rig();
        rig.register("p", FakeProvider::new(&["AAA"]).rc());
        let stop_cell: Rc<RefCell<Option<Unsubscribe>>> = Rc::default();
        let cell = stop_cell.clone();
        let ticks = Rc::new(Cell::new(0));
        let count = ticks.clone();
        let stop = rig.feed.subscribe(
            &secondary("AAA", "60"),
            Box::new(move |_| {
                count.set(count.get() + 1);
                if let Some(stop) = cell.borrow_mut().take() {
                    stop();
                }
            }),
        );
        *stop_cell.borrow_mut() = Some(stop);
        rig.platform.advance(20_000.0);
        // Both bars of that one poll land (the loop finishes), and no second poll follows.
        assert_eq!(ticks.get(), 2);
        assert_eq!(rig.platform.pending_timers(), 0);
    }

    #[test]
    fn subscribing_to_an_unresolvable_symbol_does_nothing() {
        let rig = rig();
        rig.register("p", FakeProvider::new(&["AAA"]).rc());
        let stop = rig
            .feed
            .subscribe(&secondary("NOPE", "60"), Box::new(|_| {}));
        assert_eq!(rig.platform.pending_timers(), 0);
        stop();
    }

    // ---- listing-prefix resolution, session flag, icons, groups ----

    fn equities() -> Rc<FakeProvider> {
        FakeProvider::new(&[])
            .descriptors(vec![
                SymbolDescriptor {
                    kind: Some("stock".into()),
                    prefix: Some("NASDAQ".into()),
                    ..SymbolDescriptor::new("AAPL")
                },
                SymbolDescriptor {
                    kind: Some("stock".into()),
                    prefix: Some("NYSE".into()),
                    ..SymbolDescriptor::new("IBM")
                },
                SymbolDescriptor {
                    kind: Some("stock".into()),
                    prefix: Some("AMEX".into()),
                    ..SymbolDescriptor::new("SPY")
                },
            ])
            .rc()
    }

    fn boot_equities() -> Rig {
        let rig = rig();
        rig.register("binance", FakeProvider::new(&["BTCUSDT"]).rc());
        rig.register("edgx", equities());
        rig
    }

    #[test]
    fn prefix_ticker_resolves_through_the_descriptor_prefix_to_the_serving_provider() {
        let rig = boot_equities();
        assert_eq!(rig.resolve("NASDAQ:AAPL"), pair("edgx", "AAPL"));
        assert_eq!(rig.resolve("nyse:ibm"), pair("edgx", "IBM"));
    }

    #[test]
    fn a_wrong_listing_venue_resolves_to_nothing() {
        let rig = boot_equities();
        assert_eq!(rig.resolve("NYSE:AAPL"), None);
    }

    #[test]
    fn the_provider_name_still_routes() {
        let rig = boot_equities();
        assert_eq!(rig.resolve("edgx:AAPL"), pair("edgx", "AAPL"));
    }

    #[test]
    fn display_prefix_and_canonical_form_come_from_the_data() {
        let rig = boot_equities();
        assert_eq!(rig.feed.display_prefix("AAPL").as_deref(), Some("NASDAQ"));
        assert_eq!(
            rig.feed.display_prefix("edgx:AAPL").as_deref(),
            Some("NASDAQ")
        );
        assert_eq!(
            rig.feed.canonical_symbol("edgx:ibm").as_deref(),
            Some("NYSE:IBM")
        );
        assert_eq!(
            rig.feed.display_prefix("BTCUSDT").as_deref(),
            Some("binance")
        );
        assert_eq!(
            rig.feed.canonical_symbol("BTCUSDT").as_deref(),
            Some("binance:BTCUSDT")
        );
        assert_eq!(rig.feed.display_prefix("NOPE:NOPE"), None);
    }

    #[test]
    fn the_session_flag_rides_every_request_and_the_subscribe_opts() {
        let rig = rig();
        let provider = FakeProvider::new(&["AAPL"])
            .descriptors(vec![SymbolDescriptor {
                prefix: Some("NASDAQ".into()),
                ..SymbolDescriptor::new("AAPL")
            }])
            .streaming()
            .rc();
        rig.register("edgx", provider.clone());
        let last_session = || provider.ranges.borrow().last().unwrap().session.clone();
        let extended = |c: MarketConfig| MarketConfig {
            session: Some(MarketSession::Extended),
            ..c
        };

        rig.load(&extended(cfg("NASDAQ:AAPL", "60", 5))).unwrap();
        assert_eq!(last_session().as_deref(), Some("extended"));

        rig.range(
            &extended(secondary("NASDAQ:AAPL", "60")),
            BarRange {
                from: Some(0),
                limit: Some(5),
                ..BarRange::default()
            },
        )
        .unwrap();
        let ranges = provider.ranges.borrow();
        let last = ranges.last().unwrap();
        assert_eq!(
            (last.from, last.session.as_deref()),
            (Some(0), Some("extended"))
        );
        drop(ranges);

        let off = rig
            .feed
            .subscribe(&extended(secondary("NASDAQ:AAPL", "60")), Box::new(|_| {}));
        off();
        assert_eq!(
            *provider.sub_sessions.borrow(),
            vec![Some("extended".to_owned())]
        );

        // No session set: nothing invented, the flag is simply absent.
        rig.load(&cfg("NASDAQ:AAPL", "60", 5)).unwrap();
        assert_eq!(last_session(), None);
        let off = rig
            .feed
            .subscribe(&secondary("NASDAQ:AAPL", "60"), Box::new(|_| {}));
        off();
        assert_eq!(provider.sub_sessions.borrow()[1], None);
        // A streaming provider is never polled.
        assert_eq!(rig.platform.pending_timers(), 0);
    }

    fn icon(ticker: &str) -> Option<String> {
        Some(format!("https://icons.example/{ticker}.png"))
    }

    #[test]
    fn icons_route_to_the_owning_provider() {
        let rig = rig();
        rig.register("binance", FakeProvider::new(&["BTCUSDT"]).icons(icon).rc());
        rig.register("plain", FakeProvider::new(&["AAPL"]).rc());
        let owned = |ticker: &str, provider: &str| SymbolDescriptor {
            provider: Some(provider.into()),
            ..SymbolDescriptor::new(ticker)
        };
        assert_eq!(
            rig.feed
                .symbol_icon_of(&owned("BTCUSDT", "binance"))
                .as_deref(),
            Some("https://icons.example/BTCUSDT.png")
        );
        assert_eq!(rig.feed.symbol_icon_of(&owned("AAPL", "plain")), None);
        assert_eq!(rig.feed.symbol_icon_of(&owned("X", "ghost")), None); // unknown provider
        assert_eq!(
            rig.feed.symbol_icon("binance:BTCUSDT").as_deref(),
            Some("https://icons.example/BTCUSDT.png")
        );
        assert_eq!(
            rig.feed.symbol_icon("BTCUSDT").as_deref(),
            Some("https://icons.example/BTCUSDT.png")
        );
        assert_eq!(rig.feed.symbol_icon("nope:NOTHING"), None);
    }

    fn futures_provider() -> Rc<FakeProvider> {
        let member = |ticker: &str, group: &str, default: bool| SymbolDescriptor {
            kind: Some("futures".into()),
            group: Some(group.into()),
            prefix: Some("CME".into()),
            default: default.then_some(true),
            ..SymbolDescriptor::new(ticker)
        };
        let root = |ticker: &str| SymbolDescriptor {
            kind: Some("root".into()),
            group: Some(ticker.into()),
            prefix: Some("CME".into()),
            ..SymbolDescriptor::new(ticker)
        };
        FakeProvider::new(&[])
            .descriptors(vec![
                root("ES"),
                member("ES1!", "ES", true),
                member("ES2!", "ES", false),
                root("NQ"),
                member("NQ1!", "NQ", false), // no default marked
                member("NQ2!", "NQ", false),
            ])
            .rc()
    }

    #[test]
    fn grouped_listings_resolve_roots_to_loadable_members() {
        let rig = rig();
        rig.register("lux", futures_provider());
        assert_eq!(rig.resolve("ES"), pair("lux", "ES1!"));
        assert_eq!(rig.resolve("CME:ES"), pair("lux", "ES1!"));
        assert_eq!(rig.resolve("lux:ES"), pair("lux", "ES1!"));
        assert_eq!(rig.resolve("NQ"), pair("lux", "NQ1!"));
        assert_eq!(rig.resolve("ES2!"), pair("lux", "ES2!"));
        assert_eq!(rig.resolve("CME:ES2!"), pair("lux", "ES2!"));
    }

    #[test]
    fn loading_a_group_root_fetches_its_default_member() {
        let rig = rig();
        let lux = futures_provider();
        rig.register("lux", lux.clone());
        rig.load(&cfg("ES", "60", 5)).unwrap();
        assert_eq!(lux.tickers(), ["ES1!"]);
    }

    // ---- data-unavailable.test.ts: the feed's cases ----

    #[derive(Clone, Copy, PartialEq)]
    enum Refuse {
        Unavailable,
        Error,
    }

    /// Serves `GDP` at 1D and coarser only; anything finer is refused for good,
    /// or, with `Refuse::Error`, with a plain (transient) error.
    struct MacroProvider {
        refuse: Refuse,
        calls: RefCell<Vec<(String, String)>>,
    }

    impl MacroProvider {
        fn new(refuse: Refuse) -> Rc<Self> {
            Rc::new(Self {
                refuse,
                calls: RefCell::default(),
            })
        }

        fn count(&self, tf: &str) -> usize {
            self.calls.borrow().iter().filter(|(_, t)| t == tf).count()
        }
    }

    impl DataProvider for MacroProvider {
        fn get_bars(
            &self,
            ticker: &str,
            timeframe: &str,
            range: &BarRange,
        ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
            self.calls
                .borrow_mut()
                .push((ticker.to_owned(), timeframe.to_owned()));
            let coarse = matches!(timeframe, "D" | "1D" | "W" | "M");
            let result = if coarse {
                let n = range.limit.unwrap_or(50);
                Ok((0..n as i64)
                    .map(|i| Ohlcv::new(START + i * DAY, 100.0, 101.0, 99.0, 100.0, Some(1.0)))
                    .collect())
            } else if self.refuse == Refuse::Error {
                Err(DataError::other("network down"))
            } else {
                let mut details = DataUnavailableDetails::new(DataUnavailableReason::Timeframe);
                details.finest = Some("1D".into());
                details.default = Some("1D".into());
                Err(DataError::from(DataUnavailableError::new(details, None)))
            };
            Box::pin(async move { result })
        }

        fn list_symbols(
            &self,
        ) -> Option<LocalBoxFuture<'static, Result<Vec<SymbolDescriptor>, DataError>>> {
            Some(Box::pin(async {
                Ok(vec![
                    SymbolDescriptor::new("GDP"),
                    SymbolDescriptor::new("CPI"),
                ])
            }))
        }
    }

    fn refusal(result: Result<Vec<Ohlcv>, DataError>) -> DataUnavailableError {
        match result {
            Err(DataError::Unavailable(e)) => *e,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_permanent_refusal_fails_load_and_load_range_with_the_refused_series_filled_in() {
        let rig = rig();
        rig.register("macro", MacroProvider::new(Refuse::Unavailable));
        let e = refusal(rig.load(&cfg("GDP", "60", 50)));
        assert_eq!(e.symbol.as_deref(), Some("GDP"));
        assert_eq!(e.timeframe.as_deref(), Some("60"));
        assert_eq!(e.reason, DataUnavailableReason::Timeframe);
        assert_eq!(e.finest.as_deref(), Some("1D"));
        assert_eq!(e.default.as_deref(), Some("1D"));
        assert_eq!(
            e.message(),
            "GDP is not served at 60; its finest timeframe is 1D"
        );

        let e = refusal(rig.range(&secondary("GDP", "60"), limit(10)));
        assert_eq!(e.timeframe.as_deref(), Some("60"));
    }

    #[test]
    fn a_providers_own_refusal_message_is_kept_as_written() {
        struct Custom;
        impl DataProvider for Custom {
            fn get_bars(
                &self,
                _ticker: &str,
                _timeframe: &str,
                _range: &BarRange,
            ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
                Box::pin(async {
                    Err(DataError::from(DataUnavailableError::new(
                        DataUnavailableDetails::new("timeframe".into()),
                        Some("custom words".into()),
                    )))
                })
            }
        }
        let rig = rig();
        rig.register("c", Rc::new(Custom));
        let e = refusal(rig.load(&cfg("c:X", "60", 5)));
        assert_eq!(e.message(), "custom words");
        assert_eq!(
            (e.symbol.as_deref(), e.timeframe.as_deref()),
            (Some("X"), Some("60"))
        );
    }

    #[test]
    fn a_plain_failure_keeps_reading_as_an_empty_answer() {
        let rig = rig();
        rig.register("macro", MacroProvider::new(Refuse::Error));
        assert_eq!(rig.load(&cfg("GDP", "60", 50)), Ok(Vec::new()));
        assert_eq!(
            rig.range(&secondary("GDP", "60"), limit(10)),
            Ok(Vec::new())
        );
        assert!(!rig.platform.warnings().is_empty());
    }

    #[test]
    fn a_refusal_is_never_cached_and_the_next_ask_reaches_the_provider_again() {
        let rig = rig();
        let macro_provider = MacroProvider::new(Refuse::Unavailable);
        rig.register("macro", macro_provider.clone());
        let range = from(rig.platform.now_ms() as i64 - 10 * 3_600_000);
        refusal(rig.range(&secondary("GDP", "60"), range.clone()));
        let key = series_key("macro", "GDP", "60", None);
        assert!(rig.store.borrow().get(&key).is_none());
        assert_eq!(rig.store.borrow().covered_from_of(&key), None);
        refusal(rig.range(&secondary("GDP", "60"), range));
        assert_eq!(macro_provider.count("60"), 2);
    }

    #[test]
    fn the_live_poll_stops_on_a_refusal_and_keeps_going_through_plain_failures() {
        for refuse in [Refuse::Unavailable, Refuse::Error] {
            let rig = rig();
            let macro_provider = MacroProvider::new(refuse);
            rig.register("macro", macro_provider.clone());
            let off = rig
                .feed
                .subscribe(&secondary("macro:GDP", "60"), Box::new(|_| {}));
            rig.platform.advance(30_000.0);
            if refuse == Refuse::Unavailable {
                assert_eq!(macro_provider.count("60"), 1);
                assert_eq!(rig.platform.pending_timers(), 0);
                assert_eq!(
                    rig.platform.warnings(),
                    vec![
                        "[vela] live updates stopped \u{2014} The series is not served at this timeframe; its finest timeframe is 1D"
                    ]
                );
            } else {
                assert!(macro_provider.count("60") >= 9);
            }
            off();
        }
    }

    /// A provider that streams its answer in two batches and can refuse or fail.
    struct Streaming {
        outcome: RefCell<Option<Result<Vec<Ohlcv>, DataError>>>,
        saw_signal: Cell<bool>,
    }

    impl DataProvider for Streaming {
        fn get_bars(
            &self,
            _ticker: &str,
            _timeframe: &str,
            _range: &BarRange,
        ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn list_symbols(
            &self,
        ) -> Option<LocalBoxFuture<'static, Result<Vec<SymbolDescriptor>, DataError>>> {
            Some(Box::pin(async { Ok(vec![SymbolDescriptor::new("GDP")]) }))
        }

        fn get_bars_progressive(
            &self,
            _ticker: &str,
            _timeframe: &str,
            _range: &BarRange,
            mut on_batch: BatchSink,
            signal: Option<AbortSignal>,
        ) -> Option<LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>>> {
            self.saw_signal.set(signal.is_some());
            let outcome = self
                .outcome
                .borrow_mut()
                .take()
                .unwrap_or_else(|| Ok(Vec::new()));
            on_batch(make_bars(2));
            Some(Box::pin(async move { outcome }))
        }
    }

    fn streaming(outcome: Result<Vec<Ohlcv>, DataError>) -> Rc<Streaming> {
        Rc::new(Streaming {
            outcome: RefCell::new(Some(outcome)),
            saw_signal: Cell::new(false),
        })
    }

    fn progressive(
        rig: &Rig,
        c: &MarketConfig,
    ) -> (Result<Option<Vec<Ohlcv>>, DataError>, Vec<usize>) {
        let batches = Rc::new(RefCell::new(Vec::new()));
        let sink = batches.clone();
        let controller = crate::runtime::AbortController::new();
        let load = rig
            .feed
            .load_progressive(
                c,
                Box::new(move |b| sink.borrow_mut().push(b.len())),
                Some(controller.signal()),
            )
            .unwrap();
        let result = rig.platform.block_on(load);
        let seen = batches.borrow().clone();
        (result, seen)
    }

    #[test]
    fn progressive_loads_forward_batches_the_signal_and_the_final_answer() {
        let rig = rig();
        let provider = streaming(Ok(make_bars(4)));
        rig.register("macro", provider.clone());
        let (result, batches) = progressive(&rig, &cfg("GDP", "60", 50));
        assert_eq!(result.unwrap().unwrap().len(), 4);
        assert_eq!(batches, vec![2]);
        assert!(provider.saw_signal.get());
        // The final answer is cached like a plain load's, minus the forming bar.
        let key = series_key("macro", "GDP", "60", None);
        assert_eq!(rig.store.borrow().get(&key).map(<[Ohlcv]>::len), Some(3));
    }

    #[test]
    fn a_progressive_source_that_refuses_fails_the_load_with_the_series_filled_in() {
        let rig = rig();
        let mut details = DataUnavailableDetails::new(DataUnavailableReason::Timeframe);
        details.finest = Some("D".into());
        rig.register(
            "macro",
            streaming(Err(DataError::from(DataUnavailableError::new(
                details, None,
            )))),
        );
        let (result, _) = progressive(&rig, &cfg("GDP", "60", 50));
        match result {
            Err(DataError::Unavailable(e)) => {
                assert_eq!(
                    e.message(),
                    "GDP is not served at 60; its finest timeframe is D"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_progressive_plain_failure_reads_as_empty_with_a_warning() {
        let rig = rig();
        rig.register("macro", streaming(Err(DataError::other("boom"))));
        let (result, batches) = progressive(&rig, &cfg("GDP", "60", 50));
        assert_eq!(result, Ok(Some(Vec::new()))); // the batches already painted stay painted
        assert_eq!(batches, vec![2]);
        assert_eq!(
            rig.platform.warnings(),
            vec!["[vela] progressive fetch failed for GDP 60 \u{2014} boom"]
        );
    }

    #[test]
    fn progressive_resolves_to_none_when_the_provider_cannot_stream_and_passes_offline_data_through()
     {
        let rig = rig();
        rig.register("binance", FakeProvider::new(&["BTCUSDT"]).rc());
        let (result, batches) = progressive(&rig, &cfg("BTCUSDT", "60", 10));
        assert_eq!(result, Ok(None));
        assert!(batches.is_empty());
        let data = make_bars(3);
        let (result, _) = progressive(
            &rig,
            &MarketConfig {
                data: Some(data.clone()),
                ..MarketConfig::default()
            },
        );
        assert_eq!(result, Ok(Some(data)));
    }

    #[test]
    fn a_parked_progressive_load_resumes_on_registration() {
        let rig = rig();
        let load = rig
            .feed
            .load_progressive(&cfg("GDP", "60", 50), Box::new(|_| {}), None)
            .unwrap();
        let parked = track(&rig, load);
        rig.platform.run();
        assert!(parked.borrow().is_none());
        rig.register("macro", streaming(Ok(make_bars(3))));
        rig.platform.run();
        assert_eq!(parked.borrow().clone().unwrap().unwrap().unwrap().len(), 3);
    }
}
