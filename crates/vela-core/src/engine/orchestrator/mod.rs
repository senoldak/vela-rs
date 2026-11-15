//! The engine orchestrator (`EngineOrchestrator.ts`): owns a chart's state and
//! drives the renderer, the market-data feed, the scripting engines, the native
//! indicators, the panes and the bar replay.
//!
//! Renderer- and engine-agnostic: it imports neither a concrete renderer nor a
//! concrete scripting engine.
//!
//! # Layout
//!
//! `mod.rs` holds the shared definitions (constants, [`ResolvedConfig`], the
//! [`ResolveOnce`] primitive, `Inner` and `State`, the constructor, the
//! controller trait impls). One file per section of the original holds the
//! methods, all as `impl EngineOrchestrator`:
//!
//! - `market.rs`: construction-time wiring, market load, `set_market`, live
//!   bars, gap healing, price format, symbol facts, price style, viewport,
//!   theme, destroy.
//! - `replay.rs`: everything named `replay*` and its helpers.
//! - `indicators.rs`: script and native indicators, code swaps, models.
//! - `panes.rs`: pane order, collapse and maximize state, moving indicators.
//!
//! # Ownership and shape (the rules every section follows)
//!
//! - [`EngineOrchestrator`] is one shared handle (`Rc<Inner>`). Collaborators
//!   that never change after construction are plain fields of `Inner`; every
//!   mutable field of the original is a snake_case field of `State`, behind one
//!   `RefCell`.
//! - Never hold a borrow of `State` (or of an `IndicatorRecord`) across an
//!   `.await` or while calling out: the renderer, the feed, an engine or a
//!   session, a native indicator, the event bus, a controller, a user callback.
//!   All of them can call back in. Read what is needed in a short borrow, drop
//!   it, call out, borrow again to write back.
//! - Callbacks registered on long-lived collaborators (renderer listeners, feed
//!   subscriptions, timers, native contexts) capture a `Weak<Inner>`. A future
//!   in flight holds a strong handle, as a JavaScript promise closure holds
//!   `this`: it ends when the awaited work ends.
//! - An `async` method of the original is a `fn` returning a boxed future. A
//!   JavaScript `async` function runs synchronously up to its first `await`
//!   when it is CALLED; a Rust future does nothing until polled. Where the
//!   original relies on that prefix, the Rust method does it before it builds
//!   the future, or runs the call through `eager` (`detach` does that for a
//!   fire-and-forget call).
//! - JavaScript's `await` always yields a turn, even on a settled promise; a
//!   Rust `.await` on a ready future does not. Every `await` of the original is
//!   [`js_await`] here (the awaited future, then one turn), so the code after
//!   an await never runs in the same poll as the code before it, and a
//!   constructor that starts a load returns before any of it lands. Plain
//!   `.await` is used only where the original attaches a `.then` callback
//!   (one turn by itself) or reads a promise that is not an `async` result.
//!
//! # Differences from the original, by construction
//!
//! - The module globals the original reads (the chart-type registry, the native
//!   indicator registry, the drawing registry, the theme registry, the DOM
//!   `document`, `console`) are explicit: the registries come in through
//!   [`Services`], warnings go through `Platform::warn`, and the tab-visibility
//!   catch-up is a method a host calls ([`EngineOrchestrator::on_page_visible`]).
//! - `bars` (the view) and `raw_bars` are two vectors. The original makes them
//!   the same array while no bar transform is active; Rust copies instead.
//! - The `Translator` is `Option<Rc<dyn Translator>>`; `None` is the English
//!   translator the original starts with (`text_of` reads `None` that way).
//! - The renderer's `set_native_data(layer, &Value)` takes `Value::Null` where
//!   the original passes `undefined`.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use serde_json::{Map, Value};

use crate::chart_types::ChartTypeRegistry;
use crate::controls::data_control::DataControl;
use crate::controls::panes_control::PaneController;
use crate::controls::replay_control::{
    ReplayBounds, ReplayController, ReplayEndReason, ReplayStartOptions, ReplayState, ReplayTick,
    ReplayTickSource,
};
use crate::data::provider_registry::eager;
use crate::data::timeframe_to_ms;
use crate::drawings::DrawingSeriesGateway;
use crate::drawings::controller::{DrawingController, TranslatorSource};
use crate::drawings::registry::DrawingRegistry;
use crate::drawings::toolbar::DrawingsOption;
use crate::engine::drawing_series_service::{DrawingSeriesDeps, DrawingSeriesService};
use crate::engine::indicator_handle::{
    CodeUpdateResult, IndicatorController, IndicatorHandle, IndicatorHandleImpl,
};
use crate::engine::indicator_registry::{IndicatorRegistry, RecordRef};
use crate::engine::live_session::LiveSession;
use crate::events::types::{ChartEvent, ChartEventBus};
use crate::i18n::Translator;
use crate::indicators::NativeIndicatorRegistry;
use crate::indicators::volume::register_volume;
use crate::indicators::vpvr::register_vpvr;
use crate::marks::controller::MarksController;
use crate::model::{Millis, Ohlcv};
use crate::options::ThemeSpec;
use crate::ports::chart_renderer::{
    ChartRenderer, InputChangeEvent, InputChangeKind, PaneMoveDir, PriceStyle, VelaTheme,
};
use crate::ports::market_data::{MarketConfig, MarketDataFeed, SymbolFacts};
use crate::ports::scripting_engine::{
    ContextKey, EngineContextSnapshot, EngineError, HistoryState, ScriptingEngine, VisibleBarRange,
};
use crate::price_styles::{BarTransform, bar_transform_for};
use crate::runtime::{
    AbortController, DataError, LocalBoxFuture, Platform, TimerId, Unsubscribe, js_await,
};
use crate::script_run::ScriptRunCause;
use crate::theme::ThemeRegistry;

mod indicators;
mod market;
mod panes;
mod replay;
#[cfg(test)]
pub(crate) mod test_support;

pub use indicators::NativeIndicatorOptions;

// The sections reach `SetBarOpts` (how `set_bar_series` hands a series to the renderer)
// through `super::*`.
#[allow(unused_imports)]
use market::SetBarOpts;

use crate::model::Inputs;
use crate::options::{AddIndicatorOptions, PaneInfo};
use crate::ports::chart_renderer::MoveTarget;

// ── constants ──────────────────────────────────────────────

