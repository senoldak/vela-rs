//! `chart.replay` through the public API (`replay.test.ts`, the `chart.replay`
//! cases): the control surface, its state and its `replay:*` events on a real
//! chart.

mod vela_common;

use std::cell::RefCell;
use std::rc::Rc;

use vela_common::*;
use vela_core::controls::replay_control::{
    ReplayBounds, ReplayEndReason, ReplayStartOptions, ReplayState,
};
use vela_core::events::types::{ChartEvent, ChartEventName};
use vela_core::model::Ohlcv;
use vela_core::options::MarketSwitch;

/// `total` hourly bars, close = index (so a bar is identifiable).
fn universe(total: usize) -> Vec<Ohlcv> {
    (0..total)
        .map(|i| {
            let p = i as f64;
            Ohlcv::new(T0 + i as i64 * HOUR, p, p + 1.0, p - 1.0, p, Some(1.0))
        })
        .collect()
}

fn rig_of(bars: usize, total: usize, live: bool) -> Rig {
    let rig = Rig::builder()
        .live(live)
        .bars(bars)
        .feed(move |f| f.set_series("AAA", universe(total)))
        .build();
    rig.ready().expect("ready");
    rig.flush();
    rig
}

fn rig() -> Rig {
    rig_of(100, 200, true)
}

fn shown(rig: &Rig) -> Vec<Ohlcv> {
    rig.renderer.bars.borrow().clone()
}

fn closes(bars: &[Ohlcv]) -> Vec<f64> {
    bars.iter().map(|b| b.close).collect()
}

fn start(rig: &Rig, from: i64) -> Result<(), vela_core::vela::VelaError> {
    let started = rig.chart.replay().start(ReplayStartOptions { from });
    rig.block_on(async move { started.await.map_err(Into::into) })
}

fn subs(rig: &Rig) -> usize {
    rig.feed.subscriptions().0.values().sum()
}

fn unsubs(rig: &Rig) -> usize {
    rig.feed.subscriptions().1.values().sum()
}

/// A recorder of `replay:*` events as compact strings.
fn replay_log(rig: &Rig) -> Rc<RefCell<Vec<String>>> {
    let seen: Rc<RefCell<Vec<String>>> = Rc::default();
    for name in [
        ChartEventName::ReplayStart,
        ChartEventName::ReplayStep,
        ChartEventName::ReplayPlay,
        ChartEventName::ReplayPause,
        ChartEventName::ReplayEnd,
    ] {
        let sink = Rc::clone(&seen);
        rig.chart.on(name, move |e| {
            let line = match e {
                ChartEvent::ReplayStart {
                    cursor_time,
                    remaining,
                } => format!("start:{cursor_time}:{remaining}"),
                ChartEvent::ReplayStep {
                    cursor_time,
                    remaining,
                } => format!("step:{cursor_time}:{remaining}"),
                ChartEvent::ReplayPlay { interval_ms } => format!("play:{interval_ms}"),
                ChartEvent::ReplayPause => "pause".to_string(),
                ChartEvent::ReplayEnd { reason } => format!("end:{}", end_name(*reason)),
                other => format!("{other:?}"),
            };
            sink.borrow_mut().push(line);
        });
    }
    seen
}

fn end_name(reason: ReplayEndReason) -> &'static str {
    match reason {
        ReplayEndReason::Stopped => "stopped",
        ReplayEndReason::Finished => "finished",
        ReplayEndReason::Market => "market",
    }
}

#[test]
fn start_cuts_the_history_pauses_live_updates_and_reports_the_queue() {
    let rig = rig();
    assert_eq!(subs(&rig), 1);
    let full = shown(&rig); // newest 100 of 200: closes 100..199
    let from = full[39].time;
    let log = replay_log(&rig);

    assert_eq!(start(&rig, from), Ok(()));

    let cut = shown(&rig);
    assert_eq!(cut.len(), 40);
    assert_eq!(cut.last().map(|b| b.time), Some(from));
    assert_eq!(unsubs(&rig), 1, "live paused");
    assert_eq!(*log.borrow(), [format!("start:{from}:60")]);
    assert_eq!(
        rig.chart.replay().state(),
        ReplayState {
            active: true,
            playing: false,
            cursor_time: Some(from),
            remaining: 60,
            next_time: Some(from + HOUR),
            interval_ms: 1000.0,
        }
    );
}

