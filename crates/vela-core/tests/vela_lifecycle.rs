//! The facade itself: the environment and its built-ins, what a dropped or
//! destroyed chart releases, events, the view methods, engines and the data
//! control, through the public API only.

mod vela_common;

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vela_common::*;
use vela_core::data::multi_provider_feed::MultiProviderFeed;
use vela_core::events::types::{ChartEvent, ChartEventName};
use vela_core::options::{AddIndicatorOptions, RendererConstructor, VelaOptions};
use vela_core::ports::chart_renderer::{ChartRenderer, VisibleRange};
use vela_core::ports::market_data::{MarketDataFeed, SymbolFacts, SymbolInfo};
use vela_core::runtime::testing::ManualPlatform;
use vela_core::vela::{Vela, VelaDeps, VelaEnv, VelaError};
use vela_core::visible_range::VisibleRangePreset;

// ── the environment ────────────────────────────────────────

#[test]
fn a_fresh_environment_registers_the_built_ins_in_the_originals_order() {
    let platform = ManualPlatform::new(0.0);
    let env = VelaEnv::new(platform.handle());
    assert!(
        env.registries
            .chart_types
            .borrow()
            .get("heikinashi")
            .is_some()
    );
    let natives = env.registries.native_indicators.borrow();
    let types = natives.types();
    assert_eq!(
        &types[..2],
        ["volume", "vpvr"],
        "volume, vpvr, then the classics"
    );
    assert!(types.len() > 2);
    assert!(natives.get("sma").is_some_and(|d| d.multi_instance));
    assert!(env.registries.themes.borrow().registered().is_empty());
    assert!(env.renderer_defaults.borrow().snapshot().is_empty());
    assert!(env.locales.registered_locales().is_empty());
}

#[test]
fn constructing_a_chart_registers_the_built_ins_again_so_a_removed_one_comes_back() {
    let rig = Rig::builder()
        .env(|env| {
            env.registries
                .chart_types
                .borrow_mut()
                .unregister("heikinashi");
            env.registries
                .native_indicators
                .borrow_mut()
                .unregister("vpvr");
            assert!(
                env.registries
                    .chart_types
                    .borrow()
                    .get("heikinashi")
                    .is_none()
            );
        })
        .build();
    assert!(
        rig.env
            .registries
            .chart_types
            .borrow()
            .get("heikinashi")
            .is_some()
    );
    assert!(
        rig.env
            .registries
            .native_indicators
            .borrow()
            .get("vpvr")
            .is_some()
    );
}

#[test]
fn charts_on_one_environment_share_its_registries_and_bar_store() {
    let a = Rig::builder().build();
    let b = Rig::builder().build_on(a.platform.clone(), a.env.clone());
    assert!(Rc::ptr_eq(
        &a.env.registries.chart_types,
        &b.env.registries.chart_types
    ));
    assert!(Rc::ptr_eq(&a.env.bar_store, &b.env.bar_store));
    // A native indicator registered once is available to both charts.
    let mut probe = a
        .env
        .registries
        .native_indicators
        .borrow()
        .get("sma")
        .cloned()
        .expect("sma");
    probe.indicator_type = "probe".to_string();
    a.env
        .registries
        .native_indicators
        .borrow_mut()
        .register(probe);
    let handle = b.chart.add_native_indicator("probe", Default::default());
    assert_eq!(b.chart.present_native_indicators(), ["probe"]);
    assert!(a.chart.present_native_indicators().is_empty());
    drop(handle);
}

#[test]
fn two_environments_share_nothing() {
    let a = Rig::builder().build();
    let b = Rig::builder().build();
    assert!(!Rc::ptr_eq(
        &a.env.registries.chart_types,
        &b.env.registries.chart_types
    ));
    assert!(!Rc::ptr_eq(&a.env.bar_store, &b.env.bar_store));
}

// ── release ────────────────────────────────────────────────

/// A renderer factory handing out recording renderers, keeping only a `Weak`.
fn weak_renderer() -> (RendererConstructor, Rc<RefCell<Option<Weak<RecRenderer>>>>) {
    let slot: Rc<RefCell<Option<Weak<RecRenderer>>>> = Rc::default();
    let sink = Rc::clone(&slot);
    let make: RendererConstructor = Rc::new(move |_| {
        let renderer = RecRenderer::new();
        *sink.borrow_mut() = Some(Rc::downgrade(&renderer));
        renderer as Rc<dyn ChartRenderer>
    });
    (make, slot)
}

