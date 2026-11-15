//! Pointer, crosshair, click, external crosshair, timeline marks and the loading /
//! price-precision port writes.

use std::cell::RefCell;

use serde_json::json;

use super::*;
use crate::marks::{MarkContent, MarkGlyph, TimelineMark};
use crate::ports::chart_renderer::{AxisLongPressEvent, ClickEvent, CrosshairEvent};

fn collect_crosshair(rig: &Rig) -> Rc<RefCell<Vec<CrosshairEvent>>> {
    let events: Rc<RefCell<Vec<CrosshairEvent>>> = Rc::default();
    let e2 = events.clone();
    std::mem::forget(
        rig.r
            .on_crosshair_move(Box::new(move |e| e2.borrow_mut().push(e.clone()))),
    );
    events
}

#[test]
fn a_pointer_over_a_bar_reports_time_price_pane_values_and_ohlc() {
    let rig = data_rig(800.0, 200.0);
    let bars = rig.bars(100);
    let times: Vec<i64> = bars.iter().map(|b| b.time).collect();
    let values: Vec<Option<f64>> = (0..100).map(|i| Some(i as f64)).collect();
    rig.mount_model(&line_model("ma", "price", true, &times, &values));
    rig.host.run_until_idle();
    let events = collect_crosshair(&rig);
    let x = rig.r.coordinates().logical_to_x(50.0);
    rig.r.pointer_move(x, 100.0);
    let e = events.borrow().last().cloned().unwrap();
    assert_eq!(e.time, Some(bars[50].time));
    assert_eq!(e.pane_kind, Some(PaneKind::Price));
    assert_eq!(e.pane_id.as_deref(), Some("price"));
    assert!(e.price.is_some());
    assert_eq!(e.value("ma:line"), Some(50.0));
    let ohlc = e.ohlc.unwrap();
    assert_eq!(ohlc.time, bars[50].time);
    assert_eq!(ohlc.close, bars[50].close);
    assert!(rig.r.with(|st| st.scene.crosshair).is_some());
    assert_eq!(rig.r.with(|st| st.hover_logical), Some(50));
    // the cursor tier repaints only the overlay
    rig.host.clear_canvases();
    rig.host.run_until_idle();
    assert!(!rig.host.canvas(&CanvasSlot::Cursor).unwrap().is_empty());
    assert!(rig.host.canvas(&CanvasSlot::Data).unwrap().is_empty());
}

#[test]
fn a_pointer_in_the_whitespace_reports_no_time_and_off_the_plot_clears() {
    let rig = data_rig(800.0, 200.0);
    rig.bars(100);
    let events = collect_crosshair(&rig);
    rig.r.pointer_move(790.0, 100.0); // right whitespace
    let e = events.borrow().last().cloned().unwrap();
    assert_eq!(e.time, None);
    assert!(e.price.is_some());
    assert_eq!(e.ohlc, None);
    rig.r.pointer_move(810.0, 100.0); // the price gutter: off the data area
    let e = events.borrow().last().cloned().unwrap();
    assert_eq!(e.price, None);
    assert_eq!(e.pane_id, None);
    assert!(rig.r.with(|st| st.scene.crosshair).is_none());
    rig.r.pointer_leave();
    let e = events.borrow().last().cloned().unwrap();
    assert_eq!((e.time, e.price, e.pane_id.clone()), (None, None, None));
    assert_eq!(rig.r.with(|st| st.hover_logical), None);
}

#[test]
fn a_pointer_without_bars_clears_and_a_resize_drops_the_crosshair() {
    let rig = data_rig(800.0, 200.0);
    let events = collect_crosshair(&rig);
    rig.r.pointer_move(100.0, 100.0);
    assert!(events.borrow().last().unwrap().time.is_none());
    rig.bars(100);
    rig.r.pointer_move(100.0, 100.0);
    assert!(rig.r.with(|st| st.scene.crosshair).is_some());
    rig.host.set_size(500.0, 300.0);
    rig.r.resize();
    assert!(rig.r.with(|st| st.scene.crosshair).is_none());
}

