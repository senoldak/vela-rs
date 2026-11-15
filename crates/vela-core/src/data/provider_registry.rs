//! The registry of named data providers: registration, the eager symbol index,
//! and the resolution of `name:SYMBOL` / bare symbols to the provider serving them.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::task::{Context, Poll, Waker};

use super::{default_member_of, is_group_row, is_js_space};
use crate::ports::data_provider::{
    DataProvider, ProviderCapabilities, ProviderInfo, SymbolDescriptor,
};
use crate::ports::market_data::UnresolvedSymbol;
use crate::runtime::{LocalBoxFuture, OneshotSender, Platform, Unsubscribe, oneshot};

/// Run `future` up to its first suspension point right now, as a JavaScript
/// `async` function does: whatever it does before its first `await` happens at
/// call time, not when the executor first polls it.
pub(crate) fn eager<T: 'static>(
    mut future: LocalBoxFuture<'static, T>,
) -> LocalBoxFuture<'static, T> {
    let mut cx = Context::from_waker(Waker::noop());
    match Pin::as_mut(&mut future).poll(&mut cx) {
        Poll::Ready(value) => Box::pin(std::future::ready(value)),
        Poll::Pending => future,
    }
}

/// A symbol resolved to a registered provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// Registered provider name (normalized, lower-case).
    pub provider: String,
    /// Ticker the provider expects (any `provider:` prefix stripped, any `.ext` kept).
    pub ticker: String,
}

/// The parts of a raw symbol string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSymbol {
    /// Lower-cased provider segment, or `None` when the symbol is bare.
    pub provider: Option<String>,
    /// Everything after the `provider:` prefix (includes any `.ext`).
    pub ticker: String,
    /// Best-effort trailing `.ext` token (provider-defined, never used for routing).
    pub ext: Option<String>,
}

/// How [`ProviderRegistry::resolve`] picks among providers.
#[derive(Clone, Copy, Debug, Default)]
pub struct ResolveOptions<'a> {
    /// A provider tried first for bare symbols (the chart's own).
    pub default: Option<&'a str>,
    /// Let a sole registered provider that cannot be indexed serve a bare symbol.
    pub lenient: bool,
}

fn norm_name(n: &str) -> String {
    n.trim_matches(is_js_space).to_lowercase()
}

fn norm_ticker(t: &str) -> String {
    t.trim_matches(is_js_space).to_uppercase()
}

fn is_prefix_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.'
}

/// Split a raw symbol string into its parts: THE symbol grammar, shared by
/// everything that reads one. The provider prefix may carry a dotted qualifier:
/// a venue's regional/market variant is its own provider (`BINANCE.US:BTCUSDT`),
/// never a modifier on the base venue. Case-insensitive on the prefix
/// (normalized lower).
pub fn parse_symbol(raw: &str) -> ParsedSymbol {
    let s = raw.trim_matches(is_js_space);
    let bare = || ParsedSymbol {
        provider: None,
        ticker: s.to_owned(),
        ext: None,
    };
    // `.` of the original grammar does not match a line terminator.
    if s.is_empty() || s.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return bare();
    }
    let (provider, ticker) = match s.split_once(':') {
        Some((prefix, rest))
            if !prefix.is_empty() && prefix.chars().all(is_prefix_char) && !rest.is_empty() =>
        {
            (Some(norm_name(prefix)), rest)
        }
        _ => (None, s),
    };
    let ext = ticker
        .rfind('.')
        .filter(|&dot| dot > 0)
        .map(|dot| ticker[dot + 1..].to_owned());
    ParsedSymbol {
        provider,
        ticker: ticker.to_owned(),
        ext,
    }
}

/// Resolves once an index build finishes, for any number of waiters.
#[derive(Default)]
struct Settle {
    done: Cell<bool>,
    waiters: RefCell<Vec<OneshotSender<()>>>,
}

impl Settle {
    fn finish(&self) {
        self.done.set(true);
        let waiters = std::mem::take(&mut *self.waiters.borrow_mut());
        for waiter in waiters {
            waiter.send(());
        }
    }

    fn wait(&self) -> LocalBoxFuture<'static, ()> {
        if self.done.get() {
            return Box::pin(std::future::ready(()));
        }
        let (tx, rx) = oneshot();
        self.waiters.borrow_mut().push(tx);
        Box::pin(rx)
    }
}

struct Entry {
    name: String,
    provider: Rc<dyn DataProvider>,
    /// Tells a replaced registration from its predecessor, so a late index
    /// build never lands on the wrong provider.
    generation: u64,
    /// The provider implements `list_symbols`.
    enumerates: bool,
    /// Normalized tickers this provider serves; `None` = not (yet) enumerated.
    index: Option<HashSet<String>>,
    /// `PREFIX:TICKER` (normalized) to descriptor, for symbols declaring a listing prefix.
    prefix_index: Option<HashMap<String, SymbolDescriptor>>,
    /// Normalized ticker to descriptor (canonical display lookups).
    by_ticker: Option<HashMap<String, SymbolDescriptor>>,
    descriptors: Vec<SymbolDescriptor>,
    /// The index build finished (success OR failure).
    settled: bool,
    settle: Rc<Settle>,
}

type ChangeListener = Rc<RefCell<Box<dyn FnMut(bool)>>>;
type UnresolvedListener = Rc<RefCell<Box<dyn FnMut(&UnresolvedSymbol)>>>;

#[derive(Default)]
struct State {
    /// Insertion order = registration order (relied on by bare resolution). A
    /// re-registered name keeps its place.
    entries: Vec<Entry>,
    /// Listeners fired on any registry change; `settled` = an index build just finished.
    change_listeners: Vec<(u64, ChangeListener)>,
    /// Cancellers for the currently parked `when_resolvable` waits.
    waits: BTreeMap<u64, Rc<dyn Fn()>>,
    /// Listeners told when a parked symbol stays unresolvable after an index settles.
    unresolved: Vec<(u64, UnresolvedListener)>,
    next_id: u64,
}

impl State {
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    fn names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.name.clone()).collect()
    }

    /// Registration order, with the `default` provider (when registered) first.
    fn candidate_order(&self, default: Option<&str>) -> Vec<String> {
        let names = self.names();
        if let Some(default) = default.filter(|d| !d.is_empty()) {
            let d = norm_name(default);
            if self.entry(&d).is_some() {
                return std::iter::once(d.clone())
                    .chain(names.into_iter().filter(|n| *n != d))
                    .collect();
            }
        }
        names
    }

    /// What a resolved descriptor LOADS: itself, unless it is a GROUP row
    /// (listed, never served), which loads its default member (single
    /// `default`, else first listed). A memberless group keeps its own ticker
    /// and fails downstream like any unknown symbol.
    fn loadable_ticker(&self, name: &str, d: &SymbolDescriptor) -> String {
        if !is_group_row(d) {
            return d.ticker.clone();
        }
        let pool: &[SymbolDescriptor] = self.entry(name).map_or(&[], |e| &e.descriptors);
        default_member_of(pool, d).map_or_else(|| d.ticker.clone(), |m| m.ticker.clone())
    }

    fn descriptor_of(&self, resolved: &Resolved) -> Option<&SymbolDescriptor> {
        self.entry(&resolved.provider)?
            .by_ticker
            .as_ref()?
            .get(&norm_ticker(&resolved.ticker))
    }
}