/// Coalesce rapid pan/zoom viewport events into one re-run (ms).
pub(crate) const VIEWPORT_DEBOUNCE_MS: f64 = 150.0;
// Read by a section that is not ported yet.
#[allow(dead_code)]
/// `script:run` collapse window for FORMING-bar runs only: a stream re-executes
/// the open candle several times a second, and a dashboard cannot use that rate.
/// Every other cause (a new bar above all) is emitted unconditionally.
pub(crate) const RUN_EMIT_THROTTLE_MS: f64 = 1_000.0;

/// Bars in the quick recent-window preview a RANGELESS feed paints before the
/// deep fetch.
pub(crate) const PREVIEW_BARS: usize = 300;
/// Depth a ranged feed serves in ONE request. A round trip costs far more than
/// the extra rows: splitting this range into a small head plus a stream of
/// widening chunks paid for itself only in time-to-first-candle, and nothing
/// downstream can use those first candles anyway: an indicator's first run is
/// held until the whole depth has landed (Pine is causal, so running it per
/// chunk would repaint a different curve each time). Below this line, one
/// request wins outright. (Feeds with `load_progressive` never reach this
/// split: their SOURCE decides what is paintable, snapshot by snapshot.)
pub(crate) const SINGLE_LOAD_BARS: usize = 5_000;
/// Beyond [`SINGLE_LOAD_BARS`] the history loads BACKWARD in chunks of this many
/// bars: the chart is interactive after the first chunk while older ones stream
/// in behind it (each a bounded request the data source answers quickly,
/// instead of one giant fetch that stalls silently and lands as a single
/// main-thread-freezing parse).
pub(crate) const CHUNK_BARS: usize = 10_000;
/// A progressive source's FIRST paint waits for at least this many bars (or the
/// full ask, whichever is smaller): the first paint is the frame the renderer
/// sizes the view against, and framing onto a 2-3 bar head draws a few giant
/// candles that later view-preserved repaints never fix. The FINAL answer always
/// paints whatever exists: a genesis-era symbol may simply have fewer bars than
/// this. The number's history: 100 starved short monthly contracts for the
/// provider's whole poll budget (~90 s measured); briefly 1 (no hold), which
/// framed unusable slivers; 20 frames a readable view on every timeframe now
/// that the server converges small monthlies in seconds.
pub(crate) const FIRST_PAINT_BARS: usize = 20;
/// A live bar more than this many intervals ahead of the last one signals MISSED
/// bars (a throttled background tab, a socket reconnect, system sleep): backfill.
/// 1.5 tolerates timestamp drift and variable-length periods (a 31-day month on
/// the ~30-day 'M' interval) without false positives.
pub(crate) const GAP_FACTOR: f64 = 1.5;
/// Minimum time between gap heals. A heal that comes back EMPTY (a legitimately
/// tradeless interval on an illiquid symbol: providers omit empty candles) must
/// not re-trigger on every tick; within the cooldown a discontinuous bar is
/// accepted as a real market gap.
pub(crate) const HEAL_COOLDOWN_MS: f64 = 5_000.0;

// Read by a section that is not ported yet.
#[allow(dead_code)]
/// Floor between two intrabar updates of a replayed bar (~one frame): at a
/// faster pace, ticks are folded into one update rather than painted faster than
/// the screen refreshes.
pub(crate) const MIN_TICK_UPDATE_MS: f64 = 16.0;

// ── configuration and services ─────────────────────────────

/// The chart's resolved configuration. The market is mutated in place by
/// `set_market` (the original's config object is what every reader holds), and
/// the theme by `set_theme`.
#[derive(Clone, Debug)]
pub struct ResolvedConfig {
    pub market: MarketConfig,
    pub live: bool,
    pub theme: VelaTheme,
    /// Language used when `add_indicator` doesn't specify one.
    pub default_language: String,
    /// Interactive user-drawings configuration (toolbar/tools/groups).
    pub drawings: Option<DrawingsOption>,
    /// Auto-add the built-in volume indicator (bottom-anchored bars on the price
    /// pane). Absent reads as off, as in the original.
    pub volume: Option<bool>,
}

/// The registries the original reads from module globals. The mutable ones are
/// shared cells so a host can register a chart type, a native indicator or a
/// theme after the chart exists and the chart sees it.
#[derive(Clone)]
pub struct Registries {
    pub chart_types: Rc<RefCell<ChartTypeRegistry>>,
    pub native_indicators: Rc<RefCell<NativeIndicatorRegistry>>,
    pub drawings: Rc<DrawingRegistry>,
    pub themes: Rc<RefCell<ThemeRegistry>>,
}

impl Registries {
    /// The built-in chart types and drawing tools, the `volume` and `vpvr`
    /// native indicators, and no registered themes.
    pub fn builtin() -> Self {
        let mut natives = NativeIndicatorRegistry::new();
        register_volume(&mut natives);
        register_vpvr(&mut natives);
        Self {
            chart_types: Rc::new(RefCell::new(ChartTypeRegistry::builtin())),
            native_indicators: Rc::new(RefCell::new(natives)),
            drawings: Rc::new(DrawingRegistry::builtin()),
            themes: Rc::new(RefCell::new(ThemeRegistry::new())),
        }
    }
}

/// Everything the constructor takes beyond the original's parameters.
#[derive(Clone)]
pub struct Services {
    pub registries: Registries,
}

impl Services {
    /// Built-in registries.
    pub fn builtin() -> Self {
        Self {
            registries: Registries::builtin(),
        }
    }
}

/// Why `add_indicator` refused an id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndicatorIdError {
    /// `[vela] addIndicator: `id` must be a non-empty string`.
    Empty,
    /// The id is already live on this chart.
    Duplicate(String),
}

impl fmt::Display for IndicatorIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("[vela] addIndicator: `id` must be a non-empty string"),
            Self::Duplicate(id) => write!(
                f,
                "[vela] addIndicator: indicator id \"{id}\" is already live on this chart"
            ),
        }
    }
}

impl std::error::Error for IndicatorIdError {}

// ── resolve once, await many ───────────────────────────────

struct OnceShared<T> {
    value: RefCell<Option<T>>,
    wakers: RefCell<Vec<Waker>>,
}

/// A value settled at most once and awaited by any number of callers: the
/// original's `new Promise(resolve => ...)` whose promise several callers
/// `await` (`readyPromise`, `historyCompletePromise`, the `supersedeWaiters`
/// race, a replay tick fetch's `done`). `runtime::oneshot` has one consumer;
/// this has many, and later awaiters see the value at once.
///
/// The first [`resolve`](Self::resolve) wins and every later one is ignored,
/// which is also what makes it a `Promise.race` between two producers. Clones
/// share the one cell.
pub struct ResolveOnce<T> {
    shared: Rc<OnceShared<T>>,
}

