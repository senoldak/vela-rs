//! The scripting-engine port: the seam a script interpreter plugs into.
//!
//! The orchestrator talks only to [`ScriptingEngine`], so every concrete engine
//! is swappable and Vela itself ships none: engines are separate packages or
//! host code written against this port. Engines are registered by `language` and
//! selected per `addIndicator({ language })`; market data is owned by Vela's
//! market-data feed and passed into [`ScriptingEngine::execute`].
//!
//! Differences from the original:
//! - `Promise<T>` is a [`LocalBoxFuture`]; a rejection is an [`EngineError`]
//!   (`prepare`, `get_context`) or a [`DataError`] ([`FetchSeries`], which
//!   rejects with a `DataUnavailableError` when the source refuses a series for
//!   good: report its message instead of retrying).
//! - `Error` payloads (`onError`) are the error's message, a `String`.
//! - The opaque `token: unknown` of a prepared script is an `Rc<dyn Any>`.
//! - [`EngineContextSnapshot`] values that the original types `unknown` are
//!   `serde_json::Value`. A snapshot restricted by a [`ContextSelect`] leaves the
//!   unselected fields at their defaults (empty / `None` / `0`).
//! - An optional TypeScript method (`getContext?`) is a defaulted trait method:
//!   [`ExecutionSession::get_context`] returns `None` when unsupported.

use std::any::Any;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::strategy::{StrategyState, StrategyTrade};
use crate::model::{IndicatorMeta, IndicatorModel, InputSchema, Inputs, Millis, Ohlcv};
use crate::ports::chart_renderer::PriceStyle;
use crate::ports::market_data::{BarRange, SymbolInfo};
use crate::runtime::{DataError, LocalBoxFuture};

/// Why preparing or reading from a script failed (a compile error, an engine
/// fault). Carries the engine's message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineError {
    pub message: String,
}

impl EngineError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EngineError {}

impl From<String> for EngineError {
    fn from(message: String) -> Self {
        Self { message }
    }
}

impl From<&str> for EngineError {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

/// A cache-backed data gateway the engine uses to fetch ANY
/// `(symbol, timeframe)` series it needs. The main series stays in memory, but
/// secondary series (Pine `request.security` HTF/LTF/cross-symbol) are fetched
/// and cached through this so they get real, correctly resolved data (no
/// aggregation; timeframes kept separate). Provided by the orchestrator, backed
/// by the market-data feed. Resolves to `Err(DataError::Unavailable(..))` when
/// the source refuses the series for good.
pub type FetchSeries =
    Rc<dyn Fn(&str, &str, &BarRange) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>>>;

/// The chart's visible bar-time window (epoch ms of the left/right edges).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct VisibleBarRange {
    pub left: f64,
    pub right: f64,
}

/// An alert a script raised.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineAlert {
    pub id: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub time: Millis,
    pub bar_index: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freq: Option<String>,
}

/// A warning a script raised.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineWarning {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    pub bar: i64,
}

/// A source script that has been parsed (inputs and declaration metadata
/// extracted) and is ready to execute. `token` is engine-internal and opaque to
/// core: the engine reads it back in `execute`.
#[derive(Clone)]
pub struct PreparedScript {
    /// The language that produced this (the engine's `language`).
    pub language: String,
    pub inputs: Vec<InputSchema>,
    /// Declaration-property schema (a strategy's `initial_capital`, an
    /// indicator's `precision`, ...): the settings dialog renders these on a
    /// "Properties" tab. `defval` is the EFFECTIVE default: the value the script
    /// declares, else the engine's configured default, else the language's own.
    /// `None` means the language has no declaration properties
    /// (`EngineCapabilities::props` false).
    pub props: Option<Vec<InputSchema>>,
    pub meta: IndicatorMeta,
    /// Whether the script references a viewport built-in (Pine
    /// `chart.left_visible_bar_time`). Detected statically at prepare; an engine
    /// may refine it in place after the first run, which every clone of this
    /// value observes (read it with [`reacts_to_viewport`](Self::reacts_to_viewport)).
    pub reacts_to_viewport: Rc<Cell<bool>>,
    pub token: Rc<dyn Any>,
}

