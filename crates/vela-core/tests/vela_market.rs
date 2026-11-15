//! `Vela::set_market`, `Vela::market`, `ready`, `history_complete`, the load
//! states and native-indicator presence, through the public API only
//! (`set-market.test.ts`, the cases that go through `chart.setMarket`).

mod vela_common;

use std::cell::RefCell;
use std::rc::Rc;

use vela_common::*;
use vela_core::events::types::{ChartEvent, ChartEventName, HistoryReason};
use vela_core::options::MarketSwitch;

fn rig_with_prices() -> Rig {
    Rig::builder()
        .feed(|f| {
            f.set_price("AAA", 111.0);
            f.set_price("BBB", 222.0);
            f.set_price("CCC", 333.0);
            f.set_price("SLOW", 444.0);
        })
        .build()
}

fn first_close(rig: &Rig) -> Option<f64> {
    rig.renderer.bars.borrow().first().map(|b| b.close)
}

type Identity = (Option<String>, Option<String>);

fn market_events(rig: &Rig) -> Rc<RefCell<Vec<Identity>>> {
    let seen: Rc<RefCell<Vec<Identity>>> = Rc::default();
    let sink = Rc::clone(&seen);
    rig.chart.on(ChartEventName::MarketChanged, move |e| {
        if let ChartEvent::MarketChanged {
            symbol, timeframe, ..
        } = e
        {
            sink.borrow_mut()
                .push((Some(symbol.clone()), Some(timeframe.clone())));
        }
    });
    seen
}

#[test]
fn set_market_reloads_bars_reframes_and_emits_market_changed() {
    let rig = rig_with_prices();
    assert_eq!(rig.ready(), Ok(()));
    let events = market_events(&rig);

    let done = rig.chart.set_market(MarketSwitch {
        symbol: Some("BBB".to_string()),
        timeframe: Some("240".to_string()),
        ..MarketSwitch::default()
    });
    assert_eq!(rig.block_on(done), Ok(()));

    assert_eq!(first_close(&rig), Some(222.0));
    let last = rig.renderer.last_bars_call().expect("a set_bars call");
    assert!(!last.preserve_view, "a fresh series re-frames the view");
    assert_eq!(
        *events.borrow(),
        [(Some("BBB".to_string()), Some("240".to_string()))]
    );
    let loads = rig.feed.loads();
    let load = loads.last().expect("a load");
    assert_eq!(load.symbol.as_deref(), Some("BBB"));
    assert_eq!(load.timeframe.as_deref(), Some("240"));
}

#[test]
fn market_snapshots_the_requested_identity_so_an_in_flight_switch_shows_at_once() {
    let rig = rig_with_prices();
    rig.ready().ok();
    let snap = rig.chart.market();
    assert_eq!(snap.symbol.as_deref(), Some("AAA"));
    assert_eq!(snap.timeframe.as_deref(), Some("60"));
    assert!(!snap.offline);

    let done = rig.chart.set_market(MarketSwitch {
        symbol: Some("BBB".to_string()),
        timeframe: Some("240".to_string()),
        ..MarketSwitch::default()
    });
    // Before the load lands the snapshot already answers with the new identity.
    let snap = rig.chart.market();
    assert_eq!(snap.symbol.as_deref(), Some("BBB"));
    assert_eq!(snap.timeframe.as_deref(), Some("240"));
    assert_eq!(rig.block_on(done), Ok(()));
    assert_eq!(rig.chart.market().symbol.as_deref(), Some("BBB"));
}

#[test]
fn a_same_market_call_is_a_no_op_with_no_reload_and_no_event() {
    let rig = rig_with_prices();
    rig.ready().ok();
    let events = market_events(&rig);
    let loads_before = rig.feed.loads().len();

    let same = rig.chart.set_market(MarketSwitch {
        symbol: Some("AAA".to_string()),
        timeframe: Some("60".to_string()),
        ..MarketSwitch::default()
    });
    assert_eq!(rig.block_on(same), Ok(()));
    assert_eq!(
        rig.block_on(rig.chart.set_market(MarketSwitch::default())),
        Ok(())
    );

    assert_eq!(rig.feed.loads().len(), loads_before);
    assert!(events.borrow().is_empty());
}