impl<T> Clone for ResolveOnce<T> {
    fn clone(&self) -> Self {
        Self {
            shared: Rc::clone(&self.shared),
        }
    }
}

impl<T> Default for ResolveOnce<T> {
    fn default() -> Self {
        Self {
            shared: Rc::new(OnceShared {
                value: RefCell::new(None),
                wakers: RefCell::new(Vec::new()),
            }),
        }
    }
}

impl<T: Clone> ResolveOnce<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Settle with `value` and wake every awaiter. Returns whether this call
    /// was the one that settled it (false: already settled, `value` dropped).
    pub fn resolve(&self, value: T) -> bool {
        {
            let mut slot = self.shared.value.borrow_mut();
            if slot.is_some() {
                return false;
            }
            *slot = Some(value);
        }
        let wakers = std::mem::take(&mut *self.shared.wakers.borrow_mut());
        for waker in wakers {
            waker.wake();
        }
        true
    }

    pub fn is_resolved(&self) -> bool {
        self.shared.value.borrow().is_some()
    }

    /// The settled value, if any.
    pub fn get(&self) -> Option<T> {
        self.shared.value.borrow().clone()
    }

    /// A future resolving to the value once it is settled (at once if it
    /// already is). Take it before settling; awaiting one that outlives every
    /// resolver pends forever, like a promise nobody resolves.
    pub fn wait(&self) -> ResolveWait<T> {
        ResolveWait {
            shared: Rc::clone(&self.shared),
        }
    }
}

/// The future of [`ResolveOnce::wait`].
pub struct ResolveWait<T> {
    shared: Rc<OnceShared<T>>,
}

impl<T: Clone> Future for ResolveWait<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        if let Some(value) = self.shared.value.borrow().clone() {
            return Poll::Ready(value);
        }
        let mut wakers = self.shared.wakers.borrow_mut();
        if !wakers.iter().any(|w| w.will_wake(cx.waker())) {
            wakers.push(cx.waker().clone());
        }
        Poll::Pending
    }
}

// ── state ──────────────────────────────────────────────────

/// A shared chart-type data engine. The `RefCell` is borrowed only for the
/// duration of one call into the engine, never while `State` is borrowed.
pub(crate) type TypeEngine = Rc<RefCell<Box<dyn crate::chart_types::SeriesDataEngine>>>;

/// The depth (`market.bars`) before a replay deepened the history, restored
/// when it ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReplayDepth {
    pub bars: Option<usize>,
}

/// A start is loading older history before it can cut. `seek`: a replay was
/// running (and left silently to deepen), so abandoning the load must announce
/// its end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReplayLoading {
    pub seek: bool,
}

/// A replay carried over a timeframe/session switch, between the old series'
/// silent end and the cut of the new one: still ACTIVE for the outside
/// (`replay_state`), resumed at `revealed_end` once the new bars land, unless
/// stopped (`cancelled`) meanwhile. Shared by identity (`Rc::ptr_eq`), as the
/// original compares the object.
#[derive(Debug)]
pub(crate) struct ReplayCarry {
    pub revealed_end: Millis,
    pub playing: bool,
    pub cancelled: Cell<bool>,
}

// Read by a section that is not ported yet.
#[allow(dead_code)]
/// The bar being revealed tick by tick: its stored form (what it settles on),
/// its ticks, the next tick to apply, the candle built so far, the volume each
/// tick adds when the ticks carry none, and its pacing (ticks per update, ms
/// between updates (one play interval per tick) and the clock time tick 0 is
/// due at, so late timers catch up).
#[derive(Clone, Debug)]
pub(crate) struct ReplayForming {
    pub bar: Ohlcv,
    pub ticks: Rc<[ReplayTick]>,
    pub next: usize,
    pub candle: Option<Ohlcv>,
    pub volume_each: Option<f64>,
    pub batch: usize,
    pub delay_ms: f64,
    pub origin: f64,
}

// Read by a section that is not ported yet.
#[allow(dead_code)]
/// One tick request of a queued bar, shared by prefetch and playback: `ticks`
/// once they landed, `done` settled when the request ended either way.
pub(crate) struct TickFetch {
    pub ticks: RefCell<Option<Rc<[ReplayTick]>>>,
    pub done: ResolveOnce<()>,
}

/// The volume auto-add waiting for its symbol's facts (they landed after the
/// first candles). Compared by identity.
#[derive(Debug)]
pub(crate) struct VolumeFactsWait {
    pub symbol: String,
}