impl PreparedScript {
    pub fn reacts_to_viewport(&self) -> bool {
        self.reacts_to_viewport.get()
    }

    /// Refine the flag in place (shared by every clone).
    pub fn set_reacts_to_viewport(&self, value: bool) {
        self.reacts_to_viewport.set(value);
    }
}

impl fmt::Debug for PreparedScript {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedScript")
            .field("language", &self.language)
            .field("meta", &self.meta)
            .field("reacts_to_viewport", &self.reacts_to_viewport.get())
            .finish_non_exhaustive()
    }
}

/// What an engine can do: lets the orchestrator route without engine-specific checks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineCapabilities {
    /// Can maintain a persistent incremental context for live ticks (vs a full re-run).
    pub streaming: bool,
    /// Understands viewport-dependent execution (`chart.*_visible_bar_time`-style).
    #[serde(rename = "visibleRange")]
    pub visible_range: bool,
    /// Exposes an inputs schema (drives the renderer's settings dialog).
    pub inputs: bool,
    /// Exposes a declaration-props schema (`PreparedScript::props`) and honors
    /// prop overrides on `execute`/`update`. Absent means false.
    #[serde(default)]
    pub props: bool,
}

/// Market context an execution needs. Vela owns the bars; this is the metadata.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExecutionMarket {
    pub symbol: String,
    pub timeframe: String,
    /// Provider symbol metadata (Pine `syminfo.*`). May be absent or partial: the
    /// feed fetches it asynchronously (the `MarketDataFeed::symbol_info` port is
    /// synchronous), so the first run can see a synthesized fallback and later
    /// runs the real values.
    pub symbol_info: Option<SymbolInfo>,
    /// The chart's active price style. An engine adapter encodes a
    /// bar-transforming style into the chart's ticker identity (the extended
    /// ticker `"SYM;heikinashi"`), from which chart-type builtins derive. The
    /// BARS an execution receives are already the style's view: this is
    /// metadata, not a request to transform. Engines may ignore it.
    pub chart_style: Option<PriceStyle>,
    /// The chart's trading session (`regular` / `extended`); `None` = regular /
    /// no session model. The BARS an execution receives are already that
    /// session's: this is metadata, for session-aware built-ins or data the
    /// engine obtains outside the gateway. A session switch stops every
    /// execution and starts new ones, so this never changes within one
    /// execution. It follows the chart's setting, not the symbol: a market
    /// without sessions may still carry one. Engines may ignore it.
    pub session: Option<String>,
}

/// How an execution runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionMode {
    /// Runs on demand (and re-runs when the session is poked).
    Static,
    /// Keeps a streaming context that emits per tick (only requested when
    /// `EngineCapabilities::streaming`).
    Live,
}

/// Where the chart's history load stands at session start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HistoryState {
    /// Older chunks are still streaming in (the bars snapshot is a PARTIAL
    /// history); the engine decides run policy: the bundled engines defer their
    /// first run until the `Complete` notification, a progressive engine may run
    /// immediately.
    Backfill,
    /// History fully loaded (today's behavior; the meaning of an absent value).
    Complete,
}

