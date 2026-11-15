//! Public-API fakes shared by the `vela_*` integration tests: a scriptable
//! market-data feed, a recording renderer, a fake scripting engine and session,
//! an event log, and the [`Rig`] that builds a [`Vela`] on a `ManualPlatform`.
//!
//! Everything here is built from the crate's PUBLIC types, which doubles as a
//! check that a host has what it needs. (The orchestrator's own fixtures are
//! `#[cfg(test)]`-private to the library.)

#![allow(dead_code, clippy::type_complexity)]

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use serde_json::{Map, Value};

use vela_core::controls::data_control::DataControl;
use vela_core::engine::orchestrator::ResolveOnce;
use vela_core::events::types::{ChartEvent, ChartEventName};
use vela_core::format::types::PriceFormatOptions;
use vela_core::i18n::Translator;
use vela_core::model::Inputs;
use vela_core::model::{
    IndicatorMeta, IndicatorModel, InputSchema, InputType, LineLikeKind, LineLikeStyle, LineStyle,
    Ohlcv, PaneHint, SeriesData, SeriesPoint, SeriesSpec,
};
use vela_core::options::{
    InitialRange, MarketConfig, MarketSession, MarketSwitch, MoveTarget, VelaOptions,
};
use vela_core::ports::chart_renderer::*;
use vela_core::ports::market_data::SymbolFacts;
use vela_core::ports::market_data::{
    BarRange, BarSink, BatchSink, MarketDataFeed, SymbolInfo, UnresolvedSymbol,
};
use vela_core::ports::scripting_engine::*;
use vela_core::runtime::testing::ManualPlatform;
use vela_core::runtime::{AbortSignal, DataError, LocalBoxFuture, Unsubscribe, noop_unsubscribe};
use vela_core::vela::{Vela, VelaDeps, VelaEnv, VelaError};

// ── bars ───────────────────────────────────────────────────

/// First bar of every fixture series (the original's `1_700_000_000_000`).
pub const T0: i64 = 1_700_000_000_000;
pub const HOUR: i64 = 3_600_000;

/// `n` hourly bars on a sine walk (the original's `makeBars(n)` of
/// `orchestrator.test.ts`): `price += sin(i / 3)` from 100, volume 10.
pub fn bars(n: usize) -> Vec<Ohlcv> {
    let mut price = 100.0;
    (0..n)
        .map(|i| {
            price += (i as f64 / 3.0).sin();
            Ohlcv::new(
                T0 + i as i64 * HOUR,
                price,
                price + 1.0,
                price - 1.0,
                price,
                Some(10.0),
            )
        })
        .collect()
}

/// `n` hourly bars all at `base` (the original's `makeBars(n, base)` of
/// `set-market.test.ts`: the close encodes the market, so the painted market is
/// assertable).
pub fn flat_bars(n: usize, base: f64) -> Vec<Ohlcv> {
    (0..n)
        .map(|i| {
            Ohlcv::new(
                T0 + i as i64 * HOUR,
                base,
                base + 1.0,
                base - 1.0,
                base,
                Some(1.0),
            )
        })
        .collect()
}

/// One hourly bar on the fixture grid.
pub fn bar_at(time: i64, close: f64) -> Ohlcv {
    Ohlcv::new(time, close, close + 1.0, close - 1.0, close, Some(1.0))
}

// ── the feed ───────────────────────────────────────────────

/// One call the orchestrator made on the feed.
#[derive(Clone, Debug, PartialEq)]
pub enum FeedCall {
    Load(MarketConfig),
    Range(BarRange),
    Progressive(MarketConfig),
    Subscribe(Option<String>),
    Unsubscribe(Option<String>),
    Destroy,
}

type UnresolvedCallback = Box<dyn FnMut(&UnresolvedSymbol)>;

struct Subscriber {
    symbol: Option<String>,
    sink: Rc<RefCell<BarSink>>,
    active: Rc<Cell<bool>>,
}

/// One `load_progressive` call, controllable by the test.
pub struct ProgressiveStream {
    sink: Rc<RefCell<BatchSink>>,
    done: ResolveOnce<Result<Option<Vec<Ohlcv>>, DataError>>,
    pub signal: Option<AbortSignal>,
}

/// A scriptable [`MarketDataFeed`].
///
/// Series: each symbol serves `depth[symbol]` bars (default `default_depth`),
/// flat at `price[symbol]` when one is set, else the sine walk of [`bars`]; or
/// whatever `set_series` gave it. `load` serves the newest `cfg.bars` (500 when
/// absent), `load_range` the window `from..=to` capped to the newest `limit`.
///
/// Knobs: `set_ranged` / `set_progressive` switch the capabilities ; `gate_*` hold
/// a load until `release()`; `set_load_error` / `set_range_error` make fetches
/// fail; `range_result` overrides what a ranged fetch returns.
///
/// Observation: `calls()` is the full call log; `loads()` / `ranges()` filter it.
/// Live ticks: `push_tick(bar)` reaches every active subscriber.
pub struct FakeFeed {
    pub default_depth: Cell<usize>,
    prices: RefCell<HashMap<String, f64>>,
    depths: RefCell<HashMap<String, usize>>,
    series: RefCell<HashMap<String, Vec<Ohlcv>>>,
    session_series: RefCell<HashMap<MarketSession, Vec<Ohlcv>>>,
    tick_on_subscribe: RefCell<Option<Ohlcv>>,
    cache: RefCell<HashMap<String, Rc<Vec<Ohlcv>>>>,
    log: Rc<RefCell<Vec<FeedCall>>>,
    ranged: Cell<bool>,
    progressive: Cell<bool>,
    gated_symbols: RefCell<HashSet<String>>,
    gate_all_loads: Cell<bool>,
    gate_ranges: Cell<bool>,
    held: RefCell<Vec<ResolveOnce<()>>>,
    load_error: RefCell<Option<DataError>>,
    failing_symbols: RefCell<HashSet<String>>,
    range_error: RefCell<Option<DataError>>,
    range_result: RefCell<Option<Vec<Ohlcv>>>,
    subscribers: RefCell<Vec<Subscriber>>,
    streams: RefCell<Vec<ProgressiveStream>>,
    symbol_infos: RefCell<HashMap<String, SymbolInfo>>,
    default_info: RefCell<Option<SymbolInfo>>,
    unresolved: Slot<UnresolvedCallback>,
}