// Read by a section that is not ported yet.
#[allow(dead_code)]
/// Every mutable field of the original, same names in snake_case.
struct State {
    /// The resolved config (`this.config`): the market is mutated in place.
    config: ResolvedConfig,
    /// Top-to-bottom pane display order (price always first). The routing +
    /// `chart.panes` source of truth; the renderer mirrors it via `order_panes`.
    pane_order: Vec<String>,
    /// Panes currently collapsed to a strip (mirror of the renderer's visual state).
    collapsed_panes: HashSet<String>,
    /// The maximized pane (mirror), or `None` when the split is normal.
    maximized_pane_id: Option<String>,
    pane_action_unsub: Option<Unsubscribe>,
    /// The chart's translator: native indicators' names read in its language.
    /// `None` is the English translator.
    i18n: Option<Rc<dyn Translator>>,
    /// Live handles of every indicator on the chart (script + native), in
    /// insertion order (the original's `Map` order).
    handles: Vec<Rc<IndicatorHandleImpl>>,
    engines: HashMap<String, Rc<dyn ScriptingEngine>>,
    live: Option<Rc<LiveSession>>,
    /// The chart's bar VIEW: what the renderer, engines and natives consume.
    /// With no bar transform active it equals `raw_bars` (a copy here, the same
    /// array in the original); under a transforming price style (Heikin Ashi)
    /// it is the derived series, index-aligned 1:1 with the raw one.
    bars: Vec<Ohlcv>,
    /// The RAW market data, exactly as the feed served it: the source of truth.
    /// The `bar` event, gap-heal reconciliation and `chart.data` stay on this
    /// plane; nothing synthetic is ever stored or emitted as data.
    raw_bars: Vec<Ohlcv>,
    /// The active price style's 1:1 bar transform (`None` is the raw view).
    bar_transform: Option<Arc<dyn BarTransform>>,
    // chart-type data engines (SDK): one lazily-created engine per style id
    type_engines: HashMap<String, TypeEngine>,
    /// Engines suspended by the chart: their host pushes are dropped until they
    /// resume (the original's `WeakSet`).
    suspended_engines: Vec<Weak<RefCell<Box<dyn crate::chart_types::SeriesDataEngine>>>>,
    /// Last-seen chart-type settings per type id (dialog edits + persisted
    /// restores): replayed into a data engine whenever one is (re)created.
    type_settings: HashMap<String, Map<String, Value>>,
    /// The style id whose data engine is currently ACTIVE (drives
    /// suspend/resume + pokes).
    active_engine_style: Option<String>,
    /// The chart's current price style (tracked from the renderer's change
    /// events + the initial read).
    price_style: PriceStyle,
    price_style_will_change_unsub: Option<Unsubscribe>,
    unresolved_unsub: Option<Unsubscribe>,
    /// Latest chart visible range (left/right bar times), fed to
    /// viewport-dependent scripts.
    visible_range: Option<VisibleBarRange>,
    viewport_unsub: Option<Unsubscribe>,
    viewport_timer: Option<TimerId>,
    // live gap-heal state (missed bars while a tab was backgrounded/frozen)
    /// A backfill fetch is in flight; live ticks arriving meanwhile buffer and
    /// replay after it.
    healing: bool,
    heal_buffer: Vec<Ohlcv>,
    last_heal_at: f64,
    /// The tab-restore catch-up is armed (the original holds the
    /// `visibilitychange` listener here, registered by `start_live` and removed
    /// by `stop_live`). A host calls `on_page_visible` when the tab returns.
    on_visible: bool,
    // bar replay: `raw_bars` holds the history up to the cursor, the rest waits here
    /// Bars still hidden right of the replay cursor, oldest first; `None` is
    /// replay off.
    replay_queue: Option<VecDeque<Ohlcv>>,
    replay_timer: Option<TimerId>,
    replay_playing: bool,
    replay_interval_ms: f64,
    /// The depth (`market.bars`) before a replay deepened the history: restored
    /// when it ends.
    replay_depth: Option<ReplayDepth>,
    /// A start is loading older history before it can cut.
    replay_loading: Option<ReplayLoading>,
    /// A replay carried over a timeframe/session switch (see [`ReplayCarry`]).
    replay_carrying: Option<Rc<ReplayCarry>>,
    /// Intrabar prices for the bars replay reveals; `None` is whole bars.
    replay_tick_source: Option<ReplayTickSource>,
    /// The bar being revealed tick by tick.
    replay_forming: Option<ReplayForming>,
    /// Tick requests by bar time, shared by prefetch and playback; `ticks` once
    /// they landed.
    replay_tick_fetches: HashMap<Millis, Rc<TickFetch>>,
    /// Aborts the tick requests in flight: renewed whenever the replay moves on.
    replay_tick_abort: Option<AbortController>,
    /// What the last applied bar did: refine the open candle (`Tick`), or open a
    /// new one, which settles the previous (`Bar`). Read by the `script:run`
    /// cause attribution.
    bar_cause: ScriptRunCause,
    // deep-history backfill (older chunks streaming in behind the interactive chart)
    /// Where the history load stands. Stamped into new sessions + every bar
    /// notification.
    history_state: HistoryState,
    /// Invalidates detached async work (backfill loops, in-flight loads, gap
    /// heals): bumped by `init`, `set_market` and `destroy`.
    generation: u64,
    /// Aborts the in-flight PROGRESSIVE load's source polling on supersession:
    /// an abandoned stream left polling to its own budget starves the browser's
    /// per-host connection pool, and the NEXT symbol's very first fetch with it
    /// (measured).
    progressive_abort: Option<AbortController>,
    /// Awaiters racing a superseded load (`set_market` callers): released on
    /// every bump so they never hang.
    supersede_waiters: Vec<Box<dyn FnOnce()>>,
    /// `history:complete` fired for the CURRENT load. Each market load re-arms
    /// the cycle (see `reset_history_tracking`): `history_complete()` is
    /// per-load, not per-chart.
    history_completed: bool,
    /// The current load's completion (`historyCompletePromise` +
    /// `resolveHistoryComplete`).
    history_complete: ResolveOnce<()>,
    /// A `set_market` switch is in flight: bar/viewport pokes to the OLD
    /// sessions are held (they must not re-run over the incoming market's bars:
    /// re-execution follows the load).
    switching_market: bool,
    /// A load is in flight with nothing painted: the state behind the renderer's
    /// loading affordance and the `load:start`/`load:end` event pair
    /// (transition-guarded).
    loading_up: bool,
    /// The load generation whose market the source refused for good
    /// (`data:unavailable`): nothing streams, heals or re-asks until the next
    /// load. A new generation clears it. (The original's `-1` is `None`.)
    unavailable_gen: Option<u64>,
    /// The user removed the volume indicator: the auto-add must never override that.
    volume_opted_out: bool,
    /// The volume auto-add waiting for its symbol's facts (they landed after the
    /// first candles).
    volume_facts_wait: Option<Rc<VolumeFactsWait>>,
    /// The facts the renderer was last handed (see `sync_symbol_facts`); `None`
    /// is unknown.
    facts: Option<SymbolFacts>,
    /// Bumped per facts probe and at destroy: only the latest probe may apply
    /// its answer.
    facts_probe: u64,
    /// Throttled (1/s per indicator) `context:changed` bookkeeping.
    context_emit_at: HashMap<String, f64>,
    /// `script:run` throttle bookkeeping (forming-bar runs only).
    run_emit_at: HashMap<String, f64>,
    /// The context each running native instance was started with, by indicator
    /// id (the original's context object, which the instance keeps; here the
    /// orchestrator keeps it and lends a fresh one to every lifecycle call).
    native_contexts: HashMap<String, Rc<indicators::NativeCtxMeta>>,
    /// Set by `destroy`. The original has no flag (it bumps the generation and
    /// clears the bus); this guards the callbacks a renderer or timer might
    /// still deliver to a shared handle that outlives the chart.
    destroyed: bool,
}

/// The shared core: collaborators that never change after construction, and the
/// one `RefCell` of mutable state.
struct Inner {
    /// This `Inner`, for the adapters that hand out `Weak<dyn ...Controller>`
    /// and for callbacks that must not keep the chart alive.
    me: Weak<Inner>,
    platform: Rc<dyn Platform>,
    renderer: Rc<dyn ChartRenderer>,
    feed: Rc<dyn MarketDataFeed>,
    data_control: DataControl,
    default_language: String,
    events: ChartEventBus,
    registry: IndicatorRegistry,
    chart_types: Rc<RefCell<ChartTypeRegistry>>,
    native_registry: Rc<RefCell<NativeIndicatorRegistry>>,
    theme_registry: Rc<RefCell<ThemeRegistry>>,
    /// The user-drawings manager (backs `chart.drawings`).
    drawings: DrawingController,
    /// The timeline-marks manager (backs `chart.marks`).
    marks: MarksController,
    /// Settled when the first load pipeline ends (`readyPromise`).
    ready: ResolveOnce<Result<(), DataError>>,
    state: RefCell<State>,
}