struct Inner {
    platform: Rc<dyn Platform>,
    state: RefCell<State>,
}

/// Holds the registered data providers, builds each provider's symbol index at
/// registration (eager), and resolves a symbol string to `{ provider, ticker }`.
///
/// Resolution: an explicit `name:SYMBOL` prefix routes to that provider (no
/// index needed); a prefix that is no provider name routes through the
/// descriptors' declared LISTING prefix (`NASDAQ:AAPL` goes to the provider
/// indexing Nasdaq-listed AAPL; strict, so a wrong venue resolves to nothing); a
/// bare `SYMBOL` routes to the first provider, in **registration order** (the
/// chart's default tried first), whose index contains it. `lenient` mode (used
/// for the primary chart load) lets a *sole* registered provider serve a bare
/// symbol optimistically before its index is built, so first paint is not gated
/// on enumerating thousands of symbols.
///
/// A cheap handle: clones share one registry.
#[derive(Clone)]
pub struct ProviderRegistry {
    inner: Rc<Inner>,
}

impl ProviderRegistry {
    pub fn new(platform: Rc<dyn Platform>) -> Self {
        Self {
            inner: Rc::new(Inner {
                platform,
                state: RefCell::new(State::default()),
            }),
        }
    }

    fn from_weak(weak: &Weak<Inner>) -> Option<Self> {
        weak.upgrade().map(|inner| Self { inner })
    }

