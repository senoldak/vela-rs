//! Ports of `native-hold-view`, `native-reframe-zoom`, `native-prepend-anchor`,
//! `native-preset-right-margin`, `native-depth-change` and the viewport parts of
//! `native-animation-config`.

use super::*;
use crate::model::{
    CandleBarColor, CandleKind, SeriesData, SeriesSpec, SeriesValueDelta, SeriesValues,
};
use crate::ports::chart_renderer::{VisibleRange, VisibleRangeOptions};

// ── native-hold-view ──

fn hold_bar(i: i64) -> Ohlcv {
    Ohlcv::new(1_000_000 + i * 1000, 1.0, 2.0, 0.5, 1.5, Some(1.0))
}

fn hold_rig() -> Rig {
    let rig = data_rig(800.0, 200.0);
    let bars: Vec<Ohlcv> = (0..200).map(hold_bar).collect();
    rig.set_bars(&bars, false);
    rig
}

#[test]
fn a_new_bar_while_scrolled_back_holds_the_view_on_the_same_bars() {
    let rig = hold_rig();
    rig.set_viewport(8.0, -40.0);
    rig.r.update_bar(&hold_bar(200));
    assert_eq!(rig.viewport().1, -41.0);
}

#[test]
fn a_new_bar_at_the_newest_end_follows_it() {
    let rig = hold_rig();
    rig.set_viewport(8.0, 3.0);
    rig.r.update_bar(&hold_bar(200));
    assert_eq!(rig.viewport().1, 3.0);
}

#[test]
fn a_tick_on_the_forming_bar_never_moves_the_view() {
    let rig = hold_rig();
    rig.set_viewport(8.0, -40.0);
    rig.r.update_bar(&Ohlcv {
        close: 1.8,
        ..hold_bar(199)
    });
    assert_eq!(rig.viewport().1, -40.0);
}

#[test]
fn a_glide_back_to_the_newest_bars_keeps_its_course() {
    let rig = hold_rig();
    rig.set_viewport(8.0, -40.0);
    rig.r.with(|st| st.scroll_target_ro = Some(5.0));
    rig.r.update_bar(&hold_bar(200));
    assert_eq!(rig.viewport().1, -40.0);
}

#[test]
fn an_older_bar_than_the_newest_is_ignored() {
    let rig = hold_rig();
    let before = rig.r.with(|st| st.bars.len());
    rig.r.update_bar(&hold_bar(10));
    assert_eq!(rig.r.with(|st| st.bars.len()), before);
}

// ── native-reframe-zoom ──

#[test]
fn the_first_series_fits_the_content() {
    let rig = data_rig(800.0, 200.0);
    rig.set_bars(&flat_bars(500, 0), false);
    let (bs, ro) = rig.viewport();
    near(bs, 800.0 / (200.0 + 10.0), 1e-3);
    assert_eq!(ro, 10.0);
}

#[test]
fn a_replacement_keeps_the_zoom_and_resets_the_placement() {
    let rig = data_rig(800.0, 200.0);
    rig.set_bars(&flat_bars(500, 0), false);
    rig.set_viewport(12.0, 20.0);
    rig.set_bars(&flat_bars(150, 0), false);
    assert_eq!(rig.viewport(), (12.0, 10.0));
}

#[test]
fn the_kept_zoom_is_not_clamped_to_the_new_series_depth() {
    let rig = data_rig(800.0, 200.0);
    rig.set_bars(&flat_bars(500, 0), false);
    rig.set_viewport(2.0, 6.0);
    rig.set_bars(&flat_bars(100, 0), false);
    assert_eq!(rig.viewport().0, 2.0);
}

#[test]
fn a_view_preserving_replacement_leaves_zoom_and_placement_untouched() {
    let rig = data_rig(800.0, 200.0);
    rig.set_bars(&flat_bars(150, 0), false);
    rig.set_viewport(12.0, 20.0);
    rig.set_bars(&flat_bars(300, 0), true);
    assert_eq!(rig.viewport(), (12.0, 20.0));
}

// ── native-prepend-anchor ──

fn bars_at(n: usize, t0: i64) -> Vec<Ohlcv> {
    (0..n)
        .map(|i| Ohlcv::new(t0 + i as i64 * 1000, 1.0, 2.0, 0.5, 1.5, Some(1.0)))
        .collect()
}