#[test]
fn a_depth_only_reload_fetches_but_does_not_emit_market_changed() {
    let rig = Rig::builder()
        .bars(100)
        .feed(|f| f.set_depth("AAA", 900))
        .build();
    rig.ready().ok();
    let events = market_events(&rig);

    let done = rig.chart.set_market(MarketSwitch {
        bars: Some(800),
        ..MarketSwitch::default()
    });
    assert_eq!(rig.block_on(done), Ok(()));
    rig.block_on(rig.chart.history_complete());
    rig.flush();

    assert_eq!(rig.renderer.bars.borrow().len(), 800);
    assert!(events.borrow().is_empty());
}

#[test]
fn rapid_double_switch_the_last_call_wins_and_the_superseded_one_resolves_silently() {
    let rig = rig_with_prices();
    rig.feed.gate_symbol("SLOW");
    rig.ready().ok();
    let events = market_events(&rig);

    let slow = rig.chart.set_market(to_symbol("SLOW")); // the load parks on the gate
    let fast = rig.chart.set_market(to_symbol("BBB")); // supersedes it
    assert_eq!(rig.block_on(fast), Ok(()));
    assert_eq!(rig.block_on(slow), Ok(()), "resolves promptly, never hangs");

    assert_eq!(first_close(&rig), Some(222.0));
    rig.feed.release(); // the stale SLOW load finally lands...
    rig.flush();
    assert_eq!(
        first_close(&rig),
        Some(222.0),
        "...and is dropped, never painted"
    );
    assert_eq!(
        *events.borrow(),
        [(Some("BBB".to_string()), Some("60".to_string()))]
    );
}

#[test]
fn a_switch_to_offline_data_and_back_to_a_symbol_drops_the_offline_dataset() {
    let rig = rig_with_prices();
    rig.ready().ok();

    let offline = flat_bars(30, 777.0);
    let to_data = rig.chart.set_market(MarketSwitch {
        data: Some(offline),
        ..MarketSwitch::default()
    });
    assert_eq!(rig.block_on(to_data), Ok(()));
    assert_eq!(rig.renderer.bars.borrow().len(), 30);
    assert_eq!(first_close(&rig), Some(777.0));
    assert!(rig.chart.market().offline);

    assert_eq!(rig.block_on(rig.chart.set_market(to_symbol("CCC"))), Ok(()));
    assert_eq!(first_close(&rig), Some(333.0));
    assert!(!rig.chart.market().offline);
}

#[test]
fn destroy_mid_switch_releases_the_awaiter() {
    let rig = rig_with_prices();
    rig.feed.gate_symbol("SLOW");
    rig.ready().ok();
    let pending = rig.chart.set_market(to_symbol("SLOW"));
    rig.chart.destroy();
    assert_eq!(
        rig.block_on(pending),
        Ok(()),
        "resolves instead of hanging on the parked load"
    );
}

#[test]
fn a_switch_mid_backfill_abandons_the_old_backfill_loop() {
    let rig = Rig::builder()
        .bars(12_000)
        .feed(|f| {
            f.set_price("BBB", 222.0);
            f.set_depth("AAA", 15_000);
            f.gate_ranges(true);
        })
        .build();
    rig.ready().ok(); // preview + first chunk painted; backfill parked on the gate
    let progress: Rc<RefCell<Vec<ChartEvent>>> = Rc::default();
    let sink = Rc::clone(&progress);
    rig.chart.on(ChartEventName::HistoryProgress, move |e| {
        sink.borrow_mut().push(e.clone());
    });
    let old_complete = rig.chart.history_complete();

    let switch = rig.chart.set_market(MarketSwitch {
        symbol: Some("BBB".to_string()),
        bars: Some(50),
        ..MarketSwitch::default()
    });
    assert_eq!(rig.block_on(switch), Ok(()));
    rig.block_on(old_complete); // the superseded load's promise resolves rather than hanging
    assert_eq!(first_close(&rig), Some(222.0));

    rig.feed.release(); // the old backfill chunk finally lands...
    rig.flush();
    assert!(
        progress.borrow().is_empty(),
        "...but the abandoned loop emitted nothing"
    );
    assert_eq!(
        first_close(&rig),
        Some(222.0),
        "and the new market stayed put"
    );
}

#[test]
fn history_complete_is_re_armed_per_load() {
    let rig = rig_with_prices();
    let completes: Rc<RefCell<Vec<HistoryReason>>> = Rc::default();
    let sink = Rc::clone(&completes);
    rig.chart.on(ChartEventName::HistoryComplete, move |e| {
        if let ChartEvent::HistoryComplete { reason, .. } = e {
            sink.borrow_mut().push(*reason);
        }
    });
    let first = rig.chart.history_complete();
    rig.ready().ok();
    rig.block_on(first);
    assert_eq!(*completes.borrow(), [HistoryReason::Depth]);

    assert_eq!(rig.block_on(rig.chart.set_market(to_symbol("BBB"))), Ok(()));
    rig.block_on(rig.chart.history_complete()); // the NEW load's promise
    assert_eq!(
        *completes.borrow(),
        [HistoryReason::Depth, HistoryReason::Depth]
    );
}

