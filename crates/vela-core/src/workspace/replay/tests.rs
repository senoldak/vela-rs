//! Ports of `workspace-replay.test.ts`: every cell of a workspace replays on one clock, so
//! at any moment each chart shows exactly the bars that had CLOSED by the shared replay time.
//! The cells are real charts on the in-memory host, fed by a provider that serves one 15m
//! universe aggregated to every timeframe.

use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::model::Ohlcv;
use crate::options::VelaOptions;
use crate::ports::data_provider::{DataProvider, SymbolDescriptor};
use crate::ports::market_data::BarRange;
use crate::runtime::testing::ManualPlatform;
use crate::runtime::{DataError, LocalBoxFuture};
use crate::vela::VelaEnv;
use crate::workspace::cell::CellSeed;
use crate::workspace::layouts::LayoutRegistry;
use crate::workspace::shell::{LayoutChoice, Workspace, WorkspaceOptions};
use crate::workspace::testing::TestWorkspaceHost;

const MIN: i64 = 60_000;
const Q: i64 = 15 * MIN;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;
/// 2024-01-01T00:00:00Z, a Monday.
const T0: i64 = 1_704_067_200_000;
const BASE_BARS: i64 = 4 * 24 * 40; // 40 days of 15m bars

fn tf_ms(tf: &str) -> i64 {
    match tf {
        "15" => Q,
        "D" => DAY,
        _ => HOUR,
    }
}

fn weekend(t: i64) -> bool {
    // 1970-01-01 was a Thursday.
    let weekday = (t.div_euclid(DAY) + 4).rem_euclid(7);
    weekday == 0 || weekday == 6
}

/// The symbol's bars on `tf`, aggregated from one 15m universe; `GAP` trades weekdays only.
fn series(symbol: &str, tf: &str) -> Vec<Ohlcv> {
    let step = tf_ms(tf) / Q;
    let mut out = Vec::new();
    let mut i = 0;
    while i < BASE_BARS {
        let time = T0 + i * Q;
        if !(symbol == "GAP" && weekend(time)) {
            let open = i as f64;
            out.push(Ohlcv::new(
                time,
                open,
                open + step as f64,
                open - 1.0,
                open + step as f64 - 1.0,
                Some(1.0),
            ));
        }
        i += step;
    }
    out
}

struct Universe;

impl DataProvider for Universe {
    fn get_bars(
        &self,
        ticker: &str,
        timeframe: &str,
        range: &BarRange,
    ) -> LocalBoxFuture<'static, Result<Vec<Ohlcv>, DataError>> {
        let bars: Vec<Ohlcv> = series(ticker, timeframe)
            .into_iter()
            .filter(|b| {
                range.from.is_none_or(|f| b.time >= f) && range.to.is_none_or(|t| b.time <= t)
            })
            .collect();
        Box::pin(std::future::ready(Ok(bars)))
    }

    fn list_symbols(
        &self,
    ) -> Option<LocalBoxFuture<'static, Result<Vec<SymbolDescriptor>, DataError>>> {
        let list = ["AAA", "GAP"]
            .iter()
            .map(|t| SymbolDescriptor {
                ticker: (*t).to_string(),
                ..SymbolDescriptor::default()
            })
            .collect();
        Some(Box::pin(std::future::ready(Ok(list))))
    }
}

struct Rig {
    platform: ManualPlatform,
    ws: Workspace,
    host: Rc<TestWorkspaceHost>,
}