fn live_chart(env: &VelaEnv, make: RendererConstructor) -> Vela {
    let mut options = VelaOptions::default();
    options.market.symbol = Some("AAA".to_string());
    options.market.timeframe = Some("60".to_string());
    options.live = Some(true);
    let feed = FakeFeed::new();
    Vela::with_deps(
        env,
        VelaOptions {
            renderer: Some(make),
            ..options
        },
        VelaDeps {
            engines: vec![
                AutoEngine::new() as Rc<dyn vela_core::ports::scripting_engine::ScriptingEngine>
            ],
            data_feed: Some(feed as Rc<dyn MarketDataFeed>),
            ..VelaDeps::default()
        },
    )
    .expect("built")
}

fn busy(chart: &Vela, platform: &ManualPlatform) {
    // Everything the facade registers on the engine: listeners, controls, pending runs.
    chart.on(ChartEventName::Ready, |_| {});
    let _ = chart.replay().state();
    let _ = chart.panes().list();
    let _ = chart.drawings().all();
    let _ = chart.marks().all();
    drop(chart.run_script("plot(close)", AddIndicatorOptions::default()));
    drop(chart.run_indicator("plot(close)", AddIndicatorOptions::default()));
    chart.add_native_indicator("vpvr", Default::default());
    chart.set_locale("de");
    platform.advance(10.0);
}

#[test]
fn dropping_the_last_handle_releases_the_engine() {
    let platform = ManualPlatform::new(T0 as f64);
    let env = VelaEnv::new(platform.handle());
    let (make, renderer) = weak_renderer();
    let chart = live_chart(&env, make);
    busy(&chart, &platform);
    platform.advance(1000.0);
    let weak = chart.downgrade();
    let panes = chart.panes().clone();
    let replay = chart.replay().clone();
    assert!(weak.upgrade().is_some());
    assert!(renderer.borrow().as_ref().and_then(Weak::upgrade).is_some());

    drop(chart);
    platform.advance(1000.0);

    assert!(weak.upgrade().is_none(), "the facade is gone");
    assert!(
        renderer.borrow().as_ref().and_then(Weak::upgrade).is_none(),
        "no reference cycle through the controls or the event bus keeps the engine alive"
    );
    // The controls that outlived it are inert.
    assert!(panes.list().is_empty());
    assert!(!replay.state().active);
}

#[test]
fn destroying_then_dropping_releases_the_engine() {
    let platform = ManualPlatform::new(T0 as f64);
    let env = VelaEnv::new(platform.handle());
    let (make, renderer) = weak_renderer();
    let chart = live_chart(&env, make);
    busy(&chart, &platform);
    let clone = chart.clone();
    chart.destroy();
    platform.advance(1000.0);
    drop(chart);
    assert!(
        renderer.borrow().as_ref().and_then(Weak::upgrade).is_some(),
        "a clone still holds the destroyed chart"
    );
    drop(clone);
    assert!(renderer.borrow().as_ref().and_then(Weak::upgrade).is_none());
}

#[test]
fn a_chart_is_a_cheap_handle_and_clones_are_the_same_chart() {
    let rig = Rig::builder().build();
    let clone = rig.chart.clone();
    clone.set_theme("light");
    assert_eq!(rig.log.count(ChartEventName::ThemeChanged), 1);
    let weak = rig.chart.downgrade();
    assert!(weak.upgrade().is_some());
}

#[test]
fn destroy_stops_the_renderer_and_the_feed_once_and_a_later_drop_does_not_repeat_it() {
    let rig = Rig::builder().live(true).build();
    rig.ready().ok();
    let destroys = |rig: &Rig| {
        rig.feed
            .calls()
            .iter()
            .filter(|c| matches!(c, FeedCall::Destroy))
            .count()
    };
    rig.chart.destroy();
    assert!(rig.renderer.destroyed.get());
    assert_eq!(destroys(&rig), 1);
    rig.chart.destroy();
    assert_eq!(destroys(&rig), 1, "destroy is idempotent");
    let Rig { chart, feed, .. } = rig;
    drop(chart);
    let count = feed
        .calls()
        .iter()
        .filter(|c| matches!(c, FeedCall::Destroy))
        .count();
    assert_eq!(
        count, 1,
        "dropping a destroyed chart does not destroy it again"
    );
}

#[test]
fn dropping_a_chart_that_was_never_destroyed_destroys_it() {
    let rig = Rig::builder().live(true).build();
    rig.ready().ok();
    let Rig {
        chart,
        renderer,
        feed,
        ..
    } = rig;
    drop(chart);
    assert!(renderer.destroyed.get());
    assert_eq!(
        feed.active_subscribers(),
        0,
        "the live subscription is closed"
    );
}

// ── events ─────────────────────────────────────────────────