/// One execution request. Vela owns the bars and passes them in: the engine
/// never fetches market data.
#[derive(Clone)]
pub struct ExecutionRequest {
    pub prepared: PreparedScript,
    pub market: ExecutionMarket,
    /// The initial bar snapshot.
    pub bars: Vec<Ohlcv>,
    /// Live accessor to Vela's canonical array (read on each re-run / tick).
    pub get_bars: Option<Rc<dyn Fn() -> Vec<Ohlcv>>>,
    /// Fetch a secondary `(symbol, timeframe)` series (Pine `request.security`
    /// HTF/LTF/cross-symbol), cache-backed by Vela's data feed. `None` means no
    /// gateway: secondary fetches degrade to empty.
    pub fetch_series: Option<FetchSeries>,
    pub inputs: Option<Inputs>,
    /// Declaration-prop overrides, keyed like `PreparedScript::props`. Only
    /// meaningful when `EngineCapabilities::props`; other engines ignore it.
    pub props: Option<Inputs>,
    pub visible_range: Option<VisibleBarRange>,
    pub mode: ExecutionMode,
    /// Where the history load stands at session start; `None` means
    /// [`HistoryState::Complete`].
    pub history_state: Option<HistoryState>,
}

impl ExecutionRequest {
    /// A request with only the required fields: every optional one absent.
    pub fn new(
        prepared: PreparedScript,
        market: ExecutionMarket,
        bars: Vec<Ohlcv>,
        mode: ExecutionMode,
    ) -> Self {
        Self {
            prepared,
            market,
            bars,
            get_bars: None,
            fetch_series: None,
            inputs: None,
            props: None,
            visible_range: None,
            mode,
            history_state: None,
        }
    }

    /// The history state with the default applied.
    pub fn history_state(&self) -> HistoryState {
        self.history_state.unwrap_or(HistoryState::Complete)
    }
}

/// The event sink. `on_model` fires on the first run and on every re-run / live tick.
pub struct ExecutionHandlers {
    pub on_model: Box<dyn FnMut(IndicatorModel)>,
    pub on_alert: Option<Box<dyn FnMut(EngineAlert)>>,
    pub on_warning: Option<Box<dyn FnMut(EngineWarning)>>,
    /// The failure's message.
    pub on_error: Option<Box<dyn FnMut(String)>>,
    /// A `Static` run finished; not fired for an open live stream.
    pub on_done: Option<Box<dyn FnMut()>>,
}

impl ExecutionHandlers {
    /// Handlers with only the required `on_model`.
    pub fn new(on_model: impl FnMut(IndicatorModel) + 'static) -> Self {
        Self {
            on_model: Box::new(on_model),
            on_alert: None,
            on_warning: None,
            on_error: None,
            on_done: None,
        }
    }
}

/// Why the chart's bars changed, carried on [`ExecutionSession::notify_bars`].
/// `None` there means a live tick / new bar at the tail (today's meaning). Run
/// policy is the ENGINE's: the bundled engines skip `Backfill` and run on
/// `Complete` / ticks; a progressive engine may re-run on every reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BarsChangeReason {
    /// Older history chunks prepended (the load is still in progress).
    Backfill,
    /// The history backfill just finished (fires once).
    Complete,
}

/// Where a script is in its life.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContextPhase {
    /// After done/stop.
    #[default]
    Idle,
    /// A static run is in flight.
    Computing,
    /// A live session.
    Streaming,
}

/// The declaration metadata a context snapshot carries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextMeta {
    pub title: String,
    pub overlay: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precision: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shorttitle: Option<String>,
}

/// One point of a named plot output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextPlotPoint {
    pub time: Millis,
    pub value: Value,
}

/// A read-only, serializable snapshot of a running script's execution context.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineContextSnapshot {
    pub language: String,
    pub phase: ContextPhase,
    /// Index of the last computed bar.
    pub bar_index: i64,
    pub meta: ContextMeta,
    /// Named plot outputs, per key: index-aligned `{time, value}` points.
    pub plots: BTreeMap<String, Vec<ContextPlotPoint>>,
    /// The script's variables at the last computed bar, keyed by the names
    /// WRITTEN in the source. An engine that mangles names internally un-mangles
    /// them here: a transpiler's scoping scheme is its own business and must not
    /// reach host code. Values are the serializable subset (never live
    /// references, never per-bar series buffers).
    pub variables: BTreeMap<String, Value>,
    /// A STRATEGY's broker state at the last computed bar: `None` for a plain
    /// indicator, and for an engine whose language has no strategy concept.
    /// Neutral by contract: the engine translates its own vocabulary into these
    /// fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<StrategyState>,
    /// The strategy's round trips, closed then open. `None` when there are none.
    /// Select it explicitly: a deep backtest's ledger is large.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trades: Option<Vec<StrategyTrade>>,
    pub warnings: Vec<EngineWarning>,
}