#[test]
fn a_deep_history_prepend_shifts_the_logical_interaction_anchors() {
    let rig = data_rig(800.0, 200.0);
    rig.set_bars(&bars_at(100, 1_000_000), false);
    rig.r.with(|st| {
        st.zoom_anchor_logical = 99.0;
        st.zoom_anchor_x = 800.0;
        st.hover_logical = Some(90);
    });
    let before = rig.r.with(|st| st.anchored_right_offset(50.0));
    let mut all = bars_at(400, 1_000_000 - 400 * 1000);
    all.extend(bars_at(100, 1_000_000));
    rig.set_bars(&all, true);
    rig.r.with(|st| {
        assert_eq!(st.zoom_anchor_logical, 499.0);
        assert_eq!(st.hover_logical, Some(490));
        near(st.anchored_right_offset(50.0), before, 1e-9);
    });
}

#[test]
fn a_different_series_or_an_append_shifts_nothing() {
    let rig = data_rig(800.0, 200.0);
    rig.set_bars(&bars_at(100, 1_000_000), false);
    rig.r.with(|st| st.zoom_anchor_logical = 42.0);
    rig.set_bars(&bars_at(50, 5_000_000), true);
    assert_eq!(rig.r.with(|st| st.zoom_anchor_logical), 42.0);

    let rig = data_rig(800.0, 200.0);
    rig.set_bars(&bars_at(100, 1_000_000), false);
    rig.r.with(|st| st.zoom_anchor_logical = 42.0);
    let mut grown = bars_at(100, 1_000_000);
    grown.extend(bars_at(5, 1_100_000));
    rig.set_bars(&grown, true);
    assert_eq!(rig.r.with(|st| st.zoom_anchor_logical), 42.0);
}

// ── native-preset-right-margin ──

fn all_range(bars: &[Ohlcv]) -> VisibleRange {
    VisibleRange {
        from: bars[0].time,
        to: bars[bars.len() - 1].time,
    }
}

const WITH_MARGIN: VisibleRangeOptions = VisibleRangeOptions { right_margin: true };

#[test]
fn framing_all_history_leaves_the_default_right_margin_past_the_newest_bar() {
    let rig = data_rig(800.0, 200.0);
    let bars = flat_bars(300, 0);
    rig.set_bars(&bars, false);
    rig.r.set_visible_range(all_range(&bars), WITH_MARGIN);
    let (bs, ro) = rig.viewport();
    assert_eq!(ro, 10.0);
    near(bs, 800.0 / (299.0 + 10.0), 1e-6);
}

#[test]
fn a_right_margin_set_in_the_settings_is_the_one_the_frame_keeps() {
    let rig = data_rig(800.0, 200.0);
    let bars = flat_bars(300, 0);
    rig.set_bars(&bars, false);
    rig.r
        .apply_config(&serde_json::json!({ "margins": { "right": 40 } }));
    rig.r.set_visible_range(all_range(&bars), WITH_MARGIN);
    let (bs, ro) = rig.viewport();
    assert_eq!(ro, 40.0);
    // the oldest bar stays on screen
    assert!(800.0 / bs - ro >= 299.0 - 1e-6);
}

#[test]
fn a_shorter_preset_keeps_the_margin_and_zero_margin_frames_the_newest_bar_at_the_edge() {
    let rig = data_rig(800.0, 200.0);
    let bars = flat_bars(300, 0);
    rig.set_bars(&bars, false);
    rig.r
        .apply_config(&serde_json::json!({ "margins": { "right": 25 } }));
    rig.r.set_visible_range(
        VisibleRange {
            from: bars[199].time,
            to: bars[299].time,
        },
        WITH_MARGIN,
    );
    let (bs, ro) = rig.viewport();
    assert_eq!(ro, 25.0);
    near(bs, 800.0 / (100.0 + 25.0), 1e-6);

    rig.r
        .apply_config(&serde_json::json!({ "margins": { "right": 0 } }));
    rig.r.set_visible_range(all_range(&bars), WITH_MARGIN);
    assert_eq!(rig.viewport().1, 0.0);
}

