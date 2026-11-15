//! The chart's bar-replay control surface, `chart.replay` (`ReplayControl.ts`),
//! and ready-made tick sources for it (`replayTicks.ts`).
//!
//! Replay rewinds the chart to a past bar and reveals the following bars one at a
//! time, by hand or on a timer, from the history already in memory (nothing is
//! refetched). Revealed bars travel the exact path of a live bar: indicators,
//! chart-type engines and the `bar` event see them as new bars, but script alerts
//! stay quiet (a replayed bar is history, not a live signal). Live updates pause
//! for the duration; `stop()` (or revealing the last bar) restores the full
//! history and resumes them. A timeframe or session switch carries the replay
//! over to the new bars at the same point in time; a symbol switch ends it.
//!
//! The control holds its controller (the orchestrator) as a [`Weak`], so a
//! `chart.replay` kept by user code after the chart is destroyed does not keep
//! the engine alive. Once the controller is gone the control is inert: `start`
//! resolves, `step` reports false, the state reads "off".

use std::rc::{Rc, Weak};

use serde::{Deserialize, Serialize};

use crate::controls::data_control::DataControl;
use crate::model::{Millis, Ohlcv};
use crate::options::MarketSnapshot;
use crate::ports::market_data::BarRange;
use crate::runtime::{AbortSignal, DataError, LocalBoxFuture, js_await};

/// Where a bar replay stands (`chart.replay.state`).
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayState {
    /// Replay mode is on: the chart shows history up to the cursor, live updates
    /// are paused. Stays on through a timeframe/session switch while the new bars
    /// load (`cursor_time` is `None` until the replay resumes on them).
    pub active: bool,
    /// Bars are being revealed on a timer (`play`), as opposed to paused or
    /// stepped by hand.
    pub playing: bool,
    /// Open time (epoch ms) of the newest bar on screen; `None` when replay is off.
    pub cursor_time: Option<Millis>,
    /// Bars still hidden to the right of the cursor.
    pub remaining: usize,
    /// Open time (epoch ms) of the next bar a step completes: the one forming
    /// tick by tick, else the next hidden one; `None` when nothing is left or
    /// replay is off. Lets a host keep several charts on one clock.
    pub next_time: Option<Millis>,
    /// Delay between two updates while playing (ms): two bars, or two ticks with
    /// [`ReplayControl::set_ticks`].
    pub interval_ms: f64,
}

impl ReplayState {
    /// The play interval before anyone sets one.
    pub const DEFAULT_INTERVAL_MS: f64 = 1000.0;

    /// Replay off, at the default interval.
    pub fn off() -> Self {
        Self {
            active: false,
            playing: false,
            cursor_time: None,
            remaining: 0,
            next_time: None,
            interval_ms: Self::DEFAULT_INTERVAL_MS,
        }
    }
}

/// Options for [`ReplayControl::start`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayStartOptions {
    /// Epoch ms. The chart keeps every bar that opened at or before it and hides
    /// the rest. Older than the loaded history: the history is deepened first
    /// (clamped to the oldest bar the source serves).
    pub from: Millis,
}

/// Why a replay ended (`replay:end`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplayEndReason {
    /// `chart.replay.stop()`.
    Stopped,
    /// The last hidden bar was revealed.
    Finished,
    /// A symbol switch replaced the history (a timeframe or session switch
    /// carries the replay over instead).
    Market,
}

/// One intrabar update of a replayed bar ([`ReplayControl::set_ticks`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReplayTick {
    /// The price the forming candle closes at after this update.
    pub price: f64,
    /// The extremes traded since the previous update (default: `price`): lets
    /// one update stand for a whole span, e.g. a lower-timeframe bar.
    pub high: Option<f64>,
    pub low: Option<f64>,
    /// Where the candle opens: read on a bar's first update only (default: `price`).
    pub open: Option<f64>,
    /// Volume traded by this update. When no tick of a bar carries one, the bar's
    /// volume is spread evenly across its ticks.
    pub volume: Option<f64>,
}

impl ReplayTick {
    /// A tick that only moves the close.
    pub fn at(price: f64) -> Self {
        Self {
            price,
            high: None,
            low: None,
            open: None,
            volume: None,
        }
    }
}