impl Rig {
    fn new(layout: &str, cells: &[(&str, &str, &str)]) -> Rig {
        let platform = ManualPlatform::new((T0 + 400 * DAY) as f64);
        let env = VelaEnv::new(platform.handle());
        let host = TestWorkspaceHost::new(800.0, 400.0);
        let opts = WorkspaceOptions {
            chart: VelaOptions {
                live: Some(false),
                volume: Some(false),
                ..VelaOptions::default()
            },
            layout: LayoutChoice::Id(layout.into()),
            cells: cells
                .iter()
                .map(|(id, symbol, tf)| {
                    (
                        (*id).to_string(),
                        CellSeed {
                            symbol: Some((*symbol).to_string()),
                            timeframe: Some((*tf).to_string()),
                            bars: Some(BASE_BARS as usize),
                            ..CellSeed::default()
                        },
                    )
                })
                .collect(),
            providers: vec![(
                "series".to_string(),
                Rc::new(Universe) as Rc<dyn DataProvider>,
            )],
            layouts: LayoutRegistry::with_builtins(),
            ..WorkspaceOptions::default()
        };
        let ws = Workspace::new(host.clone(), env, opts).expect("a workspace");
        let rig = Rig { platform, ws, host };
        rig.settle();
        rig
    }

    fn settle(&self) {
        for _ in 0..4 {
            self.platform.run();
            self.host.run_until_idle();
        }
        self.platform.run();
    }

    fn chart(&self, id: &str) -> crate::vela::Vela {
        self.ws.cell(id).expect("cell").try_chart().expect("chart")
    }

    /// The close of the newest bar a chart shows.
    fn shown_end(&self, id: &str) -> i64 {
        let chart = self.chart(id);
        let cursor = chart.replay().state().cursor_time.expect("a replay cursor");
        bar_close(cursor as f64, chart.market().timeframe.as_deref()) as i64
    }

    fn start(&self, from: i64) {
        self.platform.block_on(
            self.ws
                .replay()
                .start(WorkspaceReplayStartOptions { from, cell: None }),
        );
        self.settle();
    }
}

fn log() -> Rc<RefCell<Vec<String>>> {
    Rc::default()
}

#[test]
fn fixed_timeframes_add_their_length_and_month_based_ones_add_calendar_months() {
    let utc = |y: i64, m: i64, d: i64| (days_from_civil(y, m, d) * DAY) as f64;
    assert_eq!(bar_close(T0 as f64, Some("60")), (T0 + HOUR) as f64);
    assert_eq!(bar_close(T0 as f64, Some("D")), (T0 + DAY) as f64);
    assert_eq!(bar_close(utc(2024, 1, 1), Some("M")), utc(2024, 2, 1)); // 31 days
    assert_eq!(bar_close(utc(2024, 2, 1), Some("M")), utc(2024, 3, 1)); // 29 days (leap year)
    assert_eq!(bar_close(utc(2024, 1, 1), Some("3M")), utc(2024, 4, 1));
    assert_eq!(
        last_open_closed_by((T0 + HOUR) as f64, Some("60")),
        T0 as f64
    );
    assert_eq!(
        last_open_closed_by(utc(2024, 3, 1), Some("M")),
        utc(2024, 2, 1)
    );
}

#[test]
fn bar_close_edge_cases_follow_the_js_date() {
    let utc = |y: i64, m: i64, d: i64| (days_from_civil(y, m, d) * DAY) as f64;
    // Month overflow across the year, and a day past the end of the next month rolling over.
    assert_eq!(bar_close(utc(2024, 12, 1), Some("M")), utc(2025, 1, 1));
    assert_eq!(bar_close(utc(2024, 1, 31), Some("M")), utc(2024, 3, 2));
    assert_eq!(bar_close(utc(2023, 11, 15), Some("3M")), utc(2024, 2, 15));
    // Seconds and milliseconds drop out of a month-based close, as `Date.UTC` reads h and min only.
    assert_eq!(
        bar_close(utc(2024, 1, 1) + 12.0 * 3_600_000.0 + 5_123.0, Some("M")),
        utc(2024, 2, 1) + 12.0 * 3_600_000.0
    );
    // Absent and junk timeframes read as one hour; `0M` is not month-based.
    assert_eq!(bar_close(0.0, None), HOUR as f64);
    assert_eq!(
        bar_close(0.0, Some("0M")),
        crate::data::timeframe_to_ms("0M") as f64
    );
    // Negative epochs (before 1970) round-trip through the civil arithmetic.
    let before = utc(1969, 12, 1);
    assert!(before < 0.0);
    assert_eq!(bar_close(before, Some("M")), utc(1970, 1, 1));
}