#[test]
fn a_plain_visible_range_is_framed_exactly_with_no_margin_added() {
    let rig = data_rig(800.0, 200.0);
    let bars = flat_bars(300, 0);
    rig.set_bars(&bars, false);
    rig.r.set_visible_range(
        VisibleRange {
            from: bars[199].time,
            to: bars[299].time,
        },
        VisibleRangeOptions::default(),
    );
    let (bs, ro) = rig.viewport();
    assert_eq!(ro, 0.0);
    near(bs, 800.0 / 100.0, 1e-6);
}

// ── native-depth-change ──

#[test]
fn a_mounted_model_gets_a_negative_offset_when_the_chart_head_moves_forward() {
    let rig = data_rig(800.0, 200.0);
    rig.set_bars(&flat_bars(100, 0), false);
    rig.mount_model(&anchored_model("m", 100, 0));
    assert_eq!(rig.offset_of("m"), 0);
    rig.set_bars(&flat_bars(40, 60), true);
    assert_eq!(rig.offset_of("m"), -60);
}

#[test]
fn the_offset_tracks_the_head_in_both_directions() {
    let rig = data_rig(800.0, 200.0);
    rig.set_bars(&flat_bars(100, 0), false);
    rig.mount_model(&anchored_model("m", 100, 0));
    rig.set_bars(&flat_bars(70, 30), true);
    assert_eq!(rig.offset_of("m"), -30);
    rig.set_bars(&flat_bars(90, 10), true);
    assert_eq!(rig.offset_of("m"), -10);
    rig.set_bars(&flat_bars(100, 0), true);
    assert_eq!(rig.offset_of("m"), 0);
}

#[test]
fn a_suffix_model_keeps_its_positive_offset_and_an_old_model_is_pushed_off() {
    let rig = data_rig(800.0, 200.0);
    rig.set_bars(&flat_bars(100, 0), false);
    rig.mount_model(&anchored_model("late", 40, 60));
    assert_eq!(rig.offset_of("late"), 60);
    rig.mount_model(&anchored_model("old", 20, 0));
    rig.set_bars(&flat_bars(40, 60), true);
    assert_eq!(rig.offset_of("old"), -20);
}

#[test]
fn logical_anchors_follow_a_front_trim() {
    let rig = data_rig(800.0, 200.0);
    rig.set_bars(&flat_bars(100, 0), false);
    rig.r.with(|st| {
        st.zoom_anchor_logical = 90.0;
        st.hover_logical = Some(80);
    });
    rig.set_bars(&flat_bars(40, 60), true);
    rig.r.with(|st| {
        assert_eq!(st.zoom_anchor_logical, 30.0);
        assert_eq!(st.hover_logical, Some(20));
    });
}

#[test]
fn a_value_patch_can_clear_an_anchor_and_an_omitted_key_leaves_it_alone() {
    let rig = data_rig(800.0, 200.0);
    rig.set_bars(&flat_bars(100, 0), false);
    let handle = rig.mount_model(&anchored_model("m", 40, 60));
    assert_eq!(rig.offset_of("m"), 60);
    rig.r
        .update_indicator(&handle, &patch("m", AnchorUpdate::Clear));
    assert_eq!(rig.offset_of("m"), 0);
    rig.r
        .update_indicator(&handle, &patch("m", AnchorUpdate::Set(T0 + 60 * STEP)));
    assert_eq!(rig.offset_of("m"), 60);
    rig.r
        .update_indicator(&handle, &patch("m", AnchorUpdate::Keep));
    assert_eq!(rig.offset_of("m"), 60);
}

#[test]
fn an_anchor_matching_no_bar_renders_unanchored_with_a_warning() {
    let rig = data_rig(800.0, 200.0);
    rig.set_bars(&flat_bars(100, 0), false);
    let mut m = anchored_model("m", 10, 10);
    m.anchor_time = Some(T0 + 10 * STEP + 7);
    rig.mount_model(&m);
    assert_eq!(rig.offset_of("m"), 0);
    assert_eq!(rig.host.warnings().len(), 1);
}

// ── native-candle-color-patch ──

fn candle_bar(i: i64) -> Ohlcv {
    Ohlcv::new(
        T0 + i * 3_600_000,
        100.0 + i as f64,
        102.0 + i as f64,
        99.0 + i as f64,
        101.0 + i as f64,
        Some(1.0),
    )
}

fn color_of(i: i64) -> Option<CandleBarColor> {
    Some(CandleBarColor {
        color: Some(format!("#0000{i:02}")),
        wick_color: None,
        border_color: None,
    })
}

