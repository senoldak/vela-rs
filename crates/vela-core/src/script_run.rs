//! What host code observes of a script computing: the `script:run` payload
//! ([`ScriptRun`]) and what `chart.runScript()` resolves to ([`ScriptRunResult`]).

use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::Millis;
use crate::model::strategy::{StrategyState, StrategyTrade};
use crate::ports::scripting_engine::EngineWarning;
use crate::runtime::{LocalBoxFuture, Unsubscribe, noop_unsubscribe};

/// Why a script computed. The distinction that matters most is `Tick` vs `Bar`: a
/// tick refines the bar that is still open, so its values can still move, while
/// `Bar` means a new bar opened and everything before it is now settled. Anything
/// that records, alerts or exports should key off `Bar`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptRunCause {
    /// The first computation over the loaded history (also every re-computation
    /// an engine makes while a deep backfill is still landing: see
    /// [`ScriptRun::complete`]).
    History,
    /// The forming bar changed (a live tick).
    Tick,
    /// A new bar opened: the previous one is final.
    Bar,
    /// An input was edited.
    Inputs,
    /// The script's source was replaced in place (`handle.updateCode`).
    Code,
    /// The visible range moved (viewport-aware scripts only).
    Viewport,
    /// The chart's market changed and the script re-executed over the new bars.
    Market,
}

/// Whether a script is an indicator or a strategy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptKind {
    Indicator,
    Strategy,
}

/// One point of a plot's full history; `None` marks a gap.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlotSeriesPoint {
    pub time: Millis,
    pub value: Option<f64>,
}

/// Loads a strategy's round trips, closed then open.
pub type TradesLoader = Rc<dyn Fn() -> LocalBoxFuture<'static, Vec<StrategyTrade>>>;
/// Loads one plot's full history by key.
pub type SeriesLoader = Rc<dyn Fn(&str) -> LocalBoxFuture<'static, Vec<PlotSeriesPoint>>>;

/// One computation of one script, as host code observes it: the payload of
/// `script:run` and what `runScript()` resolves to.
///
/// The split is deliberate: everything FLAT and at the current bar rides the run
/// itself, while anything historical and unbounded (the trade ledger, a plot's
/// full history) is a call made only when needed, so a per-tick listener never
/// ships a 5,000-row ledger it will not read.
#[derive(Clone)]
pub struct ScriptRun {
    /// The indicator id: the one `chart.indicators()` and the lifecycle events carry.
    pub id: String,
    /// The title the script DECLARED (`strategy("SMA cross")`), not a placeholder.
    pub title: String,
    pub kind: ScriptKind,
    pub cause: ScriptRunCause,
    /// This script's first computed run.
    pub first: bool,
    /// Index of the last computed bar.
    pub bar: i64,
    /// Open time of that bar.
    pub time: Millis,
    /// That bar is still open, so these values are provisional. False on a static
    /// chart and on any run computed over settled history.
    pub forming: bool,
    /// The run saw the FULL requested history. False only while an engine that
    /// computes progressively is still being fed a deep backfill: a deeper run
    /// will follow.
    pub complete: bool,
    /// Each named plot's value at [`bar`](Self::bar); `None` marks a gap.
    pub plots: BTreeMap<String, Option<f64>>,
    /// The script's own variables at [`bar`](Self::bar), keyed by the names
    /// WRITTEN in the source. Empty for an engine that exposes none.
    pub vars: BTreeMap<String, Value>,
    /// Broker state at [`bar`](Self::bar): present iff `kind` is `Strategy` and
    /// the engine reports it.
    pub strategy: Option<StrategyState>,
    /// Warnings this script has raised so far.
    pub warnings: Vec<EngineWarning>,
    /// Backs [`trades`](Self::trades); `None` loads nothing.
    pub trades_loader: Option<TradesLoader>,
    /// Backs [`series`](Self::series); `None` loads nothing.
    pub series_loader: Option<SeriesLoader>,
}

impl ScriptRun {
    /// The strategy's round trips, closed then open. Async because the ledger is
    /// unbounded: it never rides the run. Empty for an indicator.
    pub fn trades(&self) -> LocalBoxFuture<'static, Vec<StrategyTrade>> {
        match &self.trades_loader {
            Some(load) => load(),
            None => Box::pin(async { Vec::new() }),
        }
    }

    /// One plot's full history. Async for the same reason. Empty for an unknown key.
    pub fn series(&self, key: &str) -> LocalBoxFuture<'static, Vec<PlotSeriesPoint>> {
        match &self.series_loader {
            Some(load) => load(key),
            None => Box::pin(async { Vec::new() }),
        }
    }
}

impl fmt::Debug for ScriptRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScriptRun")
            .field("id", &self.id)
            .field("title", &self.title)
            .field("kind", &self.kind)
            .field("cause", &self.cause)
            .field("bar", &self.bar)
            .field("time", &self.time)
            .finish_non_exhaustive()
    }
}

/// A listener of a script's later runs.
pub type ScriptRunHandler = Box<dyn FnMut(&ScriptRun)>;