/// Renderer- and engine-agnostic orchestration: owns market data (via the
/// injected [`MarketDataFeed`]), runs/streams indicators through registered
/// [`ScriptingEngine`]s (selected by language), routes panes, and drives the
/// injected [`ChartRenderer`]. Imports neither a concrete renderer nor a
/// concrete scripting engine.
///
/// A cheap shared handle: clones are the same chart.
#[derive(Clone)]
pub struct EngineOrchestrator {
    inner: Rc<Inner>,
}

/// Registers an engine under `language`, warning when a different one is
/// replaced (last registration wins).
fn insert_engine(
    platform: &Rc<dyn Platform>,
    engines: &mut HashMap<String, Rc<dyn ScriptingEngine>>,
    language: &str,
    engine: Rc<dyn ScriptingEngine>,
) {
    if let Some(existing) = engines.get(language)
        && !Rc::ptr_eq(existing, &engine)
    {
        platform.warn(&format!(
            "[vela] re-registering scripting engine for language \"{language}\"; replacing the previous engine."
        ));
    }
    engines.insert(language.to_string(), engine);
}

fn upgrade(weak: &Weak<Inner>) -> Option<EngineOrchestrator> {
    weak.upgrade().map(|inner| EngineOrchestrator { inner })
}

impl EngineOrchestrator {
    /// Build the chart's engine and start its first load.
    ///
    /// The original's `container` is gone (a DOM-owning renderer gets its
    /// container from its host crate). The wiring order is the original's:
    /// engines, `mount`, the renderer subscriptions, the initial price style,
    /// the drawing and marks controllers, the unresolved-symbol subscription,
    /// then the first load (`init`, whose synchronous prefix emits `load:start`
    /// before this returns), then the seed of a constructed chart-type data
    /// engine.
    pub fn new(
        platform: Rc<dyn Platform>,
        renderer: Rc<dyn ChartRenderer>,
        feed: Rc<dyn MarketDataFeed>,
        engines: Vec<Rc<dyn ScriptingEngine>>,
        config: ResolvedConfig,
        data_control: DataControl,
        services: Services,
    ) -> Self {
        let Services { registries } = services;
        let events = ChartEventBus::new();
        // Register the construction-time engines (bulk sugar for register_engine).
        let mut engine_map: HashMap<String, Rc<dyn ScriptingEngine>> = HashMap::new();
        for engine in engines {
            let language = engine.language().to_string();
            insert_engine(&platform, &mut engine_map, &language, engine);
        }
        let default_language = config.default_language.clone();
        let theme = config.theme.clone();
        let drawings_option = config.drawings.clone();
        let chart_types = Rc::clone(&registries.chart_types);

        let inner = Rc::new_cyclic(|weak: &Weak<Inner>| {
            renderer.mount(&theme);
            // The renderer's in-chart settings dialog reports edits here: re-run
            // that indicator.
            {
                let w = weak.clone();
                drop(
                    renderer.on_input_change(Box::new(move |e: &InputChangeEvent| {
                        let Some(o) = upgrade(&w) else { return };
                        let mut values = Inputs::new();
                        values.insert(e.key.clone(), e.value.clone());
                        match e.kind() {
                            InputChangeKind::Prop => o.apply_props(&e.indicator_id, values),
                            InputChangeKind::Input => o.apply_inputs(&e.indicator_id, values),
                        }
                    })),
                );
            }
            // The renderer's in-chart legend X reports a removal here: tear the
            // indicator down.
            {
                let w = weak.clone();
                drop(renderer.on_remove_indicator(Box::new(move |id: &String| {
                    if let Some(o) = upgrade(&w) {
                        o.remove_indicator(id);
                    }
                })));
            }
            // The renderer's in-chart legend eye reports a hide/show here:
            // suspend/resume the indicator.
            {
                let w = weak.clone();
                drop(
                    renderer.on_toggle_indicator_visible(Box::new(move |id: &str, visible| {
                        if let Some(o) = upgrade(&w) {
                            o.set_visible(id, visible);
                        }
                    })),
                );
            }
            // Pane hover buttons + double-clicks report layout intents here
            // (delete tears down the pane's indicators; the rest are already
            // applied by the renderer: this keeps the `chart.panes` view +
            // collapse/maximize mirror in sync).
            let pane_action_unsub = {
                let w = weak.clone();
                renderer.on_pane_action(Box::new(move |a| {
                    if let Some(o) = upgrade(&w) {
                        o.handle_pane_action(a);
                    }
                }))
            };
            // Chart-type SDK settings edited in the renderer's dialog (or
            // replayed by apply_config on a persisted restore): remember them AND
            // forward to the type's data engine. The cache matters: the engine
            // may not exist yet (a restored config lands before the style's first
            // activation) or may be recreated later (a market switch):
            // `start_chart_type_engine` replays the last-seen values into every
            // fresh engine, so one never fetches on schema defaults the user has
            // edited away.
            {
                let w = weak.clone();
                drop(renderer.on_chart_type_settings_change(Box::new(
                    move |type_id: &str, values: &Map<String, Value>| {
                        let Some(o) = upgrade(&w) else { return };
                        o.on_chart_type_settings(type_id, values);
                    },
                )));
            }
            // The renderer's legend "Move to" menu / row drag reports a move
            // here: route it.
            {
                let w = weak.clone();
                drop(
                    renderer.on_move_indicator(Box::new(move |id: &str, target: &MoveTarget| {
                        if let Some(o) = upgrade(&w) {
                            o.move_indicator(id, target.clone());
                        }
                    })),
                );
            }
            // The renderer's in-chart theme control (settings dialog Canvas,
            // Theme) reports a request here: the core owns the canonical theme,
            // applies it, and announces it.
            {
                let w = weak.clone();
                drop(renderer.on_theme_select(Box::new(move |name: &String| {
                    let Some(o) = upgrade(&w) else { return };
                    let resolved = o
                        .inner
                        .theme_registry
                        .borrow()
                        .resolve(Some(&ThemeSpec::Id(name.clone())));
                    o.set_theme(resolved);
                })));
            }
            // Pan/zoom: re-run ONLY visible-range-dependent scripts (e.g.
            // visible-range volume profile) with the new window. Debounced so a
            // drag re-runs once.
            let viewport_unsub = {
                let w = weak.clone();
                renderer.on_viewport_change(Box::new(move |range| {
                    if let Some(o) = upgrade(&w) {
                        o.on_viewport_change(*range);
                    }
                }))
            };
            // Price-style changes (feature set / settings dialog / config
            // template): a chart type may carry a DATA requirement beyond
            // drawing: a bar-stream transform (heikinashi) and/or a registered
            // data engine (SDK chart types).
            {
                let w = weak.clone();
                drop(
                    renderer.on_price_style_change(Box::new(move |style: &PriceStyle| {
                        if let Some(o) = upgrade(&w) {
                            o.sync_price_style(Some(style));
                        }
                    })),
                );
            }
            // Announced from the same write path BEFORE the renderer repaints, so
            // a host can capture the outgoing frame (style-switch transitions).
            let price_style_will_change_unsub = {
                let w = weak.clone();
                renderer.on_price_style_will_change(Box::new(
                    move |from: &PriceStyle, to: &PriceStyle| {
                        if let Some(o) = upgrade(&w) {
                            o.emit(ChartEvent::PriceStyleChange {
                                from: from.clone(),
                                to: to.clone(),
                            });
                        }
                    },
                ))
            };
            // Seed the bar transform from the CONSTRUCTED style (a chart created
            // with `priceStyle: 'heikinashi'`) so the initial load already
            // produces the view.
            let initial_style = renderer.read_feature("priceStyle");
            let initial_style = initial_style.as_ref().and_then(Value::as_str);
            let price_style = initial_style.map_or_else(|| "candles".to_string(), str::to_string);
            let bar_transform =
                initial_style.and_then(|s| bar_transform_for(&chart_types.borrow(), s));
            // User drawings: owns the model + tool/selection state, drives the
            // renderer's drawings port (inert when the renderer lacks the
            // `userDrawings` capability). The series service backs data-driven
            // drawings that read a finer timeframe: it rides the feed's
            // cache-backed ranged fetch, same path as `request.security`.
            let series_deps: Rc<dyn DrawingSeriesDeps> = Rc::new(SeriesDeps {
                inner: weak.clone(),
            });
            let gateway: Rc<dyn DrawingSeriesGateway> =
                Rc::new(DrawingSeriesService::new(series_deps, Rc::clone(&platform)));
            let i18n_source: TranslatorSource = {
                let w = weak.clone();
                Rc::new(move || {
                    w.upgrade()
                        .and_then(|inner| inner.state.borrow().i18n.clone())
                })
            };
            let drawings = DrawingController::new(
                Rc::clone(&renderer),
                events.clone(),
                Rc::clone(&registries.drawings),
                drawings_option,
                Some(gateway),
                i18n_source,
            );
            let marks =
                MarksController::new(Rc::clone(&renderer), events.clone(), Rc::clone(&platform));
            // A symbol nothing can serve leaves the load PARKED; publish it so a
            // host can say so instead of showing a blank chart forever (it still
            // resumes if a provider registers). A parked load also ends the
            // loading state: nothing is coming, and an endless animation over the
            // empty chart would promise otherwise.
            let unresolved_unsub = {
                let w = weak.clone();
                feed.on_unresolved(Box::new(move |info| {
                    let Some(o) = upgrade(&w) else { return };
                    o.end_load();
                    o.emit(ChartEvent::DataUnresolved {
                        symbol: info.symbol.clone(),
                        providers: info.providers.clone(),
                    });
                }))
            };
            Inner {
                me: weak.clone(),
                platform: Rc::clone(&platform),
                renderer: Rc::clone(&renderer),
                feed: Rc::clone(&feed),
                data_control,
                default_language,
                events: events.clone(),
                registry: IndicatorRegistry::new(),
                chart_types: Rc::clone(&registries.chart_types),
                native_registry: Rc::clone(&registries.native_indicators),
                theme_registry: Rc::clone(&registries.themes),
                drawings,
                marks,
                ready: ResolveOnce::new(),
                state: RefCell::new(State {
                    config,
                    pane_order: vec!["price".to_string()],
                    collapsed_panes: HashSet::new(),
                    maximized_pane_id: None,
                    pane_action_unsub,
                    i18n: None,
                    handles: Vec::new(),
                    engines: engine_map,
                    live: None,
                    bars: Vec::new(),
                    raw_bars: Vec::new(),
                    bar_transform,
                    type_engines: HashMap::new(),
                    suspended_engines: Vec::new(),
                    type_settings: HashMap::new(),
                    active_engine_style: None,
                    price_style,
                    price_style_will_change_unsub,
                    unresolved_unsub,
                    visible_range: None,
                    viewport_unsub: Some(viewport_unsub),
                    viewport_timer: None,
                    healing: false,
                    heal_buffer: Vec::new(),
                    last_heal_at: 0.0,
                    on_visible: false,
                    replay_queue: None,
                    replay_timer: None,
                    replay_playing: false,
                    replay_interval_ms: 1_000.0,
                    replay_depth: None,
                    replay_loading: None,
                    replay_carrying: None,
                    replay_tick_source: None,
                    replay_forming: None,
                    replay_tick_fetches: HashMap::new(),
                    replay_tick_abort: None,
                    bar_cause: ScriptRunCause::Tick,
                    history_state: HistoryState::Complete,
                    generation: 0,
                    progressive_abort: None,
                    supersede_waiters: Vec::new(),
                    history_completed: false,
                    history_complete: ResolveOnce::new(),
                    switching_market: false,
                    loading_up: false,
                    unavailable_gen: None,
                    volume_opted_out: false,
                    volume_facts_wait: None,
                    facts: None,
                    facts_probe: 0,
                    context_emit_at: HashMap::new(),
                    run_emit_at: HashMap::new(),
                    native_contexts: HashMap::new(),
                    destroyed: false,
                }),
            }
        });
        let this = Self { inner };
        // The first load. Its synchronous prefix (generation, `load:start`, the
        // facts probe, the first fetch call) runs here, as in the original.
        this.init();
        // Seed a constructed chart-type DATA engine only now: `start_chart_type_engine`
        // awaits ready, so this MUST come after `init`. Seeding earlier awaited
        // nothing: the engine started before the first load, could not resolve
        // its provider surfaces (e.g. a footprint feed), and a later style
        // re-select only RESUMED the empty engine: a restored plugin style
        // painted nothing until a market switch recreated it.
        let seed = this.inner.renderer.read_feature("priceStyle");
        if let Some(style) = seed.as_ref().and_then(Value::as_str)
            && this
                .inner
                .chart_types
                .borrow()
                .get(style)
                .is_some_and(|def| def.data_engine.is_some())
        {
            this.sync_chart_type_engine(Some(style));
        }
        this
    }