impl FakeFeed {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            default_depth: Cell::new(50),
            prices: RefCell::default(),
            depths: RefCell::default(),
            series: RefCell::default(),
            session_series: RefCell::default(),
            tick_on_subscribe: RefCell::default(),
            cache: RefCell::default(),
            log: Rc::default(),
            ranged: Cell::new(true),
            progressive: Cell::new(false),
            gated_symbols: RefCell::default(),
            gate_all_loads: Cell::new(false),
            gate_ranges: Cell::new(false),
            held: RefCell::default(),
            load_error: RefCell::default(),
            failing_symbols: RefCell::default(),
            range_error: RefCell::default(),
            range_result: RefCell::default(),
            subscribers: RefCell::default(),
            streams: RefCell::default(),
            symbol_infos: RefCell::default(),
            default_info: RefCell::default(),
            unresolved: Rc::default(),
        })
    }

    pub fn set_price(&self, symbol: &str, price: f64) {
        self.prices.borrow_mut().insert(symbol.to_string(), price);
    }

    pub fn set_depth(&self, symbol: &str, depth: usize) {
        self.depths.borrow_mut().insert(symbol.to_string(), depth);
    }

    pub fn set_series(&self, symbol: &str, series: Vec<Ohlcv>) {
        self.series.borrow_mut().insert(symbol.to_string(), series);
    }

    /// What a load of `session` serves, whatever the symbol (the RTH/ETH density shape).
    pub fn set_session_series(&self, session: MarketSession, series: Vec<Ohlcv>) {
        self.session_series.borrow_mut().insert(session, series);
    }

    /// A tick delivered from inside `subscribe`, before it returns.
    pub fn set_tick_on_subscribe(&self, bar: Option<Ohlcv>) {
        *self.tick_on_subscribe.borrow_mut() = bar;
    }

    pub fn set_ranged(&self, ranged: bool) {
        self.ranged.set(ranged);
    }

    pub fn set_progressive(&self, on: bool) {
        self.progressive.set(on);
    }

    /// Hold the loads of `symbol` until `release()`.
    pub fn gate_symbol(&self, symbol: &str) {
        self.gated_symbols.borrow_mut().insert(symbol.to_string());
    }

    pub fn gate_loads(&self, on: bool) {
        self.gate_all_loads.set(on);
    }

    pub fn gate_ranges(&self, on: bool) {
        self.gate_ranges.set(on);
    }

    pub fn fail_symbol(&self, symbol: &str) {
        self.failing_symbols.borrow_mut().insert(symbol.to_string());
    }

    pub fn set_load_error(&self, error: Option<DataError>) {
        *self.load_error.borrow_mut() = error;
    }

    pub fn set_range_error(&self, error: Option<DataError>) {
        *self.range_error.borrow_mut() = error;
    }

    pub fn set_range_result(&self, bars: Option<Vec<Ohlcv>>) {
        *self.range_result.borrow_mut() = bars;
    }

    /// The facts every symbol answers (`SymbolInfo` of the feed's sync door).
    pub fn set_symbol_info(&self, info: Option<SymbolInfo>) {
        *self.default_info.borrow_mut() = info;
    }

    pub fn set_symbol_info_for(&self, symbol: &str, info: SymbolInfo) {
        self.symbol_infos
            .borrow_mut()
            .insert(symbol.to_string(), info);
    }

    /// Release every held load and ranged fetch.
    pub fn release(&self) {
        let held = std::mem::take(&mut *self.held.borrow_mut());
        for gate in held {
            gate.resolve(());
        }
    }

    pub fn held(&self) -> usize {
        self.held
            .borrow()
            .iter()
            .filter(|g| !g.is_resolved())
            .count()
    }

    pub fn calls(&self) -> Vec<FeedCall> {
        self.log.borrow().clone()
    }

    pub fn loads(&self) -> Vec<MarketConfig> {
        self.log
            .borrow()
            .iter()
            .filter_map(|c| match c {
                FeedCall::Load(cfg) => Some(cfg.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn ranges(&self) -> Vec<BarRange> {
        self.log
            .borrow()
            .iter()
            .filter_map(|c| match c {
                FeedCall::Range(r) => Some(r.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn count(&self, pick: impl Fn(&FeedCall) -> bool) -> usize {
        self.log.borrow().iter().filter(|c| pick(c)).count()
    }

    /// Active live subscriptions per symbol, and the unsubscribe count per symbol.
    pub fn subscriptions(&self) -> (HashMap<String, usize>, HashMap<String, usize>) {
        let mut subs: HashMap<String, usize> = HashMap::new();
        let mut unsubs: HashMap<String, usize> = HashMap::new();
        for call in self.log.borrow().iter() {
            match call {
                FeedCall::Subscribe(s) => {
                    *subs.entry(s.clone().unwrap_or_default()).or_default() += 1;
                }
                FeedCall::Unsubscribe(s) => {
                    *unsubs.entry(s.clone().unwrap_or_default()).or_default() += 1;
                }
                _ => {}
            }
        }
        (subs, unsubs)
    }

    pub fn active_subscribers(&self) -> usize {
        self.subscribers
            .borrow()
            .iter()
            .filter(|s| s.active.get())
            .count()
    }

    /// A live tick: every active subscriber gets it.
    pub fn push_tick(&self, bar: Ohlcv) {
        let sinks: Vec<_> = self
            .subscribers
            .borrow()
            .iter()
            .filter(|s| s.active.get())
            .map(|s| Rc::clone(&s.sink))
            .collect();
        for sink in sinks {
            (sink.borrow_mut())(bar);
        }
    }

    /// What `symbol` serves in full.
    pub fn all(&self, symbol: &str) -> Rc<Vec<Ohlcv>> {
        if let Some(custom) = self.series.borrow().get(symbol) {
            return Rc::new(custom.clone());
        }
        if let Some(hit) = self.cache.borrow().get(symbol) {
            return Rc::clone(hit);
        }
        let n = self
            .depths
            .borrow()
            .get(symbol)
            .copied()
            .unwrap_or(self.default_depth.get());
        let series = Rc::new(match self.prices.borrow().get(symbol) {
            Some(price) => flat_bars(n, *price),
            None => bars(n),
        });
        self.cache
            .borrow_mut()
            .insert(symbol.to_string(), Rc::clone(&series));
        series
    }

    /// Report a parked load (what the real feed does for a symbol nothing serves).
    pub fn park(&self, symbol: &str) {
        let taken = self.unresolved.borrow_mut().take();
        if let Some(mut cb) = taken {
            cb(&UnresolvedSymbol {
                symbol: symbol.to_string(),
                providers: Vec::new(),
            });
            if self.unresolved.borrow().is_none() {
                *self.unresolved.borrow_mut() = Some(cb);
            }
        }
    }

    /// The progressive stream `n` (0 is the first `load_progressive` call).
    pub fn stream_count(&self) -> usize {
        self.streams.borrow().len()
    }

    pub fn stream_signal(&self, n: usize) -> Option<AbortSignal> {
        self.streams.borrow().get(n).and_then(|s| s.signal.clone())
    }

    /// Emit a snapshot on the newest progressive stream.
    pub fn emit_batch(&self, batch: Vec<Ohlcv>) {
        let sink = self.streams.borrow().last().map(|s| Rc::clone(&s.sink));
        if let Some(sink) = sink {
            (sink.borrow_mut())(batch);
        }
    }

    /// Emit a snapshot on stream `n` (an abandoned stream may still deliver).
    pub fn emit_batch_on(&self, n: usize, batch: Vec<Ohlcv>) {
        let sink = self.streams.borrow().get(n).map(|s| Rc::clone(&s.sink));
        if let Some(sink) = sink {
            (sink.borrow_mut())(batch);
        }
    }

    /// Settle the newest progressive stream.
    pub fn finish_progressive(&self, result: Result<Option<Vec<Ohlcv>>, DataError>) {
        let done = self.streams.borrow().last().map(|s| s.done.clone());
        if let Some(done) = done {
            done.resolve(result);
        }
    }

    /// Settle stream `n`.
    pub fn finish_progressive_on(&self, n: usize, result: Result<Option<Vec<Ohlcv>>, DataError>) {
        let done = self.streams.borrow().get(n).map(|s| s.done.clone());
        if let Some(done) = done {
            done.resolve(result);
        }
    }

    fn window(&self, symbol: &str, range: &BarRange) -> Vec<Ohlcv> {
        let all = self.all(symbol);
        let mut out: Vec<Ohlcv> = all
            .iter()
            .filter(|b| {
                range.to.is_none_or(|to| b.time <= to) && range.from.is_none_or(|f| b.time >= f)
            })
            .copied()
            .collect();
        if let Some(limit) = range.limit
            && out.len() > limit
        {
            out.drain(..out.len() - limit);
        }
        out
    }

    fn gate_for(&self, gated: bool) -> Option<ResolveOnce<()>> {
        gated.then(|| {
            let gate = ResolveOnce::new();
            self.held.borrow_mut().push(gate.clone());
            gate
        })
    }
}

impl MarketDataFeed for FakeFeed {
    fn supports_load_range(&self) -> bool {
        self.ranged.get()
    }

    fn supports_load_progressive(&self) -> bool {
        self.progressive.get()
    }

    fn load(&self, cfg: &MarketConfig) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
        self.log.borrow_mut().push(FeedCall::Load(cfg.clone()));
        if let Some(data) = cfg.data.as_ref().filter(|d| !d.is_empty()) {
            let data = data.clone();
            return Box::pin(async move { Ok(data) });
        }
        let symbol = cfg.symbol.clone().unwrap_or_else(|| "TEST".to_string());
        if let Some(series) = cfg
            .session
            .and_then(|s| self.session_series.borrow().get(&s).cloned())
        {
            let want = cfg.bars.unwrap_or(500).min(series.len());
            return Box::pin(async move { Ok(series[series.len() - want..].to_vec()) });
        }
        let gated = self.gate_all_loads.get() || self.gated_symbols.borrow().contains(&symbol);
        let gate = self.gate_for(gated);
        let failure = if self.failing_symbols.borrow().contains(&symbol) {
            Some(DataError::other(format!("no venue serves {symbol}")))
        } else {
            self.load_error.borrow().clone()
        };
        let all = self.all(&symbol);
        let want = cfg.bars.unwrap_or(500);
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.wait().await;
            }
            if let Some(e) = failure {
                return Err(e);
            }
            let take = want.min(all.len());
            Ok(all[all.len() - take..].to_vec())
        })
    }

    fn load_progressive(
        &self,
        cfg: &MarketConfig,
        on_batch: BatchSink,
        signal: Option<AbortSignal>,
    ) -> Option<LocalBoxFuture<'static, Result<Option<Vec<Ohlcv>>, DataError>>> {
        if !self.progressive.get() {
            return None;
        }
        self.log
            .borrow_mut()
            .push(FeedCall::Progressive(cfg.clone()));
        let done = ResolveOnce::new();
        self.streams.borrow_mut().push(ProgressiveStream {
            sink: Rc::new(RefCell::new(on_batch)),
            done: done.clone(),
            signal,
        });
        Some(Box::pin(async move { done.wait().await }))
    }

    fn subscribe(&self, cfg: &MarketConfig, on_bar: BarSink) -> Unsubscribe {
        self.log
            .borrow_mut()
            .push(FeedCall::Subscribe(cfg.symbol.clone()));
        let active = Rc::new(Cell::new(true));
        let sink = Rc::new(RefCell::new(on_bar));
        self.subscribers.borrow_mut().push(Subscriber {
            symbol: cfg.symbol.clone(),
            sink: Rc::clone(&sink),
            active: Rc::clone(&active),
        });
        let immediate = *self.tick_on_subscribe.borrow();
        if let Some(bar) = immediate {
            (sink.borrow_mut())(bar);
        }
        let symbol = cfg.symbol.clone();
        let log = Rc::clone(&self.log);
        Box::new(move || {
            if active.replace(false) {
                log.borrow_mut().push(FeedCall::Unsubscribe(symbol));
            }
        })
    }

    fn symbol_info(&self, cfg: &MarketConfig) -> Option<SymbolInfo> {
        if cfg.data.as_ref().is_some_and(|d| !d.is_empty()) {
            return None;
        }
        cfg.symbol
            .as_ref()
            .and_then(|s| self.symbol_infos.borrow().get(s).cloned())
            .or_else(|| self.default_info.borrow().clone())
    }

    fn load_range(
        &self,
        cfg: &MarketConfig,
        range: &BarRange,
    ) -> Option<LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>>> {
        if !self.ranged.get() {
            return None;
        }
        self.log.borrow_mut().push(FeedCall::Range(range.clone()));
        let symbol = cfg.symbol.clone().unwrap_or_else(|| "TEST".to_string());
        let gate = self.gate_for(self.gate_ranges.get());
        let failure = self.range_error.borrow().clone();
        let out = match self.range_result.borrow().clone() {
            Some(fixed) => fixed,
            None => self.window(&symbol, range),
        };
        Some(Box::pin(async move {
            if let Some(gate) = gate {
                gate.wait().await;
            }
            match failure {
                Some(e) => Err(e),
                None => Ok(out),
            }
        }))
    }

    fn on_unresolved(&self, callback: Box<dyn FnMut(&UnresolvedSymbol)>) -> Option<Unsubscribe> {
        *self.unresolved.borrow_mut() = Some(callback);
        let slot = Rc::clone(&self.unresolved);
        Some(Box::new(move || {
            *slot.borrow_mut() = None;
        }))
    }

    fn destroy(&self) {
        self.log.borrow_mut().push(FeedCall::Destroy);
    }
}

// ── the renderer ───────────────────────────────────────────

type Slot<F> = Rc<RefCell<Option<F>>>;

/// A knob applied to a fixture before the chart is built.
type Setup<T> = Box<dyn FnOnce(&T)>;

/// Builds the chart's `DataControl` from the rig's platform.
type DataControlSetup = Box<dyn FnOnce(&ManualPlatform) -> DataControl>;

fn subscribe<F: 'static>(slot: &Slot<F>, cb: F) -> Unsubscribe {
    *slot.borrow_mut() = Some(cb);
    let slot = Rc::clone(slot);
    Box::new(move || {
        *slot.borrow_mut() = None;
    })
}

/// Call the listener in `slot` with the slot released, so it may re-enter the
/// renderer (as a callback fired from a renderer must be able to).
fn fire<F>(slot: &Slot<F>, call: impl FnOnce(&mut F)) {
    let taken = slot.borrow_mut().take();
    if let Some(mut cb) = taken {
        call(&mut cb);
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(cb);
        }
    }
}