    /// Register (or replace) a provider; the returned future settles when its
    /// index is built.
    pub fn register(
        &self,
        raw_name: &str,
        provider: Rc<dyn DataProvider>,
    ) -> LocalBoxFuture<'static, ()> {
        let name = norm_name(raw_name);
        let list = provider.list_symbols();
        let settle = Rc::new(Settle::default());
        let generation = {
            let mut state = self.inner.state.borrow_mut();
            let generation = state.next_id();
            let entry = Entry {
                name: name.clone(),
                provider,
                generation,
                enumerates: list.is_some(),
                index: None,
                prefix_index: None,
                by_ticker: None,
                descriptors: Vec::new(),
                settled: false,
                settle: settle.clone(),
            };
            match state.entries.iter_mut().find(|e| e.name == name) {
                Some(existing) => *existing = entry,
                None => state.entries.push(entry),
            }
            generation
        };
        let weak = Rc::downgrade(&self.inner);
        let build_settle = settle.clone();
        self.inner.platform.spawn(Box::pin(async move {
            let outcome = match list {
                Some(list) => Some(list.await),
                None => None,
            };
            Self::finish_index(&weak, &name, generation, outcome);
            build_settle.finish();
        }));
        // Re-check parked loads after the burst: batches a synchronous run of
        // registrations so a multi-provider bare symbol is not resolved against
        // the first one before the rest are in. An explicit `name:SYMBOL`
        // resolves here (no index wait).
        let weak = Rc::downgrade(&self.inner);
        self.inner.platform.spawn(Box::pin(async move {
            if let Some(registry) = Self::from_weak(&weak) {
                registry.notify(false);
            }
        }));
        settle.wait()
    }

    /// Land the result of an index build on the entry it was started for.
    fn finish_index(
        weak: &Weak<Inner>,
        name: &str,
        generation: u64,
        outcome: Option<Result<Vec<SymbolDescriptor>, crate::runtime::DataError>>,
    ) {
        let Some(registry) = Self::from_weak(weak) else {
            return;
        };
        {
            let mut state = registry.inner.state.borrow_mut();
            if let Some(entry) = state
                .entries
                .iter_mut()
                .find(|e| e.name == name && e.generation == generation)
            {
                // An enumeration failure is non-fatal: explicit-prefix routing
                // still works, and bare resolution falls back to the lenient
                // sole-provider path.
                if let Some(Ok(symbols)) = outcome {
                    let mut index = HashSet::new();
                    let mut by_ticker = HashMap::new();
                    let mut prefix_index = HashMap::new();
                    for s in &symbols {
                        let ticker = norm_ticker(&s.ticker);
                        index.insert(ticker.clone());
                        by_ticker.insert(ticker.clone(), s.clone());
                        if let Some(prefix) = s.prefix.as_deref().filter(|p| !p.is_empty()) {
                            prefix_index
                                .insert(format!("{}:{ticker}", norm_name(prefix)), s.clone());
                        }
                    }
                    entry.descriptors = symbols;
                    entry.index = Some(index);
                    entry.by_ticker = Some(by_ticker);
                    entry.prefix_index = Some(prefix_index);
                } else {
                    entry.index = None;
                }
                entry.settled = true;
            }
        }
        registry.notify(true);
    }

    fn notify(&self, settled: bool) {
        let snapshot: Vec<(u64, ChangeListener)> =
            self.inner.state.borrow().change_listeners.clone();
        for (id, listener) in snapshot {
            let still_registered = self
                .inner
                .state
                .borrow()
                .change_listeners
                .iter()
                .any(|(other, _)| *other == id);
            if !still_registered {
                continue;
            }
            // A listener already running (re-entered through `unregister`) is skipped.
            if let Ok(mut listener) = listener.try_borrow_mut() {
                (*listener)(settled);
            }
        }
    }

    /// Fires on each registry change (`settled` true after an index build finishes).
    pub fn on_change(&self, listener: impl FnMut(bool) + 'static) -> Unsubscribe {
        let id = {
            let mut state = self.inner.state.borrow_mut();
            let id = state.next_id();
            let boxed: Box<dyn FnMut(bool)> = Box::new(listener);
            state
                .change_listeners
                .push((id, Rc::new(RefCell::new(boxed))));
            id
        };
        let weak = Rc::downgrade(&self.inner);
        Box::new(move || Self::remove_listener(&weak, id))
    }

    fn remove_listener(weak: &Weak<Inner>, id: u64) {
        if let Some(inner) = weak.upgrade() {
            inner
                .state
                .borrow_mut()
                .change_listeners
                .retain(|(other, _)| *other != id);
        }
    }

    pub fn unregister(&self, raw_name: &str) {
        let name = norm_name(raw_name);
        self.inner
            .state
            .borrow_mut()
            .entries
            .retain(|e| e.name != name);
        // Notify so any parked `when_resolvable` listener re-evaluates against
        // the new set (and so resolution never depends on a removed provider).
        self.notify(false);
    }

    pub fn has(&self, raw_name: &str) -> bool {
        self.inner
            .state
            .borrow()
            .entry(&norm_name(raw_name))
            .is_some()
    }

    pub fn get(&self, raw_name: &str) -> Option<Rc<dyn DataProvider>> {
        self.inner
            .state
            .borrow()
            .entry(&norm_name(raw_name))
            .map(|e| e.provider.clone())
    }

    /// Registered provider names in registration order.
    pub fn names(&self) -> Vec<String> {
        self.inner.state.borrow().names()
    }

    pub fn parse(&self, raw: &str) -> ParsedSymbol {
        parse_symbol(raw)
    }

    /// Resolve a symbol to `{ provider, ticker }`, or `None` when nothing can
    /// serve it yet. Explicit prefix: that provider (must be registered), else a
    /// provider whose index declares that LISTING prefix for that ticker
    /// (`NASDAQ:AAPL` routes to the provider serving Nasdaq-listed AAPL;
    /// TradingView-strict, so `NYSE:AAPL` resolves to nothing rather than
    /// auto-correcting). Bare: the first indexed provider (registration order,
    /// `opts.default` first) that contains the ticker; `opts.lenient`
    /// additionally allows a sole registered provider before its index exists.
    pub fn resolve(&self, raw: &str, opts: ResolveOptions<'_>) -> Option<Resolved> {
        let ParsedSymbol {
            provider, ticker, ..
        } = self.parse(raw);
        let state = self.inner.state.borrow();
        if let Some(provider) = provider {
            if let Some(entry) = state.entry(&provider) {
                // Even the explicit form translates a GROUP row: `cme:ES` must
                // load the root's default member, never dead-end on the
                // unloadable listing.
                let d = entry
                    .by_ticker
                    .as_ref()
                    .and_then(|m| m.get(&norm_ticker(&ticker)));
                let ticker = match d {
                    Some(d) if is_group_row(d) => state.loadable_ticker(&provider, d),
                    _ => ticker,
                };
                return Some(Resolved { provider, ticker });
            }
            // Listing-prefix routing: matched against the DESCRIPTOR's declared
            // prefix, so the verdict comes from the data, never from string
            // similarity. The resolved ticker is the descriptor's spelling
            // (canonical casing for `nyse:ibm` to `IBM`).
            let key = format!("{provider}:{}", norm_ticker(&ticker));
            for name in state.candidate_order(opts.default) {
                let d = state
                    .entry(&name)
                    .and_then(|e| e.prefix_index.as_ref())
                    .and_then(|m| m.get(&key));
                if let Some(d) = d {
                    let ticker = state.loadable_ticker(&name, d);
                    return Some(Resolved {
                        provider: name,
                        ticker,
                    });
                }
            }
            return None;
        }

        let norm = norm_ticker(&ticker);
        for name in state.candidate_order(opts.default) {
            let Some(entry) = state.entry(&name) else {
                continue;
            };
            if entry.index.as_ref().is_some_and(|i| i.contains(&norm)) {
                let d = entry.by_ticker.as_ref().and_then(|m| m.get(&norm));
                // Group rows are listed, never loadable: a bare root (`ES`)
                // resolves to the group's default member. Anything else keeps
                // the CALLER's spelling.
                let ticker = match d {
                    Some(d) if is_group_row(d) => state.loadable_ticker(&name, d),
                    _ => ticker.clone(),
                };
                return Some(Resolved {
                    provider: name,
                    ticker,
                });
            }
        }

        // Lenient (primary load only): a SOLE registered provider that cannot be
        // indexed (no `list_symbols`, or enumeration failed) is used
        // optimistically: there is no index to consult, so the `get_bars` call
        // itself confirms support. An enumerable provider is NOT short-circuited:
        // we wait for its index so an unsupported symbol stays parked instead of
        // rendering an empty chart.
        if opts.lenient
            && let [only] = state.entries.as_slice()
            && only.settled
            && only.index.is_none()
        {
            return Some(Resolved {
                provider: only.name.clone(),
                ticker,
            });
        }
        None
    }

    /// Resolve now if possible, else resolve later, re-attempting after each
    /// provider settles. The future stays pending until some registered provider
    /// can serve the symbol (a diagnostic is emitted on each unsuccessful round).
    pub fn when_resolvable(
        &self,
        raw: &str,
        default: Option<&str>,
    ) -> LocalBoxFuture<'static, Resolved> {
        let opts = ResolveOptions {
            default,
            lenient: true,
        };
        if let Some(immediate) = self.resolve(raw, opts) {
            return Box::pin(std::future::ready(immediate));
        }
        let (tx, rx) = oneshot();
        let weak = Rc::downgrade(&self.inner);
        let (listener_id, wait_id) = {
            let mut state = self.inner.state.borrow_mut();
            (state.next_id(), state.next_id())
        };
        // A parked wait outlives its chart unless it is cancellable: every
        // rebuild over an unservable symbol would otherwise leave its listener
        // behind for the page's life.
        let stop: Rc<dyn Fn()> = {
            let weak = weak.clone();
            Rc::new(move || {
                Self::remove_listener(&weak, listener_id);
                if let Some(inner) = weak.upgrade() {
                    inner.state.borrow_mut().waits.remove(&wait_id);
                }
            })
        };
        let raw = raw.to_owned();
        let default = default.map(str::to_owned);
        let listener_stop = stop.clone();
        let listener = move |settled: bool| {
            let Some(registry) = Self::from_weak(&weak) else {
                return;
            };
            let opts = ResolveOptions {
                default: default.as_deref(),
                lenient: true,
            };
            if let Some(resolved) = registry.resolve(&raw, opts) {
                listener_stop();
                tx.send(resolved);
            } else if settled && registry.all_settled() {
                // Report only once EVERY registered provider has finished
                // indexing. One index settling proves nothing: with several
                // providers the first to finish is usually not the one that
                // serves the symbol, and reporting there is a false verdict (the
                // user sees "no provider serves X" on a symbol that resolves a
                // moment later).
                registry.report_unresolved(&raw);
            }
        };
        {
            let mut state = self.inner.state.borrow_mut();
            let boxed: Box<dyn FnMut(bool)> = Box::new(listener);
            state
                .change_listeners
                .push((listener_id, Rc::new(RefCell::new(boxed))));
            state.waits.insert(wait_id, stop);
        }
        Box::pin(rx)
    }

    /// Every registered provider has finished building its index (or failed trying).
    fn all_settled(&self) -> bool {
        self.inner.state.borrow().entries.iter().all(|e| e.settled)
    }

    /// Abandon every parked wait (chart/feed teardown). Their futures simply
    /// never resolve.
    pub fn cancel_waits(&self) {
        let stops: Vec<Rc<dyn Fn()>> = self.inner.state.borrow().waits.values().cloned().collect();
        for stop in stops {
            stop();
        }
    }

    /// Notified when a parked symbol is still unresolvable after an index settles.
    pub fn on_unresolved(&self, listener: Box<dyn FnMut(&UnresolvedSymbol)>) -> Unsubscribe {
        let id = {
            let mut state = self.inner.state.borrow_mut();
            let id = state.next_id();
            state.unresolved.push((id, Rc::new(RefCell::new(listener))));
            id
        };
        let weak = Rc::downgrade(&self.inner);
        Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner
                    .state
                    .borrow_mut()
                    .unresolved
                    .retain(|(other, _)| *other != id);
            }
        })
    }

    fn report_unresolved(&self, raw: &str) {
        let names = self.names();
        let listed = if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        };
        self.inner.platform.warn(&format!(
            "[vela] symbol \"{raw}\" not yet resolvable \u{2014} registered providers: [{listed}]. Register a provider that serves it."
        ));
        let listeners: Vec<UnresolvedListener> = self
            .inner
            .state
            .borrow()
            .unresolved
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        let info = UnresolvedSymbol {
            symbol: raw.to_owned(),
            providers: names,
        };
        for listener in listeners {
            if let Ok(mut listener) = listener.try_borrow_mut() {
                (*listener)(&info);
            }
        }
    }

    /// The DISPLAY prefix for a resolved symbol: the descriptor's LISTING prefix
    /// when the data declares one, else the provider name. This is the single
    /// seam every label (legend chip, committed/persisted `PREFIX:TICKER`
    /// strings) derives from, which is what makes a non-canonical spelling
    /// impossible to display: the form always comes from the data, never from
    /// what was typed.
    pub fn display_prefix_of(&self, resolved: &Resolved) -> String {
        let state = self.inner.state.borrow();
        state
            .descriptor_of(resolved)
            .and_then(|d| d.prefix.clone())
            .unwrap_or_else(|| resolved.provider.clone())
    }

    /// The canonical `PREFIX:TICKER` string for a resolved symbol (descriptor spellings).
    pub fn canonical_symbol(&self, resolved: &Resolved) -> String {
        let state = self.inner.state.borrow();
        let d = state.descriptor_of(resolved);
        let prefix = d
            .and_then(|d| d.prefix.as_deref())
            .unwrap_or(&resolved.provider);
        let ticker = d.map_or(resolved.ticker.as_str(), |d| d.ticker.as_str());
        format!("{prefix}:{ticker}")
    }

    /// The icon URL for a DESCRIPTOR, routed to its OWNING provider's resolver
    /// (the provider owns the knowledge of where its asset class's icons live).
    /// A missing provider or an absent resolver means "no icon": the shells'
    /// initials badge takes over, never an error.
    pub fn symbol_icon_of(&self, d: &SymbolDescriptor) -> Option<String> {
        let provider = {
            let state = self.inner.state.borrow();
            let entry = match d.provider.as_deref().filter(|p| !p.is_empty()) {
                Some(name) => state.entry(&norm_name(name)),
                None => state.entries.first(),
            };
            entry.map(|e| e.provider.clone())
        };
        provider?.resolve_symbol_icon(d)
    }

    /// The icon URL for a resolved SYMBOL (the statusline/object-tree path): find
    /// its descriptor and route to the resolver. Before the index settles (or
    /// for a lenient sole-provider resolution) a minimal synthetic descriptor is
    /// offered: crypto resolvers can answer from the ticker alone.
    pub fn symbol_icon(&self, resolved: Option<&Resolved>) -> Option<String> {
        let resolved = resolved?;
        let found = self.inner.state.borrow().descriptor_of(resolved).cloned();
        let mut d = found.unwrap_or_else(|| SymbolDescriptor::new(resolved.ticker.clone()));
        // The indexed descriptor carries no owner annotation (that happens on
        // aggregation): stamp the resolved provider so the resolver sees it.
        if d.provider.is_none() {
            d.provider = Some(resolved.provider.clone());
        }
        self.symbol_icon_of(&d)
    }

    /// Indexed symbols for one provider (or all, concatenated), for autocomplete.
    pub fn symbols_of(&self, raw_name: Option<&str>) -> Vec<SymbolDescriptor> {
        let state = self.inner.state.borrow();
        match raw_name {
            Some(name) => state
                .entry(&norm_name(name))
                .map(|e| e.descriptors.clone())
                .unwrap_or_default(),
            None => state
                .entries
                .iter()
                .flat_map(|e| {
                    e.descriptors.iter().map(|d| SymbolDescriptor {
                        provider: Some(e.name.clone()),
                        ..d.clone()
                    })
                })
                .collect(),
        }
    }

    /// Provider metadata: the provider's own `info()` if present, else
    /// synthesized from what it implements.
    ///
    /// The synthesis cannot ask a provider whether it streams without opening a
    /// stream, so `stream` reads `false` there: a provider that streams states
    /// it through `info()`.
    pub fn info_of(&self, raw_name: &str) -> ProviderInfo {
        let name = norm_name(raw_name);
        let found = self
            .inner
            .state
            .borrow()
            .entry(&name)
            .map(|e| (e.name.clone(), e.provider.clone(), e.enumerates));
        let Some((entry_name, provider, enumerates)) = found else {
            return ProviderInfo {
                name,
                display_name: None,
                requires_api_key: None,
                supported_timeframes: None,
                capabilities: ProviderCapabilities::default(),
            };
        };
        provider.info().unwrap_or_else(|| ProviderInfo {
            name: entry_name,
            display_name: None,
            requires_api_key: None,
            supported_timeframes: None,
            capabilities: ProviderCapabilities {
                enumerate: enumerates,
                stream: provider.supports_stream(),
                symbol_info: provider.supports_symbol_info(),
            },
        })
    }

    /// Resolves when every currently registered provider's index has settled.
    pub fn index_ready(&self) -> LocalBoxFuture<'static, ()> {
        let settles: Vec<Rc<Settle>> = self
            .inner
            .state
            .borrow()
            .entries
            .iter()
            .map(|e| e.settle.clone())
            .collect();
        Box::pin(async move {
            for settle in settles {
                settle.wait().await;
            }
        })
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Fake providers shared by the data layer's tests.

    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use serde_json::{Map, Value};

    use crate::model::Ohlcv;
    use crate::ports::data_provider::{DataProvider, ProviderInfo, SymbolDescriptor};
    use crate::ports::market_data::{BarRange, BarSink, SymbolInfo};
    use crate::runtime::{DataError, LocalBoxFuture, Unsubscribe};

    pub(crate) const START: i64 = 1_700_000_000_000;

    pub(crate) fn make_bars(n: usize) -> Vec<Ohlcv> {
        (0..n)
            .map(|i| {
                let price = 100.0 + (i as f64 / 4.0).sin() * 5.0;
                Ohlcv::new(
                    START + i as i64 * 3_600_000,
                    price,
                    price + 1.0,
                    price - 1.0,
                    price,
                    Some(1.0),
                )
            })
            .collect()
    }

    /// A neutral fake provider. `served` = tickers `get_bars` returns data for;
    /// `list` = what `list_symbols` enumerates (defaults to `served`).
    pub(crate) struct FakeProvider {
        served: Vec<String>,
        list: Option<Vec<SymbolDescriptor>>,
        sym_info: Option<Map<String, Value>>,
        throws: bool,
        streams: bool,
        icon: Option<fn(&str) -> Option<String>>,
        info: Option<ProviderInfo>,
        pub(crate) calls: RefCell<Vec<(String, String)>>,
        pub(crate) ranges: RefCell<Vec<BarRange>>,
        pub(crate) sub_sessions: RefCell<Vec<Option<String>>>,
        pub(crate) icon_seen: RefCell<Vec<SymbolDescriptor>>,
        pub(crate) subscribed: Cell<usize>,
    }

    impl FakeProvider {
        pub(crate) fn new(served: &[&str]) -> Self {
            Self {
                served: served.iter().map(|s| s.to_uppercase()).collect(),
                list: Some(served.iter().map(|t| SymbolDescriptor::new(*t)).collect()),
                sym_info: None,
                throws: false,
                streams: false,
                icon: None,
                info: None,
                calls: RefCell::default(),
                ranges: RefCell::default(),
                sub_sessions: RefCell::default(),
                icon_seen: RefCell::default(),
                subscribed: Cell::new(0),
            }
        }

        /// Serves `served` but enumerates only `list`.
        pub(crate) fn listing(mut self, list: &[&str]) -> Self {
            self.list = Some(list.iter().map(|t| SymbolDescriptor::new(*t)).collect());
            self
        }

        pub(crate) fn descriptors(mut self, list: Vec<SymbolDescriptor>) -> Self {
            self.list = Some(list);
            self
        }

        pub(crate) fn not_enumerable(mut self) -> Self {
            self.list = None;
            self
        }

        pub(crate) fn sym_info(mut self, info: Value) -> Self {
            self.sym_info = info.as_object().cloned();
            self
        }

        pub(crate) fn throws(mut self) -> Self {
            self.throws = true;
            self
        }

        pub(crate) fn streaming(mut self) -> Self {
            self.streams = true;
            self
        }

        pub(crate) fn icons(mut self, icon: fn(&str) -> Option<String>) -> Self {
            self.icon = Some(icon);
            self
        }

        pub(crate) fn with_info(mut self, info: ProviderInfo) -> Self {
            self.info = Some(info);
            self
        }

        pub(crate) fn rc(self) -> Rc<Self> {
            Rc::new(self)
        }

        pub(crate) fn tickers(&self) -> Vec<String> {
            self.calls.borrow().iter().map(|(t, _)| t.clone()).collect()
        }
    }

    impl DataProvider for FakeProvider {
        fn supports_stream(&self) -> bool {
            self.streams
        }

        fn supports_symbol_info(&self) -> bool {
            self.sym_info.is_some()
        }

        fn get_bars(
            &self,
            ticker: &str,
            timeframe: &str,
            range: &BarRange,
        ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
            self.calls
                .borrow_mut()
                .push((ticker.to_owned(), timeframe.to_owned()));
            self.ranges.borrow_mut().push(range.clone());
            let result = if self.throws {
                Err(DataError::other("boom"))
            } else if !self.served.contains(&ticker.to_uppercase()) {
                Ok(Vec::new())
            } else {
                Ok(make_bars(range.limit.unwrap_or(10)))
            };
            Box::pin(async move { result })
        }

        fn list_symbols(
            &self,
        ) -> Option<LocalBoxFuture<'static, Result<Vec<SymbolDescriptor>, DataError>>> {
            let list = self.list.clone()?;
            Some(Box::pin(async move { Ok(list) }))
        }

        fn info(&self) -> Option<ProviderInfo> {
            self.info.clone()
        }

        fn get_symbol_info(
            &self,
            ticker: &str,
        ) -> Option<LocalBoxFuture<'static, Result<Option<SymbolInfo>, DataError>>> {
            let extra = self.sym_info.clone()?;
            let info = SymbolInfo {
                ticker: ticker.to_owned(),
                extra,
                ..SymbolInfo::default()
            };
            Some(Box::pin(async move { Ok(Some(info)) }))
        }

        fn resolve_symbol_icon(&self, symbol: &SymbolDescriptor) -> Option<String> {
            self.icon_seen.borrow_mut().push(symbol.clone());
            self.icon.and_then(|f| f(&symbol.ticker))
        }

        fn subscribe(
            &self,
            _ticker: &str,
            _timeframe: &str,
            _on_bar: BarSink,
            session: Option<&str>,
        ) -> Option<Unsubscribe> {
            if !self.streams {
                return None;
            }
            self.sub_sessions
                .borrow_mut()
                .push(session.map(str::to_owned));
            self.subscribed.set(self.subscribed.get() + 1);
            Some(Box::new(|| {}))
        }
    }
}

