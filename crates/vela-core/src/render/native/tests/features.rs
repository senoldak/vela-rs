//! Ports of the renderer-level cases of `native-axis-bundle` (feature defaults,
//! keyboard, animation settling, size sync), `native-live-bar-ease`,
//! `native-manual-scale` (feature defaults), `crosshair-override` (the feature),
//! `free-pan`, `scale-price-only`, `data-window-readout`, `series-past-last-bar`
//! (the renderer part) and the other feature round-trips.

use std::cell::RefCell;

use serde_json::{Value, json};

use super::*;
use crate::model::{NativeTag, Pane, PaneKind};
use crate::ports::chart_renderer::{DataWindowReadout, VisibleRange};
use crate::render::core::input_logic::KeyInput;

fn bare() -> Rc<NativeRenderer> {
    NativeRenderer::new(TestHost::new(800.0, 400.0, 1.0), None)
}

// ── feature defaults and setters ──

#[test]
fn the_axis_bundle_features_have_sensible_defaults_and_are_settable_without_a_mount() {
    let r = bare();
    let features = r.features();
    for f in [
        "highlights",
        "gridlines",
        "axisLabels",
        "scaleMode",
        "timezone",
        "keyboard",
        "paneResize",
    ] {
        assert!(features.iter().any(|k| k == f), "{f}");
    }
    assert_eq!(r.read_feature("gridlines"), Some(json!(true)));
    assert_eq!(r.read_feature("axisLabels"), Some(json!(true)));
    assert_eq!(r.read_feature("scaleMode"), Some(json!("price")));
    assert_eq!(r.read_feature("timezone"), Some(json!("UTC")));
    assert_eq!(r.read_feature("keyboard"), Some(json!(true)));
    assert_eq!(r.read_feature("highlights"), Some(json!([])));
    assert_eq!(r.read_feature("paneResize"), Some(json!(true)));
    r.apply_feature("scaleMode", &json!("percent"));
    assert_eq!(r.read_feature("scaleMode"), Some(json!("percent")));
    r.apply_feature("scaleMode", &json!("price"));
    r.apply_feature("timezone", &json!("America/New_York"));
    assert_eq!(r.read_feature("timezone"), Some(json!("America/New_York")));
    r.apply_feature("gridlines", &json!(false));
    assert_eq!(r.read_feature("gridlines"), Some(json!(false)));
    r.apply_feature("paneResize", &json!(false));
    assert_eq!(r.read_feature("paneResize"), Some(json!(false)));
    assert!(!r.input_settings().pane_resize);
    r.apply_feature("paneResize", &json!(true));
    assert!(r.input_settings().pane_resize);
}