/// One `set_bars` call as the renderer saw it.
#[derive(Clone, Debug, PartialEq)]
pub struct SetBarsCall {
    pub n: usize,
    pub first_close: Option<f64>,
    pub preserve_view: bool,
}

/// A recording [`ChartRenderer`] for orchestrator tests: every call is kept,
/// every listener can be fired by hand. `user_drawings` is off, which is what the
/// original's `FakeRenderer` did.
/// What the chart handed the renderer through `set_localization`.
#[derive(Clone, Debug)]
pub struct Localization {
    pub locale: String,
    /// `i18n.format().price(1234.5, 2 decimals, grouped)`, read at the time of the call.
    pub price: String,
    pub root: RendererRootAttributes,
}

pub struct RecRenderer {
    /// Coarse, ordered timeline of series and loading calls (`loading:true`,
    /// `bars:50`, `loading:false`): the order is what the load-state tests pin.
    pub timeline: RefCell<Vec<String>>,
    pub mounted_with: RefCell<Option<VelaTheme>>,
    pub themes: RefCell<Vec<VelaTheme>>,
    pub bars: RefCell<Vec<Ohlcv>>,
    pub set_bars_calls: RefCell<Vec<SetBarsCall>>,
    pub updated_bars: RefCell<Vec<Ohlcv>>,
    pub panes: RefCell<Vec<vela_core::model::Pane>>,
    pub removed_panes: RefCell<Vec<String>>,
    /// Every `set_indicator_pane(indicator, pane, own_scale)`.
    pub set_pane_calls: RefCell<Vec<(String, String, bool)>>,
    pub order_calls: RefCell<Vec<Vec<String>>>,
    pub collapse_calls: RefCell<Vec<(String, bool)>>,
    pub maximize_calls: RefCell<Vec<Option<String>>>,
    /// The pane height weights the renderer holds (set by the chart or by a test).
    pub weights: RefCell<HashMap<String, f64>>,
    /// Whether the renderer supports pane sizing.
    pub sizing: Cell<bool>,
    pub mounted_models: RefCell<Vec<IndicatorModel>>,
    pub updated: RefCell<Vec<(String, vela_core::model::ScenePatch)>>,
    pub removed: RefCell<Vec<String>>,
    pub statuses: RefCell<Vec<(String, IndicatorStatus)>>,
    pub indicator_visible: RefCell<HashMap<String, bool>>,
    pub loading: Cell<bool>,
    pub visible_range_calls: RefCell<Vec<(VisibleRange, VisibleRangeOptions)>>,
    /// What `get_visible_range` answers (what a session-only switch may carry over).
    pub visible_range: Cell<Option<VisibleRange>>,
    /// Every `set_native_data(layer, data)`; `Value::Null` is the original's `undefined`.
    pub native_pushes: RefCell<Vec<(String, Value)>>,
    pub precision_calls: RefCell<Vec<Option<f64>>>,
    pub facts_calls: RefCell<Vec<Option<SymbolFacts>>>,
    pub pan_calls: RefCell<Vec<f64>>,
    pub destroyed: Cell<bool>,
    /// The feature keys the renderer claims to support.
    pub feature_names: RefCell<Vec<String>>,
    /// Every `apply_feature(key, value)`, oldest first.
    pub applied_features: RefCell<Vec<(String, Value)>>,
    /// Every `set_localization`, oldest first.
    pub localizations: RefCell<Vec<Localization>>,
    // knobs
    pub native_data: Cell<bool>,
    pub price_precision: Cell<bool>,
    pub pan_by_supported: Cell<bool>,
    pub pane_management: Cell<bool>,
    /// What `read_feature("priceStyle")` reports (a chart CONSTRUCTED in a style).
    pub price_style_feature: RefCell<Option<Value>>,
    /// Runs inside every `set_bars`, after it is recorded: a renderer callback that
    /// calls back into the chart.
    pub set_bars_hook: RefCell<Option<Rc<dyn Fn()>>>,
    // listeners
    viewport: Slot<Listener<VisibleRange>>,
    input_change: Slot<Listener<InputChangeEvent>>,
    remove: Slot<Listener<String>>,
    toggle: Slot<ToggleVisibleListener>,
    price_style: Slot<Listener<PriceStyle>>,
    will_change: Slot<PriceStyleWillChangeListener>,
    chart_type_settings: Slot<ChartTypeSettingsListener>,
    pane_action: Slot<Listener<PaneAction>>,
    move_indicator: Slot<MoveIndicatorListener>,
    theme_select: Slot<Listener<ThemeId>>,
}