/// What `chart.runScript()` resolves to: the script's first run, plus the
/// controls for the indicator it put on the chart. A compile or runtime failure
/// is a value (`ok == false`), never an error, and the indicator is removed
/// again (no dead legend row).
#[derive(Clone)]
pub struct ScriptRunResult {
    pub ok: bool,
    /// The first computed run on success; `None` on failure.
    pub run: Option<ScriptRun>,
    /// The failure's message, or `None`.
    pub error: Option<String>,
    follow: Rc<dyn Fn(ScriptRunHandler) -> Unsubscribe>,
    take_off: Rc<dyn Fn()>,
}

impl ScriptRunResult {
    /// A success. `follow` subscribes to the script's later runs and `take_off`
    /// removes the script from the chart.
    pub fn succeeded(
        run: ScriptRun,
        follow: impl Fn(ScriptRunHandler) -> Unsubscribe + 'static,
        take_off: impl Fn() + 'static,
    ) -> Self {
        Self {
            ok: true,
            run: Some(run),
            error: None,
            follow: Rc::new(follow),
            take_off: Rc::new(take_off),
        }
    }

    /// A failure: following and removing do nothing.
    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            run: None,
            error: Some(error.into()),
            follow: Rc::new(|_| noop_unsubscribe()),
            take_off: Rc::new(|| {}),
        }
    }

    /// Follow this script's later runs. Returns the unsubscriber; does nothing
    /// after a failure.
    pub fn on_update(&self, handler: impl FnMut(&ScriptRun) + 'static) -> Unsubscribe {
        (self.follow)(Box::new(handler))
    }

    /// Take the script off the chart. Does nothing after a failure.
    pub fn remove(&self) {
        (self.take_off)();
    }
}

impl fmt::Debug for ScriptRunResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScriptRunResult")
            .field("ok", &self.ok)
            .field("run", &self.run)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use serde_json::json;

    use super::*;
    use crate::runtime::testing::ManualPlatform;

    fn run() -> ScriptRun {
        ScriptRun {
            id: "ind-1".into(),
            title: "SMA cross".into(),
            kind: ScriptKind::Strategy,
            cause: ScriptRunCause::History,
            first: true,
            bar: 41,
            time: 1_000,
            forming: false,
            complete: true,
            plots: BTreeMap::from([("fast".to_string(), Some(1.5)), ("slow".to_string(), None)]),
            vars: BTreeMap::from([("n".to_string(), json!(3))]),
            strategy: None,
            warnings: Vec::new(),
            trades_loader: None,
            series_loader: None,
        }
    }

    #[test]
    fn a_run_without_loaders_has_no_ledger_and_no_history() {
        let p = ManualPlatform::new(0.0);
        let r = run();
        assert!(p.block_on(r.trades()).is_empty());
        assert!(p.block_on(r.series("fast")).is_empty());
    }

    #[test]
    fn loaders_back_the_async_accessors() {
        let p = ManualPlatform::new(0.0);
        let asked = Rc::new(RefCell::new(Vec::new()));
        let log = asked.clone();
        let mut r = run();
        r.series_loader = Some(Rc::new(move |key| {
            log.borrow_mut().push(key.to_string());
            Box::pin(async {
                vec![
                    PlotSeriesPoint {
                        time: 1,
                        value: Some(2.0),
                    },
                    PlotSeriesPoint {
                        time: 2,
                        value: None,
                    },
                ]
            })
        }));
        let points = p.block_on(r.series("fast"));
        assert_eq!(points.len(), 2);
        assert_eq!(points[1].value, None);
        assert_eq!(*asked.borrow(), ["fast"]);
    }

    #[test]
    fn cause_and_kind_use_the_wire_names() {
        assert_eq!(
            serde_json::to_value(ScriptRunCause::Viewport).unwrap(),
            json!("viewport")
        );
        assert_eq!(
            serde_json::from_value::<ScriptKind>(json!("strategy")).unwrap(),
            ScriptKind::Strategy
        );
    }

    #[test]
    fn a_failed_result_is_inert() {
        let result = ScriptRunResult::failed("boom");
        assert!(!result.ok);
        assert!(result.run.is_none());
        assert_eq!(result.error.as_deref(), Some("boom"));
        let heard = Rc::new(Cell::new(0));
        let h = heard.clone();
        let off = result.on_update(move |_| h.set(h.get() + 1));
        off();
        result.remove();
        assert_eq!(heard.get(), 0);
    }

    #[test]
    fn a_successful_result_follows_runs_and_removes_the_script() {
        let handlers: Rc<RefCell<Vec<ScriptRunHandler>>> = Rc::default();
        let removed = Rc::new(Cell::new(false));
        let (hs, rm) = (handlers.clone(), removed.clone());
        let result = ScriptRunResult::succeeded(
            run(),
            move |handler| {
                hs.borrow_mut().push(handler);
                Box::new(|| {})
            },
            move || rm.set(true),
        );
        assert!(result.ok);
        assert_eq!(result.run.as_ref().map(|r| r.bar), Some(41));
        assert!(result.error.is_none());
        let heard = Rc::new(Cell::new(0));
        let h = heard.clone();
        let _off = result.on_update(move |r| h.set(h.get() + r.bar));
        let mut registered = handlers.borrow_mut();
        registered[0](&run());
        assert_eq!(heard.get(), 41);
        drop(registered);
        result.remove();
        assert!(removed.get());
    }
}