/// What a [`ReplayTickSource`] is asked: `end` is the open time of the bar after
/// the replayed one (the span its ticks cover is `[bar.time, end)`); `signal`
/// aborts when the replay moves on (seek, stop, new source).
#[derive(Clone, Debug)]
pub struct ReplayTickRequest {
    pub end: Millis,
    pub signal: AbortSignal,
}

/// Where a replay gets the intrabar prices of a bar it is about to reveal: the
/// stored bar and the [`ReplayTickRequest`]. Prices come oldest first. An empty
/// list, or a failure, reveals the bar whole. (The original also accepts a
/// synchronous source; return a ready future.)
pub type ReplayTickSource = Rc<
    dyn Fn(
        &Ohlcv,
        ReplayTickRequest,
    ) -> LocalBoxFuture<'static, Result<Vec<ReplayTick>, DataError>>,
>;

/// The open times of the oldest and newest bar a replay can start from
/// (`chart.replay.bounds`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayBounds {
    pub first: Millis,
    pub last: Millis,
}

/// What the chart implements behind [`ReplayControl`].
pub trait ReplayController {
    fn replay_start(
        &self,
        opts: ReplayStartOptions,
    ) -> LocalBoxFuture<'static, Result<(), DataError>>;
    fn replay_step(&self) -> bool;
    fn replay_step_update(&self) -> bool;
    fn replay_play(&self, interval_ms: Option<f64>);
    fn replay_pause(&self);
    fn replay_stop(&self);
    fn replay_state(&self) -> ReplayState;
    fn replay_bounds(&self) -> Option<ReplayBounds>;
    fn replay_set_ticks(&self, source: Option<ReplayTickSource>);
}

/// `chart.replay`. A cheap handle.
#[derive(Clone)]
pub struct ReplayControl {
    ctrl: Weak<dyn ReplayController>,
}

impl ReplayControl {
    pub fn new(ctrl: Weak<dyn ReplayController>) -> Self {
        Self { ctrl }
    }

    /// Enter replay (or seek, when already replaying) at `opts.from`. Resolves
    /// once the chart shows the rewound history, after any in-flight history load
    /// has finished. Starts paused; call [`play`](Self::play) or [`step`](Self::step).
    pub fn start(
        &self,
        opts: ReplayStartOptions,
    ) -> LocalBoxFuture<'static, Result<(), DataError>> {
        match self.ctrl.upgrade() {
            Some(ctrl) => ctrl.replay_start(opts),
            None => Box::pin(std::future::ready(Ok(()))),
        }
    }

    /// Reveal the next bar: or, midway through a bar played tick by tick,
    /// complete it. Returns false when replay is off or nothing is left.
    pub fn step(&self) -> bool {
        self.ctrl.upgrade().is_some_and(|c| c.replay_step())
    }

    /// Reveal the next UPDATE: with [`set_ticks`](Self::set_ticks), the next tick
    /// of the forming bar, or the next bar opened at its first tick (once its
    /// ticks are in, if still loading); otherwise the next whole bar, like
    /// [`step`](Self::step). Returns false when nothing is left.
    pub fn step_update(&self) -> bool {
        self.ctrl.upgrade().is_some_and(|c| c.replay_step_update())
    }

    /// Reveal one bar (one tick, with [`set_ticks`](Self::set_ticks)) every
    /// `interval_ms` (default: the last interval used, else 1000). Calling it
    /// again while playing changes the pace. A non-positive or non-finite
    /// interval is ignored with a warning by the chart.
    pub fn play(&self, interval_ms: Option<f64>) -> &Self {
        if let Some(c) = self.ctrl.upgrade() {
            c.replay_play(interval_ms);
        }
        self
    }

    pub fn pause(&self) -> &Self {
        if let Some(c) = self.ctrl.upgrade() {
            c.replay_pause();
        }
        self
    }

    /// Leave replay: the full history comes back and live updates resume. Also
    /// cancels a `start()` still loading older history (the chart returns to its
    /// previous depth).
    pub fn stop(&self) -> &Self {
        if let Some(c) = self.ctrl.upgrade() {
            c.replay_stop();
        }
        self
    }

    pub fn state(&self) -> ReplayState {
        self.ctrl
            .upgrade()
            .map_or_else(ReplayState::off, |c| c.replay_state())
    }

    /// The loaded history a replay can walk: hidden bars included while
    /// replaying. `None` before any bar loaded. A start older than `first`
    /// deepens the history first.
    pub fn bounds(&self) -> Option<ReplayBounds> {
        self.ctrl.upgrade().and_then(|c| c.replay_bounds())
    }

    /// Play each revealed bar as a series of intrabar updates instead of whole:
    /// while playing, the forming candle opens at the first tick, then every tick
    /// moves its close and stretches its high and low, exactly like a live tick
    /// (indicators, chart types and the `bar` event follow), and the last one
    /// settles it on the stored bar. The play interval is then the time between
    /// two TICKS: a bar lasts as many intervals as it has ticks (below ~16 ms,
    /// ticks are batched per frame). The next bar's ticks are requested while the
    /// current one plays; playback waits for them. `None` goes back to whole
    /// bars. Takes effect from the next bar; a bar forming when the source
    /// changes completes at once.
    pub fn set_ticks(&self, source: Option<ReplayTickSource>) -> &Self {
        if let Some(c) = self.ctrl.upgrade() {
            c.replay_set_ticks(source);
        }
        self
    }
}