impl RecRenderer {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            timeline: RefCell::default(),
            mounted_with: RefCell::default(),
            themes: RefCell::default(),
            bars: RefCell::default(),
            set_bars_calls: RefCell::default(),
            updated_bars: RefCell::default(),
            panes: RefCell::default(),
            removed_panes: RefCell::default(),
            set_pane_calls: RefCell::default(),
            order_calls: RefCell::default(),
            collapse_calls: RefCell::default(),
            maximize_calls: RefCell::default(),
            weights: RefCell::default(),
            sizing: Cell::new(false),
            mounted_models: RefCell::default(),
            updated: RefCell::default(),
            removed: RefCell::default(),
            statuses: RefCell::default(),
            indicator_visible: RefCell::default(),
            loading: Cell::new(false),
            visible_range_calls: RefCell::default(),
            visible_range: Cell::new(None),
            native_pushes: RefCell::default(),
            precision_calls: RefCell::default(),
            facts_calls: RefCell::default(),
            pan_calls: RefCell::default(),
            destroyed: Cell::new(false),
            feature_names: RefCell::default(),
            applied_features: RefCell::default(),
            localizations: RefCell::default(),
            native_data: Cell::new(true),
            price_precision: Cell::new(false),
            pan_by_supported: Cell::new(false),
            pane_management: Cell::new(false),
            price_style_feature: RefCell::default(),
            set_bars_hook: RefCell::default(),
            viewport: Rc::default(),
            input_change: Rc::default(),
            remove: Rc::default(),
            toggle: Rc::default(),
            price_style: Rc::default(),
            will_change: Rc::default(),
            chart_type_settings: Rc::default(),
            pane_action: Rc::default(),
            move_indicator: Rc::default(),
            theme_select: Rc::default(),
        })
    }

    pub fn fire_viewport(&self, range: VisibleRange) {
        fire(&self.viewport, |cb| cb(&range));
    }

    pub fn fire_price_style(&self, style: &str) {
        let style = style.to_string();
        fire(&self.price_style, |cb| cb(&style));
    }

    pub fn fire_price_style_will_change(&self, from: &str, to: &str) {
        let (from, to) = (from.to_string(), to.to_string());
        fire(&self.will_change, |cb| cb(&from, &to));
    }

    pub fn fire_input_change(&self, event: InputChangeEvent) {
        fire(&self.input_change, |cb| cb(&event));
    }

    pub fn fire_remove(&self, id: &str) {
        let id = id.to_string();
        fire(&self.remove, |cb| cb(&id));
    }

    pub fn fire_toggle_visible(&self, id: &str, visible: bool) {
        fire(&self.toggle, |cb| cb(id, visible));
    }

    pub fn fire_chart_type_settings(&self, type_id: &str, values: &Map<String, Value>) {
        fire(&self.chart_type_settings, |cb| cb(type_id, values));
    }

    pub fn fire_pane_action(&self, action: PaneAction) {
        fire(&self.pane_action, |cb| cb(&action));
    }

    pub fn fire_move_indicator(&self, id: &str, target: MoveTarget) {
        fire(&self.move_indicator, |cb| cb(id, &target));
    }

    pub fn fire_theme_select(&self, name: &str) {
        let name = name.to_string();
        fire(&self.theme_select, |cb| cb(&name));
    }

    /// Every native-layer push to `layer`, oldest first.
    pub fn pushes_to(&self, layer: &str) -> Vec<Value> {
        self.native_pushes
            .borrow()
            .iter()
            .filter(|(l, _)| l == layer)
            .map(|(_, v)| v.clone())
            .collect()
    }

    pub fn last_bars_call(&self) -> Option<SetBarsCall> {
        self.set_bars_calls.borrow().last().cloned()
    }

    pub fn clear_timeline(&self) {
        self.timeline.borrow_mut().clear();
    }
}

impl ChartRenderer for RecRenderer {
    fn capabilities(&self) -> RendererCapabilities {
        RendererCapabilities {
            panes: true,
            pane_management: self.pane_management.get(),
            fills: Support::Primitive,
            bgcolor: Support::Primitive,
            hline: Support::Native,
            markers: true,
            barcolor: BarColorSupport::Approximated,
            per_point_color: true,
            drawings: true,
            user_drawings: false,
            drawing_depth: false,
            tables: true,
            trades: false,
            timeline_marks: false,
            inputs_ui: true,
        }
    }

    fn name(&self) -> &str {
        "fake"
    }

    fn features(&self) -> Vec<String> {
        self.feature_names.borrow().clone()
    }

    fn apply_feature(&self, key: &str, value: &Value) {
        self.applied_features
            .borrow_mut()
            .push((key.to_string(), value.clone()));
    }

    fn set_localization(&self, i18n: Rc<dyn Translator>, root: RendererRootAttributes) {
        self.localizations.borrow_mut().push(Localization {
            locale: i18n.locale().to_string(),
            price: i18n.format().price(
                1234.5,
                &PriceFormatOptions {
                    grouping: Some(true),
                    ..PriceFormatOptions::new(2)
                },
            ),
            root,
        });
    }

    fn read_feature(&self, key: &str) -> Option<Value> {
        if key == "priceStyle" {
            self.price_style_feature.borrow().clone()
        } else {
            None
        }
    }

    fn mount(&self, theme: &VelaTheme) {
        *self.mounted_with.borrow_mut() = Some(theme.clone());
    }

    fn set_theme(&self, theme: &VelaTheme) {
        self.themes.borrow_mut().push(theme.clone());
    }

    fn resize(&self) {}

    fn destroy(&self) {
        self.destroyed.set(true);
    }