    // ── accessors ──────────────────────────────────────────

    /// The chart's event bus (`events`): a handle on the one bus.
    pub fn events(&self) -> ChartEventBus {
        self.inner.events.clone()
    }

    /// The user-drawings manager (backs `chart.drawings`).
    pub fn drawings(&self) -> DrawingController {
        self.inner.drawings.clone()
    }

    /// The timeline-marks manager (backs `chart.marks`).
    pub fn marks(&self) -> MarksController {
        self.inner.marks.clone()
    }

    /// The chart as the controller the indicator handles hold weakly.
    pub fn indicator_controller(&self) -> Weak<dyn IndicatorController> {
        self.inner.me.clone()
    }

    /// The chart as the controller `chart.panes` holds weakly.
    pub fn pane_controller(&self) -> Weak<dyn PaneController> {
        self.inner.me.clone()
    }

    /// The chart as the controller `chart.replay` holds weakly.
    pub fn replay_controller(&self) -> Weak<dyn ReplayController> {
        self.inner.me.clone()
    }

    // ── helpers shared by the sections ─────────────────────

    fn weak(&self) -> Weak<Inner> {
        self.inner.me.clone()
    }

    /// Publish an event on the chart's bus.
    fn emit(&self, event: ChartEvent) {
        self.inner.events.emit(&event.name(), &event);
    }