#[cfg(test)]
#[allow(clippy::let_underscore_future)] // registering needs no awaiting: the index builds on the platform
mod tests {
    use std::cell::Cell;

    use serde_json::json;

    use super::test_support::FakeProvider;
    use super::*;
    use crate::runtime::DataError;
    use crate::runtime::testing::ManualPlatform;

    fn registry() -> (ManualPlatform, ProviderRegistry) {
        let platform = ManualPlatform::new(0.0);
        let registry = ProviderRegistry::new(platform.handle());
        (platform, registry)
    }

    fn resolve(registry: &ProviderRegistry, raw: &str) -> Option<(String, String)> {
        registry
            .resolve(raw, ResolveOptions::default())
            .map(|r| (r.provider, r.ticker))
    }

    fn pair(provider: &str, ticker: &str) -> Option<(String, String)> {
        Some((provider.to_owned(), ticker.to_owned()))
    }

    fn descriptor(ticker: &str, prefix: &str) -> SymbolDescriptor {
        SymbolDescriptor {
            kind: Some("stock".into()),
            prefix: Some(prefix.into()),
            ..SymbolDescriptor::new(ticker)
        }
    }

    fn member(ticker: &str, group: &str, default: bool) -> SymbolDescriptor {
        SymbolDescriptor {
            kind: Some("futures".into()),
            group: Some(group.into()),
            prefix: Some("CME".into()),
            default: default.then_some(true),
            ..SymbolDescriptor::new(ticker)
        }
    }