#[test]
fn step_reveals_the_next_bar_through_the_live_bar_path() {
    let rig = rig();
    rig.chart
        .add_indicator("probe", Default::default())
        .expect("added");
    rig.flush();
    let from = shown(&rig)[49].time;
    assert_eq!(start(&rig, from), Ok(()));
    rig.flush();
    // The indicator re-ran over the CUT history: its newest bar is the cursor bar.
    let executions = rig.engine.inner.executions.borrow().clone();
    assert_eq!(executions.last().map(|e| e.bar_count), Some(50));
    let session = rig.engine.inner.last_session().expect("a session");
    let notified = session.notifies().len();
    let log = replay_log(&rig);
    let bar_events = rig.log.count(ChartEventName::Bar);

    assert!(rig.chart.replay().step());

    assert_eq!(closes(&rig.renderer.updated_bars.borrow()), [150.0]);
    assert_eq!(rig.log.count(ChartEventName::Bar), bar_events + 1);
    assert_eq!(*log.borrow(), [format!("step:{}:49", from + HOUR)]);
    assert_eq!(session.notifies().len(), notified + 1);
    assert_eq!(rig.chart.replay().state().cursor_time, Some(from + HOUR));
}

#[test]
fn play_reveals_one_bar_per_interval_pause_stops_and_play_again_changes_the_pace() {
    let rig = rig();
    let from = shown(&rig)[9].time;
    assert_eq!(start(&rig, from), Ok(()));
    let log = replay_log(&rig);

    rig.chart.replay().play(Some(15.0));
    assert!(rig.chart.replay().state().playing);
    rig.advance(100.0);
    rig.chart.replay().pause();
    let revealed = rig.renderer.updated_bars.borrow().len();
    assert!(revealed >= 3, "{revealed}");
    assert!(!rig.chart.replay().state().playing);
    rig.advance(60.0);
    assert_eq!(
        rig.renderer.updated_bars.borrow().len(),
        revealed,
        "paused: nothing more"
    );

    rig.chart.replay().play(Some(10.0));
    rig.chart.replay().play(Some(20.0)); // a pace change while playing
    assert_eq!(rig.chart.replay().state().interval_ms, 20.0);
    rig.chart.replay().pause();
    let events: Vec<String> = log
        .borrow()
        .iter()
        .filter(|l| l.starts_with("play") || *l == "pause")
        .cloned()
        .collect();
    assert_eq!(events, ["play:15", "pause", "play:10", "play:20", "pause"]);
}

#[test]
fn a_non_positive_interval_is_rejected_with_a_warning_and_changes_nothing() {
    let rig = rig();
    let from = shown(&rig)[9].time;
    assert_eq!(start(&rig, from), Ok(()));
    rig.chart.replay().play(Some(0.0));
    assert!(!rig.platform.warnings().is_empty());
    assert!(!rig.chart.replay().state().playing);
}

#[test]
fn stop_restores_the_full_history_reruns_indicators_resumes_live_and_heals_the_gap() {
    let rig = rig();
    rig.chart
        .add_indicator("probe", Default::default())
        .expect("added");
    rig.flush();
    let full = shown(&rig);
    assert_eq!(start(&rig, full[29].time), Ok(()));
    assert!(rig.chart.replay().step());
    let log = replay_log(&rig);
    let ranges_before = rig.feed.ranges().len();

    rig.chart.replay().stop();
    rig.flush();

    let now = shown(&rig);
    assert_eq!(now.len(), 100);
    assert_eq!(now[99].close, 199.0);
    let executions = rig.engine.inner.executions.borrow().clone();
    assert_eq!(
        executions.last().map(|e| e.bar_count),
        Some(100),
        "re-ran over the full history"
    );
    assert_eq!(subs(&rig), 2, "live resumed");
    let ranges = rig.feed.ranges();
    assert_eq!(ranges.len(), ranges_before + 1);
    assert_eq!(
        ranges.last().and_then(|r| r.from),
        Some(now[99].time),
        "heal from the newest known bar"
    );
    assert_eq!(*log.borrow(), ["end:stopped"]);
    assert_eq!(
        rig.chart.replay().state(),
        ReplayState {
            interval_ms: rig.chart.replay().state().interval_ms,
            ..ReplayState::off()
        }
    );
    assert!(!rig.chart.replay().step());
}

#[test]
fn revealing_the_last_bar_ends_the_replay_as_finished_and_resumes_live() {
    let rig = rig();
    let from = shown(&rig)[97].time;
    assert_eq!(start(&rig, from), Ok(()));
    let log = replay_log(&rig);

    assert!(rig.chart.replay().step());
    assert!(log.borrow().iter().all(|l| !l.starts_with("end")));
    assert!(rig.chart.replay().step());

    assert!(log.borrow().iter().any(|l| l == "end:finished"));
    // The cut series, then the two revealed bars as live updates.
    let mut now = closes(&shown(&rig));
    now.extend(closes(&rig.renderer.updated_bars.borrow()));
    assert_eq!(now[now.len() - 3..], [197.0, 198.0, 199.0]);
    assert_eq!(subs(&rig), 2);
    assert!(!rig.chart.replay().state().active);
}

