//! Ports of the remaining renderer-level cases: the data window on a daily chart
//! (`coarse-bar-dates`), `previousClose` and the close-style config
//! (`native-close-styles`), `tradeMarkers` (`trade-markers`), the shade-right
//! veil (`shade-right-element`), `drawingsInteractive`
//! (`input-drawings-interactive`) and `priceAxisTicks` (`price-axis-ticks`).

use serde_json::{Value, json};

use super::*;
use crate::options::{PriceAxisTick, PriceAxisTicksFn};

fn bare() -> Rc<NativeRenderer> {
    NativeRenderer::new(TestHost::new(800.0, 400.0, 1.0), None)
}

const HOUR: i64 = 3_600_000;

fn utc(y: i32, m: u32, d: u32) -> i64 {
    // days from civil, 1970-01-01 epoch
    let (y, m) = if m <= 2 {
        (i64::from(y) - 1, i64::from(m) + 12)
    } else {
        (i64::from(y), i64::from(m))
    };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (m - 3) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) * 24 * HOUR
}

fn readout(times: &[i64], zone: &str) -> (String, String) {
    let rig = data_rig(800.0, 200.0);
    rig.r.apply_feature("timezone", &json!(zone));
    let bars: Vec<Ohlcv> = times
        .iter()
        .map(|t| Ohlcv::new(*t, 1.0, 2.0, 0.5, 1.5, None))
        .collect();
    rig.set_bars(&bars, false);
    let ro = rig.r.get_data_window_readout().unwrap();
    (ro.date, ro.time)
}

#[test]
fn the_data_window_of_a_daily_chart_reads_the_utc_date_in_every_zone() {
    let daily: Vec<i64> = [19, 20, 21, 22, 23, 26, 27]
        .iter()
        .map(|d| utc(2026, 10, *d))
        .collect();
    for zone in ["America/New_York", "Asia/Tokyo", "UTC", "Etc/UTC"] {
        assert_eq!(readout(&daily, zone).0, "2026-10-27", "{zone}");
    }
}

#[test]
fn the_data_window_of_an_intraday_chart_keeps_the_display_zone() {
    let hourly: Vec<i64> = (0..48).map(|i| utc(2026, 10, 22) + i * HOUR).collect();
    // The built-in Intl carries no zone data (the browser's does): a zone it does
    // not know reads as UTC, so the intraday readout keeps the instant's wall clock.
    assert_eq!(
        readout(&hourly, "UTC"),
        ("2026-10-23".to_string(), "23:00".to_string())
    );
    assert_eq!(readout(&hourly, "America/New_York").0, "2026-10-23");
}

#[test]
fn previous_close_reads_the_close_before_the_bar_the_readouts_show() {
    let rig = data_rig(800.0, 200.0);
    let closes = [10.0, 12.0, 13.0, 11.0, 12.0];
    let bars: Vec<Ohlcv> = closes
        .iter()
        .enumerate()
        .map(|(i, c)| Ohlcv::new(i as i64 * 60_000, *c, *c + 1.0, *c - 1.0, *c, None))
        .collect();
    rig.set_bars(&bars, false);
    assert_eq!(rig.r.read_feature("previousClose"), Some(json!(11)));
    rig.r.with(|st| st.hover_logical = Some(2));
    assert_eq!(rig.r.read_feature("previousClose"), Some(json!(12)));
    rig.r.with(|st| st.hover_logical = Some(0));
    assert_eq!(rig.r.read_feature("previousClose"), Some(Value::Null));
}

#[test]
fn the_close_styles_persist_and_the_columns_default_to_half_opacity_washes() {
    for style in ["linemarkers", "stepline", "columns"] {
        let r = bare();
        r.apply_config(&json!({ "series": { "style": style } }));
        let cfg = r.get_config().unwrap();
        assert_eq!(cfg["series"]["style"], style);
        let restored = bare();
        restored.apply_config(&serde_json::from_str::<Value>(&cfg.to_string()).unwrap());
        assert_eq!(restored.read_feature("priceStyle"), Some(json!(style)));
    }
    let r = bare();
    let cfg = r.get_config().unwrap();
    assert_eq!(cfg["columns"]["upColor"], "rgba(8,153,129,0.5)");
    assert_eq!(cfg["columns"]["downColor"], "rgba(242,54,69,0.5)");
    r.apply_config(&json!({ "columns": { "upColor": "#010203", "downColor": 7 } }));
    let after = r.get_config().unwrap();
    assert_eq!(after["columns"]["upColor"], "#010203");
    assert_eq!(after["columns"]["downColor"], cfg["columns"]["downColor"]);
}