    fn root(ticker: &str) -> SymbolDescriptor {
        SymbolDescriptor {
            kind: Some("root".into()),
            group: Some(ticker.into()),
            prefix: Some("CME".into()),
            ..SymbolDescriptor::new(ticker)
        }
    }

    #[test]
    fn parse_symbol_splits_prefix_ticker_and_ext() {
        let p = parse_symbol("  Binance.US:BTCUSDT.P ");
        assert_eq!(p.provider.as_deref(), Some("binance.us"));
        assert_eq!(p.ticker, "BTCUSDT.P");
        assert_eq!(p.ext.as_deref(), Some("P"));
        let bare = parse_symbol("BTCUSDT");
        assert_eq!(
            (bare.provider, bare.ticker, bare.ext),
            (None, "BTCUSDT".into(), None)
        );
        // A leading dot is not an extension; a trailing one is an empty one.
        assert_eq!(parse_symbol(".X").ext, None);
        assert_eq!(parse_symbol("A.").ext.as_deref(), Some(""));
    }

    #[test]
    fn parse_symbol_degenerate_inputs_stay_bare() {
        let empty = parse_symbol("   ");
        assert_eq!((empty.provider, empty.ticker.as_str()), (None, ""));
        // Nothing after the colon: the whole string is the ticker.
        let colon = parse_symbol("binance:");
        assert_eq!((colon.provider, colon.ticker.as_str()), (None, "binance:"));
        // A prefix with a character outside the grammar is not a prefix.
        let odd = parse_symbol("a-b:C");
        assert_eq!((odd.provider, odd.ticker.as_str()), (None, "a-b:C"));
        // A line break defeats the whole pattern, as `.` does not match it.
        let multi = parse_symbol("binance:BT\nC");
        assert_eq!(
            (multi.provider, multi.ticker.as_str()),
            (None, "binance:BT\nC")
        );
        // Only the first colon splits.
        let nested = parse_symbol("a:b:c");
        assert_eq!(
            (nested.provider.as_deref(), nested.ticker.as_str()),
            (Some("a"), "b:c")
        );
    }