    fn set_bars(&self, bars: &[Ohlcv], opts: SetBarsOptions) {
        *self.bars.borrow_mut() = bars.to_vec();
        self.set_bars_calls.borrow_mut().push(SetBarsCall {
            n: bars.len(),
            first_close: bars.first().map(|b| b.close),
            preserve_view: opts.preserve_view,
        });
        self.timeline
            .borrow_mut()
            .push(format!("bars:{}", bars.len()));
        let hook = self.set_bars_hook.borrow().clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    fn update_bar(&self, bar: &Ohlcv) {
        self.updated_bars.borrow_mut().push(*bar);
    }

    fn set_loading(&self, loading: bool) {
        if loading != self.loading.get() {
            self.timeline
                .borrow_mut()
                .push(format!("loading:{loading}"));
        }
        self.loading.set(loading);
    }

    fn supports_set_native_data(&self) -> bool {
        self.native_data.get()
    }

    fn set_native_data(&self, layer_type: &str, data: &Value) {
        self.native_pushes
            .borrow_mut()
            .push((layer_type.to_string(), data.clone()));
    }

    fn supports_set_price_precision(&self) -> bool {
        self.price_precision.get()
    }

    fn set_price_precision(&self, mintick: Option<f64>) {
        self.precision_calls.borrow_mut().push(mintick);
    }

    fn set_symbol_facts(&self, facts: Option<SymbolFacts>) {
        self.facts_calls.borrow_mut().push(facts);
    }

    fn set_indicator_status(&self, handle: &IndicatorRenderHandle, status: IndicatorStatus) {
        self.statuses.borrow_mut().push((handle.id.clone(), status));
    }

    fn set_indicator_visible(&self, handle: &IndicatorRenderHandle, visible: bool) {
        self.indicator_visible
            .borrow_mut()
            .insert(handle.id.clone(), visible);
    }

    fn ensure_pane(&self, pane: &vela_core::model::Pane) {
        self.panes.borrow_mut().push(pane.clone());
        if self.sizing.get() {
            // The native defaults: price 3, studies 1; only an explicit weight replaces one.
            let default = if pane.kind == vela_core::model::PaneKind::Price {
                3.0
            } else {
                1.0
            };
            let mut weights = self.weights.borrow_mut();
            match pane.height_weight {
                Some(w) => {
                    weights.insert(pane.id.clone(), w);
                }
                None => {
                    weights.entry(pane.id.clone()).or_insert(default);
                }
            }
        }
    }

    fn remove_pane(&self, id: &str) {
        self.removed_panes.borrow_mut().push(id.to_string());
    }

    fn supports_set_indicator_pane(&self) -> bool {
        self.pane_management.get()
    }

    fn set_indicator_pane(&self, handle: &IndicatorRenderHandle, pane_id: &str, own_scale: bool) {
        self.set_pane_calls
            .borrow_mut()
            .push((handle.id.clone(), pane_id.to_string(), own_scale));
    }

    fn order_panes(&self, ordered_ids: &[String]) {
        self.order_calls.borrow_mut().push(ordered_ids.to_vec());
    }

    fn set_pane_collapsed(&self, pane_id: &str, collapsed: bool) {
        self.collapse_calls
            .borrow_mut()
            .push((pane_id.to_string(), collapsed));
    }

    fn set_pane_maximized(&self, pane_id: Option<&str>) {
        self.maximize_calls
            .borrow_mut()
            .push(pane_id.map(str::to_string));
    }

    fn set_pane_height_weight(&self, pane_id: &str, weight: f64) {
        self.weights
            .borrow_mut()
            .insert(pane_id.to_string(), weight);
    }

    fn supports_set_pane_height_weight(&self) -> bool {
        self.sizing.get()
    }

    fn pane_height_weight(&self, pane_id: &str) -> Option<f64> {
        self.weights.borrow().get(pane_id).copied()
    }

    fn mount_indicator(&self, model: &IndicatorModel) -> IndicatorRenderHandle {
        self.mounted_models.borrow_mut().push(model.clone());
        IndicatorRenderHandle {
            id: model.id.clone(),
        }
    }

    fn update_indicator(
        &self,
        handle: &IndicatorRenderHandle,
        patch: &vela_core::model::ScenePatch,
    ) {
        self.updated
            .borrow_mut()
            .push((handle.id.clone(), patch.clone()));
    }

    fn remove_indicator(&self, handle: &IndicatorRenderHandle) {
        self.removed.borrow_mut().push(handle.id.clone());
    }

    fn set_indicator_inputs(
        &self,
        _handle: &IndicatorRenderHandle,
        _values: &Inputs,
        _props: Option<&Inputs>,
    ) {
    }

    fn on_input_change(&self, cb: Listener<InputChangeEvent>) -> Unsubscribe {
        subscribe(&self.input_change, cb)
    }

    fn on_remove_indicator(&self, cb: Listener<String>) -> Unsubscribe {
        subscribe(&self.remove, cb)
    }

    fn on_toggle_indicator_visible(&self, cb: ToggleVisibleListener) -> Option<Unsubscribe> {
        Some(subscribe(&self.toggle, cb))
    }

    fn on_price_style_change(&self, cb: Listener<PriceStyle>) -> Option<Unsubscribe> {
        Some(subscribe(&self.price_style, cb))
    }

    fn on_price_style_will_change(&self, cb: PriceStyleWillChangeListener) -> Option<Unsubscribe> {
        Some(subscribe(&self.will_change, cb))
    }

    fn on_chart_type_settings_change(&self, cb: ChartTypeSettingsListener) -> Option<Unsubscribe> {
        Some(subscribe(&self.chart_type_settings, cb))
    }

    fn on_pane_action(&self, cb: Listener<PaneAction>) -> Option<Unsubscribe> {
        Some(subscribe(&self.pane_action, cb))
    }

    fn on_move_indicator(&self, cb: MoveIndicatorListener) -> Option<Unsubscribe> {
        Some(subscribe(&self.move_indicator, cb))
    }

    fn on_theme_select(&self, cb: Listener<ThemeId>) -> Option<Unsubscribe> {
        Some(subscribe(&self.theme_select, cb))
    }

    fn on_crosshair_move(&self, _cb: Listener<CrosshairEvent>) -> Unsubscribe {
        noop_unsubscribe()
    }

    fn on_click(&self, _cb: Listener<ClickEvent>) -> Unsubscribe {
        noop_unsubscribe()
    }

    fn get_visible_range(&self) -> Option<VisibleRange> {
        self.visible_range.get()
    }

    fn set_visible_range(&self, range: VisibleRange, opts: VisibleRangeOptions) {
        self.visible_range_calls.borrow_mut().push((range, opts));
    }

    fn supports_pan_by(&self) -> bool {
        self.pan_by_supported.get()
    }

    fn pan_by(&self, fraction: f64) {
        self.pan_calls.borrow_mut().push(fraction);
    }

    fn on_viewport_change(&self, cb: Listener<VisibleRange>) -> Unsubscribe {
        subscribe(&self.viewport, cb)
    }
}

// ── the scripting engine ───────────────────────────────────

type Snapshot = vela_core::ports::scripting_engine::EngineContextSnapshot;

/// One call on a [`FakeSession`].
#[derive(Clone, Debug, PartialEq)]
pub enum SessionCall {
    Stop,
    Update {
        inputs: Inputs,
        props: Option<Inputs>,
    },
    SetVisibleRange(VisibleBarRange),
    NotifyBars(Option<BarsChangeReason>),
}

/// What an `execute` call was asked.
#[derive(Clone, Debug)]
pub struct ExecutionLog {
    pub instance: String,
    pub mode: ExecutionMode,
    pub market: ExecutionMarket,
    pub bar_count: usize,
    pub first_close: Option<f64>,
    pub inputs: Option<Inputs>,
    pub props: Option<Inputs>,
    pub visible_range: Option<VisibleBarRange>,
    pub history_state: HistoryState,
}

/// A running fake execution: records the calls it gets and lets a test push a
/// model, an alert, a warning, an error or `done` through the handlers the
/// orchestrator registered.
pub struct FakeSession {
    pub instance: String,
    pub calls: RefCell<Vec<SessionCall>>,
    handlers: RefCell<Option<ExecutionHandlers>>,
    context: RefCell<Option<Snapshot>>,
}

impl FakeSession {
    pub fn calls(&self) -> Vec<SessionCall> {
        self.calls.borrow().clone()
    }