/// A key of [`EngineContextSnapshot`] a caller may restrict a snapshot to
/// (limits worker structured-clone cost).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContextKey {
    Language,
    Phase,
    BarIndex,
    Meta,
    Plots,
    Variables,
    Strategy,
    Trades,
    Warnings,
}

impl ContextKey {
    /// The snapshot field's name in the original.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Language => "language",
            Self::Phase => "phase",
            Self::BarIndex => "barIndex",
            Self::Meta => "meta",
            Self::Plots => "plots",
            Self::Variables => "variables",
            Self::Strategy => "strategy",
            Self::Trades => "trades",
            Self::Warnings => "warnings",
        }
    }
}

/// Keys a caller may restrict a context snapshot to.
pub type ContextSelect = Vec<ContextKey>;

/// A running execution: the control surface the orchestrator drives.
pub trait ExecutionSession {
    /// OPTIONAL capability: resolve a read-only context snapshot (`Ok(None)`
    /// when the run has not produced one yet). Always a COPY: mutating it never
    /// touches the engine. Returns `None` (no future) when the engine does not
    /// support it.
    fn get_context(
        &self,
        _select: Option<&[ContextKey]>,
    ) -> Option<LocalBoxFuture<'static, Result<Option<EngineContextSnapshot>, EngineError>>> {
        None
    }
    /// Tear down (stops any streaming / incremental re-execution).
    fn stop(&self);
    /// Re-run / re-stream with merged input overrides. `props` (when given)
    /// merges declaration-prop overrides the same way: a props-capable engine
    /// re-runs with both applied; others may ignore the second argument.
    fn update(&self, inputs: &Inputs, props: Option<&Inputs>);
    /// Update the viewport window (re-runs viewport-dependent scripts; no-op otherwise).
    fn set_visible_range(&self, range: VisibleBarRange);
    /// Signal that Vela's bars changed. No reason = live tick; see [`BarsChangeReason`].
    fn notify_bars(&self, reason: Option<BarsChangeReason>);
}

/// The scripting-engine abstraction (see the module doc). Shared as
/// `Rc<dyn ScriptingEngine>`.
pub trait ScriptingEngine {
    /// Language id this engine handles (`pine`). The registry key.
    fn language(&self) -> &str;
    fn capabilities(&self) -> EngineCapabilities;
    /// Parse a script: extract its inputs schema and declaration metadata. No market data.
    fn prepare(
        &self,
        source: &str,
        instance_id: &str,
    ) -> LocalBoxFuture<'static, Result<PreparedScript, EngineError>>;
    /// Execute (static or live). Returns a session control handle.
    fn execute(
        &self,
        req: ExecutionRequest,
        handlers: ExecutionHandlers,
    ) -> Rc<dyn ExecutionSession>;
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::runtime::testing::ManualPlatform;
    use serde_json::json;

    struct Session {
        log: Rc<RefCell<Vec<String>>>,
    }

    impl ExecutionSession for Session {
        fn stop(&self) {
            self.log.borrow_mut().push("stop".into());
        }
        fn update(&self, inputs: &Inputs, props: Option<&Inputs>) {
            self.log
                .borrow_mut()
                .push(format!("update {} {}", inputs.len(), props.is_some()));
        }
        fn set_visible_range(&self, range: VisibleBarRange) {
            self.log.borrow_mut().push(format!("range {}", range.left));
        }
        fn notify_bars(&self, reason: Option<BarsChangeReason>) {
            self.log.borrow_mut().push(format!("bars {reason:?}"));
        }
    }