    #[test]
    fn bare_symbols_resolve_in_registration_order_with_the_default_first() {
        let (platform, registry) = registry();
        let _ = registry.register("hyperliquid", FakeProvider::new(&["BTC"]).rc());
        let _ = registry.register("Binance", FakeProvider::new(&["BTC"]).rc());
        platform.run();
        assert_eq!(registry.names(), ["hyperliquid", "binance"]);
        assert_eq!(resolve(&registry, "BTC"), pair("hyperliquid", "BTC"));
        let opts = ResolveOptions {
            default: Some("BINANCE"),
            lenient: false,
        };
        assert_eq!(
            registry.resolve("btc", opts),
            Some(Resolved {
                provider: "binance".into(),
                ticker: "btc".into()
            })
        );
        // An unregistered default changes nothing.
        let ghost = ResolveOptions {
            default: Some("ghost"),
            lenient: false,
        };
        assert_eq!(
            registry.resolve("BTC", ghost).map(|r| r.provider),
            Some("hyperliquid".into())
        );
    }

    #[test]
    fn an_explicit_prefix_resolves_without_an_index_and_unknown_prefixes_do_not() {
        let (platform, registry) = registry();
        let _ = registry.register("binance", FakeProvider::new(&["BTCUSDT"]).rc());
        // Not even indexed yet: the prefix still routes.
        assert_eq!(
            resolve(&registry, "binance:ETHUSDT"),
            pair("binance", "ETHUSDT")
        );
        platform.run();
        assert_eq!(resolve(&registry, "NASDAQ:AAPL"), None);
        assert_eq!(resolve(&registry, "BTCUSDT"), pair("binance", "BTCUSDT"));
    }

    #[test]
    fn listing_prefixes_route_strictly_and_take_the_descriptor_spelling() {
        let (platform, registry) = registry();
        let _ = registry.register("binance", FakeProvider::new(&["BTCUSDT"]).rc());
        let _ = registry.register(
            "edgx",
            FakeProvider::new(&[])
                .descriptors(vec![
                    descriptor("AAPL", "NASDAQ"),
                    descriptor("IBM", "NYSE"),
                ])
                .rc(),
        );
        platform.run();
        assert_eq!(resolve(&registry, "NASDAQ:AAPL"), pair("edgx", "AAPL"));
        assert_eq!(resolve(&registry, "nyse:ibm"), pair("edgx", "IBM"));
        assert_eq!(resolve(&registry, "NYSE:AAPL"), None);
        assert_eq!(resolve(&registry, "edgx:AAPL"), pair("edgx", "AAPL"));
        let aapl = Resolved {
            provider: "edgx".into(),
            ticker: "AAPL".into(),
        };
        assert_eq!(registry.display_prefix_of(&aapl), "NASDAQ");
        assert_eq!(registry.canonical_symbol(&aapl), "NASDAQ:AAPL");
        let lower = Resolved {
            provider: "edgx".into(),
            ticker: "ibm".into(),
        };
        assert_eq!(registry.canonical_symbol(&lower), "NYSE:IBM");
        let btc = Resolved {
            provider: "binance".into(),
            ticker: "BTCUSDT".into(),
        };
        assert_eq!(registry.display_prefix_of(&btc), "binance");
        assert_eq!(registry.canonical_symbol(&btc), "binance:BTCUSDT");
    }

    #[test]
    fn group_rows_resolve_to_their_default_member_in_every_form() {
        let (platform, registry) = registry();
        let _ = registry.register(
            "lux",
            FakeProvider::new(&[])
                .descriptors(vec![
                    root("ES"),
                    member("ES1!", "ES", true),
                    member("ES2!", "ES", false),
                    root("NQ"),
                    member("NQ1!", "NQ", false),
                    member("NQ2!", "NQ", false),
                ])
                .rc(),
        );
        platform.run();
        assert_eq!(resolve(&registry, "ES"), pair("lux", "ES1!"));
        assert_eq!(resolve(&registry, "CME:ES"), pair("lux", "ES1!"));
        assert_eq!(resolve(&registry, "lux:ES"), pair("lux", "ES1!"));
        assert_eq!(resolve(&registry, "NQ"), pair("lux", "NQ1!"));
        assert_eq!(resolve(&registry, "ES2!"), pair("lux", "ES2!"));
        assert_eq!(resolve(&registry, "CME:ES2!"), pair("lux", "ES2!"));
    }

    #[test]
    fn a_memberless_group_keeps_its_own_ticker() {
        let (platform, registry) = registry();
        let _ = registry.register(
            "lux",
            FakeProvider::new(&[]).descriptors(vec![root("XX")]).rc(),
        );
        platform.run();
        assert_eq!(resolve(&registry, "XX"), pair("lux", "XX"));
    }

    #[test]
    fn dotted_provider_prefixes_and_market_suffixes_route() {
        let (platform, registry) = registry();
        let _ = registry.register("binance", FakeProvider::new(&["BTCUSDT", "BTCUSDT.P"]).rc());
        let _ = registry.register("binance.us", FakeProvider::new(&["BTCUSDT"]).rc());
        platform.run();
        assert_eq!(
            resolve(&registry, "BINANCE.US:BTCUSDT"),
            pair("binance.us", "BTCUSDT")
        );
        assert_eq!(
            resolve(&registry, "BINANCE:BTCUSDT.P"),
            pair("binance", "BTCUSDT.P")
        );
        assert_eq!(
            resolve(&registry, "binance.us:btcusdt").map(|r| r.0),
            Some("binance.us".into())
        );
        assert_eq!(
            resolve(&registry, "BTCUSDT.P").map(|r| r.0),
            Some("binance".into())
        );
    }