    pub fn count(&self, pick: impl Fn(&SessionCall) -> bool) -> usize {
        self.calls.borrow().iter().filter(|c| pick(c)).count()
    }

    pub fn stops(&self) -> usize {
        self.count(|c| matches!(c, SessionCall::Stop))
    }

    pub fn notifies(&self) -> Vec<Option<BarsChangeReason>> {
        self.calls
            .borrow()
            .iter()
            .filter_map(|c| match c {
                SessionCall::NotifyBars(r) => Some(*r),
                _ => None,
            })
            .collect()
    }

    pub fn visible_ranges(&self) -> Vec<VisibleBarRange> {
        self.calls
            .borrow()
            .iter()
            .filter_map(|c| match c {
                SessionCall::SetVisibleRange(r) => Some(*r),
                _ => None,
            })
            .collect()
    }

    /// What `get_context` answers (`None`: the engine has no capability).
    pub fn set_context(&self, snapshot: Option<Snapshot>) {
        *self.context.borrow_mut() = snapshot;
    }

    /// Run `call` on the handlers with the slot released (a handler may re-enter).
    fn with_handlers(&self, call: impl FnOnce(&mut ExecutionHandlers)) {
        let taken = self.handlers.borrow_mut().take();
        if let Some(mut handlers) = taken {
            call(&mut handlers);
            if self.handlers.borrow().is_none() {
                *self.handlers.borrow_mut() = Some(handlers);
            }
        }
    }

    pub fn push_model(&self, model: IndicatorModel) {
        self.with_handlers(|h| (h.on_model)(model));
    }

    pub fn push_alert(&self, alert: EngineAlert) {
        self.with_handlers(|h| {
            if let Some(f) = h.on_alert.as_mut() {
                f(alert);
            }
        });
    }

    pub fn push_warning(&self, warning: EngineWarning) {
        self.with_handlers(|h| {
            if let Some(f) = h.on_warning.as_mut() {
                f(warning);
            }
        });
    }

    pub fn push_error(&self, message: &str) {
        let message = message.to_string();
        self.with_handlers(|h| {
            if let Some(f) = h.on_error.as_mut() {
                f(message);
            }
        });
    }

    pub fn push_done(&self) {
        self.with_handlers(|h| {
            if let Some(f) = h.on_done.as_mut() {
                f();
            }
        });
    }
}

impl ExecutionSession for FakeSession {
    fn get_context(
        &self,
        _select: Option<&[ContextKey]>,
    ) -> Option<LocalBoxFuture<'static, Result<Option<Snapshot>, EngineError>>> {
        let snapshot = self.context.borrow().clone()?;
        Some(Box::pin(async move { Ok(Some(snapshot)) }))
    }

    fn stop(&self) {
        self.calls.borrow_mut().push(SessionCall::Stop);
    }

    fn update(&self, inputs: &Inputs, props: Option<&Inputs>) {
        self.calls.borrow_mut().push(SessionCall::Update {
            inputs: inputs.clone(),
            props: props.cloned(),
        });
    }

    fn set_visible_range(&self, range: VisibleBarRange) {
        self.calls
            .borrow_mut()
            .push(SessionCall::SetVisibleRange(range));
    }

    fn notify_bars(&self, reason: Option<BarsChangeReason>) {
        self.calls
            .borrow_mut()
            .push(SessionCall::NotifyBars(reason));
    }
}

/// A scripting engine whose scripts are plain text with markers: `overlay=true`
/// declares an overlay, a mention of `visible` makes it viewport-dependent,
/// `// broken` fails to compile, and `// inputs: A,B` declares those inputs
/// (default: one `Length`, `// default: N` its default, 14 otherwise). Nothing
/// runs by itself: a test pushes the models through the session.
pub struct FakeEngine {
    pub language: String,
    pub caps: Cell<EngineCapabilities>,
    pub prepared: RefCell<Vec<(String, String)>>,
    pub executions: RefCell<Vec<ExecutionLog>>,
    pub sessions: RefCell<Vec<Rc<FakeSession>>>,
}

impl FakeEngine {
    pub fn new() -> Rc<Self> {
        Self::with_language("pine")
    }

    pub fn with_language(language: &str) -> Rc<Self> {
        Rc::new(Self {
            language: language.to_string(),
            caps: Cell::new(EngineCapabilities {
                streaming: true,
                visible_range: true,
                inputs: true,
                props: false,
            }),
            prepared: RefCell::default(),
            executions: RefCell::default(),
            sessions: RefCell::default(),
        })
    }

    pub fn session(&self, n: usize) -> Option<Rc<FakeSession>> {
        self.sessions.borrow().get(n).cloned()
    }

    pub fn last_session(&self) -> Option<Rc<FakeSession>> {
        self.sessions.borrow().last().cloned()
    }

    /// The prepared script of `source` (what `prepare` resolves to), built without
    /// recording a call.
    pub fn prepared_for(
        &self,
        source: &str,
        instance_id: &str,
    ) -> Result<PreparedScript, EngineError> {
        if source.contains("// broken") {
            return Err(EngineError::new("mock: syntax error"));
        }
        let keys: Vec<String> = source
            .split("// inputs:")
            .nth(1)
            .and_then(|rest| rest.lines().next())
            .map(|list| {
                list.trim()
                    .split(',')
                    .map(|k| k.trim().to_string())
                    .collect()
            })
            .unwrap_or_else(|| vec!["Length".to_string()]);
        let defval = source
            .split("// default:")
            .nth(1)
            .and_then(|rest| {
                rest.trim_start()
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
            })
            .and_then(|n| n.parse::<f64>().ok())
            .unwrap_or(14.0);
        Ok(PreparedScript {
            language: self.language.clone(),
            inputs: keys
                .iter()
                .map(|key| InputSchema::new(key.clone(), key.clone(), InputType::Int, defval))
                .collect(),
            props: None,
            meta: IndicatorMeta {
                title: "Mock".to_string(),
                shorttitle: None,
                overlay: source.contains("overlay=true"),
                precision: None,
                format: None,
            },
            reacts_to_viewport: Rc::new(Cell::new(source.to_lowercase().contains("visible"))),
            token: Rc::new(instance_id.to_string()) as Rc<dyn Any>,
        })
    }
}

impl ScriptingEngine for FakeEngine {
    fn language(&self) -> &str {
        &self.language
    }

    fn capabilities(&self) -> EngineCapabilities {
        self.caps.get()
    }

    fn prepare(
        &self,
        source: &str,
        instance_id: &str,
    ) -> LocalBoxFuture<'static, Result<PreparedScript, EngineError>> {
        self.prepared
            .borrow_mut()
            .push((source.to_string(), instance_id.to_string()));
        let result = self.prepared_for(source, instance_id);
        Box::pin(async move { result })
    }

    fn execute(
        &self,
        req: ExecutionRequest,
        handlers: ExecutionHandlers,
    ) -> Rc<dyn ExecutionSession> {
        let instance = req
            .prepared
            .token
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        let live_bars = req
            .get_bars
            .as_ref()
            .map_or_else(|| req.bars.clone(), |f| f());
        self.executions.borrow_mut().push(ExecutionLog {
            instance: instance.clone(),
            mode: req.mode,
            market: req.market.clone(),
            bar_count: live_bars.len(),
            first_close: live_bars.first().map(|b| b.close),
            inputs: req.inputs.clone(),
            props: req.props.clone(),
            visible_range: req.visible_range,
            history_state: req.history_state(),
        });
        let session = Rc::new(FakeSession {
            instance,
            calls: RefCell::default(),
            handlers: RefCell::new(Some(handlers)),
            context: RefCell::default(),
        });
        self.sessions.borrow_mut().push(Rc::clone(&session));
        session
    }
}