    /// Implements only the required methods.
    struct Echo {
        log: Rc<RefCell<Vec<String>>>,
    }

    impl ScriptingEngine for Echo {
        fn language(&self) -> &str {
            "echo"
        }
        fn capabilities(&self) -> EngineCapabilities {
            EngineCapabilities::default()
        }
        fn prepare(
            &self,
            source: &str,
            _instance_id: &str,
        ) -> LocalBoxFuture<'static, Result<PreparedScript, EngineError>> {
            let source = source.to_owned();
            Box::pin(async move {
                if source.is_empty() {
                    return Err(EngineError::new("empty script"));
                }
                Ok(PreparedScript {
                    language: "echo".into(),
                    inputs: Vec::new(),
                    props: None,
                    meta: IndicatorMeta {
                        title: source.clone(),
                        shorttitle: None,
                        overlay: true,
                        precision: None,
                        format: None,
                    },
                    reacts_to_viewport: Rc::new(Cell::new(false)),
                    token: Rc::new(source),
                })
            })
        }
        fn execute(
            &self,
            _req: ExecutionRequest,
            mut handlers: ExecutionHandlers,
        ) -> Rc<dyn ExecutionSession> {
            if let Some(done) = handlers.on_done.as_mut() {
                done();
            }
            Rc::new(Session {
                log: self.log.clone(),
            })
        }
    }

    #[test]
    fn a_minimal_engine_is_object_safe_and_prepares_asynchronously() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let engine: Rc<dyn ScriptingEngine> = Rc::new(Echo { log: log.clone() });
        let platform = ManualPlatform::new(0.0);

        let prepared = platform.block_on(engine.prepare("hello", "i1")).unwrap();
        assert_eq!(prepared.language, "echo");
        assert_eq!(prepared.meta.title, "hello");
        assert_eq!(prepared.token.downcast_ref::<String>().unwrap(), "hello");
        let err = platform.block_on(engine.prepare("", "i2")).unwrap_err();
        assert_eq!(err.to_string(), "empty script");

        let session = engine.execute(
            ExecutionRequest::new(
                prepared.clone(),
                ExecutionMarket::default(),
                Vec::new(),
                ExecutionMode::Static,
            ),
            ExecutionHandlers::new(|_| {}),
        );
        assert!(session.get_context(None).is_none());
        session.update(&Inputs::new(), None);
        session.set_visible_range(VisibleBarRange {
            left: 1.0,
            right: 2.0,
        });
        session.notify_bars(Some(BarsChangeReason::Complete));
        session.stop();
        assert_eq!(
            *log.borrow(),
            ["update 0 false", "range 1", "bars Some(Complete)", "stop"]
        );
    }

    #[test]
    fn the_viewport_flag_is_shared_between_clones() {
        let prepared = PreparedScript {
            language: "echo".into(),
            inputs: Vec::new(),
            props: None,
            meta: IndicatorMeta {
                title: "t".into(),
                shorttitle: None,
                overlay: false,
                precision: None,
                format: None,
            },
            reacts_to_viewport: Rc::new(Cell::new(false)),
            token: Rc::new(()),
        };
        let copy = prepared.clone();
        assert!(!copy.reacts_to_viewport());
        prepared.set_reacts_to_viewport(true);
        assert!(copy.reacts_to_viewport());
    }

    #[test]
    fn fetch_series_can_reject_with_a_data_error() {
        let fetch: FetchSeries = Rc::new(|_symbol, _timeframe, _range| {
            Box::pin(async { Err(DataError::other("offline")) })
        });
        let platform = ManualPlatform::new(0.0);
        let result = platform.block_on(fetch("BTCUSDT", "60", &BarRange::default()));
        assert_eq!(result.unwrap_err().to_string(), "offline");
    }

    #[test]
    fn defaults_follow_the_absent_means_rule() {
        let request = ExecutionRequest::new(
            PreparedScript {
                language: "echo".into(),
                inputs: Vec::new(),
                props: None,
                meta: IndicatorMeta {
                    title: "t".into(),
                    shorttitle: None,
                    overlay: false,
                    precision: None,
                    format: None,
                },
                reacts_to_viewport: Rc::new(Cell::new(false)),
                token: Rc::new(()),
            },
            ExecutionMarket::default(),
            Vec::new(),
            ExecutionMode::Live,
        );
        assert_eq!(request.history_state(), HistoryState::Complete);
        assert!(request.fetch_series.is_none() && request.get_bars.is_none());
        assert!(!EngineCapabilities::default().props);
    }

    #[test]
    fn wire_names_match_the_original() {
        assert_eq!(
            serde_json::to_value(ExecutionMode::Live).unwrap(),
            json!("live")
        );
        assert_eq!(
            serde_json::to_value(BarsChangeReason::Backfill).unwrap(),
            json!("backfill")
        );
        assert_eq!(
            serde_json::to_value(HistoryState::Complete).unwrap(),
            json!("complete")
        );
        assert_eq!(
            serde_json::to_value(ContextPhase::Streaming).unwrap(),
            json!("streaming")
        );
        let keys = [
            (ContextKey::Language, "language"),
            (ContextKey::Phase, "phase"),
            (ContextKey::BarIndex, "barIndex"),
            (ContextKey::Meta, "meta"),
            (ContextKey::Plots, "plots"),
            (ContextKey::Variables, "variables"),
            (ContextKey::Strategy, "strategy"),
            (ContextKey::Trades, "trades"),
            (ContextKey::Warnings, "warnings"),
        ];
        for (key, name) in keys {
            assert_eq!(key.as_str(), name);
            assert_eq!(serde_json::to_value(key).unwrap(), json!(name));
        }
        let caps = EngineCapabilities {
            streaming: true,
            visible_range: true,
            inputs: true,
            props: false,
        };
        assert_eq!(
            serde_json::to_value(caps).unwrap(),
            json!({"streaming": true, "visibleRange": true, "inputs": true, "props": false})
        );
        let no_props: EngineCapabilities = serde_json::from_value(
            json!({"streaming": false, "visibleRange": false, "inputs": true}),
        )
        .unwrap();
        assert!(!no_props.props);
    }

    #[test]
    fn alerts_warnings_and_snapshots_round_trip() {
        let alert = EngineAlert {
            id: "a1".into(),
            message: "cross".into(),
            title: None,
            time: 1_700_000_000_000,
            bar_index: 42,
            freq: Some("once_per_bar".into()),
        };
        let wire = serde_json::to_value(&alert).unwrap();
        assert_eq!(wire["barIndex"], json!(42));
        assert!(wire.get("title").is_none());
        assert_eq!(serde_json::from_value::<EngineAlert>(wire).unwrap(), alert);

        let warning = EngineWarning {
            message: "careful".into(),
            method: Some("ta.sma".into()),
            bar: 3,
        };
        assert_eq!(
            serde_json::from_value::<EngineWarning>(serde_json::to_value(&warning).unwrap())
                .unwrap(),
            warning
        );

        let mut snapshot = EngineContextSnapshot {
            language: "pine".into(),
            phase: ContextPhase::Computing,
            bar_index: 9,
            meta: ContextMeta {
                title: "SMA".into(),
                overlay: true,
                precision: Some(2),
                shorttitle: None,
            },
            ..EngineContextSnapshot::default()
        };
        snapshot.plots.insert(
            "sma".into(),
            vec![ContextPlotPoint {
                time: 1,
                value: json!(1.5),
            }],
        );
        snapshot.variables.insert("len".into(), json!(14));
        let wire = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(wire["barIndex"], json!(9));
        assert_eq!(wire["phase"], json!("computing"));
        assert!(wire.get("strategy").is_none());
        let back: EngineContextSnapshot = serde_json::from_value(wire).unwrap();
        assert_eq!(back, snapshot);
    }
}