fn candle_model() -> IndicatorModel {
    let mut m = line_model("trend", "price", true, &[], &[]);
    m.series = vec![SeriesSpec {
        id: "trend:candle".into(),
        title: "Trend".into(),
        display_title: None,
        pane_id: "price".into(),
        z_order: None,
        visible: None,
        display: None,
        overlay: false,
        data: SeriesData::Candle {
            kind: CandleKind::Candle,
            bars: (0..5).map(candle_bar).collect(),
            style: None,
            bar_colors: Some((0..5).map(color_of).collect()),
        },
    }];
    m
}

type Colors = Option<Vec<Option<CandleBarColor>>>;

fn patched_candles(bars: std::ops::Range<i64>, colors: Colors) -> (Vec<Ohlcv>, Colors) {
    let rig = data_rig(800.0, 200.0);
    let handle = rig.mount_model(&candle_model());
    let mut vp = value_patch("trend", AnchorUpdate::Keep);
    vp.series = vec![SeriesValueDelta {
        series_id: "trend:candle".into(),
        values: SeriesValues::Bars {
            bars: bars.map(candle_bar).collect(),
            bar_colors: colors,
        },
    }];
    rig.r.update_indicator(&handle, &ScenePatch::Value(vp));
    let model = rig.r.indicator_model("trend").unwrap();
    match &model.series[0].data {
        SeriesData::Candle {
            bars, bar_colors, ..
        } => (bars.clone(), bar_colors.clone()),
        _ => unreachable!(),
    }
}

#[test]
fn a_candle_patch_replaces_the_colors_together_with_the_bars() {
    let (bars, colors) = patched_candles(1..6, Some((1..6).map(color_of).collect()));
    assert_eq!(bars.len(), 5);
    assert_eq!(bars[0].time, T0 + 3_600_000);
    assert_eq!(colors, Some((1..6).map(color_of).collect()));
}

#[test]
fn a_candle_patch_clears_the_colors_on_an_empty_set_and_keeps_them_when_it_omits() {
    assert_eq!(patched_candles(1..6, Some(vec![])).1, Some(vec![]));
    let (bars, colors) = patched_candles(0..5, None);
    assert_eq!(bars.len(), 5);
    assert_eq!(colors, Some((0..5).map(color_of).collect()));
}

#[test]
fn a_growing_candle_patch_colors_the_added_candles() {
    let (bars, colors) = patched_candles(0..6, Some((0..6).map(color_of).collect()));
    assert_eq!(bars.len(), 6);
    assert_eq!(colors.unwrap()[5], color_of(5));
}

// ── zoom / pan / fling behavior of the viewport (animation) ──

#[test]
fn a_zoom_glides_toward_its_target_and_settles_with_the_anchor_pinned() {
    let rig = data_rig(800.0, 200.0);
    rig.bars(300);
    let (bs0, _) = rig.viewport();
    let anchor = rig.r.coordinates().right_edge_logical();
    rig.r.zoom_to(bs0 * 1.5, anchor, 800.0);
    assert!(rig.host.pending_frames() > 0);
    rig.host.run_until_idle();
    let (bs, ro) = rig.viewport();
    near(bs, bs0 * 1.5, bs0 * 1.5 * 1e-3 + 1e-9);
    // the right edge stays where it was
    near(rig.r.coordinates().right_edge_logical(), anchor, 1e-6);
    assert!(ro.is_finite());
}

#[test]
fn with_the_zoom_glide_off_a_zoom_applies_at_once() {
    let rig = data_rig(800.0, 200.0);
    rig.bars(300);
    rig.r.apply_feature("animZoom", &serde_json::json!(false));
    let (bs0, _) = rig.viewport();
    let anchor = rig.r.coordinates().right_edge_logical();
    rig.r.zoom_to(bs0 * 1.2, anchor, 800.0);
    near(rig.viewport().0, bs0 * 1.2, 1e-9);
    assert_eq!(rig.host.pending_frames(), 1); // the repaint, not a glide
}

#[test]
fn a_fling_coasts_and_stops_by_itself() {
    let rig = data_rig(800.0, 200.0);
    rig.bars(300);
    rig.set_viewport(8.0, 2.0);
    rig.r.fling(-0.02);
    let steps = rig.host.run_until_idle();
    assert!(steps > 1);
    let ro = rig.viewport().1;
    assert!(ro < 2.0, "the fling moved the view back in time: {ro}");
    assert_eq!(rig.r.with(|st| st.pan_velocity), 0.0);
}