    /// `console.warn`.
    fn warn(&self, message: &str) {
        self.inner.platform.warn(message);
    }

    /// `Date.now()` and the file's `now()`: the platform clock.
    fn now(&self) -> f64 {
        self.inner.platform.now_ms()
    }

    /// Run a future in the background (`void promise`).
    fn spawn(&self, future: LocalBoxFuture<'static, ()>) {
        self.inner.platform.spawn(future);
    }

    /// `void this.asyncMethod()`: run the future to its first suspension now
    /// (the synchronous prefix of a JavaScript `async` function), the rest in
    /// the background.
    fn detach(&self, future: LocalBoxFuture<'static, ()>) {
        self.inner.platform.spawn(eager(future));
    }

    fn generation(&self) -> u64 {
        self.inner.state.borrow().generation
    }

    /// A copy of the market config the feed calls take by reference.
    fn market_cfg(&self) -> MarketConfig {
        self.inner.state.borrow().config.market.clone()
    }

    /// The chart's translator; `None` is English.
    fn translator(&self) -> Option<Rc<dyn Translator>> {
        self.inner.state.borrow().i18n.clone()
    }

    /// The live handle of an indicator, by id.
    fn handle_of(&self, id: &str) -> Option<Rc<IndicatorHandleImpl>> {
        self.inner
            .state
            .borrow()
            .handles
            .iter()
            .find(|h| h.id() == id)
            .cloned()
    }

    // Read by a section that is not ported yet.
    #[allow(dead_code)]
    /// The chart's current bar VIEW (a copy).
    fn view_bars(&self) -> Vec<Ohlcv> {
        self.inner.state.borrow().bars.clone()
    }

    /// The timeframe string the chart runs on (`"60"` when unset).
    fn timeframe(&self) -> String {
        self.inner
            .state
            .borrow()
            .config
            .market
            .timeframe
            .clone()
            .unwrap_or_else(|| "60".to_string())
    }

    /// Duration of one chart bar in ms (for live-bar continuity checks).
    fn bar_interval_ms(&self) -> Millis {
        timeframe_to_ms(&self.timeframe())
    }
}

/// What the drawing series service reads from the orchestrator, all live per
/// call. Holds the chart weakly: a fetch in flight keeps the service alive.
struct SeriesDeps {
    inner: Weak<Inner>,
}

impl DrawingSeriesDeps for SeriesDeps {
    fn fetch_bars(
        &self,
        timeframe: &str,
        range: crate::ports::market_data::BarRange,
    ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
        match upgrade(&self.inner) {
            Some(o) => {
                let symbol = o.market_cfg().symbol.unwrap_or_default();
                o.fetch_series(&symbol, timeframe, &range)
            }
            None => Box::pin(std::future::ready(Ok(Vec::new()))),
        }
    }

    fn can_fetch(&self) -> bool {
        upgrade(&self.inner).is_some_and(|o| {
            let market = o.market_cfg();
            o.inner.feed.supports_load_range()
                && market.data.as_ref().is_none_or(|d| d.is_empty())
                && market.symbol.as_deref().is_some_and(|s| !s.is_empty())
        })
    }

    fn chart_timeframe(&self) -> String {
        upgrade(&self.inner).map_or_else(|| "60".to_string(), |o| o.timeframe())
    }

    fn market_key(&self) -> String {
        upgrade(&self.inner).map_or_else(String::new, |o| {
            let market = o.market_cfg();
            format!(
                "{}|{}",
                market.symbol.unwrap_or_default(),
                market.session.map_or("", |s| s.as_str())
            )
        })
    }
}

// ── the controller traits the periphery expects ────────────
//
// The handles and controls hold their controller as `Weak<dyn ...Controller>`,
// and an unsized `Weak` needs the `Rc` payload to implement the trait, so the
// traits are implemented on `Inner` and delegate to the `EngineOrchestrator`
// section methods through `me`.

impl Inner {
    fn orchestrator(&self) -> Option<EngineOrchestrator> {
        upgrade(&self.me)
    }
}

impl IndicatorController for Inner {
    fn get_indicator_context(
        &self,
        id: &str,
        select: Option<&[ContextKey]>,
    ) -> LocalBoxFuture<'static, Result<Option<EngineContextSnapshot>, EngineError>> {
        match self.orchestrator() {
            Some(o) => o.get_indicator_context(id, select),
            None => Box::pin(std::future::ready(Ok(None))),
        }
    }

    fn apply_inputs(&self, id: &str, values: Inputs) {
        if let Some(o) = self.orchestrator() {
            o.apply_inputs(id, values);
        }
    }

    fn input_values_of(&self, id: &str) -> Inputs {
        self.orchestrator()
            .map(|o| o.input_values_of(id))
            .unwrap_or_default()
    }

    fn prop_values_of(&self, id: &str) -> Inputs {
        self.orchestrator()
            .map(|o| o.prop_values_of(id))
            .unwrap_or_default()
    }

    fn apply_props(&self, id: &str, values: Inputs) {
        if let Some(o) = self.orchestrator() {
            o.apply_props(id, values);
        }
    }

    fn update_code(&self, id: &str, source: &str) -> LocalBoxFuture<'static, CodeUpdateResult> {
        match self.orchestrator() {
            Some(o) => o.update_code(id, source),
            None => Box::pin(std::future::ready(CodeUpdateResult::failure(
                "[vela] updateCode: the chart was destroyed.",
            ))),
        }
    }

    fn remove_indicator(&self, id: &str) {
        if let Some(o) = self.orchestrator() {
            o.remove_indicator(id);
        }
    }

    fn set_visible(&self, id: &str, visible: bool) {
        if let Some(o) = self.orchestrator() {
            o.set_visible(id, visible);
        }
    }

    fn move_indicator(&self, id: &str, target: MoveTarget) {
        if let Some(o) = self.orchestrator() {
            o.move_indicator(id, target);
        }
    }
}