    #[test]
    fn lenient_serves_only_a_sole_provider_that_cannot_be_indexed() {
        let lenient = ResolveOptions {
            default: None,
            lenient: true,
        };
        // No `list_symbols` at all.
        let (platform, registry) = registry();
        let _ = registry.register("p", FakeProvider::new(&["X"]).not_enumerable().rc());
        assert_eq!(registry.resolve("X", lenient), None); // index not settled yet
        platform.run();
        assert_eq!(
            registry.resolve("X", lenient).map(|r| r.provider),
            Some("p".into())
        );
        assert_eq!(registry.resolve("X", ResolveOptions::default()), None);

        // An enumerable provider that does not list the symbol is not short-circuited.
        let (platform, registry) = registry_with_listing();
        platform.run();
        assert_eq!(registry.resolve("NOPE", lenient), None);

        // Two providers: never lenient.
        let (platform, registry) = registry_with_two_unindexed();
        platform.run();
        assert_eq!(registry.resolve("X", lenient), None);
    }

    fn registry_with_listing() -> (ManualPlatform, ProviderRegistry) {
        let (platform, registry) = registry();
        let _ = registry.register("p", FakeProvider::new(&["BTC"]).rc());
        (platform, registry)
    }

    fn registry_with_two_unindexed() -> (ManualPlatform, ProviderRegistry) {
        let (platform, registry) = registry();
        let _ = registry.register("a", FakeProvider::new(&["X"]).not_enumerable().rc());
        let _ = registry.register("b", FakeProvider::new(&["X"]).not_enumerable().rc());
        (platform, registry)
    }

    struct FailingList;