// ── ready-made tick sources ──

/// The slice of a chart [`lower_timeframe_ticks`] reads: its market and its data
/// control, both read when a bar is due, so the source follows a market switch.
pub trait LowerTimeframeChart {
    fn market(&self) -> MarketSnapshot;
    fn data(&self) -> DataControl;
}

/// Ticks from the chart's own data provider at a finer `timeframe` (`"1"`,
/// `"5"`, ...): for each replayed bar, the lower-timeframe bars inside its span
/// are fetched and each becomes one update ([`bars_to_ticks`]): a 15m bar
/// replayed from 1m bars takes 15 intervals. The symbol and session are read when
/// the bar is due, so the source follows a market switch. An unresolvable symbol
/// or nothing served makes the bar play whole.
///
/// The original is an `async` function, so the answer always arrives a turn
/// after the call, even for an unresolvable symbol (a synchronous source would
/// be an array there); every path below yields once for that reason.
///
/// The source holds the chart as a [`Weak`] (the chart owns the orchestrator that
/// owns the source); once the chart is gone it serves nothing.
pub fn lower_timeframe_ticks(
    chart: Weak<dyn LowerTimeframeChart>,
    timeframe: impl Into<String>,
) -> ReplayTickSource {
    let timeframe = timeframe.into();
    Rc::new(move |bar: &Ohlcv, request: ReplayTickRequest| {
        let none = || -> LocalBoxFuture<'static, Result<Vec<ReplayTick>, DataError>> {
            Box::pin(js_await(std::future::ready(Ok(Vec::new()))))
        };
        let Some(chart) = chart.upgrade() else {
            return none();
        };
        let market = chart.market();
        let data = chart.data();
        let resolved = market
            .symbol
            .as_deref()
            .filter(|symbol| !symbol.is_empty())
            .and_then(|symbol| data.resolve(symbol));
        let Some(resolved) = resolved else {
            return none();
        };
        let Some(provider) = data.provider_instance(&resolved.provider) else {
            return none();
        };
        let (from, end) = (bar.time, request.end);
        let range = BarRange {
            from: Some(from),
            to: Some(end - 1),
            limit: None,
            session: market.session.map(|s| s.as_str().to_string()),
        };
        let pending = provider.get_bars(&resolved.ticker, &timeframe, &range);
        let signal = request.signal;
        Box::pin(async move {
            let bars = js_await(pending).await?;
            if signal.aborted() {
                return Ok(Vec::new());
            }
            let inside: Vec<Ohlcv> = bars
                .into_iter()
                .filter(|b| b.time >= from && b.time < end)
                .collect();
            Ok(bars_to_ticks(&inside))
        })
    })
}