#[test]
fn start_rewinds_every_chart_to_the_shared_time_so_no_chart_shows_a_bar_that_had_not_closed() {
    let rig = Rig::new(
        "4",
        &[
            ("a", "AAA", "60"),
            ("b", "AAA", "15"),
            ("c", "AAA", "D"),
            ("d", "AAA", "60"),
        ],
    );
    let pick = T0 + 20 * DAY + 10 * HOUR; // the 10:00 bar of the 1h chart
    rig.start(pick);
    let clock = pick + HOUR; // that bar's close
    assert_eq!(rig.shown_end("a"), clock);
    assert_eq!(
        rig.shown_end("b"),
        clock,
        "the 15m chart up to its 10:45 bar"
    );
    assert!(
        rig.shown_end("c") <= clock,
        "the daily chart up to yesterday"
    );
    assert_eq!(
        rig.chart("c").replay().state().cursor_time,
        Some(T0 + 19 * DAY)
    );
    let state = rig.ws.replay().state();
    assert!(state.active && !state.playing);
    assert_eq!(state.cursor_time, Some(pick));
    for id in ["a", "b", "c", "d"] {
        assert!(rig.chart(id).replay().state().active, "{id} replays");
    }
}

#[test]
fn a_step_moves_the_clock_to_the_next_bar_close_on_any_chart_so_the_finest_sets_the_pace() {
    let rig = Rig::new("2h", &[("a", "AAA", "60"), ("b", "AAA", "15")]);
    let pick = T0 + 10 * DAY;
    rig.start(pick);
    let clock0 = pick + HOUR;
    let steps: Rc<RefCell<Vec<i64>>> = Rc::default();
    let s = steps.clone();
    rig.ws
        .replay()
        .on(WorkspaceReplayEventName::Step, move |e| {
            if let WorkspaceReplayEvent::Step { cursor_time, .. } = e {
                s.borrow_mut().push(*cursor_time);
            }
        });
    for i in 1..=3 {
        assert!(rig.ws.replay().step());
        assert_eq!(rig.shown_end("b"), clock0 + i * Q, "one 15m bar per step");
        assert_eq!(rig.shown_end("a"), clock0, "the 1h bar is still forming");
    }
    rig.ws.replay().step();
    assert_eq!(rig.shown_end("b"), clock0 + HOUR);
    assert_eq!(
        rig.shown_end("a"),
        clock0 + HOUR,
        "and completes with the 4th"
    );
    assert_eq!(
        steps.borrow().as_slice(),
        [pick, pick, pick, pick + HOUR],
        "reported for the active (1h) cell"
    );
}

#[test]
fn a_weekday_only_market_never_shows_monday_while_the_clock_is_still_in_the_weekend() {
    let rig = Rig::new("2h", &[("a", "AAA", "60"), ("g", "GAP", "60")]);
    let friday = T0 + 4 * DAY; // Jan 5
    rig.start(friday + 22 * HOUR);
    assert_eq!(
        rig.chart("g").replay().state().cursor_time,
        Some(friday + 22 * HOUR)
    );
    for _ in 0..30 {
        rig.ws.replay().step(); // through Saturday into Sunday
    }
    assert_eq!(rig.shown_end("a"), friday + 23 * HOUR + 30 * HOUR);
    assert_eq!(
        rig.chart("g").replay().state().cursor_time,
        Some(friday + 23 * HOUR),
        "Friday's last bar, nothing more"
    );
    for _ in 0..25 {
        rig.ws.replay().step(); // into Monday
    }
    assert!(rig.chart("g").replay().state().cursor_time.unwrap() < rig.shown_end("a"));
    assert_eq!(rig.shown_end("g"), rig.shown_end("a"));
}