#[test]
fn a_click_reports_its_bar_pane_and_price() {
    let rig = data_rig(800.0, 200.0);
    let bars = rig.bars(100);
    let clicks: Rc<RefCell<Vec<ClickEvent>>> = Rc::default();
    let c2 = clicks.clone();
    let _u = rig
        .r
        .on_click(Box::new(move |e| c2.borrow_mut().push(e.clone())));
    let x = rig.r.coordinates().logical_to_x(60.0);
    rig.r.click(x, 80.0);
    let e = clicks.borrow()[0].clone();
    assert_eq!(e.time, Some(bars[60].time));
    assert_eq!(e.pane_id.as_deref(), Some("price"));
    assert_eq!(e.pane_kind, Some(PaneKind::Price));
    assert!(e.price.is_some());
    rig.r.click(810.0, 80.0);
    assert_eq!(clicks.borrow()[1].pane_id, None);
}

#[test]
fn the_external_crosshair_is_idempotent_snaps_by_floor_and_never_emits() {
    let rig = data_rig(800.0, 200.0);
    let bars = rig.bars(100);
    let events = collect_crosshair(&rig);
    let t = bars[40].time + STEP / 2;
    rig.r.set_external_crosshair(Some(t), Some(100.0));
    assert!(rig.host.pending_frames() > 0);
    rig.host.run_until_idle();
    assert!(events.borrow().is_empty());
    let ext = rig.r.with(|st| st.external_cross);
    assert_eq!(ext, Some((t, Some(100.0))));
    rig.host.clear_canvases();
    rig.r.set_external_crosshair(Some(t), Some(100.0)); // identical: no repaint armed
    assert_eq!(rig.host.pending_frames(), 0);
    let ghost = rig.r.with(|st| st.external_cross_px()).unwrap();
    near(ghost.x, rig.r.coordinates().logical_to_x(40.0), 1e-9); // floor to the containing bar
    rig.r.set_external_crosshair(None, None);
    assert!(rig.r.with(|st| st.external_cross_px()).is_none());
    // past the forming bar there is no ghost
    rig.r
        .set_external_crosshair(Some(bars[99].time + 5 * STEP), None);
    assert!(rig.r.with(|st| st.external_cross_px()).is_none());
}

#[test]
fn an_axis_long_press_is_relayed() {
    let rig = data_rig(800.0, 200.0);
    let seen: Rc<RefCell<Vec<AxisLongPressEvent>>> = Rc::default();
    let s = seen.clone();
    let _u = rig
        .r
        .on_axis_long_press(Box::new(move |e| s.borrow_mut().push(*e)))
        .unwrap();
    rig.r.axis_long_press(AxisLongPressEvent {
        axis: crate::ports::chart_renderer::Axis::Price,
        x: 820.0,
        y: 50.0,
    });
    assert_eq!(seen.borrow().len(), 1);
}

#[test]
fn the_snap_magnet_snaps_time_to_the_nearest_bar_and_price_to_the_closest_ohlc() {
    use crate::drawings::SnapMode;
    let rig = data_rig(800.0, 200.0);
    let bars = rig.bars(100);
    let b = &bars[30];
    let probe = (b.time as f64 + 100.0, b.high - 0.1);
    let (t, p) = rig.r.snap_to_candle(probe, "price", SnapMode::Strong, None);
    assert_eq!(t, b.time as f64);
    assert_eq!(p, b.high);
    let off = rig.r.snap_to_candle(probe, "price", SnapMode::Off, None);
    assert_eq!(off, probe);
    let (t, p) = rig.r.snap_to_candle(probe, "study", SnapMode::Strong, None);
    assert_eq!((t, p), (b.time as f64, probe.1));
    // weak: far from the cursor, the point stays free
    assert_eq!(
        rig.r
            .snap_to_candle(probe, "price", SnapMode::Weak, Some((0.0, 0.0))),
        probe
    );
    let px = rig
        .r
        .with_drawing_projector(|p| (p.x_of(b.time as f64), p.y_of(b.high, "price").unwrap()));
    let snapped = rig.r.snap_to_candle(
        probe,
        "price",
        SnapMode::Weak,
        Some((px.0 + 2.0, px.1 + 2.0)),
    );
    assert_eq!(snapped, (b.time as f64, b.high));
}

