//! The chart on a market the source refuses for good (`data-unavailable.test.ts`,
//! the chart-level cases): built on the default provider-registry feed, through
//! the public API only.

mod vela_common;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use vela_common::*;
use vela_core::data_unavailable::{
    DataUnavailableDetails, DataUnavailableError, DataUnavailableReason,
};
use vela_core::events::types::{ChartEvent, ChartEventName, HistoryReason};
use vela_core::model::Ohlcv;
use vela_core::options::{MarketSwitch, VelaOptions};
use vela_core::ports::chart_renderer::ChartRenderer;
use vela_core::ports::data_provider::{DataProvider, SymbolDescriptor};
use vela_core::ports::market_data::{BarRange, BatchSink};
use vela_core::runtime::testing::ManualPlatform;
use vela_core::runtime::{AbortSignal, DataError, LocalBoxFuture};
use vela_core::vela::{Vela, VelaDeps, VelaEnv, VelaError};

const DAY: i64 = 86_400_000;

fn make_bars(n: usize, step: i64, now: i64) -> Vec<Ohlcv> {
    let start = now - n as i64 * step;
    (0..n)
        .map(|i| {
            Ohlcv::new(
                start + i as i64 * step,
                100.0,
                101.0,
                99.0,
                100.0,
                Some(1.0),
            )
        })
        .collect()
}

fn refusal(symbol: &str, tf: &str, finest: &str) -> DataError {
    let mut details = DataUnavailableDetails::new(DataUnavailableReason::Timeframe);
    details.symbol = Some(symbol.to_string());
    details.timeframe = Some(tf.to_string());
    details.finest = Some(finest.to_string());
    details.default = Some(finest.to_string());
    DataUnavailableError::new(details, None).into()
}

#[derive(Clone, Copy, PartialEq)]
enum Refuse {
    Unavailable,
    PlainError,
}

/// Serves `GDP` at 1D and coarser only; anything finer is refused for good, or
/// (`PlainError`) fails with a transient error. Counts every `get_bars` call per
/// timeframe.
struct MacroProvider {
    refuse: Refuse,
    now: i64,
    calls: RefCell<Vec<(String, String)>>,
    progressive_calls: Cell<usize>,
    refuse_progressive: Cell<bool>,
}

impl MacroProvider {
    fn new(refuse: Refuse, now: i64) -> Rc<Self> {
        Rc::new(Self {
            refuse,
            now,
            calls: RefCell::default(),
            progressive_calls: Cell::new(0),
            refuse_progressive: Cell::new(false),
        })
    }

    fn count(&self, tf: &str) -> usize {
        self.calls.borrow().iter().filter(|(_, t)| t == tf).count()
    }

    fn total(&self) -> usize {
        self.calls.borrow().len()
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
            .push((ticker.to_string(), timeframe.to_string()));
        let coarse = matches!(timeframe, "D" | "1D" | "W" | "M");
        let result = if coarse {
            let n = range.limit.unwrap_or(50);
            Ok(make_bars(n, DAY, self.now))
        } else if self.refuse == Refuse::PlainError {
            Err(DataError::other("network down"))
        } else {
            Err(refusal(ticker, timeframe, "1D"))
        };
        Box::pin(async move { result })
    }