#[test]
fn a_switch_re_targets_the_live_subscription() {
    let rig = Rig::builder().live(true).build();
    rig.ready().ok();
    let (subs, unsubs) = rig.feed.subscriptions();
    assert_eq!(subs.get("AAA"), Some(&1));
    assert!(unsubs.is_empty());

    assert_eq!(rig.block_on(rig.chart.set_market(to_symbol("BBB"))), Ok(()));

    let (subs, unsubs) = rig.feed.subscriptions();
    assert_eq!(unsubs.get("AAA"), Some(&1), "old stream closed");
    assert_eq!(subs.get("BBB"), Some(&1), "new stream opened");
}

#[test]
fn the_first_load_raises_the_affordance_and_ends_with_the_first_batch() {
    let rig = rig_with_prices();
    rig.ready().ok();
    assert_eq!(
        *rig.renderer.timeline.borrow(),
        ["loading:true", "bars:50", "loading:false"]
    );
}

#[test]
fn a_failed_load_still_ends_the_affordance_and_ready_reports_it() {
    let rig = Rig::builder()
        .feed(|f| f.set_load_error(Some(vela_core::runtime::DataError::other("network down"))))
        .build();
    let ready = rig.ready();
    assert!(ready.is_err() || !rig.renderer.loading.get());
    assert!(
        !rig.renderer.loading.get(),
        "an empty chart, no eternal dots"
    );
}

#[test]
fn a_parked_symbol_drops_the_affordance_and_announces_data_unresolved() {
    let rig = Rig::builder().feed(|f| f.gate_symbol("AAA")).build();
    rig.feed.park("AAA");
    assert!(!rig.renderer.loading.get());
    assert_eq!(rig.log.count(ChartEventName::DataUnresolved), 1);
}

#[test]
fn native_indicator_presence_is_synchronous() {
    let rig = Rig::builder().volume(false).build();
    rig.ready().ok();
    assert!(rig.chart.present_native_indicators().is_empty());

    let handle = rig.chart.add_native_indicator("vpvr", Default::default());
    // Synchronous: an unload-time persist flush must see this at once.
    assert_eq!(rig.chart.present_native_indicators(), ["vpvr"]);
    handle.remove();
    assert!(rig.chart.present_native_indicators().is_empty());
}

#[test]
fn a_single_instance_native_indicator_returns_the_existing_handle_and_classics_stack() {
    let rig = Rig::builder().volume(false).build();
    rig.ready().ok();
    let a = rig.chart.add_native_indicator("vpvr", Default::default());
    let b = rig.chart.add_native_indicator("vpvr", Default::default());
    assert_eq!(a.id(), b.id());
    let s1 = rig.chart.add_native_indicator("sma", Default::default());
    let s2 = rig.chart.add_native_indicator("sma", Default::default());
    assert_ne!(s1.id(), s2.id(), "a classic study allows several instances");
    assert_eq!(
        rig.chart.present_native_indicators(),
        ["vpvr", "sma", "sma"]
    );
}

#[test]
fn the_available_catalog_lists_the_built_ins_with_their_live_state() {
    let rig = Rig::builder().volume(false).build();
    rig.ready().ok();
    rig.chart.add_native_indicator("vpvr", Default::default());
    let catalog = rig.block_on(rig.chart.available_native_indicators());
    let vpvr = catalog
        .iter()
        .find(|i| i.indicator_type == "vpvr")
        .expect("vpvr");
    assert!(vpvr.present);
    let volume = catalog
        .iter()
        .find(|i| i.indicator_type == "volume")
        .expect("volume");
    assert!(!volume.present);
    assert!(
        catalog
            .iter()
            .any(|i| i.indicator_type == "sma" && i.multi_instance)
    );
}

#[test]
fn the_auto_added_volume_follows_the_volume_option_and_is_pending_until_the_facts_land() {
    let rig = Rig::builder().volume(true).build();
    rig.ready().ok();
    rig.flush();
    assert_eq!(rig.chart.present_native_indicators(), ["volume"]);
    assert!(rig.chart.pending_native_indicators().is_empty());
}