/// A model with one line series over `bars` (`value = close + length`), the unit
/// the fake engine's tests push.
pub fn mock_model(id: &str, overlay: bool, bars: &[Ohlcv], length: f64) -> IndicatorModel {
    let mut model = empty_model(id, "Mock", overlay);
    model.series.push(SeriesSpec {
        id: format!("{id}:line:mock#0"),
        title: "Mock".to_string(),
        display_title: None,
        pane_id: "unrouted".to_string(),
        z_order: None,
        visible: None,
        display: None,
        overlay: false,
        data: SeriesData::LineLike {
            kind: LineLikeKind::Line,
            points: bars
                .iter()
                .map(|b| SeriesPoint {
                    time: b.time,
                    value: Some(b.close + length),
                    color: None,
                })
                .collect(),
            style: LineLikeStyle {
                color: "#f00".to_string(),
                width: 2.0,
                line_style: LineStyle::Solid,
                base: None,
            },
        },
    });
    model
}

/// A model with no output.
pub fn empty_model(id: &str, title: &str, overlay: bool) -> IndicatorModel {
    IndicatorModel {
        id: id.to_string(),
        title: title.to_string(),
        shorttitle: None,
        display_title: None,
        display_shorttitle: None,
        overlay,
        pane_hint: if overlay {
            PaneHint::Price
        } else {
            PaneHint::New
        },
        native: None,
        legend: None,
        pane_axis: None,
        pane_id: None,
        own_scale: false,
        anchor_time: None,
        series: Vec::new(),
        fills: Vec::new(),
        backgrounds: Vec::new(),
        price_lines: Vec::new(),
        lines: Vec::new(),
        boxes: Vec::new(),
        labels: Vec::new(),
        polylines: Vec::new(),
        linefills: Vec::new(),
        tables: Vec::new(),
        bar_colors: Vec::new(),
        trades: Vec::new(),
        inputs: Vec::new(),
        input_values: Inputs::new(),
        props: Vec::new(),
        prop_values: Inputs::new(),
    }
}

// ── events ─────────────────────────────────────────────────

/// Every event the chart emitted, in order, from the first one of the
/// constructor on.
#[derive(Clone, Default)]
pub struct EventLog {
    events: Rc<RefCell<Vec<ChartEvent>>>,
}

impl EventLog {
    pub fn attach(chart: &Vela) -> Self {
        let log = Self::default();
        for name in ChartEventName::ALL {
            let sink = Rc::clone(&log.events);
            chart.on(*name, move |event| sink.borrow_mut().push(event.clone()));
        }
        log
    }

    pub fn all(&self) -> Vec<ChartEvent> {
        self.events.borrow().clone()
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.events
            .borrow()
            .iter()
            .map(|e| e.name().as_str())
            .collect()
    }

    pub fn of(&self, name: ChartEventName) -> Vec<ChartEvent> {
        self.events
            .borrow()
            .iter()
            .filter(|e| e.name() == name)
            .cloned()
            .collect()
    }

    pub fn count(&self, name: ChartEventName) -> usize {
        self.of(name).len()
    }

    pub fn clear(&self) {
        self.events.borrow_mut().clear();
    }
}

// ── the rig ────────────────────────────────────────────────

// ── an engine that runs by itself ──────────────────────────

/// The original `MockEngine`: an `execute` emits a model over the request's
/// bars and finishes at once, unless `hold` defers it until [`finish`](Self::finish)
/// or [`fail_all`](Self::fail_all). Prepare, sessions and context come from the
/// wrapped [`FakeEngine`].
pub struct AutoEngine {
    pub inner: Rc<FakeEngine>,
    /// Defer every run until `finish()` / `fail_all()`.
    pub hold: Cell<bool>,
    /// What every new session answers to `get_context` (`None`: no capability).
    pub context: RefCell<Option<EngineContextSnapshot>>,
    /// `prepare` rejects with this message.
    pub prepare_error: RefCell<Option<String>>,
    pending: RefCell<Vec<(Rc<FakeSession>, String, bool, Vec<Ohlcv>)>>,
}

impl AutoEngine {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            inner: FakeEngine::new(),
            hold: Cell::new(false),
            context: RefCell::new(None),
            prepare_error: RefCell::new(None),
            pending: RefCell::default(),
        })
    }

    /// The held runs complete: each emits its model and finishes.
    pub fn finish(&self) {
        let pending = std::mem::take(&mut *self.pending.borrow_mut());
        for (session, id, overlay, bars) in pending {
            session.push_model(mock_model(&id, overlay, &bars, 14.0));
            session.push_done();
        }
    }

    /// Every held run fails with `message`.
    pub fn fail_all(&self, message: &str) {
        let pending = std::mem::take(&mut *self.pending.borrow_mut());
        for (session, ..) in pending {
            session.push_error(message);
        }
    }
}

impl ScriptingEngine for AutoEngine {
    fn language(&self) -> &str {
        self.inner.language()
    }

    fn capabilities(&self) -> EngineCapabilities {
        self.inner.capabilities()
    }

    fn prepare(
        &self,
        source: &str,
        instance_id: &str,
    ) -> LocalBoxFuture<'static, Result<PreparedScript, EngineError>> {
        match self.prepare_error.borrow().clone() {
            Some(message) => Box::pin(async move { Err(EngineError::new(message)) }),
            None => self.inner.prepare(source, instance_id),
        }
    }

    fn execute(
        &self,
        req: ExecutionRequest,
        handlers: ExecutionHandlers,
    ) -> Rc<dyn ExecutionSession> {
        let bars = req
            .get_bars
            .as_ref()
            .map_or_else(|| req.bars.clone(), |f| f());
        let overlay = req.prepared.meta.overlay;
        let id = req
            .prepared
            .token
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        let session = self.inner.execute(req, handlers);
        let fake = self
            .inner
            .last_session()
            .unwrap_or_else(|| panic!("execute records its session"));
        fake.set_context(self.context.borrow().clone());
        if self.hold.get() {
            self.pending.borrow_mut().push((fake, id, overlay, bars));
        } else {
            fake.push_model(mock_model(&id, overlay, &bars, 14.0));
            fake.push_done();
        }
        session
    }
}

// ── the rig ────────────────────────────────────────────────

/// A `MarketSwitch` of only the symbol.
pub fn to_symbol(symbol: &str) -> MarketSwitch {
    MarketSwitch {
        symbol: Some(symbol.to_string()),
        ..MarketSwitch::default()
    }
}

/// A chart on a `ManualPlatform` with the fakes: build one with [`Rig::builder`].
/// The first load starts inside `build()`.
pub struct Rig {
    pub platform: ManualPlatform,
    pub env: VelaEnv,
    pub renderer: Rc<RecRenderer>,
    pub feed: Rc<FakeFeed>,
    pub engine: Rc<AutoEngine>,
    pub chart: Vela,
    pub log: EventLog,
}

pub struct RigBuilder {
    options: VelaOptions,
    ranged: bool,
    progressive: bool,
    feed_setup: Vec<Setup<FakeFeed>>,
    renderer_setup: Vec<Setup<RecRenderer>>,
    env_setup: Vec<Box<dyn FnOnce(&VelaEnv)>>,
    with_engine: bool,
}

impl Rig {
    pub fn builder() -> RigBuilder {
        let mut options = VelaOptions::default();
        options.market.symbol = Some("AAA".to_string());
        options.market.timeframe = Some("60".to_string());
        options.volume = Some(false);
        RigBuilder {
            options,
            ranged: true,
            progressive: false,
            feed_setup: Vec::new(),
            renderer_setup: Vec::new(),
            env_setup: Vec::new(),
            with_engine: true,
        }
    }