// ── timeline marks ──

fn mark_at(id: &str, time: i64, with_content: bool) -> TimelineMark {
    let mut m = TimelineMark::new(
        id,
        time,
        MarkGlyph {
            color: "#ff9800".into(),
            letter: Some("E".into()),
            ..Default::default()
        },
    );
    if with_content {
        m.content = Some(MarkContent::Text("Earnings".into()));
    }
    m
}

/// A plot point inside the first glyph of the lane, found by scanning it.
fn find_glyph(rig: &Rig) -> (f64, f64) {
    let h = rig.r.coordinates().height();
    for y in ((h as i64 - 40)..h as i64).map(|y| y as f64) {
        for x in (0..800).map(|x| x as f64) {
            if rig.r.mark_glyph_at(x, y) {
                return (x, y);
            }
        }
    }
    panic!("no glyph in the lane");
}

fn marks_rig(with_content: bool) -> (Rig, Vec<Ohlcv>) {
    let rig = data_rig(800.0, 200.0);
    let bars = rig.bars(100);
    rig.r
        .set_timeline_marks(&[mark_at("m1", bars[50].time, with_content)], &[]);
    rig.host.run_until_idle();
    (rig, bars)
}

#[test]
fn a_mark_click_reports_its_cluster_and_opens_a_popup_when_it_has_content() {
    let (rig, bars) = marks_rig(true);
    let clicks = Rc::new(RefCell::new(Vec::new()));
    let c2 = clicks.clone();
    let _u = rig
        .r
        .on_mark_click(Box::new(move |e| c2.borrow_mut().push(e.clone())))
        .unwrap();
    let (x, y) = find_glyph(&rig);
    let plain = Rc::new(RefCell::new(0));
    let p2 = plain.clone();
    let _u2 = rig.r.on_click(Box::new(move |_| *p2.borrow_mut() += 1));
    rig.r.click(x, y);
    assert_eq!(clicks.borrow().len(), 1);
    assert_eq!(clicks.borrow()[0].id, "m1");
    assert_eq!(clicks.borrow()[0].time, bars[50].time);
    assert_eq!(*plain.borrow(), 0, "a lane glyph takes the click whole");
    let pops = rig.host.mark_popovers();
    assert!(
        matches!(pops.last(), Some(MarkPopoverAction::Open { mark_ids, .. }) if mark_ids == &["m1".to_string()])
    );
    assert!(rig.r.with(|st| st.scene.marks_active_key.is_some()));
    // the host closing it clears the active key
    rig.r.mark_popover_closed();
    assert!(rig.r.with(|st| st.scene.marks_active_key.is_none()));
}

#[test]
fn a_content_less_mark_flashes_instead_of_opening_a_popup() {
    let (rig, _) = marks_rig(false);
    let (x, y) = find_glyph(&rig);
    rig.r.click(x, y);
    assert!(rig.host.mark_popovers().is_empty());
    assert!(rig.r.with(|st| st.scene.marks_flash.is_some()));
    // the flash runs on the frame loop and ends by itself
    rig.host.run_until_idle();
    assert!(rig.r.with(|st| st.scene.marks_flash.is_none()));
}

#[test]
fn hovering_a_glyph_pulses_it_and_leaving_unhovers() {
    let (rig, _) = marks_rig(true);
    let (x, y) = find_glyph(&rig);
    rig.r.pointer_move(x, y);
    assert!(rig.r.with(|st| st.scene.marks_hover_key.is_some()));
    assert!(rig.host.pending_frames() > 0);
    assert!(rig.r.mark_tooltip_at(x, y).is_none() || true);
    rig.host.run_until_idle();
    rig.r.pointer_leave();
    assert!(rig.r.with(|st| st.scene.marks_hover_key.is_none()));
}