#[test]
fn with_the_pan_animation_off_a_fling_stops_dead() {
    let rig = data_rig(800.0, 200.0);
    rig.bars(300);
    rig.r.apply_feature("animPan", &serde_json::json!(false));
    rig.r.fling(-0.02);
    assert_eq!(rig.host.pending_frames(), 0);
}

#[test]
fn pan_by_glides_to_the_clamped_target_and_scroll_to_realtime_returns_to_the_margin() {
    let rig = data_rig(800.0, 200.0);
    rig.bars(300);
    rig.r.pan_by(-0.5);
    rig.host.run_until_idle();
    let ro = rig.viewport().1;
    assert!(ro < 0.0, "panned into history: {ro}");
    rig.r.scroll_to_realtime();
    rig.host.run_until_idle();
    near(rig.viewport().1, 10.0, 1e-2);
}

#[test]
fn pan_by_never_runs_past_the_minimum_visible_bars() {
    let rig = data_rig(800.0, 200.0);
    rig.bars(50);
    rig.r.apply_feature("animScroll", &serde_json::json!(false));
    rig.r.pan_by(100.0);
    let (bs, ro) = rig.viewport();
    let vis = 800.0 / bs;
    assert!(ro <= vis - 1.0 + 1e-9, "ro {ro} vis {vis}");
}

#[test]
fn the_scroll_to_realtime_button_shows_only_while_the_latest_bar_is_off_screen_and_the_cursor_is_near()
 {
    let rig = data_rig(800.0, 200.0);
    rig.bars(300);
    rig.r.apply_feature("animPan", &serde_json::json!(false));
    rig.r.apply_feature("animScroll", &serde_json::json!(false));
    rig.r.apply_viewport(crate::render::core::ViewportState {
        bar_spacing: 8.0,
        right_offset: -30.0,
    });
    assert_eq!(
        rig.host.last_scroll_button().map(|s| s.visible),
        Some(false)
    );
    // the button sits bottom-right: (plot w - right - 13, plot h - bottom - 13)
    let (w, h) = rig.r.with(|st| st.css_size);
    rig.r
        .scroll_proximity_move(w - 76.0 - 13.0, h - 36.0 - 13.0);
    assert_eq!(rig.host.last_scroll_button().map(|s| s.visible), Some(true));
    rig.r.scroll_proximity_leave();
    assert_eq!(
        rig.host.last_scroll_button().map(|s| s.visible),
        Some(false)
    );
    // mobile: shown whenever the latest bar is off screen
    rig.r
        .set_layout_mode(crate::ports::chart_renderer::LayoutMode::Mobile);
    assert_eq!(
        rig.host.last_scroll_button().map(|s| (s.visible, s.touch)),
        Some((true, true))
    );
    rig.r.scroll_to_realtime();
    rig.host.run_until_idle();
    assert_eq!(
        rig.host.last_scroll_button().map(|s| s.visible),
        Some(false)
    );
}

#[test]
fn viewport_changes_are_reported_to_subscribers() {
    let rig = data_rig(800.0, 200.0);
    rig.bars(300);
    let seen: Rc<std::cell::RefCell<Vec<VisibleRange>>> = Rc::default();
    let s2 = seen.clone();
    let _unsub = rig
        .r
        .on_viewport_change(Box::new(move |r| s2.borrow_mut().push(*r)));
    rig.r.apply_viewport(crate::render::core::ViewportState {
        bar_spacing: 5.0,
        right_offset: 0.0,
    });
    assert_eq!(seen.borrow().len(), 1);
    let range = rig.r.get_visible_range().unwrap();
    assert_eq!(*seen.borrow().last().unwrap(), range);
}

#[test]
fn the_visible_range_is_data_bounded_and_none_when_scrolled_into_whitespace_or_unsized() {
    let rig = data_rig(800.0, 200.0);
    assert_eq!(rig.r.get_visible_range(), None);
    let bars = rig.bars(100);
    let range = rig.r.get_visible_range().unwrap();
    assert!(range.from >= bars[0].time && range.to <= bars[99].time);
    assert_eq!(range.to, bars[99].time);
}