#[test]
fn play_advances_on_the_shared_clock_and_pause_stops_it() {
    let rig = Rig::new("2h", &[("a", "AAA", "60"), ("b", "AAA", "15")]);
    rig.start(T0 + 10 * DAY);
    let before = rig.shown_end("b");
    let events = log();
    let (e1, e2) = (events.clone(), events.clone());
    rig.ws
        .replay()
        .on(WorkspaceReplayEventName::Play, move |_| {
            e1.borrow_mut().push("play".into())
        });
    rig.ws
        .replay()
        .on(WorkspaceReplayEventName::Pause, move |_| {
            e2.borrow_mut().push("pause".into())
        });
    rig.ws.replay().play(Some(10.0));
    rig.platform.advance(120.0);
    rig.ws.replay().pause();
    let moved = rig.shown_end("b");
    assert!(moved >= before + 4 * Q, "{moved} vs {before}");
    assert!(
        !rig.chart("a").replay().state().playing,
        "the charts own timers never ran"
    );
    assert!(!rig.chart("b").replay().state().playing);
    rig.platform.advance(40.0);
    assert_eq!(rig.shown_end("b"), moved, "paused: nothing moves");
    assert_eq!(events.borrow().as_slice(), ["play", "pause"]);
}

#[test]
fn play_ignores_a_bad_interval_and_does_nothing_without_a_session() {
    let rig = Rig::new("2h", &[("a", "AAA", "60"), ("b", "AAA", "15")]);
    rig.ws.replay().play(None); // no session yet
    assert!(!rig.ws.replay().state().playing);
    rig.start(T0 + 10 * DAY);
    rig.ws.replay().play(Some(0.0));
    rig.ws.replay().play(Some(f64::NAN));
    assert!(!rig.ws.replay().state().playing);
    assert!(
        rig.platform
            .warnings()
            .iter()
            .any(|w| w.contains("interval must be a positive number"))
    );
    rig.ws.replay().play(Some(250.0));
    assert_eq!(rig.ws.replay().state().interval_ms, 250.0);
    rig.ws.replay().stop();
    assert!(!rig.ws.replay().state().playing, "a stop ends the play");
    rig.ws.replay().pause(); // not playing: nothing
}

#[test]
fn stop_ends_the_replay_on_every_chart_and_so_does_any_chart_reaching_its_last_bar() {
    let rig = Rig::new("2h", &[("a", "AAA", "60"), ("b", "AAA", "15")]);
    let ends = log();
    let e = ends.clone();
    rig.ws
        .replay()
        .on(WorkspaceReplayEventName::End, move |ev| {
            if let WorkspaceReplayEvent::End { reason } = ev {
                e.borrow_mut().push(format!("{reason:?}").to_lowercase());
            }
        });
    rig.start(T0 + 10 * DAY);
    rig.ws.replay().stop();
    assert!(!rig.chart("a").replay().state().active && !rig.chart("b").replay().state().active);
    assert!(!rig.ws.replay().state().active);

    let hourly = series("AAA", "60");
    let last_hour = hourly.last().unwrap().time;
    rig.start(last_hour - 2 * HOUR); // two 1h bars left
    for _ in 0..20 {
        if !rig.ws.replay().state().active {
            break;
        }
        rig.ws.replay().step();
    }
    assert!(!rig.ws.replay().state().active);
    assert!(!rig.chart("a").replay().state().active && !rig.chart("b").replay().state().active);
    assert_eq!(ends.borrow().as_slice(), ["stopped", "finished"]);
    rig.ws.replay().stop(); // idempotent: no second end
    assert_eq!(ends.borrow().len(), 2);
}