#[test]
fn seeking_while_active_rewinds_over_the_whole_tape_without_ending_the_replay() {
    let rig = rig();
    let full = shown(&rig);
    assert_eq!(start(&rig, full[59].time), Ok(()));
    rig.chart.replay().play(Some(1000.0));
    let log = replay_log(&rig);

    assert_eq!(start(&rig, full[19].time), Ok(())); // backward
    assert_eq!(shown(&rig).len(), 20);
    assert_eq!(start(&rig, full[79].time), Ok(())); // forward, past the old cursor
    assert_eq!(shown(&rig).len(), 80);

    assert_eq!(
        *log.borrow(),
        [
            "pause".to_string(),
            format!("start:{}:80", full[19].time),
            format!("start:{}:20", full[79].time),
        ]
    );
    assert!(!rig.chart.replay().state().playing);
}

#[test]
fn a_from_older_than_the_loaded_history_deepens_it_first() {
    let rig = rig_of(100, 300, true);
    let oldest_loaded = shown(&rig)[0].time; // close 200
    let ranges_before = rig.feed.ranges().len();

    assert_eq!(start(&rig, oldest_loaded - 50 * HOUR), Ok(())); // close 150

    assert!(rig.feed.ranges().len() > ranges_before);
    assert_eq!(shown(&rig).last().map(|b| b.close), Some(150.0));
    assert_eq!(rig.chart.replay().state().remaining, 49 + 100);
}

#[test]
fn bounds_span_the_whole_walkable_history_hidden_bars_included() {
    let rig = Rig::builder()
        .live(true)
        .bars(100)
        .feed(|f| f.set_series("AAA", universe(200)))
        .build();
    assert_eq!(rig.chart.replay().bounds(), None);
    rig.ready().expect("ready");
    rig.flush();
    let full = shown(&rig);
    let bounds = ReplayBounds {
        first: full[0].time,
        last: full[99].time,
    };
    assert_eq!(rig.chart.replay().bounds(), Some(bounds));
    assert_eq!(start(&rig, full[40].time), Ok(()));
    assert_eq!(rig.chart.replay().bounds(), Some(bounds));
}

#[test]
fn a_from_with_no_bar_after_it_is_ignored_with_a_warning() {
    let rig = rig();
    let last = shown(&rig)[99].time;
    assert_eq!(start(&rig, last), Ok(()));
    assert!(!rig.platform.warnings().is_empty());
    assert!(!rig.chart.replay().state().active);
    assert_eq!(shown(&rig).len(), 100);
}

#[test]
fn a_market_switch_ends_the_replay() {
    let rig = rig();
    let from = shown(&rig)[49].time;
    assert_eq!(start(&rig, from), Ok(()));
    rig.chart.replay().play(Some(1000.0));
    let log = replay_log(&rig);

    let switch = rig.chart.set_market(to_symbol("BBB"));
    assert_eq!(rig.block_on(switch), Ok(()));

    assert!(
        log.borrow().iter().any(|l| l == "end:market"),
        "{:?}",
        log.borrow()
    );
    let state = rig.chart.replay().state();
    assert!(!state.active && !state.playing);
    assert_eq!(
        shown(&rig).len(),
        50,
        "the new symbol serves the default depth"
    );
    assert_eq!(subs(&rig), 2, "the switch's own live restart");
}

#[test]
fn a_timeframe_switch_keeps_the_replay_same_point_still_playing_live_still_off() {
    let rig = rig();
    let cursor = shown(&rig)[49].time;
    assert_eq!(start(&rig, cursor), Ok(()));
    rig.chart.replay().play(Some(10_000.0));
    let log = replay_log(&rig);
    let market_changes = rig.log.count(ChartEventName::MarketChanged);
    let subs_before = subs(&rig);

    let switch = rig.chart.set_market(MarketSwitch {
        timeframe: Some("30".to_string()),
        ..MarketSwitch::default()
    });
    assert_eq!(rig.block_on(switch), Ok(()));
    rig.flush();

    assert_eq!(
        rig.log.count(ChartEventName::MarketChanged),
        market_changes + 1
    );
    assert!(
        log.borrow().iter().all(|l| !l.starts_with("end")),
        "{:?}",
        log.borrow()
    );
    let state = rig.chart.replay().state();
    assert!(state.active && state.playing);
    assert_eq!(state.cursor_time, Some(cursor));
    assert_eq!(
        subs(&rig),
        subs_before,
        "no live subscription while replaying"
    );
    rig.chart.replay().stop();
}

#[test]
fn a_replay_control_outliving_the_chart_is_inert() {
    let rig = rig();
    let control = rig.chart.replay().clone();
    let Rig { chart, .. } = rig;
    drop(chart);
    assert!(!control.step());
    assert_eq!(control.state(), ReplayState::off());
    assert_eq!(control.bounds(), None);
}