#[test]
fn the_trade_markers_feature_merges_partials_and_reads_back_a_copy_and_persists_in_the_config() {
    let r = bare();
    assert!(r.features().iter().any(|f| f == "tradeMarkers"));
    r.apply_feature(
        "tradeMarkers",
        &json!({ "labels": false, "colors": { "long": "#000001" } }),
    );
    let state = r.read_feature("tradeMarkers").unwrap();
    assert_eq!(state["labels"], false);
    assert_eq!(state["visible"], true);
    assert_eq!(state["colors"]["long"], "#000001");
    assert_eq!(state["colors"]["exit"], "#d500f9");
    let a = bare();
    a.apply_feature(
        "tradeMarkers",
        &json!({ "qty": false, "colors": { "short": "#010101" } }),
    );
    let cfg = a.get_config().unwrap();
    assert_eq!(
        cfg["trades"],
        json!({
            "visible": true, "labels": true, "qty": false,
            "longColor": "#2962ff", "shortColor": "#010101", "exitColor": "#d500f9"
        })
    );
    let b = bare();
    b.apply_config(&cfg);
    assert_eq!(
        b.read_feature("tradeMarkers"),
        Some(json!({
            "visible": true, "labels": true, "qty": false,
            "colors": { "long": "#2962ff", "short": "#010101", "exit": "#d500f9" }
        }))
    );
}

#[test]
fn the_shade_right_veil_is_reported_to_the_host_while_it_paints_and_hidden_otherwise() {
    let rig = data_rig(380.0, 280.0);
    rig.bars(60);
    let bar_x = rig.r.coordinates().logical_to_x(20.0);
    rig.r.pointer_move(bar_x, 120.0);
    rig.host.run_until_idle();
    assert_eq!(
        rig.host.shade_right_area(),
        None,
        "no override yet: nothing veiled"
    );
    rig.r.apply_feature(
        "crosshairOverride",
        &json!({ "horizontal": false, "shadeRight": { "color": "rgba(0,0,0,0.3)" } }),
    );
    rig.host.run_until_idle();
    let (x, w, h) = rig.host.shade_right_area().unwrap();
    assert_eq!(h, 280.0);
    near(x + w, 380.0, 1e-9);
    // the same bar at another level: the same veil
    rig.r.pointer_move(bar_x + 1.0, 200.0);
    rig.host.run_until_idle();
    assert_eq!(rig.host.shade_right_area(), Some((x, w, h)));
    // another bar: it follows
    rig.r.pointer_move(bar_x + 60.0, 120.0);
    rig.host.run_until_idle();
    assert!(rig.host.shade_right_area().unwrap().0 > x);
    // the pointer left: no veil
    rig.r.pointer_leave();
    rig.host.run_until_idle();
    assert_eq!(rig.host.shade_right_area(), None);
    // a synced ghost veils from its bar
    rig.r.set_external_crosshair(Some(T0 + 40 * STEP), None);
    rig.host.run_until_idle();
    let left = rig.host.shade_right_area().unwrap().0;
    near(left, rig.r.coordinates().logical_to_x(40.5).round(), 1.0);
}

#[test]
fn the_drawings_interactive_feature_is_on_by_default_toggles_null_restores_and_skips_the_config() {
    let r = bare();
    assert!(r.features().iter().any(|f| f == "drawingsInteractive"));
    assert_eq!(r.read_feature("drawingsInteractive"), Some(json!(true)));
    let before = r.get_config().unwrap();
    r.apply_feature("drawingsInteractive", &json!(false));
    assert_eq!(r.read_feature("drawingsInteractive"), Some(json!(false)));
    assert!(!r.drawings_interactive());
    assert_eq!(r.get_config().unwrap(), before);
    r.apply_feature("drawingsInteractive", &Value::Null);
    assert_eq!(r.read_feature("drawingsInteractive"), Some(json!(true)));
}