#[test]
fn a_cell_added_mid_replay_joins_at_the_shared_time() {
    let rig = Rig::new(
        "2h",
        &[("a", "AAA", "60"), ("b", "AAA", "15"), ("c", "AAA", "15")],
    );
    rig.start(T0 + 10 * DAY);
    rig.ws.replay().step();
    let clock = rig.shown_end("b");
    rig.ws.set_layout("4").unwrap(); // the declared cell c takes the third slot
    rig.settle();
    rig.platform.advance(10.0);
    rig.settle();
    assert!(
        rig.chart("c").replay().state().active,
        "the late cell replays"
    );
    assert_eq!(rig.shown_end("c"), clock);
}

#[test]
fn a_cell_switching_symbol_rejoins_the_replay_once_its_new_bars_land() {
    let rig = Rig::new("2h", &[("a", "AAA", "60"), ("b", "AAA", "60")]);
    rig.start(T0 + 10 * DAY);
    rig.ws.replay().step();
    let clock = rig.shown_end("a");
    let ends = log();
    let e = ends.clone();
    rig.ws.replay().on(WorkspaceReplayEventName::End, move |_| {
        e.borrow_mut().push("end".into())
    });
    drop(rig.chart("b").set_market(crate::options::MarketSwitch {
        symbol: Some("GAP".into()),
        ..Default::default()
    }));
    rig.settle();
    rig.platform.advance(10.0);
    rig.settle();
    assert_eq!(rig.chart("b").market().symbol.as_deref(), Some("GAP"));
    assert!(rig.chart("b").replay().state().active);
    assert!(rig.shown_end("b") <= clock);
    assert!(rig.ws.replay().state().active);
    assert!(ends.borrow().is_empty(), "the workspace replay never ended");
}

#[test]
fn a_single_chart_layout_hands_every_call_to_the_chart_itself() {
    let rig = Rig::new("1", &[("a", "AAA", "60")]);
    rig.start(T0 + 10 * DAY);
    let chart = rig.chart("a");
    chart.replay().set_ticks(Some(Rc::new(|bar: &Ohlcv, _req| {
        let ticks = vec![
            crate::controls::replay_control::ReplayTick::at(bar.open + 1.0),
            crate::controls::replay_control::ReplayTick::at(bar.open + 2.0),
            crate::controls::replay_control::ReplayTick::at(bar.close),
        ];
        Box::pin(std::future::ready(Ok(ticks)))
    })));
    let ticks: Rc<RefCell<Vec<usize>>> = Rc::default();
    let t = ticks.clone();
    rig.ws
        .replay()
        .on(WorkspaceReplayEventName::Tick, move |e| {
            if let WorkspaceReplayEvent::Tick { index, .. } = e {
                t.borrow_mut().push(*index);
            }
        });
    rig.ws.replay().step_update();
    rig.ws.replay().step_update();
    assert_eq!(
        ticks.borrow().as_slice(),
        [0, 1],
        "tick by tick, through the chart's own replay"
    );
    rig.ws.replay().play(Some(10.0));
    assert!(
        chart.replay().state().playing,
        "the chart's own timer paces the ticks"
    );
    rig.ws.replay().pause();
    assert!(!chart.replay().state().playing);
}

#[test]
fn the_active_cells_bounds_and_an_empty_start_are_safe() {
    let rig = Rig::new("2h", &[("a", "AAA", "60"), ("b", "AAA", "15")]);
    let bounds = rig.ws.replay().bounds().expect("bounds");
    assert_eq!(bounds.first, T0);
    // Not started: the state is off and a step does nothing.
    assert!(!rig.ws.replay().state().active);
    assert!(!rig.ws.replay().step());
    assert!(!rig.ws.replay().step_update());
    // A start on an unknown reference cell falls back to the first cell.
    rig.platform
        .block_on(rig.ws.replay().start(WorkspaceReplayStartOptions {
            from: T0 + 10 * DAY,
            cell: Some("nope".into()),
        }));
    rig.settle();
    assert!(rig.ws.replay().state().active);
}