#[test]
fn off_detaches_a_handler_and_a_handler_may_call_back_into_the_chart() {
    let rig = Rig::builder().build();
    let count = Rc::new(Cell::new(0));
    let sink = Rc::clone(&count);
    let sub = rig.chart.on(ChartEventName::ThemeChanged, move |_| {
        sink.set(sink.get() + 1)
    });
    rig.chart.set_theme("light");
    assert_eq!(count.get(), 1);
    assert!(rig.chart.off(&sub));
    assert!(!rig.chart.off(&sub), "already detached");
    rig.chart.set_theme("dark");
    assert_eq!(count.get(), 1);

    // Re-entrancy: a handler that reads and writes the chart (a host reacting to a change).
    let chart = rig.chart.downgrade();
    let seen: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
    let sink = Rc::clone(&seen);
    rig.chart.on(ChartEventName::MarketChanged, move |_| {
        if let Some(chart) = chart.upgrade() {
            sink.borrow_mut().push(chart.market().symbol);
            chart.set_theme("light");
        }
    });
    rig.switch(to_symbol("BBB")).expect("switched");
    assert_eq!(*seen.borrow(), [Some("BBB".to_string())]);
}

// ── the view ───────────────────────────────────────────────

#[test]
fn the_view_methods_reach_the_renderer() {
    let rig = Rig::builder().build();
    rig.ready().ok();
    rig.flush();
    assert_eq!(rig.chart.get_visible_range(), None);
    rig.renderer.visible_range.set(Some(VisibleRange {
        from: 1_000,
        to: 5_000,
    }));
    assert_eq!(
        rig.chart.get_visible_range(),
        Some(VisibleRange {
            from: 1_000,
            to: 5_000
        })
    );

    rig.chart
        .set_visible_range(VisibleRange { from: 10, to: 20 });
    assert_eq!(
        rig.renderer
            .visible_range_calls
            .borrow()
            .last()
            .map(|(r, _)| *r),
        Some(VisibleRange { from: 10, to: 20 })
    );

    // No drag-equivalent pan: an instant shift of the visible range.
    rig.chart.pan_by(0.5);
    assert_eq!(
        rig.renderer
            .visible_range_calls
            .borrow()
            .last()
            .map(|(r, _)| *r),
        Some(VisibleRange {
            from: 3_000,
            to: 7_000
        })
    );
    rig.renderer.pan_by_supported.set(true);
    rig.chart.pan_by(0.25);
    assert_eq!(*rig.renderer.pan_calls.borrow(), [0.25]);

    rig.chart.set_visible_range_preset(VisibleRangePreset::All);
    let last = rig
        .renderer
        .visible_range_calls
        .borrow()
        .last()
        .cloned()
        .expect("a call");
    assert!(last.1.right_margin, "a preset keeps the right margin");
}

#[test]
fn the_chart_methods_chain() {
    let rig = Rig::builder().build();
    rig.chart
        .set_visible_range(VisibleRange { from: 1, to: 2 })
        .pan_by(0.1)
        .set_theme("light")
        .set_locale("en")
        .register_engine("pine", AutoEngine::new());
}

// ── engines, data and facts ────────────────────────────────

#[test]
fn an_engine_registered_after_construction_runs_scripts_and_a_replacement_warns() {
    let rig = Rig::builder().no_engine().build();
    rig.ready().ok();
    // No engine: the indicator fails soft, with its error on the handle's event.
    let failed = rig.block_on(
        rig.chart
            .run_indicator("plot(close)", AddIndicatorOptions::default()),
    );
    assert!(!failed.ok);
    assert!(failed.error.is_some());

    let first = AutoEngine::new();
    rig.chart.register_engine(
        "pine",
        Rc::clone(&first) as Rc<dyn vela_core::ports::scripting_engine::ScriptingEngine>,
    );
    let ok = rig.block_on(
        rig.chart
            .run_indicator("plot(close)", AddIndicatorOptions::default()),
    );
    assert!(ok.ok);
    assert!(
        rig.platform
            .warnings()
            .iter()
            .all(|w| !w.contains("re-registering"))
    );

    rig.chart.register_engine("pine", AutoEngine::new());
    assert!(
        rig.platform
            .warnings()
            .iter()
            .any(|w| w.contains("re-registering scripting engine"))
    );
}

#[test]
fn the_default_language_follows_the_first_engine_unless_the_options_say_otherwise() {
    let platform = ManualPlatform::new(T0 as f64);
    let env = VelaEnv::new(platform.handle());
    let renderer = RecRenderer::new();
    let engine = FakeEngine::with_language("luau");
    let mut options = VelaOptions::default();
    options.market.data = Some(bars(10));
    options.volume = Some(false);
    let chart = Vela::with_deps(
        &env,
        options,
        VelaDeps {
            renderer: Some(renderer as Rc<dyn ChartRenderer>),
            engines: vec![
                Rc::clone(&engine) as Rc<dyn vela_core::ports::scripting_engine::ScriptingEngine>
            ],
            ..VelaDeps::default()
        },
    )
    .expect("built");
    let handle = chart
        .add_indicator("x", AddIndicatorOptions::default())
        .expect("added");
    assert_eq!(handle.language(), Some("luau"));
    let _ = platform;
}