    fn get_bars_progressive(
        &self,
        ticker: &str,
        timeframe: &str,
        _range: &BarRange,
        _on_batch: BatchSink,
        _signal: Option<AbortSignal>,
    ) -> Option<LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>>> {
        if !self.refuse_progressive.get() {
            return None;
        }
        self.progressive_calls.set(self.progressive_calls.get() + 1);
        let mut details = DataUnavailableDetails::new(DataUnavailableReason::Timeframe);
        details.symbol = Some(ticker.to_string());
        details.timeframe = Some(timeframe.to_string());
        details.finest = Some("D".to_string());
        let err: DataError = DataUnavailableError::new(details, None).into();
        Some(Box::pin(async move { Err(err) }))
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

struct Setup {
    platform: ManualPlatform,
    chart: Vela,
    renderer: Rc<RecRenderer>,
    unavailable: Rc<RefCell<Vec<ChartEvent>>>,
}

fn chart_on(symbol: &str, timeframe: &str, provider: Rc<MacroProvider>, live: bool) -> Setup {
    chart_with(symbol, timeframe, provider, live, Vec::new())
}

fn chart_with(
    symbol: &str,
    timeframe: &str,
    provider: Rc<MacroProvider>,
    live: bool,
    engines: Vec<Rc<dyn vela_core::ports::scripting_engine::ScriptingEngine>>,
) -> Setup {
    let platform = ManualPlatform::new(provider.now as f64);
    let env = VelaEnv::new(platform.handle());
    let renderer = RecRenderer::new();
    let mut options = VelaOptions::default();
    options.market.symbol = Some(symbol.to_string());
    options.market.timeframe = Some(timeframe.to_string());
    options.live = Some(live);
    options.volume = Some(false);
    let chart = Vela::with_deps(
        &env,
        options,
        VelaDeps {
            renderer: Some(Rc::clone(&renderer) as Rc<dyn ChartRenderer>),
            engines,
            ..VelaDeps::default()
        },
    )
    .expect("built");
    let unavailable: Rc<RefCell<Vec<ChartEvent>>> = Rc::default();
    let sink = Rc::clone(&unavailable);
    chart.on(ChartEventName::DataUnavailable, move |e| {
        sink.borrow_mut().push(e.clone())
    });
    chart.data().register_provider("macro", provider);
    Setup {
        platform,
        chart,
        renderer,
        unavailable,
    }
}

fn expected(symbol: &str, tf: &str, finest: &str, default: Option<&str>) -> String {
    format!(
        "{:?}",
        ChartEvent::DataUnavailable {
            symbol: symbol.to_string(),
            timeframe: tf.to_string(),
            reason: DataUnavailableReason::Timeframe,
            finest: Some(finest.to_string()),
            default: default.map(str::to_string),
        }
    )
}

fn shown(events: &RefCell<Vec<ChartEvent>>) -> Vec<String> {
    events.borrow().iter().map(|e| format!("{e:?}")).collect()
}

#[test]
fn a_refused_market_ends_the_load_fires_data_unavailable_once_and_never_polls() {
    let provider = MacroProvider::new(Refuse::Unavailable, T0);
    let s = chart_on("GDP", "60", Rc::clone(&provider), true);
    let ends: Rc<RefCell<Vec<usize>>> = Rc::default();
    let sink = Rc::clone(&ends);
    s.chart.on(ChartEventName::LoadEnd, move |e| {
        if let ChartEvent::LoadEnd { bars, .. } = e {
            sink.borrow_mut().push(*bars);
        }
    });
    let completes: Rc<RefCell<Vec<HistoryReason>>> = Rc::default();
    let sink = Rc::clone(&completes);
    s.chart.on(ChartEventName::HistoryComplete, move |e| {
        if let ChartEvent::HistoryComplete { reason, .. } = e {
            sink.borrow_mut().push(*reason);
        }
    });
    s.platform.advance(10.0);
    assert!(s.platform.block_on(s.chart.ready()).is_ok());

    assert_eq!(
        shown(&s.unavailable),
        [expected("GDP", "60", "1D", Some("1D"))]
    );
    assert!(!s.renderer.loading.get(), "no endless loading animation");
    assert!(s.renderer.bars.borrow().is_empty());
    assert_eq!(*ends.borrow(), [0]);
    assert_eq!(*completes.borrow(), [HistoryReason::Aborted]);
    // The core never changes the timeframe the host set.
    assert_eq!(s.chart.market().timeframe.as_deref(), Some("60"));

    // Live is on, but a refused series has nothing to stream: no poll ever starts.
    let asked = provider.total();
    s.platform.advance(60_000.0);
    assert_eq!(provider.total(), asked);
    assert_eq!(s.unavailable.borrow().len(), 1);
}

#[test]
fn a_switch_to_a_refused_timeframe_settles_and_stops_the_old_market_streaming() {
    let provider = MacroProvider::new(Refuse::Unavailable, T0);
    let s = chart_on("GDP", "D", Rc::clone(&provider), true);
    s.platform.advance(10.0);
    assert!(s.platform.block_on(s.chart.ready()).is_ok());
    assert!(!s.renderer.bars.borrow().is_empty());
    assert!(s.unavailable.borrow().is_empty());

    let to_60 = s.chart.set_market(MarketSwitch {
        timeframe: Some("60".to_string()),
        ..MarketSwitch::default()
    });
    // Resolves: the refusal is handled, not thrown.
    assert_eq!(s.platform.block_on(to_60), Ok(()));
    assert_eq!(
        shown(&s.unavailable),
        [expected("GDP", "60", "1D", Some("1D"))]
    );
    assert!(!s.renderer.loading.get());
    assert!(s.renderer.bars.borrow().is_empty());

    let asked = provider.total();
    s.platform.advance(60_000.0);
    assert_eq!(
        provider.total(),
        asked,
        "neither the old D poll nor a 60 poll runs"
    );

    // A served timeframe brings the market back, live included.
    let back = s.chart.set_market(MarketSwitch {
        timeframe: Some("D".to_string()),
        ..MarketSwitch::default()
    });
    assert_eq!(s.platform.block_on(back), Ok(()));
    assert!(!s.renderer.bars.borrow().is_empty());
    let before = provider.count("D");
    s.platform.advance(10_000.0);
    assert!(provider.count("D") > before, "the live poll runs again");
    assert_eq!(s.unavailable.borrow().len(), 1);
}

#[test]
fn a_progressive_source_that_refuses_ends_the_load_the_same_way() {
    let provider = MacroProvider::new(Refuse::Unavailable, T0);
    provider.refuse_progressive.set(true);
    let s = chart_on("GDP", "60", Rc::clone(&provider), false);
    let _ = s.platform.block_on(s.chart.ready());
    assert_eq!(provider.progressive_calls.get(), 1);
    assert_eq!(shown(&s.unavailable), [expected("GDP", "60", "D", None)]);
    assert!(!s.renderer.loading.get());
}

#[test]
fn a_refusal_while_deepening_the_history_reports_once_and_keeps_what_is_painted() {
    let rig = Rig::builder()
        .symbol("GDP")
        .timeframe("1")
        .bars(30_000)
        .feed(|f| {
            f.set_depth("GDP", 10_000);
            f.set_range_error(Some(refusal("GDP", "1", "1D")));
        })
        .build();
    let events: Rc<RefCell<Vec<ChartEvent>>> = Rc::default();
    let sink = Rc::clone(&events);
    rig.chart.on(ChartEventName::DataUnavailable, move |e| {
        sink.borrow_mut().push(e.clone())
    });
    let completes: Rc<RefCell<Vec<HistoryReason>>> = Rc::default();
    let sink = Rc::clone(&completes);
    rig.chart.on(ChartEventName::HistoryComplete, move |e| {
        if let ChartEvent::HistoryComplete { reason, .. } = e {
            sink.borrow_mut().push(*reason);
        }
    });
    rig.loaded();
    assert_eq!(events.borrow().len(), 1);
    assert!(matches!(
        &events.borrow()[0],
        ChartEvent::DataUnavailable { symbol, timeframe, finest, .. }
            if symbol == "GDP" && timeframe == "1" && finest.as_deref() == Some("1D")
    ));
    assert_eq!(*completes.borrow(), [HistoryReason::Aborted]);
    assert_eq!(rig.renderer.bars.borrow().len(), 10_000);
}

#[test]
fn a_plain_provider_failure_keeps_an_empty_chart_no_event_and_polling_continues() {
    let provider = MacroProvider::new(Refuse::PlainError, T0);
    let s = chart_on("GDP", "60", Rc::clone(&provider), true);
    s.platform.advance(10.0);
    assert!(s.platform.block_on(s.chart.ready()).is_ok());
    assert!(!s.renderer.loading.get());
    assert!(s.renderer.bars.borrow().is_empty());
    assert!(s.unavailable.borrow().is_empty());
    let asked = provider.count("60");
    s.platform.advance(10_000.0);
    assert!(
        provider.count("60") > asked,
        "the transient-failure poll keeps trying"
    );
}

/// A script that asks the gateway for one secondary series and keeps the outcome.
struct SecondaryEngine {
    auto: Rc<AutoEngine>,
    symbol: &'static str,
    timeframe: &'static str,
    outcome: RefCell<Option<LocalBoxFuture<'static, Result<usize, DataError>>>>,
}

impl vela_core::ports::scripting_engine::ScriptingEngine for SecondaryEngine {
    fn language(&self) -> &str {
        self.auto.language()
    }

    fn capabilities(&self) -> vela_core::ports::scripting_engine::EngineCapabilities {
        self.auto.capabilities()
    }

    fn prepare(
        &self,
        source: &str,
        instance_id: &str,
    ) -> LocalBoxFuture<
        'static,
        Result<
            vela_core::ports::scripting_engine::PreparedScript,
            vela_core::ports::scripting_engine::EngineError,
        >,
    > {
        self.auto.prepare(source, instance_id)
    }

    fn execute(
        &self,
        req: vela_core::ports::scripting_engine::ExecutionRequest,
        handlers: vela_core::ports::scripting_engine::ExecutionHandlers,
    ) -> Rc<dyn vela_core::ports::scripting_engine::ExecutionSession> {
        if let Some(fetch) = req.fetch_series.clone() {
            let range = BarRange {
                limit: Some(10),
                ..BarRange::default()
            };
            let pending = fetch(self.symbol, self.timeframe, &range);
            *self.outcome.borrow_mut() =
                Some(Box::pin(async move { pending.await.map(|b| b.len()) }));
        }
        self.auto.execute(req, handlers)
    }
}

#[test]
fn a_refused_secondary_series_rejects_to_the_engine_and_leaves_the_chart_alone() {
    let provider = MacroProvider::new(Refuse::Unavailable, T0);
    let engine = Rc::new(SecondaryEngine {
        auto: AutoEngine::new(),
        symbol: "CPI",
        timeframe: "60",
        outcome: RefCell::default(),
    });
    let s = chart_with(
        "GDP",
        "D",
        Rc::clone(&provider),
        false,
        vec![Rc::clone(&engine)
            as Rc<
                dyn vela_core::ports::scripting_engine::ScriptingEngine,
            >],
    );
    s.platform.advance(10.0);
    assert!(s.platform.block_on(s.chart.ready()).is_ok());
    let painted = s.renderer.bars.borrow().len();
    assert!(painted > 0);
    let ends: Rc<RefCell<Vec<usize>>> = Rc::default();
    let sink = Rc::clone(&ends);
    s.chart.on(ChartEventName::LoadEnd, move |e| {
        if let ChartEvent::LoadEnd { bars, .. } = e {
            sink.borrow_mut().push(*bars);
        }
    });

    s.chart
        .add_indicator("//@version=6\nindicator(\"x\")", Default::default())
        .expect("added");
    for _ in 0..4 {
        s.platform.run();
        s.platform.advance(0.0);
    }
    let outcome = engine
        .outcome
        .borrow_mut()
        .take()
        .expect("the script asked for a series");
    match s.platform.block_on(outcome) {
        Err(DataError::Unavailable(e)) => {
            assert_eq!(e.symbol.as_deref(), Some("CPI"));
            assert_eq!(e.timeframe.as_deref(), Some("60"));
            assert_eq!(e.finest.as_deref(), Some("1D"));
            assert_eq!(
                e.message(),
                "CPI is not served at 60; its finest timeframe is 1D"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }

    assert!(s.unavailable.borrow().is_empty(), "no chart-level event");
    assert!(
        ends.borrow().is_empty(),
        "the chart load state is untouched"
    );
    assert_eq!(s.chart.market().timeframe.as_deref(), Some("D"));
    assert_eq!(s.renderer.bars.borrow().len(), painted);
}

#[test]
fn ready_on_a_refused_market_is_ok_and_the_error_type_is_the_facade_one() {
    let provider = MacroProvider::new(Refuse::Unavailable, T0);
    let s = chart_on("GDP", "60", provider, false);
    let result: Result<(), VelaError> = s.platform.block_on(s.chart.ready());
    assert_eq!(result, Ok(()));
}