    impl DataProvider for FailingList {
        fn get_bars(
            &self,
            _ticker: &str,
            _timeframe: &str,
            _range: &crate::ports::market_data::BarRange,
        ) -> LocalBoxFuture<'static, Result<Vec<crate::model::Ohlcv>, DataError>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn list_symbols(
            &self,
        ) -> Option<LocalBoxFuture<'static, Result<Vec<SymbolDescriptor>, DataError>>> {
            Some(Box::pin(async {
                Err(DataError::other("enumeration down"))
            }))
        }
    }

    #[test]
    fn a_failed_enumeration_settles_without_an_index() {
        let (platform, registry) = registry();
        let settled = Rc::new(Cell::new(false));
        let flag = settled.clone();
        let ready = registry.register("p", Rc::new(FailingList));
        platform.spawn(Box::pin(async move {
            ready.await;
            flag.set(true);
        }));
        platform.run();
        assert!(settled.get());
        assert!(registry.symbols_of(Some("p")).is_empty());
        let lenient = ResolveOptions {
            default: None,
            lenient: true,
        };
        // The failure leaves the sole provider usable optimistically.
        assert_eq!(
            registry.resolve("ANY", lenient).map(|r| r.provider),
            Some("p".into())
        );
        assert_eq!(resolve(&registry, "p:ANY"), pair("p", "ANY"));
    }

    #[test]
    fn re_registering_replaces_in_place_and_ignores_the_stale_index() {
        let (platform, registry) = registry();
        let _ = registry.register("a", FakeProvider::new(&["OLD"]).rc());
        let _ = registry.register("b", FakeProvider::new(&["SAME"]).rc());
        let _ = registry.register("a", FakeProvider::new(&["NEW", "SAME"]).rc());
        platform.run();
        assert_eq!(registry.names(), ["a", "b"]);
        assert_eq!(resolve(&registry, "OLD"), None);
        assert_eq!(resolve(&registry, "NEW"), pair("a", "NEW"));
        assert_eq!(resolve(&registry, "SAME"), pair("a", "SAME")); // keeps its first slot
    }

    #[test]
    fn unregister_removes_the_provider_and_notifies() {
        let (platform, registry) = registry();
        let _ = registry.register("binance", FakeProvider::new(&["BTCUSDT"]).rc());
        platform.run();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = seen.clone();
        let off = registry.on_change(move |settled| log.borrow_mut().push(settled));
        registry.unregister("BINANCE");
        assert!(!registry.has("binance"));
        assert!(registry.get("binance").is_none());
        assert_eq!(resolve(&registry, "BTCUSDT"), None);
        assert_eq!(*seen.borrow(), vec![false]);
        off();
        registry.unregister("binance");
        assert_eq!(seen.borrow().len(), 1);
    }

    #[test]
    fn a_burst_of_registrations_notifies_after_all_are_in() {
        let (platform, registry) = registry();
        let names_at_notify = Rc::new(RefCell::new(Vec::new()));
        let (log, reg) = (names_at_notify.clone(), registry.clone());
        let _off =
            registry.on_change(move |settled| log.borrow_mut().push((settled, reg.names().len())));
        let _ = registry.register("a", FakeProvider::new(&["A"]).rc());
        let _ = registry.register("b", FakeProvider::new(&["B"]).rc());
        assert!(names_at_notify.borrow().is_empty()); // nothing fires synchronously
        platform.run();
        let log = names_at_notify.borrow();
        assert!(log.iter().all(|(_, n)| *n == 2));
        assert_eq!(log.iter().filter(|(s, _)| !*s).count(), 2);
        assert_eq!(log.iter().filter(|(s, _)| *s).count(), 2);
    }

    #[test]
    fn when_resolvable_parks_then_resumes_when_a_provider_serves_it() {
        let (platform, registry) = registry();
        let got = Rc::new(RefCell::new(None));
        let slot = got.clone();
        let wait = registry.when_resolvable("AAPL", None);
        platform.spawn(Box::pin(async move {
            *slot.borrow_mut() = Some(wait.await);
        }));
        platform.run();
        assert!(got.borrow().is_none());
        let _ = registry.register("binance", FakeProvider::new(&["BTCUSDT"]).rc());
        platform.run();
        assert!(got.borrow().is_none()); // indexed, and AAPL is not there
        let _ = registry.register("fmp", FakeProvider::new(&["AAPL"]).rc());
        platform.run();
        assert_eq!(
            *got.borrow(),
            Some(Resolved {
                provider: "fmp".into(),
                ticker: "AAPL".into()
            })
        );
    }

    #[test]
    fn unresolved_is_reported_once_every_index_settled_with_the_original_text() {
        let (platform, registry) = registry();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = seen.clone();
        let _off =
            registry.on_unresolved(Box::new(move |info| log.borrow_mut().push(info.clone())));
        let _wait = registry.when_resolvable("NOPE", None);
        let _ = registry.register("hyperliquid", FakeProvider::new(&["BTC"]).rc());
        platform.run();
        assert_eq!(
            *seen.borrow(),
            vec![UnresolvedSymbol {
                symbol: "NOPE".into(),
                providers: vec!["hyperliquid".into()]
            }]
        );
        assert_eq!(
            platform.warnings(),
            vec![
                "[vela] symbol \"NOPE\" not yet resolvable \u{2014} registered providers: [hyperliquid]. Register a provider that serves it."
            ]
        );
    }

    #[test]
    fn unresolved_text_names_no_providers_as_none() {
        let (platform, registry) = registry();
        let _wait = registry.when_resolvable("X", None);
        registry.report_unresolved("X");
        assert!(platform.warnings()[0].contains("[none]"));
    }

    #[test]
    fn cancel_waits_drops_listeners_and_reports_nothing_more() {
        let (platform, registry) = registry();
        let seen = Rc::new(Cell::new(0));
        let count = seen.clone();
        let _off = registry.on_unresolved(Box::new(move |_| count.set(count.get() + 1)));
        let wait = registry.when_resolvable("NOPE", None);
        let resolved = Rc::new(Cell::new(false));
        let flag = resolved.clone();
        platform.spawn(Box::pin(async move {
            wait.await;
            flag.set(true);
        }));
        let _ = registry.register("a", FakeProvider::new(&["BTC"]).rc());
        platform.run();
        let before = seen.get();
        assert!(before > 0);
        registry.cancel_waits();
        let _ = registry.register("b", FakeProvider::new(&["NOPE"]).rc());
        platform.run();
        assert_eq!(seen.get(), before);
        assert!(!resolved.get()); // the wait simply never settles
    }

    #[test]
    fn a_listener_can_unsubscribe_itself_and_others_during_a_round() {
        let (platform, registry) = registry();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let off_b: Rc<RefCell<Option<Unsubscribe>>> = Rc::default();
        let (log, kill) = (calls.clone(), off_b.clone());
        let _a = registry.on_change(move |_| {
            log.borrow_mut().push("a");
            if let Some(off) = kill.borrow_mut().take() {
                off();
            }
        });
        let log = calls.clone();
        *off_b.borrow_mut() = Some(registry.on_change(move |_| log.borrow_mut().push("b")));
        let _ = registry.register("p", FakeProvider::new(&["X"]).rc());
        platform.run();
        assert!(!calls.borrow().contains(&"b")); // removed before its turn
        assert!(calls.borrow().contains(&"a"));
    }

    #[test]
    fn index_ready_waits_for_every_provider() {
        let (platform, registry) = registry();
        let _ = registry.register("a", FakeProvider::new(&["A"]).rc());
        let _ = registry.register("b", FakeProvider::new(&["B"]).rc());
        let done = Rc::new(Cell::new(false));
        let flag = done.clone();
        let ready = registry.index_ready();
        platform.spawn(Box::pin(async move {
            ready.await;
            flag.set(true);
        }));
        assert!(!done.get());
        platform.run();
        assert!(done.get());
        // Nothing registered: ready at once.
        let (platform, empty) = self::registry();
        platform.block_on(empty.index_ready());
    }

    #[test]
    fn symbols_of_annotates_the_owner_on_aggregation_only() {
        let (platform, registry) = registry();
        let _ = registry.register("binance", FakeProvider::new(&["BTCUSDT"]).rc());
        let _ = registry.register("fmp", FakeProvider::new(&["AAPL"]).rc());
        platform.run();
        assert_eq!(
            registry.symbols_of(Some("BINANCE")),
            vec![SymbolDescriptor::new("BTCUSDT")]
        );
        assert!(registry.symbols_of(Some("ghost")).is_empty());
        let all = registry.symbols_of(None);
        assert_eq!(
            all.iter()
                .map(|d| (d.ticker.as_str(), d.provider.as_deref()))
                .collect::<Vec<_>>(),
            vec![("BTCUSDT", Some("binance")), ("AAPL", Some("fmp"))]
        );
    }

    #[test]
    fn info_comes_from_the_provider_or_is_synthesized() {
        let (platform, registry) = registry();
        let _ = registry.register("plain", FakeProvider::new(&["A"]).rc());
        let _ = registry.register("rich", FakeProvider::new(&["B"]).sym_info(json!({})).rc());
        let _ = registry.register("bare", FakeProvider::new(&["C"]).not_enumerable().rc());
        let _ = registry.register(
            "own",
            FakeProvider::new(&["D"])
                .with_info(ProviderInfo {
                    name: "x".into(),
                    display_name: Some("X".into()),
                    requires_api_key: None,
                    supported_timeframes: None,
                    capabilities: ProviderCapabilities {
                        enumerate: true,
                        stream: true,
                        symbol_info: false,
                    },
                })
                .rc(),
        );
        platform.run();
        let caps = |name: &str| registry.info_of(name).capabilities;
        assert_eq!(
            caps("plain"),
            ProviderCapabilities {
                enumerate: true,
                stream: false,
                symbol_info: false
            }
        );
        assert!(caps("rich").symbol_info);
        assert!(!caps("bare").enumerate);
        assert_eq!(registry.info_of("PLAIN").name, "plain");
        assert_eq!(registry.info_of("own").name, "x");
        assert!(caps("own").stream);
        let ghost = registry.info_of(" Ghost ");
        assert_eq!(ghost.name, "ghost");
        assert_eq!(ghost.capabilities, ProviderCapabilities::default());
    }

    fn icon_for(ticker: &str) -> Option<String> {
        Some(format!("u/{ticker}"))
    }

    #[test]
    fn icons_route_to_the_owning_provider() {
        let (platform, registry) = registry();
        let binance = FakeProvider::new(&["BTCUSDT"]).icons(icon_for).rc();
        let _ = registry.register("binance", binance.clone());
        let _ = registry.register("plain", FakeProvider::new(&["AAPL"]).rc());
        platform.run();
        let owned = |ticker: &str, provider: &str| SymbolDescriptor {
            provider: Some(provider.into()),
            ..SymbolDescriptor::new(ticker)
        };
        assert_eq!(
            registry
                .symbol_icon_of(&owned("BTCUSDT", "Binance"))
                .as_deref(),
            Some("u/BTCUSDT")
        );
        assert_eq!(registry.symbol_icon_of(&owned("AAPL", "plain")), None);
        assert_eq!(registry.symbol_icon_of(&owned("X", "ghost")), None);
        // No owner: the first registered provider answers.
        assert_eq!(
            registry
                .symbol_icon_of(&SymbolDescriptor::new("ETH"))
                .as_deref(),
            Some("u/ETH")
        );

        let resolved = registry.resolve("BTCUSDT", ResolveOptions::default());
        assert_eq!(
            registry.symbol_icon(resolved.as_ref()).as_deref(),
            Some("u/BTCUSDT")
        );
        assert_eq!(registry.symbol_icon(None), None);
        let seen = binance.icon_seen.borrow();
        assert_eq!(
            seen.last().and_then(|d| d.provider.as_deref()),
            Some("binance")
        );
    }

    #[test]
    fn a_synthetic_descriptor_is_offered_before_the_index_lands() {
        let (platform, registry) = registry();
        let binance = FakeProvider::new(&["BTCUSDT"]).icons(icon_for).rc();
        let _ = registry.register("binance", binance.clone());
        let resolved = registry.resolve("binance:BTCUSDT", ResolveOptions::default());
        assert_eq!(
            registry.symbol_icon(resolved.as_ref()).as_deref(),
            Some("u/BTCUSDT")
        );
        assert_eq!(
            binance.icon_seen.borrow()[0].provider.as_deref(),
            Some("binance")
        );
        platform.run();
    }
}