    /// Poll every task; time does not move.
    pub fn settle(&self) {
        self.platform.run();
    }

    /// Move the clock `ms` forward, firing timers.
    pub fn advance(&self, ms: f64) {
        self.platform.advance(ms);
    }

    /// Let everything pending run, including the zero-delay paint yield.
    pub fn flush(&self) {
        for _ in 0..4 {
            self.settle();
            self.advance(0.0);
        }
        self.settle();
    }

    pub fn block_on<T: 'static>(
        &self,
        future: impl std::future::Future<Output = T> + 'static,
    ) -> T {
        self.platform.block_on(future)
    }

    /// Wait for the first load pipeline (`chart.ready()`).
    pub fn ready(&self) -> Result<(), VelaError> {
        let ready = self.chart.ready();
        self.block_on(ready)
    }

    /// `ready()` then `history_complete()`, then every task polled.
    pub fn loaded(&self) {
        self.ready().ok();
        let complete = self.chart.history_complete();
        self.block_on(complete);
        self.flush();
    }

    /// `set_market` and wait for it.
    pub fn switch(&self, next: MarketSwitch) -> Result<(), VelaError> {
        let done = self.chart.set_market(next);
        self.block_on(done)
    }

    /// The engine answers an input edit or a poke with a fresh model of
    /// indicator `id` (the original MockEngine re-emits on `update`).
    pub fn rerun(&self, id: &str) {
        self.flush();
        let session = self
            .engine
            .inner
            .sessions
            .borrow()
            .iter()
            .rev()
            .find(|s| s.instance == id)
            .cloned()
            .unwrap_or_else(|| panic!("no session for {id}"));
        let symbol = self.chart.market().symbol.unwrap_or_default();
        session.push_model(mock_model(id, false, &self.feed.all(&symbol), 21.0));
        self.flush();
    }

    /// The handles' ids, in insertion order.
    pub fn indicator_ids(&self) -> Vec<String> {
        self.chart
            .indicators()
            .iter()
            .map(|h| h.id().to_string())
            .collect()
    }
}

impl RigBuilder {
    pub fn symbol(mut self, symbol: &str) -> Self {
        self.options.market.symbol = Some(symbol.to_string());
        self
    }

    pub fn timeframe(mut self, timeframe: &str) -> Self {
        self.options.market.timeframe = Some(timeframe.to_string());
        self
    }

    pub fn bars(mut self, bars: usize) -> Self {
        self.options.market.bars = Some(bars);
        self
    }

    pub fn session(mut self, session: MarketSession) -> Self {
        self.options.market.session = Some(session);
        self
    }

    pub fn visible_range(mut self, range: InitialRange) -> Self {
        self.options.market.visible_range = Some(range);
        self
    }

    pub fn data(mut self, data: Vec<Ohlcv>) -> Self {
        self.options.market.data = Some(data);
        self
    }

    pub fn live(mut self, live: bool) -> Self {
        self.options.live = Some(live);
        self
    }

    pub fn volume(mut self, volume: bool) -> Self {
        self.options.volume = Some(volume);
        self
    }

    pub fn ranged(mut self, ranged: bool) -> Self {
        self.ranged = ranged;
        self
    }

    pub fn progressive(mut self, progressive: bool) -> Self {
        self.progressive = progressive;
        self
    }

    /// No `pine` engine registered at construction.
    pub fn no_engine(mut self) -> Self {
        self.with_engine = false;
        self
    }

    /// Edit the chart options before it is built.
    pub fn options(mut self, edit: impl FnOnce(&mut VelaOptions)) -> Self {
        edit(&mut self.options);
        self
    }

    pub fn feed(mut self, setup: impl FnOnce(&FakeFeed) + 'static) -> Self {
        self.feed_setup.push(Box::new(setup));
        self
    }

    pub fn renderer(mut self, setup: impl FnOnce(&RecRenderer) + 'static) -> Self {
        self.renderer_setup.push(Box::new(setup));
        self
    }

    /// Prepare the environment (register a chart type, a locale, a default) before
    /// the chart exists.
    pub fn env(mut self, setup: impl FnOnce(&VelaEnv) + 'static) -> Self {
        self.env_setup.push(Box::new(setup));
        self
    }

    pub fn build(self) -> Rig {
        let platform = ManualPlatform::new(T0 as f64 + 100.0 * HOUR as f64);
        let env = VelaEnv::new(platform.handle());
        self.build_on(platform, env)
    }

    /// Build on an existing environment (charts sharing the globals).
    pub fn build_on(self, platform: ManualPlatform, env: VelaEnv) -> Rig {
        for setup in self.env_setup {
            setup(&env);
        }
        let feed = FakeFeed::new();
        feed.set_ranged(self.ranged);
        feed.set_progressive(self.progressive);
        for setup in self.feed_setup {
            setup(&feed);
        }
        let renderer = RecRenderer::new();
        for setup in self.renderer_setup {
            setup(&renderer);
        }
        let engine = AutoEngine::new();
        let engines: Vec<Rc<dyn ScriptingEngine>> = if self.with_engine {
            vec![Rc::clone(&engine) as Rc<dyn ScriptingEngine>]
        } else {
            Vec::new()
        };
        let chart = Vela::with_deps(
            &env,
            self.options,
            VelaDeps {
                renderer: Some(Rc::clone(&renderer) as Rc<dyn ChartRenderer>),
                engines,
                data_feed: Some(Rc::clone(&feed) as Rc<dyn MarketDataFeed>),
                ..VelaDeps::default()
            },
        )
        .unwrap_or_else(|e| panic!("the chart is built: {e}"));
        // A host subscribes right after constructing the chart.
        let log = EventLog::attach(&chart);
        Rig {
            platform,
            env,
            renderer,
            feed,
            engine,
            chart,
            log,
        }
    }
}

/// An engine context snapshot with a strategy state, one variable pair and two
/// trades (the context of the original RunEngine).
pub fn run_context() -> EngineContextSnapshot {
    use vela_core::model::strategy::{StrategyFill, StrategySide, StrategyState, StrategyTrade};
    let fill = |id: &str, time: i64, price: f64| StrategyFill {
        id: id.to_string(),
        time,
        price,
        comment: None,
    };
    EngineContextSnapshot {
        language: "pine".to_string(),
        phase: ContextPhase::Idle,
        bar_index: 9,
        meta: ContextMeta {
            title: "Ctx".to_string(),
            overlay: false,
            precision: None,
            shorttitle: None,
        },
        plots: [(
            "a".to_string(),
            vec![ContextPlotPoint {
                time: 1,
                value: serde_json::json!(2),
            }],
        )]
        .into(),
        variables: [
            ("posSize".to_string(), serde_json::json!(2)),
            ("len".to_string(), serde_json::json!(14)),
        ]
        .into(),
        strategy: Some(StrategyState {
            position: 2.0,
            equity: 10_000.0,
            ..StrategyState::default()
        }),
        trades: Some(vec![
            StrategyTrade {
                id: "t0".to_string(),
                side: StrategySide::Long,
                qty: 2.0,
                entry: fill("Long", 1, 100.0),
                exit: Some(fill("Exit", 2, 110.0)),
                open: false,
                pnl: Some(9.5),
                commission: Some(0.5),
                max_drawdown: Some(2.5),
                max_runup: Some(12.0),
            },
            StrategyTrade {
                id: "t1".to_string(),
                side: StrategySide::Long,
                qty: 2.0,
                entry: fill("Long", 3, 105.0),
                exit: None,
                open: true,
                pnl: None,
                commission: None,
                max_drawdown: None,
                max_runup: None,
            },
        ]),
        warnings: Vec::new(),
    }
}