#[test]
fn highlights_are_sanitized_dropped_sorted_and_given_the_default_color() {
    let r = bare();
    r.apply_feature(
        "highlights",
        &json!([
            { "from": 300, "to": 400, "color": "#abc" },
            { "from": 100, "to": 200 },
            { "from": 500, "to": 500 },
            { "from": "x", "to": 10 },
            null,
        ]),
    );
    let hs = r.read_feature("highlights").unwrap();
    let hs = hs.as_array().unwrap();
    assert_eq!(
        hs.iter()
            .map(|h| h["from"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [100, 300]
    );
    assert!(hs[0]["color"].as_str().unwrap().starts_with("rgba("));
    assert_eq!(hs[1]["color"], "#abc");
}

#[test]
fn session_zones_are_sanitized_and_null_clears_them() {
    let r = bare();
    r.apply_feature(
        "sessionZones",
        &json!({ "pre": [[200, 300], [100, 150], [5, 5]], "post": "x", "extended": [[1, 2]] }),
    );
    assert_eq!(
        r.read_feature("sessionZones"),
        Some(json!({ "pre": [[100, 150], [200, 300]], "post": [], "extended": [[1, 2]] }))
    );
    r.apply_feature("sessionZones", &Value::Null);
    assert_eq!(r.read_feature("sessionZones"), Some(Value::Null));
}

#[test]
fn the_toggle_features_round_trip_and_the_dom_only_ones_tell_the_host() {
    let rig = rig();
    for key in ["indicatorValues", "indicatorTitles"] {
        assert_eq!(rig.r.read_feature(key), Some(json!(true)));
        rig.r.apply_feature(key, &json!(false));
        assert_eq!(rig.r.read_feature(key), Some(json!(false)));
        rig.r.apply_feature(key, &json!(true));
        assert_eq!(rig.r.read_feature(key), Some(json!(true)));
    }
    rig.r.apply_feature("settings", &json!(true));
    assert_eq!(rig.r.read_feature("settings"), Some(json!(true)));
    let changed = rig.host.feature_changes();
    assert!(
        changed
            .iter()
            .any(|(k, v)| k == "settings" && v == &json!(true))
    );
    assert!(changed.iter().any(|(k, _)| k == "indicatorTitles"));
    rig.r.apply_feature("keyboard", &json!(false));
    assert_eq!(rig.host.keyboard_enabled(), Some(false));
    assert_eq!(rig.r.read_feature("keyboard"), Some(json!(false)));
    assert_eq!(rig.r.read_feature("dialogHost"), None);
}

#[test]
fn an_unknown_feature_reads_undefined_and_applies_nothing() {
    let r = bare();
    assert_eq!(r.read_feature("nope"), None);
    r.apply_feature("nope", &json!(1));
}

#[test]
fn the_crosshair_override_reads_back_sanitized_clears_with_null_and_never_reaches_the_config() {
    let r = bare();
    assert_eq!(r.read_feature("crosshairOverride"), Some(Value::Null));
    let before = r.get_config().unwrap();
    r.apply_feature(
        "crosshairOverride",
        &json!({ "horizontal": false, "style": "solid", "bogus": true }),
    );
    assert_eq!(
        r.read_feature("crosshairOverride"),
        Some(json!({ "horizontal": false, "style": "solid" }))
    );
    r.apply_feature(
        "crosshairOverride",
        &json!({ "horizontal": false, "color": "#ff0000", "style": "solid", "width": 2, "opacity": 1 }),
    );
    assert_eq!(r.get_config().unwrap(), before);
    r.apply_feature("crosshairOverride", &Value::Null);
    assert_eq!(r.read_feature("crosshairOverride"), Some(Value::Null));
}

#[test]
fn free_pan_round_trips_through_features_and_the_config_keeping_custom_triggers() {
    let r = bare();
    r.apply_feature("freePan", &json!("alt"));
    assert_eq!(r.read_feature("freePan"), Some(json!("alt")));
    assert_eq!(r.get_config().unwrap()["priceScale"]["freePan"], true);
    r.apply_config(&json!({ "priceScale": { "freePan": false } }));
    assert_eq!(r.read_feature("freePan"), Some(json!(false)));
    assert!(r.free_pan_trigger().is_none());
    r.apply_config(&json!({ "priceScale": { "freePan": true } }));
    assert_eq!(r.read_feature("freePan"), Some(json!("alt")));
    r.apply_feature("freePan", &json!(true));
    assert_eq!(r.read_feature("freePan"), Some(json!(true)));
    assert!(matches!(
        r.free_pan_trigger(),
        Some(crate::options::FreePanTrigger::Drag)
    ));
    r.set_free_pan(crate::options::FreePanTrigger::Mod);
    assert_eq!(r.read_feature("freePan"), Some(json!("mod")));
}

// ── scale price chart only ──

fn price_only_rig() -> Rig {
    let rig = rig_with(false, 864.0, 422.0);
    let times: Vec<i64> = (0..50).map(|i| T0 + i * STEP).collect();
    let bars: Vec<Ohlcv> = times
        .iter()
        .map(|t| Ohlcv::new(*t, 105.0, 110.0, 100.0, 105.0, None))
        .collect();
    rig.set_bars(&bars, false);
    rig.r.ensure_pane(&Pane {
        id: "study".into(),
        kind: PaneKind::Study,
        order: 1,
        height_weight: Some(1.0),
        title: None,
    });
    rig.mount_model(&line_model(
        "zones",
        "price",
        true,
        &times,
        &vec![Some(200.0); 50],
    ));
    let mut osc = line_model("osc", "study", false, &times, &vec![Some(0.0); 50]);
    if let SeriesData::LineLike { points, .. } = &mut osc.series[0].data {
        points[25].value = Some(1000.0);
    }
    rig.mount_model(&osc);
    rig
}

use crate::model::SeriesData;

fn windows(rig: &Rig) -> ((f64, f64), (f64, f64)) {
    rig.r.with(|st| {
        st.compute_scales();
        let p = st.scene.panes.get("price").unwrap().scale_target;
        let s = st.scene.panes.get("study").unwrap().scale_target;
        ((p.min, p.max), (s.min, s.max))
    })
}

#[test]
fn scale_price_only_is_off_by_default_and_when_on_fits_the_bars_alone() {
    let rig = price_only_rig();
    assert_eq!(rig.feature("scalePriceOnly"), json!(false));
    let ((_, max), _) = windows(&rig);
    assert!(max >= 200.0);
    rig.r.apply_feature("scalePriceOnly", &json!(true));
    let ((min, max), (smin, smax)) = windows(&rig);
    assert!(min < 100.0 && min > 90.0, "{min}");
    assert!(max > 110.0 && max < 120.0, "{max}");
    // study panes keep fitting their own plots
    assert!(smin <= 0.0 && smax >= 1000.0);
}

#[test]
fn with_the_price_series_hidden_there_are_no_bars_to_fit_so_the_indicators_keep_the_window() {
    let rig = price_only_rig();
    rig.r.apply_feature("scalePriceOnly", &json!(true));
    rig.r.apply_feature("candleVisible", &json!(false));
    let ((_, max), _) = windows(&rig);
    assert!(max >= 200.0);
}

#[test]
fn scale_price_only_is_saved_with_the_chart_settings_and_restored_from_them() {
    let rig = price_only_rig();
    rig.r.apply_feature("scalePriceOnly", &json!(true));
    let saved = rig.r.get_config().unwrap();
    assert_eq!(saved["priceScale"]["priceOnly"], true);
    let restored = bare();
    restored.apply_config(&saved);
    assert_eq!(restored.read_feature("scalePriceOnly"), Some(json!(true)));
}

// ── series-past-last-bar (the renderer part) ──

#[test]
fn the_renderer_scales_the_price_pane_over_the_visible_right_whitespace() {
    let rig = data_rig(800.0, 200.0);
    rig.set_bars(
        &(0..3)
            .map(|i| Ohlcv::new(T0 + i * 3_600_000, 10.0, 11.0, 9.0, 10.0, None))
            .collect::<Vec<_>>(),
        false,
    );
    let times: Vec<i64> = (0..5).map(|i| T0 + i * 3_600_000).collect();
    rig.mount_model(&line_model(
        "m",
        "price",
        true,
        &times,
        &[Some(10.0), Some(11.0), Some(12.0), Some(13.0), Some(500.0)],
    ));
    assert!(rig.r.coordinates().visible_logical_range().to > 4.0);
    rig.r.with(|st| st.compute_scales());
    let max = rig
        .r
        .with(|st| st.scene.panes.get("price").unwrap().scale_target.max);
    assert!(max > 500.0, "{max}");
}

// ── keyboard ──

#[test]
fn the_keyboard_maps_keys_to_chart_navigation_and_stands_down_on_chords() {
    let rig = data_rig(800.0, 200.0);
    rig.bars(100);
    assert!(rig.r.handle_key(&KeyInput {
        key: "ArrowLeft",
        ..Default::default()
    }));
    let hovered = rig.r.with(|st| st.hover_logical);
    assert!(hovered.is_some());
    assert_eq!(rig.host.announcements().len(), 1);
    assert!(
        rig.host.announcements()[0].contains("Open "),
        "{:?}",
        rig.host.announcements()
    );
    let before = rig.r.with(|st| st.hover_logical).unwrap();
    rig.r.handle_key(&KeyInput {
        key: "ArrowLeft",
        ..Default::default()
    });
    assert_eq!(rig.r.with(|st| st.hover_logical), Some(before - 1));
    assert!(!rig.r.handle_key(&KeyInput {
        key: "ArrowLeft",
        ctrl: true,
        ..Default::default()
    }));
    assert!(!rig.r.handle_key(&KeyInput {
        key: "a",
        ..Default::default()
    }));
    assert!(rig.r.handle_key(&KeyInput {
        key: "Escape",
        ..Default::default()
    }));
    assert_eq!(rig.r.with(|st| st.hover_logical), None);
    // Home / End jump to the edges and announce
    rig.r.handle_key(&KeyInput {
        key: "Home",
        ..Default::default()
    });
    assert_eq!(rig.r.with(|st| st.hover_logical), Some(0));
    rig.r.handle_key(&KeyInput {
        key: "End",
        ..Default::default()
    });
    assert_eq!(rig.r.with(|st| st.hover_logical), Some(99));
    // plus zooms in, shift+arrow pans by a chunk
    let bs = rig.viewport().0;
    rig.r.handle_key(&KeyInput {
        key: "+",
        ..Default::default()
    });
    rig.host.run_until_idle();
    assert!(rig.viewport().0 > bs);
    let ro = rig.viewport().1;
    rig.r.handle_key(&KeyInput {
        key: "ArrowLeft",
        shift: true,
        ..Default::default()
    });
    assert_eq!(rig.viewport().1, ro - 10.0);
    // 0 resets the view
    rig.r.handle_key(&KeyInput {
        key: "0",
        ..Default::default()
    });
    assert_eq!(rig.viewport().1, 10.0);
}

#[test]
fn stepping_the_focused_bar_scrolls_it_into_view_and_draws_the_crosshair() {
    let rig = data_rig(800.0, 200.0);
    rig.bars(300);
    rig.r.focus_bar(5);
    assert!(rig.r.with(|st| st.scene.crosshair).is_some());
    let range = rig.r.get_visible_range().unwrap();
    assert!(range.from <= T0 + 5 * STEP);
}

// ── animation settling ──

fn settle_rig() -> Rig {
    let rig = data_rig(800.0, 200.0);
    let bars: Vec<Ohlcv> = (0..100)
        .map(|i| Ohlcv::new((i + 1) * 1000, 1.0, 2.0, 0.5, 1.5, Some(1.0)))
        .collect();
    rig.set_bars(&bars, false);
    rig.host.run_until_idle();
    rig
}

fn ticks_to_settle(rig: &Rig) -> usize {
    let mut n = 0;
    while rig.r.with(|st| st.anim_tick(16.0)) {
        n += 1;
        assert!(n < 300, "never settled");
    }
    n + 1
}

#[test]
fn a_zoom_target_below_the_reachable_minimum_snaps_to_the_bound() {
    let rig = settle_rig();
    rig.set_viewport(10.0, 5.0);
    rig.r.with(|st| st.target_bar_spacing = 2.0);
    assert!(ticks_to_settle(&rig) < 10);
    let bs = rig.viewport().0;
    near(bs, 800.0 / 110.0, 1e-3);
    near(rig.r.with(|st| st.target_bar_spacing), bs, 1e-6);
}

#[test]
fn a_zoom_target_above_the_reachable_maximum_snaps_to_the_bound() {
    let rig = settle_rig();
    rig.set_viewport(100.0, 5.0);
    rig.r.with(|st| st.target_bar_spacing = 900.0);
    assert!(ticks_to_settle(&rig) < 10);
    near(rig.viewport().0, 400.0, 1e-3);
}

#[test]
fn an_in_bounds_zoom_ease_still_glides_over_several_frames_and_lands_on_its_target() {
    let rig = settle_rig();
    rig.set_viewport(10.0, 5.0);
    rig.r.with(|st| st.target_bar_spacing = 20.0);
    let frames = ticks_to_settle(&rig);
    assert!(frames > 3 && frames < 200, "{frames}");
    near(rig.viewport().0, 20.0, 1e-2);
}

#[test]
fn an_unreachable_scroll_target_settles_on_the_pan_bound() {
    let rig = settle_rig();
    rig.set_viewport(10.0, 5.0);
    rig.r.with(|st| {
        st.target_bar_spacing = 10.0;
        st.scroll_target_ro = Some(9999.0);
    });
    assert!(ticks_to_settle(&rig) < 200);
    assert_eq!(rig.r.with(|st| st.scroll_target_ro), None);
    near(rig.viewport().1, 79.0, 1e-2);
}

#[test]
fn a_settled_loop_keeps_reporting_settled_and_never_re_arms() {
    let rig = settle_rig();
    rig.set_viewport(10.0, 5.0);
    rig.r.with(|st| st.target_bar_spacing = 2.0);
    ticks_to_settle(&rig);
    let seen: Rc<RefCell<usize>> = Rc::default();
    let s = seen.clone();
    let _u = rig
        .r
        .on_viewport_change(Box::new(move |_| *s.borrow_mut() += 1));
    for _ in 0..50 {
        rig.r.with(|st| st.anim_tick(16.0));
    }
    assert_eq!(*seen.borrow(), 50);
    assert!(!rig.r.with(|st| st.anim_tick(16.0)));
}

#[test]
fn a_resize_paints_synchronously_whether_or_not_the_animator_owns_the_frame() {
    for animating in [true, false] {
        let rig = data_rig(800.0, 200.0);
        rig.bars(100);
        if animating {
            rig.r.with(|st| st.anim.start());
        }
        rig.host.set_size(500.0, 300.0);
        rig.r.resize();
        let data = rig.host.canvas(&CanvasSlot::Data).unwrap();
        assert!(
            !data.is_empty(),
            "animating={animating}: the cleared canvas was left blank"
        );
        let c = rig.host.test_canvas(&CanvasSlot::Data).unwrap();
        assert_eq!(c.backing_size(), (500.0 - 0.0, 300.0));
    }
}

#[test]
fn resizing_to_an_unlaid_out_size_changes_nothing() {
    let rig = data_rig(800.0, 200.0);
    rig.bars(100);
    let sized = rig
        .host
        .test_canvas(&CanvasSlot::Data)
        .unwrap()
        .resize_count();
    rig.host.set_size(0.0, 0.0);
    rig.r.resize();
    assert_eq!(
        rig.host
            .test_canvas(&CanvasSlot::Data)
            .unwrap()
            .resize_count(),
        sized
    );
}

// ── forming-bar glide ──

fn live_bar(time: i64, close: f64) -> Ohlcv {
    Ohlcv::new(
        time,
        100.0,
        close.max(100.0),
        close.min(100.0),
        close,
        Some(1.0),
    )
}

fn live_rig(live_bar_ms: f64) -> Rig {
    let host = TestHost::new(864.0, 222.0, 1.0);
    let mut opts = display(false);
    opts.anim_live_bar = live_bar_ms;
    let r = NativeRenderer::new(host.clone(), Some(opts));
    r.mount(&dark_theme());
    let rig = Rig { host, r };
    rig.set_bars(&[live_bar(1000, 100.0), live_bar(2000, 101.0)], false);
    rig.host.run_until_idle();
    rig
}

fn live(rig: &Rig) -> (i64, f64, f64) {
    rig.r
        .with(|st| (st.live_ease_time, st.live_ease_close, st.live_ease_high))
}

#[test]
fn with_the_live_bar_glide_off_a_tick_snaps_and_never_starts_the_animator() {
    let rig = live_rig(0.0);
    rig.r.update_bar(&live_bar(2000, 105.0));
    rig.r.update_bar(&live_bar(2000, 110.0));
    assert_eq!(live(&rig).1, 110.0);
    assert_eq!(live(&rig).2, 110.0);
    assert!(!rig.r.with(|st| st.ease_live_bar(16.0)));
    assert!(!rig.r.with(|st| st.anim.active()));
    assert_eq!(rig.r.read_feature("animLiveBar"), Some(json!(0)));
}

#[test]
fn with_the_glide_on_the_first_tick_snaps_and_later_ticks_glide_and_settle() {
    let rig = live_rig(90.0);
    rig.r.update_bar(&live_bar(2000, 105.0));
    assert_eq!(live(&rig).1, 105.0);
    assert!(!rig.r.with(|st| st.anim.active()));
    rig.r.update_bar(&live_bar(2000, 110.0));
    assert!(rig.r.with(|st| st.anim.active()));
    assert_eq!(live(&rig).1, 105.0);
    assert!(rig.r.with(|st| st.ease_live_bar(16.0)));
    let c = live(&rig).1;
    assert!(c > 105.0 && c < 110.0);
    let mut frames = 1;
    while rig.r.with(|st| st.ease_live_bar(16.0)) && frames < 200 {
        frames += 1;
    }
    assert!(frames < 200);
    assert_eq!(live(&rig).1, 110.0);
    assert_eq!(live(&rig).2, 110.0);
}

#[test]
fn a_new_bar_always_snaps_the_ease_never_runs_across_bars() {
    let rig = live_rig(90.0);
    rig.r.update_bar(&live_bar(2000, 105.0));
    rig.r.update_bar(&live_bar(3000, 130.0));
    assert_eq!(live(&rig).0, 3000);
    assert_eq!(live(&rig).1, 130.0);
    assert!(!rig.r.with(|st| st.anim.active()));
}

#[test]
fn a_longer_time_constant_glides_slower_and_switching_off_mid_glide_snaps() {
    let fast = live_rig(50.0);
    let slow = live_rig(400.0);
    for rig in [&fast, &slow] {
        rig.r.update_bar(&live_bar(2000, 105.0));
        rig.r.update_bar(&live_bar(2000, 110.0));
        rig.r.with(|st| st.ease_live_bar(16.0));
    }
    assert!(live(&fast).1 > live(&slow).1);
    slow.r.apply_feature("animLiveBar", &json!(false));
    assert_eq!(slow.r.read_feature("animLiveBar"), Some(json!(0)));
    assert!(!slow.r.with(|st| st.ease_live_bar(16.0)));
    assert_eq!(live(&slow).1, 110.0);
}

#[test]
fn the_config_carries_an_on_off_switch_for_the_glide_that_restores_the_configured_duration() {
    let rig = live_rig(250.0);
    assert_eq!(
        rig.r.get_config().unwrap()["priceScale"]["animateLastPrice"],
        true
    );
    rig.r
        .apply_config(&json!({ "priceScale": { "animateLastPrice": false } }));
    assert_eq!(rig.r.read_feature("animLiveBar"), Some(json!(0)));
    rig.r
        .apply_config(&json!({ "priceScale": { "animateLastPrice": true } }));
    assert_eq!(rig.r.read_feature("animLiveBar"), Some(json!(250)));

    let off = live_rig(0.0);
    assert_eq!(
        off.r.get_config().unwrap()["priceScale"]["animateLastPrice"],
        false
    );
    off.r
        .apply_config(&json!({ "priceScale": { "animateLastPrice": true } }));
    assert_eq!(off.r.read_feature("animLiveBar"), Some(json!(90)));
    off.r
        .apply_config(&json!({ "priceScale": { "animateLastPrice": "yes" } }));
    assert_eq!(off.r.read_feature("animLiveBar"), Some(json!(90)));
}

#[test]
fn the_live_bar_feature_accepts_true_and_clamps_numbers() {
    let rig = live_rig(0.0);
    rig.r.apply_feature("animLiveBar", &json!(true));
    assert_eq!(rig.r.read_feature("animLiveBar"), Some(json!(90)));
    rig.r.apply_feature("animLiveBar", &json!(99_999));
    assert_eq!(rig.r.read_feature("animLiveBar"), Some(json!(1000)));
}

#[test]
fn the_eased_forming_bar_is_painted_and_the_true_bar_restored_for_readers() {
    let rig = live_rig(90.0);
    rig.r.update_bar(&live_bar(2000, 105.0));
    rig.r.update_bar(&live_bar(2000, 110.0));
    rig.r.with(|st| {
        st.ease_live_bar(16.0);
        st.paint_data();
    });
    rig.r.with(|st| {
        assert_eq!(st.scene.bars.last().unwrap().close, 110.0);
        assert_eq!(st.bars.last().unwrap().close, 110.0);
    });
}

// ── data window readout ──

fn closes(values: &[f64], volume: bool) -> Vec<Ohlcv> {
    values
        .iter()
        .enumerate()
        .map(|(i, c)| {
            Ohlcv::new(
                1_700_000_000_000 + i as i64 * 3_600_000,
                c - 1.0,
                c + 2.0,
                c - 2.0,
                *c,
                volume.then_some(1234.0),
            )
        })
        .collect()
}

fn dw_rig(bars: &[Ohlcv]) -> Rig {
    let rig = data_rig(800.0, 200.0);
    rig.set_bars(bars, false);
    rig
}

#[test]
fn the_readout_is_empty_until_there_are_bars_and_reads_the_latest_bar_off_the_plot() {
    let rig = data_rig(800.0, 200.0);
    assert_eq!(
        rig.r.get_data_window_readout(),
        Some(DataWindowReadout {
            date: String::new(),
            time: String::new(),
            ohlc: None,
            groups: vec![]
        })
    );
    let rig = dw_rig(&closes(&[100.0, 101.0, 105.0], false));
    let ro = rig.r.get_data_window_readout().unwrap();
    assert_eq!(ro.date.len(), 10);
    assert_eq!(&ro.date[4..5], "-");
    assert_eq!(ro.time.len(), 5);
    let ohlc = ro.ohlc.unwrap();
    let n = |s: &str| s.parse::<f64>().unwrap();
    near(n(&ohlc.c), 105.0, 1e-9);
    near(n(&ohlc.o), 104.0, 1e-9);
    near(n(&ohlc.h), 107.0, 1e-9);
    near(n(&ohlc.l), 103.0, 1e-9);
    assert!(ohlc.up);
    assert_eq!(ohlc.vol, None);
}

#[test]
fn the_readout_reads_the_hovered_bar_and_a_down_bar_reads_down() {
    let mut bars = closes(&[100.0, 101.0], false);
    bars.push(Ohlcv::new(
        1_700_000_000_000 + 2 * 3_600_000,
        110.0,
        111.0,
        98.0,
        99.0,
        None,
    ));
    let rig = dw_rig(&bars);
    rig.r.with(|st| st.hover_logical = Some(0));
    near(
        rig.r
            .get_data_window_readout()
            .unwrap()
            .ohlc
            .unwrap()
            .c
            .parse()
            .unwrap(),
        100.0,
        1e-9,
    );
    rig.r.with(|st| st.hover_logical = Some(2));
    let hovered = rig.r.get_data_window_readout().unwrap().ohlc.unwrap();
    near(hovered.c.parse().unwrap(), 99.0, 1e-9);
    assert!(!hovered.up);
}

#[test]
fn the_readout_carries_volume_only_when_the_bar_has_it() {
    let rig = dw_rig(&closes(&[100.0, 101.0], true));
    assert_eq!(
        rig.r
            .get_data_window_readout()
            .unwrap()
            .ohlc
            .unwrap()
            .vol
            .as_deref(),
        Some("1.23K")
    );
}

fn volume_model() -> IndicatorModel {
    let mut m = line_model("vol1", "price", true, &[], &[]);
    m.title = "Volume".into();
    m.series.clear();
    m.native = Some(NativeTag {
        indicator_type: "volume".into(),
        channel: None,
    });
    m
}

fn volume_rig(with_volume: bool) -> Rig {
    let rig = dw_rig(&closes(&[100.0, 101.0, 105.0], with_volume));
    rig.mount_model(&volume_model());
    rig.r.set_native_data(
        "volume",
        &json!({ "upColor": "#11aa11", "downColor": "#aa1111", "heightFrac": 0.2 }),
    );
    rig
}

#[test]
fn the_volume_indicator_reads_the_bar_volume_tinted_with_its_own_colors() {
    let rig = volume_rig(true);
    let ro = rig.r.get_data_window_readout().unwrap();
    let group = ro.groups.iter().find(|g| g.name == "Volume").unwrap();
    assert_eq!(group.rows.len(), 1);
    assert_eq!(group.rows[0].label, "Volume");
    assert_eq!(group.rows[0].value, "1.23K");
    assert_eq!(group.rows[0].color, "#11aa11");
    // a down bar uses the down color
    let mut bars = closes(&[100.0, 101.0], true);
    bars.push(Ohlcv::new(
        1_700_000_000_000 + 2 * 3_600_000,
        110.0,
        111.0,
        98.0,
        99.0,
        Some(1234.0),
    ));
    rig.set_bars(&bars, false);
    let ro = rig.r.get_data_window_readout().unwrap();
    assert_eq!(ro.groups[0].rows[0].color, "#aa1111");
}

#[test]
fn the_volume_readout_is_absent_without_bar_volume_and_while_hidden() {
    assert!(
        volume_rig(false)
            .r
            .get_data_window_readout()
            .unwrap()
            .groups
            .is_empty()
    );
    let hidden = volume_rig(true);
    hidden
        .r
        .set_indicator_visible(&IndicatorRenderHandle { id: "vol1".into() }, false);
    assert!(
        hidden
            .r
            .get_data_window_readout()
            .unwrap()
            .groups
            .is_empty()
    );
}

#[test]
fn an_indicator_group_lists_a_row_per_plot_formatted_on_its_pane() {
    let rig = dw_rig(&closes(&[100.0, 101.0, 105.0], false));
    let times: Vec<i64> = (0..3).map(|i| 1_700_000_000_000 + i * 3_600_000).collect();
    rig.mount_model(&line_model(
        "ma",
        "price",
        true,
        &times,
        &[Some(99.5), None, Some(102.5)],
    ));
    let ro = rig.r.get_data_window_readout().unwrap();
    let g = ro.groups.iter().find(|g| g.name == "ma").unwrap();
    assert_eq!(g.rows.len(), 1);
    assert_eq!(g.rows[0].color, "#2962ff");
    near(g.rows[0].value.parse().unwrap(), 102.5, 1e-9);
    // a gap yields no row
    rig.r.with(|st| st.hover_logical = Some(1));
    assert!(rig.r.get_data_window_readout().unwrap().groups.is_empty());
}

#[test]
fn the_legend_values_follow_the_read_bar_and_are_pushed_to_the_host_after_a_frame() {
    let rig = data_rig(800.0, 200.0);
    let bars = sample_bars(3, T0, STEP);
    rig.set_bars(&bars, false);
    let times: Vec<i64> = bars.iter().map(|b| b.time).collect();
    rig.mount_model(&line_model(
        "ma",
        "price",
        true,
        &times,
        &[Some(1.0), Some(2.0), Some(3.0)],
    ));
    rig.host.run_until_idle();
    let values = rig.host.legend_plot_values();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].0, "ma");
    assert_eq!(values[0].1[0].value, "3.00");
    assert_eq!(rig.r.legend_values(), values);
}

#[test]
fn the_visible_range_option_defaults_do_not_leak_between_calls() {
    let rig = data_rig(800.0, 200.0);
    let bars = rig.bars(50);
    rig.r.set_visible_range(
        VisibleRange {
            from: bars[10].time,
            to: bars[19].time,
        },
        crate::ports::chart_renderer::VisibleRangeOptions::default(),
    );
    let range = rig.r.get_visible_range().unwrap();
    assert!(range.from <= bars[10].time + STEP && range.to >= bars[19].time - STEP);
}