#[test]
fn the_data_control_drives_the_default_feed_and_is_inert_on_a_custom_one() {
    let rig = Rig::builder().build();
    assert!(
        !rig.chart.data().has_registry(),
        "a custom feed is used bare"
    );
    rig.chart.data().register_provider("x", Rc::new(NoProvider));
    assert!(!rig.platform.warnings().is_empty());

    let platform = ManualPlatform::new(T0 as f64);
    let env = VelaEnv::new(platform.handle());
    let mut options = VelaOptions::default();
    options.market.data = Some(bars(10));
    let chart = Vela::with_deps(
        &env,
        options,
        VelaDeps {
            renderer: Some(RecRenderer::new() as Rc<dyn ChartRenderer>),
            ..VelaDeps::default()
        },
    )
    .expect("built");
    assert!(chart.data().has_registry());
    chart.data().register_provider("x", Rc::new(NoProvider));
    assert_eq!(chart.data().providers().len(), 1);
}

#[test]
fn a_registry_feed_the_host_built_is_driven_by_the_data_control_and_shares_the_environment_cache() {
    let platform = ManualPlatform::new(T0 as f64);
    let env = VelaEnv::new(platform.handle());
    let feed = MultiProviderFeed::new(Rc::clone(&env.bar_store), platform.handle());
    let mut options = VelaOptions::default();
    options.market.data = Some(bars(10));
    let chart = Vela::with_deps(
        &env,
        options,
        VelaDeps {
            renderer: Some(RecRenderer::new() as Rc<dyn ChartRenderer>),
            registry_feed: Some(feed.clone()),
            ..VelaDeps::default()
        },
    )
    .expect("built");
    assert!(chart.data().has_registry());
    chart.data().register_provider("x", Rc::new(NoProvider));
    assert_eq!(
        feed.providers().len(),
        1,
        "the host's own feed got the provider"
    );
}

struct NoProvider;

impl vela_core::ports::data_provider::DataProvider for NoProvider {
    fn get_bars(
        &self,
        _ticker: &str,
        _timeframe: &str,
        _range: &vela_core::ports::market_data::BarRange,
    ) -> vela_core::runtime::LocalBoxFuture<
        'static,
        Result<Vec<vela_core::model::Ohlcv>, vela_core::runtime::DataError>,
    > {
        Box::pin(async { Ok(Vec::new()) })
    }
}

#[test]
fn symbol_facts_come_from_the_feed_symbol_info_and_announce_themselves() {
    let rig = Rig::builder()
        .feed(|f| {
            f.set_symbol_info_for(
                "AAA",
                SymbolInfo {
                    ticker: "AAA".into(),
                    volume: Some(false),
                    ..SymbolInfo::default()
                },
            )
        })
        .volume(true)
        .build();
    rig.ready().ok();
    rig.flush();
    assert_eq!(
        rig.chart.symbol_facts(),
        Some(SymbolFacts {
            ohlc: None,
            volume: Some(false)
        })
    );
    // The first facts landed inside the constructor, before the host subscribed: a
    // switch resets them to unknown and announces the new symbol's.
    rig.switch(to_symbol("BBB")).expect("switched");
    rig.flush();
    assert!(rig.log.count(ChartEventName::SymbolFacts) >= 1);
    // A source stating no volume keeps the auto-added volume indicator off.
    assert!(rig.chart.present_native_indicators().is_empty());
}

#[test]
fn inspect_reports_the_generated_scene_without_a_renderer() {
    let rig = Rig::builder().build();
    rig.ready().ok();
    assert!(rig.chart.inspect().indicators.is_empty());
    let result = rig.block_on(
        rig.chart
            .run_indicator("plot(close)", AddIndicatorOptions::default()),
    );
    assert!(result.ok);
    rig.flush();
    let scene = rig.chart.inspect();
    assert_eq!(scene.indicators.len(), 1);
}

#[test]
fn the_facade_errors_display_the_underlying_message() {
    assert!(VelaError::MissingRenderer.to_string().contains("renderer"));
    let data = VelaError::from(vela_core::runtime::DataError::other("network down"));
    assert_eq!(data.to_string(), "network down");
    assert!(matches!(data, VelaError::Data(_)));
}

#[test]
fn a_chart_event_payload_reaches_a_listener_registered_through_the_facade() {
    let rig = Rig::builder().build();
    let seen: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = Rc::clone(&seen);
    rig.chart.on(ChartEventName::LoadEnd, move |e| {
        if let ChartEvent::LoadEnd { bars, .. } = e {
            sink.borrow_mut().push(bars.to_string());
        }
    });
    rig.ready().ok();
    assert_eq!(*seen.borrow(), ["50"]);
}