/// Turn bars into the ticks a replay plays: one update per bar. The candle closes
/// at its close and stretches to its high and low, with its volume; the first bar
/// also sets the open.
pub fn bars_to_ticks(bars: &[Ohlcv]) -> Vec<ReplayTick> {
    bars.iter()
        .map(|b| ReplayTick {
            price: b.close,
            open: Some(b.open),
            high: Some(b.high),
            low: Some(b.low),
            volume: b.volume,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::data::BarStore;
    use crate::data::multi_provider_feed::MultiProviderFeed;
    use crate::ports::data_provider::DataProvider;
    use crate::ports::market_data::MarketSession;
    use crate::runtime::AbortController;
    use crate::runtime::testing::ManualPlatform;
    use serde_json::json;

    const T0: i64 = 1_700_000_000_000;
    const HOUR: i64 = 3_600_000;

    // ── ReplayControl ──

    #[derive(Default)]
    struct Recorder {
        calls: RefCell<Vec<String>>,
        remaining: Cell<usize>,
    }

    impl ReplayController for Recorder {
        fn replay_start(
            &self,
            opts: ReplayStartOptions,
        ) -> LocalBoxFuture<'static, Result<(), DataError>> {
            self.calls.borrow_mut().push(format!("start {}", opts.from));
            Box::pin(std::future::ready(Ok(())))
        }
        fn replay_step(&self) -> bool {
            self.calls.borrow_mut().push("step".into());
            self.remaining.get() > 0
        }
        fn replay_step_update(&self) -> bool {
            self.calls.borrow_mut().push("stepUpdate".into());
            true
        }
        fn replay_play(&self, interval_ms: Option<f64>) {
            self.calls
                .borrow_mut()
                .push(format!("play {interval_ms:?}"));
        }
        fn replay_pause(&self) {
            self.calls.borrow_mut().push("pause".into());
        }
        fn replay_stop(&self) {
            self.calls.borrow_mut().push("stop".into());
        }
        fn replay_state(&self) -> ReplayState {
            ReplayState {
                active: true,
                playing: false,
                cursor_time: Some(T0),
                remaining: self.remaining.get(),
                next_time: Some(T0 + HOUR),
                interval_ms: 250.0,
            }
        }
        fn replay_bounds(&self) -> Option<ReplayBounds> {
            Some(ReplayBounds {
                first: T0 - 10 * HOUR,
                last: T0,
            })
        }
        fn replay_set_ticks(&self, source: Option<ReplayTickSource>) {
            self.calls
                .borrow_mut()
                .push(format!("setTicks {}", source.is_some()));
        }
    }

    fn control() -> (Rc<Recorder>, ReplayControl) {
        let controller = Rc::new(Recorder::default());
        let as_dyn: Rc<dyn ReplayController> = controller.clone();
        let control = ReplayControl::new(Rc::downgrade(&as_dyn));
        (controller, control)
    }

    #[test]
    fn every_call_reaches_the_controller_and_the_chainable_ones_chain() {
        let platform = ManualPlatform::new(0.0);
        let (controller, replay) = control();
        controller.remaining.set(3);
        assert_eq!(
            platform.block_on(replay.start(ReplayStartOptions { from: T0 })),
            Ok(())
        );
        assert!(replay.step());
        assert!(replay.step_update());
        replay
            .play(Some(500.0))
            .play(None)
            .pause()
            .set_ticks(Some(Rc::new(|_, _| {
                Box::pin(std::future::ready(Ok(Vec::new())))
            })))
            .set_ticks(None)
            .stop();
        assert_eq!(
            *controller.calls.borrow(),
            [
                format!("start {T0}"),
                "step".to_string(),
                "stepUpdate".to_string(),
                "play Some(500.0)".to_string(),
                "play None".to_string(),
                "pause".to_string(),
                "setTicks true".to_string(),
                "setTicks false".to_string(),
                "stop".to_string(),
            ]
        );
        assert_eq!(replay.state().remaining, 3);
        assert_eq!(replay.state().interval_ms, 250.0);
        assert_eq!(
            replay.bounds(),
            Some(ReplayBounds {
                first: T0 - 10 * HOUR,
                last: T0
            })
        );
    }

    #[test]
    fn a_step_reports_whether_anything_was_revealed() {
        let (controller, replay) = control();
        assert!(!replay.step());
        controller.remaining.set(1);
        assert!(replay.step());
    }

    #[test]
    fn a_control_outliving_its_chart_is_inert_and_does_not_keep_it_alive() {
        let platform = ManualPlatform::new(0.0);
        let (controller, replay) = control();
        let weak = Rc::downgrade(&controller);
        drop(controller);
        assert!(
            weak.upgrade().is_none(),
            "the control must not own the controller"
        );
        assert_eq!(
            platform.block_on(replay.start(ReplayStartOptions { from: 0 })),
            Ok(())
        );
        assert!(!replay.step() && !replay.step_update());
        replay.play(None).pause().stop().set_ticks(None);
        assert_eq!(replay.state(), ReplayState::off());
        assert_eq!(replay.bounds(), None);
    }

    #[test]
    fn the_off_state_reads_as_the_original() {
        let off = ReplayState::off();
        assert!(!off.active && !off.playing);
        assert_eq!(
            (off.cursor_time, off.next_time, off.remaining),
            (None, None, 0)
        );
        assert_eq!(off.interval_ms, 1000.0);
    }

    #[test]
    fn end_reasons_use_the_wire_names() {
        for (reason, name) in [
            (ReplayEndReason::Stopped, "stopped"),
            (ReplayEndReason::Finished, "finished"),
            (ReplayEndReason::Market, "market"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), json!(name));
            assert_eq!(
                serde_json::from_value::<ReplayEndReason>(json!(name)).unwrap(),
                reason
            );
        }
    }

    // ── tick sources ──

    #[test]
    fn bars_to_ticks_makes_one_update_per_bar_its_close_its_span_its_volume_and_the_open() {
        let up = Ohlcv::new(0, 10.0, 12.0, 9.0, 11.0, Some(8.0));
        let down = Ohlcv::new(60_000, 11.0, 13.0, 10.0, 10.0, None);
        assert_eq!(
            bars_to_ticks(&[up, down]),
            [
                ReplayTick {
                    price: 11.0,
                    open: Some(10.0),
                    high: Some(12.0),
                    low: Some(9.0),
                    volume: Some(8.0)
                },
                ReplayTick {
                    price: 10.0,
                    open: Some(11.0),
                    high: Some(13.0),
                    low: Some(10.0),
                    volume: None
                },
            ]
        );
        assert!(bars_to_ticks(&[]).is_empty());
        assert_eq!(ReplayTick::at(5.0).high, None);
    }

    /// Serves fixed bars and records what it was asked.
    struct StubProvider {
        bars: Vec<Ohlcv>,
        calls: RefCell<Vec<(String, String, BarRange)>>,
        fail: Cell<bool>,
    }

    impl DataProvider for StubProvider {
        fn get_bars(
            &self,
            ticker: &str,
            timeframe: &str,
            range: &BarRange,
        ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
            self.calls
                .borrow_mut()
                .push((ticker.into(), timeframe.into(), range.clone()));
            let answer = if self.fail.get() {
                Err(DataError::other("offline"))
            } else {
                Ok(self.bars.clone())
            };
            Box::pin(std::future::ready(answer))
        }
    }

    struct Chart {
        market: RefCell<MarketSnapshot>,
        data: DataControl,
    }

    impl LowerTimeframeChart for Chart {
        fn market(&self) -> MarketSnapshot {
            self.market.borrow().clone()
        }
        fn data(&self) -> DataControl {
            self.data.clone()
        }
    }

    struct Rig {
        platform: ManualPlatform,
        provider: Rc<StubProvider>,
        chart: Rc<Chart>,
    }

    fn rig(symbol: Option<&str>, session: Option<MarketSession>) -> Rig {
        let platform = ManualPlatform::new(0.0);
        let provider = Rc::new(StubProvider {
            bars: vec![
                Ohlcv::new(T0 - 60_000, 1.0, 1.0, 1.0, 1.0, None), // before the bar: dropped
                Ohlcv::new(T0, 5.0, 6.0, 4.0, 6.0, None),
                Ohlcv::new(T0 + HOUR, 9.0, 9.0, 9.0, 9.0, None), // the next bar: dropped
            ],
            calls: RefCell::default(),
            fail: Cell::new(false),
        });
        let feed =
            MultiProviderFeed::new(Rc::new(RefCell::new(BarStore::new())), platform.handle());
        let data = DataControl::new(Some(feed), platform.handle());
        data.register_provider("binance", provider.clone());
        platform.run();
        let chart = Rc::new(Chart {
            market: RefCell::new(MarketSnapshot {
                symbol: symbol.map(String::from),
                session,
                ..MarketSnapshot::default()
            }),
            data,
        });
        Rig {
            platform,
            provider,
            chart,
        }
    }

    fn source_of(rig: &Rig, timeframe: &str) -> ReplayTickSource {
        let as_dyn: Rc<dyn LowerTimeframeChart> = rig.chart.clone();
        lower_timeframe_ticks(Rc::downgrade(&as_dyn), timeframe)
    }

    fn bar() -> Ohlcv {
        Ohlcv::new(T0, 5.0, 6.0, 4.0, 6.0, None)
    }

    fn request(signal: AbortSignal) -> ReplayTickRequest {
        ReplayTickRequest {
            end: T0 + HOUR,
            signal,
        }
    }

    #[test]
    fn lower_timeframe_ticks_fetch_the_finer_bars_inside_the_replayed_bar() {
        let rig = rig(Some("BINANCE:BTCUSDT"), Some(MarketSession::Extended));
        let source = source_of(&rig, "1");
        let ticks = rig
            .platform
            .block_on(source(&bar(), request(AbortController::new().signal())))
            .expect("served");
        assert_eq!(
            *rig.provider.calls.borrow(),
            [(
                "BTCUSDT".to_string(),
                "1".to_string(),
                BarRange {
                    from: Some(T0),
                    to: Some(T0 + HOUR - 1),
                    limit: None,
                    session: Some("extended".into()),
                }
            )]
        );
        assert_eq!(
            ticks,
            [ReplayTick {
                price: 6.0,
                open: Some(5.0),
                high: Some(6.0),
                low: Some(4.0),
                volume: None
            }]
        );
    }

    #[test]
    fn no_session_means_no_session_in_the_request() {
        let rig = rig(Some("BINANCE:BTCUSDT"), None);
        let source = source_of(&rig, "5");
        rig.platform
            .block_on(source(&bar(), request(AbortController::new().signal())))
            .expect("served");
        assert_eq!(rig.provider.calls.borrow()[0].2.session, None);
        assert_eq!(rig.provider.calls.borrow()[0].1, "5");
    }

    #[test]
    fn the_symbol_is_read_when_the_bar_is_due_so_the_source_follows_a_market_switch() {
        let rig = rig(Some("BINANCE:BTCUSDT"), None);
        let source = source_of(&rig, "1");
        rig.chart.market.borrow_mut().symbol = Some("BINANCE:ETHUSDT".into());
        rig.platform
            .block_on(source(&bar(), request(AbortController::new().signal())))
            .expect("served");
        assert_eq!(rig.provider.calls.borrow()[0].0, "ETHUSDT");
    }

    #[test]
    fn a_symbol_no_provider_resolves_serves_nothing() {
        for symbol in [Some("NOPE"), Some(""), None] {
            let rig = rig(symbol, None);
            let source = source_of(&rig, "1");
            let ticks = rig
                .platform
                .block_on(source(&bar(), request(AbortController::new().signal())))
                .expect("not an error");
            assert!(ticks.is_empty(), "{symbol:?}");
            assert!(rig.provider.calls.borrow().is_empty());
        }
    }

    #[test]
    fn an_abort_while_fetching_drops_the_answer() {
        let rig = rig(Some("BINANCE:BTCUSDT"), None);
        let source = source_of(&rig, "1");
        let abort = AbortController::new();
        let pending = source(&bar(), request(abort.signal()));
        abort.abort();
        assert_eq!(rig.platform.block_on(pending), Ok(Vec::new()));
    }

    #[test]
    fn a_provider_failure_is_an_error_the_replay_reads_as_whole_bars() {
        let rig = rig(Some("BINANCE:BTCUSDT"), None);
        rig.provider.fail.set(true);
        let source = source_of(&rig, "1");
        let result = rig
            .platform
            .block_on(source(&bar(), request(AbortController::new().signal())));
        assert_eq!(result, Err(DataError::other("offline")));
    }

    #[test]
    fn a_source_outliving_its_chart_serves_nothing_and_does_not_keep_it_alive() {
        let Rig {
            platform,
            provider,
            chart,
        } = rig(Some("BINANCE:BTCUSDT"), None);
        let as_dyn: Rc<dyn LowerTimeframeChart> = chart.clone();
        let source = lower_timeframe_ticks(Rc::downgrade(&as_dyn), "1");
        drop(as_dyn);
        let weak = Rc::downgrade(&chart);
        drop(chart);
        assert!(
            weak.upgrade().is_none(),
            "the source must not own the chart"
        );
        let ticks = platform
            .block_on(source(&bar(), request(AbortController::new().signal())))
            .expect("not an error");
        assert!(ticks.is_empty());
        assert!(provider.calls.borrow().is_empty());
    }
}