impl PaneController for Inner {
    fn pane_management_supported(&self) -> bool {
        self.orchestrator()
            .is_some_and(|o| o.pane_management_supported())
    }

    fn list_panes(&self) -> Vec<PaneInfo> {
        self.orchestrator()
            .map(|o| o.list_panes())
            .unwrap_or_default()
    }

    fn move_pane(&self, pane_id: &str, dir: PaneMoveDir) {
        if let Some(o) = self.orchestrator() {
            o.move_pane(pane_id, dir);
        }
    }

    fn remove_pane_and_indicators(&self, pane_id: &str) {
        if let Some(o) = self.orchestrator() {
            o.remove_pane_and_indicators(pane_id);
        }
    }

    fn collapse_pane(&self, pane_id: &str, collapsed: bool) {
        if let Some(o) = self.orchestrator() {
            o.collapse_pane(pane_id, collapsed);
        }
    }

    fn maximize_pane(&self, pane_id: Option<&str>) {
        if let Some(o) = self.orchestrator() {
            o.maximize_pane(pane_id);
        }
    }

    fn set_pane_height_weight(&self, pane_id: &str, weight: f64) {
        if let Some(o) = self.orchestrator() {
            o.set_pane_height_weight(pane_id, weight);
        }
    }

    fn move_indicator(&self, id: &str, target: MoveTarget) {
        if let Some(o) = self.orchestrator() {
            o.move_indicator(id, target);
        }
    }
}

impl ReplayController for Inner {
    fn replay_start(
        &self,
        opts: ReplayStartOptions,
    ) -> LocalBoxFuture<'static, Result<(), DataError>> {
        match self.orchestrator() {
            Some(o) => o.replay_start(opts),
            None => Box::pin(std::future::ready(Ok(()))),
        }
    }

    fn replay_step(&self) -> bool {
        self.orchestrator().is_some_and(|o| o.replay_step())
    }

    fn replay_step_update(&self) -> bool {
        self.orchestrator().is_some_and(|o| o.replay_step_update())
    }

    fn replay_play(&self, interval_ms: Option<f64>) {
        if let Some(o) = self.orchestrator() {
            o.replay_play(interval_ms);
        }
    }

    fn replay_pause(&self) {
        if let Some(o) = self.orchestrator() {
            o.replay_pause();
        }
    }

    fn replay_stop(&self) {
        if let Some(o) = self.orchestrator() {
            o.replay_stop();
        }
    }

    fn replay_state(&self) -> ReplayState {
        self.orchestrator()
            .map_or_else(ReplayState::off, |o| o.replay_state())
    }

    fn replay_bounds(&self) -> Option<ReplayBounds> {
        self.orchestrator().and_then(|o| o.replay_bounds())
    }

    fn replay_set_ticks(&self, source: Option<ReplayTickSource>) {
        if let Some(o) = self.orchestrator() {
            o.replay_set_ticks(source);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::task::Waker;

    use super::*;
    use crate::runtime::testing::ManualPlatform;

    #[test]
    fn every_awaiter_sees_the_one_value_and_the_first_resolve_wins() {
        let platform = ManualPlatform::new(0.0);
        let once = ResolveOnce::<u32>::new();
        let seen = Rc::new(RefCell::new(Vec::new()));
        for label in ["a", "b", "c"] {
            let (wait, seen) = (once.wait(), Rc::clone(&seen));
            platform.spawn(Box::pin(async move {
                let v = wait.await;
                seen.borrow_mut().push((label, v));
            }));
        }
        platform.run();
        assert!(seen.borrow().is_empty());
        assert!(!once.is_resolved());
        assert!(once.resolve(7));
        assert!(!once.resolve(8), "a second resolve is ignored");
        platform.run();
        assert_eq!(*seen.borrow(), [("a", 7), ("b", 7), ("c", 7)]);
        assert_eq!(once.get(), Some(7));
    }

    #[test]
    fn an_awaiter_that_comes_late_is_ready_at_once() {
        let platform = ManualPlatform::new(0.0);
        let once = ResolveOnce::<&'static str>::new();
        once.resolve("done");
        let wait = once.wait();
        assert_eq!(platform.block_on(wait), "done");
        // A clone shares the cell.
        let twin = once.clone();
        assert!(!twin.resolve("again"));
        assert_eq!(platform.block_on(twin.wait()), "done");
    }

    #[test]
    fn polling_registers_one_waker_per_task_and_a_dropped_awaiter_is_harmless() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::task::Wake;

        struct Counter(AtomicUsize);
        impl Wake for Counter {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let counter = Arc::new(Counter(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&counter));
        let mut cx = Context::from_waker(&waker);
        let once = ResolveOnce::<()>::new();
        let mut wait = Box::pin(once.wait());
        assert!(wait.as_mut().poll(&mut cx).is_pending());
        assert!(wait.as_mut().poll(&mut cx).is_pending());
        assert_eq!(once.shared.wakers.borrow().len(), 1);
        // An awaiter dropped before the value lands leaves a stale waker: woken without effect.
        let dropped = once.wait();
        drop(dropped);
        assert!(once.resolve(()));
        assert!(once.shared.wakers.borrow().is_empty());
        assert_eq!(
            counter.0.load(Ordering::SeqCst),
            1,
            "the live awaiter was woken once"
        );
        assert!(wait.as_mut().poll(&mut cx).is_ready());
    }

    #[test]
    fn it_works_as_a_race_between_two_producers() {
        let platform = ManualPlatform::new(0.0);
        let race = ResolveOnce::<&'static str>::new();
        let loaded = race.clone();
        let superseded = race.clone();
        let wait = race.wait();
        loaded.resolve("loaded");
        superseded.resolve("superseded");
        assert_eq!(platform.block_on(wait), "loaded");
    }
}