#[test]
fn turning_the_drawings_off_drops_the_hover_through_the_hook() {
    use super::super::UserDrawingsHook;
    use std::cell::Cell;
    struct Hook(Cell<usize>);
    impl UserDrawingsHook for Hook {
        fn prepare_slices(
            &self,
            _: &[String],
            _: &super::super::FrameView,
        ) -> std::collections::HashMap<String, Vec<crate::render::core::scene_graph::DrawingSlice>>
        {
            Default::default()
        }
        fn render(&self, _: &super::super::FrameView) {}
        fn clear_hover(&self) {
            self.0.set(self.0.get() + 1);
        }
        fn set_history_chords(&self, _: bool) {}
    }
    let rig = rig();
    let hook = Rc::new(Hook(Cell::new(0)));
    struct Port;
    impl crate::ports::chart_renderer::DrawingsRendererPort for Port {
        fn set_toolbar(&self, _: &Value) {}
        fn show_toolbar(&self, _: bool) {}
        fn sync_drawings(&self, _: &[crate::drawings::SerializedDrawing]) {}
        fn set_active_tool(
            &self,
            _: Option<&str>,
            _: Option<&crate::drawings::style::DrawingStyle>,
            _: Option<&crate::drawings::DrawingToolDefaults>,
        ) {
        }
        fn set_selection(&self, _: &[String]) {}
        fn open_settings(&self, _: &str) {}
        fn on_drawing_intent(
            &self,
            _: crate::ports::chart_renderer::Listener<crate::ports::chart_renderer::DrawingIntent>,
        ) -> crate::runtime::Unsubscribe {
            crate::runtime::noop_unsubscribe()
        }
    }
    rig.r.set_user_drawings(Some((hook.clone(), Rc::new(Port))));
    rig.r.apply_feature("drawingsInteractive", &json!(false));
    assert_eq!(hook.0.get(), 1);
    rig.r.apply_feature("historyChords", &json!(false));
    assert_eq!(rig.r.read_feature("historyChords"), Some(json!(false)));
}

#[test]
fn the_price_axis_ticks_hook_is_set_cleared_and_reported() {
    let r = bare();
    assert_eq!(r.read_feature("priceAxisTicks"), Some(Value::Null));
    let hook: PriceAxisTicksFn = Rc::new(|_| {
        Some(vec![PriceAxisTick {
            price: 1.0,
            label: "one".into(),
            major: None,
        }])
    });
    r.set_price_axis_ticks(Some(hook));
    assert_eq!(r.read_feature("priceAxisTicks"), Some(json!(true)));
    r.apply_feature("priceAxisTicks", &Value::Null);
    assert_eq!(r.read_feature("priceAxisTicks"), Some(Value::Null));
    r.apply_feature("priceAxisTicks", &json!("not a function"));
    assert_eq!(r.read_feature("priceAxisTicks"), Some(Value::Null));
}

#[test]
fn the_constructor_options_seed_the_hook_and_free_pan() {
    let hook: PriceAxisTicksFn = Rc::new(|_| None);
    let mut opts = display(false);
    opts.price_axis_ticks = Some(hook);
    opts.free_pan = Some(crate::options::FreePanTrigger::Alt);
    let r = NativeRenderer::new(TestHost::new(800.0, 400.0, 1.0), Some(opts));
    assert_eq!(r.read_feature("priceAxisTicks"), Some(json!(true)));
    assert_eq!(r.read_feature("freePan"), Some(json!("alt")));
}

#[test]
fn a_hook_ticks_reach_the_chrome_labels() {
    let rig = data_rig(800.0, 400.0);
    let hook: PriceAxisTicksFn = Rc::new(|ctx| {
        Some(vec![PriceAxisTick {
            price: (ctx.min + ctx.max) / 2.0,
            label: "MIDPOINT".into(),
            major: None,
        }])
    });
    rig.r.set_price_axis_ticks(Some(hook));
    rig.bars(80);
    let chrome = rig.host.canvas(&CanvasSlot::Chrome).unwrap();
    assert!(
        chrome.texts().iter().any(|t| t == "MIDPOINT"),
        "{:?}",
        chrome.texts()
    );
}

#[test]
fn candle_painting_follows_the_candle_colors_and_up_down_features() {
    let rig = data_rig(800.0, 200.0);
    rig.r.apply_feature("upColor", &json!("#00ff00"));
    rig.r.apply_feature("downColor", &json!("#ff0000"));
    rig.bars(60);
    let data = rig.host.canvas(&CanvasSlot::Data).unwrap();
    let styles: Vec<String> = data
        .calls("fillStyle")
        .iter()
        .chain(data.calls("strokeStyle").iter())
        .map(|o| o.to_string())
        .collect();
    assert!(styles.iter().any(|s| s.contains("#00ff00")), "{styles:?}");
}