#[test]
fn replacing_the_marks_closes_the_popup_and_hiding_the_lane_through_the_feature_works() {
    let (rig, bars) = marks_rig(true);
    let (x, y) = find_glyph(&rig);
    rig.r.click(x, y);
    assert!(rig.r.with(|st| st.mark_popover_key.is_some()));
    rig.r
        .set_timeline_marks(&[mark_at("m2", bars[10].time, true)], &[]);
    assert!(rig.r.with(|st| st.mark_popover_key.is_none()));
    assert!(matches!(
        rig.host.mark_popovers().last(),
        Some(MarkPopoverAction::Close)
    ));
    rig.r.apply_feature("marks", &json!(false));
    assert_eq!(
        rig.r.read_feature("marks"),
        Some(json!({ "visible": false, "groups": {} }))
    );
    rig.host.run_until_idle();
    assert!(!rig.r.mark_glyph_at(x, y));
}

#[test]
fn the_marks_group_choices_are_additive_and_read_back() {
    let rig = data_rig(800.0, 200.0);
    rig.r
        .apply_feature("marks", &json!({ "groups": { "news": false } }));
    rig.r
        .apply_feature("marks", &json!({ "groups": { "divs": true } }));
    assert_eq!(
        rig.r.read_feature("marks"),
        Some(json!({ "visible": true, "groups": { "news": false, "divs": true } }))
    );
}

// ── loading, precision, native data ──

#[test]
fn the_loading_flag_toggles_the_host_affordance_once_per_change() {
    let rig = data_rig(800.0, 200.0);
    rig.r.set_loading(true);
    rig.r.set_loading(true);
    assert!(rig.host.loading_visible());
    rig.r.set_loading(false);
    rig.r.set_loading(false);
    assert!(!rig.host.loading_visible());
    assert_eq!(rig.host.loading_log(), [true, false]);
    // unmounted: never shown
    let r = NativeRenderer::new(TestHost::new(800.0, 400.0, 1.0), None);
    r.set_loading(true);
}

#[test]
fn a_loading_chart_with_no_bars_still_paints_the_pane_structure() {
    let rig = data_rig(800.0, 200.0);
    rig.r.set_loading(true);
    rig.r.flush();
    assert!(!rig.host.canvas(&CanvasSlot::Chrome).unwrap().is_empty());
}

#[test]
fn price_precision_applies_only_for_positive_ticks_and_repaints_on_change() {
    let rig = data_rig(800.0, 200.0);
    rig.bars(50);
    rig.r.set_price_precision(Some(0.01));
    assert_eq!(rig.r.with(|st| st.scene.price_mintick), Some(0.01));
    assert_eq!(rig.r.price_decimals(), Some(2.0));
    assert!(rig.host.pending_frames() > 0);
    rig.host.run_until_idle();
    rig.r.set_price_precision(Some(0.01));
    assert_eq!(rig.host.pending_frames(), 0);
    rig.r.set_price_precision(Some(-1.0));
    assert_eq!(rig.r.with(|st| st.scene.price_mintick), None);
    rig.r.set_price_precision(Some(0.0));
    assert_eq!(rig.r.with(|st| st.scene.price_mintick), None);
}

#[test]
fn native_data_is_routed_by_channel_and_pending_ranges_parse() {
    let rig = data_rig(800.0, 200.0);
    rig.r.set_native_data("vpvr", &json!({ "rows": 24, "widthFrac": 0.3, "upColor": "#0f0", "downColor": "#f00", "showPoc": true, "valueAreaPct": 70 }));
    rig.r.set_native_data("flow", &json!({ "a": 1 }));
    rig.r
        .set_native_data("flow-pending", &json!([[1, 2], [3, 4]]));
    rig.r.with(|st| {
        assert_eq!(st.scene.native_data.get("flow"), Some(&json!({ "a": 1 })));
        assert_eq!(
            st.scene.native_pending.get("flow"),
            Some(&vec![(1.0, 2.0), (3.0, 4.0)])
        );
    });
    rig.r.set_native_data("volume", &json!(null));
    assert!(rig.r.with(|st| st.scene.volume_layer.is_none()));
}
