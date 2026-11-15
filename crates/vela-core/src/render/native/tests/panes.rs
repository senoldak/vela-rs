//! Ports of `native-pane-resize` (the renderer part), `native-manual-scale` (the
//! gesture part), `native-axis-bundle` (pane scale settings) plus the pane
//! management the original covers through the settings and legend UIs.

use std::cell::RefCell;

use serde_json::json;

use super::*;
use crate::model::{Pane, PaneKind};
use crate::ports::chart_renderer::{MoveTarget, PaneAction, PaneMoveDir};

fn study(id: &str, order: i32, weight: Option<f64>) -> Pane {
    Pane {
        id: id.into(),
        kind: PaneKind::Study,
        order,
        height_weight: weight,
        title: None,
    }
}

fn two_pane_rig() -> Rig {
    let rig = data_rig(800.0, 400.0);
    let bars = rig.bars(100);
    rig.r.ensure_pane(&study("rsi", 1, None));
    let times: Vec<i64> = bars.iter().map(|b| b.time).collect();
    let values: Vec<Option<f64>> = (0..100).map(|i| Some(30.0 + (i % 40) as f64)).collect();
    rig.mount_model(&line_model("rsi", "rsi", false, &times, &values));
    rig.host.run_until_idle();
    rig
}

fn bounds(rig: &Rig, id: &str) -> (f64, f64) {
    rig.r.with(|st| {
        let p = st.scene.panes.get(id).unwrap();
        (p.bounds.top, p.bounds.height)
    })
}

fn collect_actions(rig: &Rig) -> Rc<RefCell<Vec<PaneAction>>> {
    let seen: Rc<RefCell<Vec<PaneAction>>> = Rc::default();
    let s = seen.clone();
    std::mem::forget(
        rig.r
            .on_pane_action(Box::new(move |a| s.borrow_mut().push(a.clone())))
            .unwrap(),
    );
    seen
}

#[test]
fn panes_share_the_plot_height_by_weight_the_price_pane_defaulting_to_three() {
    let rig = two_pane_rig();
    let (pt, ph) = bounds(&rig, "price");
    let (st, sh) = bounds(&rig, "rsi");
    assert_eq!(pt, 0.0);
    near(ph, 300.0, 1e-9);
    near(st, 300.0, 1e-9);
    near(sh, 100.0, 1e-9);
    assert_eq!(rig.r.pane_height_weight("price"), Some(3.0));
    assert_eq!(rig.r.pane_height_weight("rsi"), Some(1.0));
    assert_eq!(rig.r.pane_height_weight("nope"), None);
    // the host's pane-pinned DOM was told
    assert!(rig.host.layout_change_count() > 0);
}

#[test]
fn a_weight_change_replans_the_layout_and_an_ensure_without_a_weight_keeps_it() {
    let rig = two_pane_rig();
    rig.r.set_pane_height_weight("rsi", 3.0);
    near(bounds(&rig, "rsi").1, 200.0, 1e-9);
    rig.r.ensure_pane(&study("rsi", 1, None));
    assert_eq!(rig.r.pane_height_weight("rsi"), Some(3.0));
    rig.r.set_pane_height_weight("rsi", -1.0);
    assert_eq!(rig.r.pane_height_weight("rsi"), Some(3.0));
    rig.r.set_pane_height_weight("rsi", 1.0);
    near(bounds(&rig, "price").1, 300.0, 1e-9);
}

#[test]
fn collapse_gives_the_pane_a_fixed_strip_and_the_legend_a_master_row() {
    let rig = two_pane_rig();
    rig.r.set_pane_collapsed("rsi", true);
    assert_eq!(bounds(&rig, "rsi").1, 26.0);
    near(bounds(&rig, "price").1, 374.0, 1e-9);
    assert_eq!(
        rig.host.legend_collapsed(),
        [("rsi".to_string(), Some("rsi".to_string()))]
    );
    rig.r.set_pane_collapsed("rsi", false);
    near(bounds(&rig, "rsi").1, 100.0, 1e-9);
    assert!(rig.host.legend_collapsed().is_empty());
}

#[test]
fn maximize_fills_the_plot_and_hides_the_rest_and_removing_the_pane_restores() {
    let rig = two_pane_rig();
    rig.r.set_pane_maximized(Some("rsi"));
    assert_eq!(bounds(&rig, "rsi"), (0.0, 400.0));
    assert_eq!(bounds(&rig, "price"), (400.0, 0.0));
    assert_eq!(rig.r.pane_views().iter().filter(|p| p.maximized).count(), 1);
    rig.r.set_pane_maximized(Some("nope"));
    near(bounds(&rig, "price").1, 300.0, 1e-9);
    rig.r.set_pane_maximized(Some("rsi"));
    rig.r.remove_pane("rsi");
    near(bounds(&rig, "price").1, 400.0, 1e-9);
    // the price pane can never be removed
    rig.r.remove_pane("price");
    assert!(rig.r.with(|st| st.scene.panes.contains_key("price")));
}

#[test]
fn the_user_actions_apply_locally_and_are_reported_to_core() {
    let rig = two_pane_rig();
    let actions = collect_actions(&rig);
    rig.r.toggle_pane_collapse("rsi");
    rig.r.toggle_pane_maximize("rsi");
    rig.r.toggle_pane_maximize("rsi");
    rig.r.ensure_pane(&study("macd", 2, None));
    rig.r.move_pane("macd", PaneMoveDir::Up);
    rig.r.move_pane("macd", PaneMoveDir::Up); // the price pane stays pinned
    assert_eq!(
        *actions.borrow(),
        [
            PaneAction::Collapse {
                pane_id: "rsi".into(),
                collapsed: true
            },
            PaneAction::Maximize {
                pane_id: "rsi".into(),
                maximized: true
            },
            PaneAction::Maximize {
                pane_id: "rsi".into(),
                maximized: false
            },
            PaneAction::Move {
                pane_id: "macd".into(),
                dir: PaneMoveDir::Up
            },
        ]
    );
    assert_eq!(
        rig.r.with(|st| st.scene.ordered_pane_ids()),
        ["price", "macd", "rsi"]
    );
}

#[test]
fn a_data_double_click_maximizes_the_clicked_pane_and_a_second_restores() {
    let rig = two_pane_rig();
    let actions = collect_actions(&rig);
    rig.r.data_double_click(100.0, 350.0);
    assert_eq!(bounds(&rig, "rsi"), (0.0, 400.0));
    rig.r.data_double_click(100.0, 200.0);
    near(bounds(&rig, "price").1, 300.0, 1e-9);
    assert_eq!(actions.borrow().len(), 2);
    // a point in the gap below the last pane still snaps to the nearest one
    rig.r.data_double_click(100.0, 999.0);
    assert!(
        matches!(actions.borrow().last(), Some(PaneAction::Maximize { pane_id, maximized: true }) if pane_id == "rsi")
    );
}

#[test]
fn dragging_a_separator_resizes_the_panes_and_reports_once_at_the_end() {
    let rig = two_pane_rig();
    let actions = collect_actions(&rig);
    assert!(rig.r.pane_separator_at(300.0));
    assert!(rig.r.pane_separator_at(303.0));
    assert!(!rig.r.pane_separator_at(250.0));
    rig.r.begin_pane_resize(300.0);
    rig.r.pane_resize_by(-60.0);
    near(bounds(&rig, "price").1, 240.0, 1e-9);
    near(bounds(&rig, "rsi").1, 160.0, 1e-9);
    rig.r.pane_resize_by(-10_000.0); // clamps at the minimum height
    near(bounds(&rig, "price").1, 24.0, 1e-9);
    assert!(actions.borrow().is_empty());
    rig.r.end_pane_resize();
    assert_eq!(
        *actions.borrow(),
        [PaneAction::Resize {
            pane_ids: vec!["price".into(), "rsi".into()]
        }]
    );
    // releasing without a change reports nothing
    rig.r.begin_pane_resize(24.0);
    rig.r.end_pane_resize();
    assert_eq!(actions.borrow().len(), 1);
}

#[test]
fn double_clicking_a_separator_splits_the_two_panes_evenly() {
    let rig = two_pane_rig();
    let actions = collect_actions(&rig);
    rig.r.reset_pane_size(300.0);
    near(bounds(&rig, "price").1, 200.0, 1e-9);
    near(bounds(&rig, "rsi").1, 200.0, 1e-9);
    assert_eq!(actions.borrow().len(), 1);
    rig.r.reset_pane_size(50.0); // not on a separator
    assert_eq!(actions.borrow().len(), 1);
}

#[test]
fn the_pane_resize_feature_gates_the_hover_highlight_only() {
    let rig = two_pane_rig();
    rig.r.pointer_move(100.0, 300.0);
    assert_eq!(rig.r.with(|st| st.hover_separator_y), Some(300.0));
    rig.r.apply_feature("paneResize", &json!(false));
    rig.r.pointer_move(100.0, 301.0);
    assert_eq!(rig.r.with(|st| st.hover_separator_y), None);
}

#[test]
fn order_panes_sets_the_top_to_bottom_order() {
    let rig = two_pane_rig();
    rig.r.ensure_pane(&study("macd", 2, None));
    rig.r
        .order_panes(&["price".into(), "macd".into(), "rsi".into()]);
    assert_eq!(
        rig.r.with(|st| st.scene.ordered_pane_ids()),
        ["price", "macd", "rsi"]
    );
}

#[test]
fn moving_an_indicator_to_another_pane_with_its_own_scale_adds_a_gutter_column() {
    let rig = two_pane_rig();
    assert_eq!(rig.r.right_axis_width(), 64.0);
    let handle = IndicatorRenderHandle { id: "rsi".into() };
    rig.r.set_indicator_pane(&handle, "price", true);
    assert_eq!(rig.r.right_axis_width(), 120.0);
    assert_eq!(rig.host.last_layout().unwrap().scale_gutter, 120.0);
    assert_eq!(
        rig.host.legend_panes().last(),
        Some(&("rsi".to_string(), "price".to_string()))
    );
    // the plot narrowed by the extra column
    near(rig.r.coordinates().width(), 864.0 - 120.0, 1e-9);
    rig.host.run_until_idle();
    assert!(
        rig.r
            .with(|st| st.scene.indicator_scales.contains_key("rsi"))
    );
    // merged back onto the pane scale
    rig.r.set_indicator_pane(&handle, "rsi", false);
    assert_eq!(rig.r.right_axis_width(), 64.0);
    assert!(
        rig.r
            .with(|st| !st.scene.indicator_scales.contains_key("rsi"))
    );
}

#[test]
fn the_layout_facts_follow_the_panes_the_toolbar_and_a_maximized_study_pane() {
    let rig = two_pane_rig();
    let l = rig.host.last_layout().unwrap();
    assert_eq!(l.toolbar_gutter, 0.0);
    assert_eq!(l.price_pane_top, Some(0.0));
    assert_eq!(l.price_pane_bottom, Some(122.0)); // study pane + time axis
    assert_eq!(l.bottom_gutter, Some(22.0));
    rig.r.set_pane_collapsed("rsi", true);
    assert_eq!(
        rig.host.last_layout().unwrap().bottom_gutter,
        Some(22.0 + 26.0)
    );
    rig.r.set_pane_collapsed("rsi", false);
    rig.r.set_pane_maximized(Some("rsi"));
    let l = rig.host.last_layout().unwrap();
    assert_eq!(l.price_pane_top, Some(400.0));
    assert_eq!(l.price_pane_bottom, Some(22.0));
    rig.r.set_pane_maximized(None);
    rig.r.set_toolbar_gutter(40.0);
    assert_eq!(rig.host.last_layout().unwrap().toolbar_gutter, 40.0);
    near(rig.r.coordinates().width(), 864.0 - 40.0 - 64.0, 1e-9);
    assert_eq!(rig.r.attribution_state().left_px, 52.0);
}

// ── manual scale / per-pane settings ──

#[test]
fn a_price_axis_drag_freezes_the_scale_and_rescales_around_its_center() {
    let rig = data_rig(800.0, 400.0);
    rig.bars(100);
    let (min0, max0) = rig.r.with(|st| {
        let s = st.scene.panes.get("price").unwrap().scale;
        (s.min, s.max)
    });
    rig.r.begin_price_scale(820.0, 100.0);
    assert!(
        rig.r
            .with(|st| st.scene.panes.get("price").unwrap().manual_scale.is_some())
    );
    rig.r.price_scale_by(100.0); // down: zoom out by e^(0.4)
    let (min1, max1) = rig.r.with(|st| {
        let s = st.scene.panes.get("price").unwrap().scale;
        (s.min, s.max)
    });
    near((min1 + max1) / 2.0, (min0 + max0) / 2.0, 1e-6);
    near((max1 - min1) / (max0 - min0), (0.4f64).exp(), 1e-6);
    // an autoscale pass leaves the manual window verbatim
    rig.r.with(|st| st.compute_scales());
    let s = rig.r.with(|st| st.scene.panes.get("price").unwrap().scale);
    assert_eq!((s.min, s.max), (min1, max1));
    // double-click the axis: autoscale resumes
    rig.r.reset_price_scale(820.0, 100.0);
    assert!(
        rig.r
            .with(|st| st.scene.panes.get("price").unwrap().manual_scale.is_none())
    );
    assert_eq!(rig.r.read_feature("autoScale"), Some(json!(true)));
}

#[test]
fn a_vertical_price_pan_needs_a_manual_pane_or_an_unlock() {
    let rig = data_rig(800.0, 400.0);
    rig.bars(100);
    assert!(!rig.r.begin_price_pan(100.0, false));
    assert!(rig.r.begin_price_pan(100.0, true));
    let s0 = rig.r.with(|st| st.scene.panes.get("price").unwrap().scale);
    rig.r.price_pan_by(40.0); // down: show lower prices... bounds move up in price
    let s1 = rig.r.with(|st| st.scene.panes.get("price").unwrap().scale);
    near(s1.max - s1.min, s0.max - s0.min, 1e-9);
    assert!(s1.min > s0.min);
    assert!(rig.r.begin_price_pan(100.0, false));
    assert_eq!(rig.r.read_feature("autoScale"), Some(json!(false)));
    rig.r.apply_feature("autoScale", &json!(true));
    assert_eq!(rig.r.read_feature("autoScale"), Some(json!(true)));
    rig.r.apply_feature("autoScale", &json!(false));
    assert!(
        rig.r
            .with(|st| st.scene.panes.get("price").unwrap().manual_scale.is_some())
    );
}

#[test]
fn a_price_axis_grab_on_a_merged_column_targets_that_indicators_own_scale() {
    let rig = two_pane_rig();
    rig.r
        .set_indicator_pane(&IndicatorRenderHandle { id: "rsi".into() }, "price", true);
    rig.host.run_until_idle();
    let data_w = rig.r.coordinates().width();
    // the first merged column sits right of the 64px master column
    rig.r.begin_price_scale(data_w + 64.0 + 10.0, 50.0);
    assert!(rig.r.with(|st| {
        st.scene
            .indicator_scales
            .get("rsi")
            .unwrap()
            .manual_scale
            .is_some()
    }));
    assert!(
        rig.r
            .with(|st| st.scene.panes.get("price").unwrap().manual_scale.is_none())
    );
    rig.r.price_scale_by(20.0);
    rig.r.reset_price_scale(data_w + 64.0 + 10.0, 50.0);
    assert!(rig.r.with(|st| {
        st.scene
            .indicator_scales
            .get("rsi")
            .unwrap()
            .manual_scale
            .is_none()
    }));
}

#[test]
fn the_per_pane_scale_settings_target_one_pane_and_a_bare_value_is_the_price_pane() {
    let rig = two_pane_rig();
    rig.r
        .apply_feature("logScale", &json!({ "pane": "rsi", "value": true }));
    rig.r
        .apply_feature("scaleMode", &json!({ "pane": "rsi", "mode": "percent" }));
    rig.r
        .apply_feature("invertScale", &json!({ "pane": "rsi", "value": true }));
    let infos = rig.r.read_feature("paneScales").unwrap();
    let rsi = infos
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "rsi")
        .unwrap();
    assert_eq!(
        (
            rsi["log"].clone(),
            rsi["mode"].clone(),
            rsi["invert"].clone()
        ),
        (json!(true), json!("percent"), json!(true))
    );
    assert_eq!(rsi["kind"], "study");
    assert_eq!(rig.r.read_feature("logScale"), Some(json!(false)));
    assert_eq!(rig.r.read_feature("scaleMode"), Some(json!("price")));
    rig.r.apply_feature("logScale", &json!(true));
    rig.r.apply_feature("scaleMode", &json!("indexed"));
    assert_eq!(rig.r.read_feature("logScale"), Some(json!(true)));
    assert_eq!(rig.r.read_feature("scaleMode"), Some(json!("indexed")));
    let views = rig.r.pane_views();
    assert!(views[0].log && views[1].log && views[1].invert);
    // an unknown pane is ignored
    rig.r
        .apply_feature("logScale", &json!({ "pane": "nope", "value": true }));
}

#[test]
fn a_log_flip_on_a_frozen_window_keeps_the_bounds_and_only_flips_its_tag() {
    let rig = data_rig(800.0, 400.0);
    rig.bars(100);
    rig.r.apply_feature("autoScale", &json!(false));
    let before = rig
        .r
        .with(|st| st.scene.panes.get("price").unwrap().manual_scale.unwrap());
    rig.r
        .apply_feature("logScale", &json!({ "pane": "price", "value": true }));
    let after = rig
        .r
        .with(|st| st.scene.panes.get("price").unwrap().manual_scale.unwrap());
    assert_eq!((after.min, after.max), (before.min, before.max));
    assert_eq!(after.log, Some(true));
    rig.r.toggle_pane_log("price");
    assert_eq!(rig.r.read_feature("logScale"), Some(json!(false)));
    rig.r.toggle_pane_auto("price");
    assert_eq!(rig.r.read_feature("autoScale"), Some(json!(true)));
}

#[test]
fn an_inverted_price_axis_flips_the_y_mapping_across_the_coordinate_system() {
    let rig = data_rig(800.0, 400.0);
    rig.bars(100);
    let y_of = |rig: &Rig| {
        rig.r.with(|st| {
            let p = st.scene.panes.get("price").unwrap();
            st.coords.price_to_y(p.scale.max, &p.scale, &p.bounds)
        })
    };
    let top = y_of(&rig);
    rig.r.apply_feature("invertScale", &json!(true));
    rig.host.run_until_idle();
    assert!(
        y_of(&rig) > top + 100.0,
        "the max price moved to the bottom"
    );
}

#[test]
fn the_pane_master_title_names_a_study_pane_after_its_master_indicator() {
    let rig = two_pane_rig();
    assert_eq!(rig.r.pane_master_title("rsi").as_deref(), Some("rsi"));
    assert_eq!(rig.r.pane_master_title("price"), None);
}

#[test]
fn a_move_request_from_the_legend_is_relayed_to_core() {
    let rig = two_pane_rig();
    let seen: Rc<RefCell<Vec<(String, MoveTarget)>>> = Rc::default();
    let s = seen.clone();
    let _u = rig
        .r
        .on_move_indicator(Box::new(move |id, t| {
            s.borrow_mut().push((id.to_string(), t.clone()))
        }))
        .unwrap();
    rig.r.notify_move_indicator("rsi", &MoveTarget::Price);
    assert_eq!(*seen.borrow(), [("rsi".to_string(), MoveTarget::Price)]);
}

#[test]
fn the_legend_events_raised_by_the_host_reach_the_subscribers() {
    let rig = two_pane_rig();
    let log: Rc<RefCell<Vec<String>>> = Rc::default();
    let (a, b, c) = (log.clone(), log.clone(), log.clone());
    let _u1 = rig.r.on_remove_indicator(Box::new(move |id| {
        a.borrow_mut().push(format!("remove {id}"))
    }));
    let _u2 = rig
        .r
        .on_toggle_indicator_visible(Box::new(move |id, v| {
            b.borrow_mut().push(format!("toggle {id} {v}"))
        }))
        .unwrap();
    let _u3 = rig
        .r
        .on_theme_select(Box::new(move |t| c.borrow_mut().push(format!("theme {t}"))))
        .unwrap();
    rig.r.notify_remove_indicator("rsi");
    rig.r.notify_toggle_visible("rsi", false);
    rig.r.notify_theme_select("light");
    assert_eq!(
        *log.borrow(),
        ["remove rsi", "toggle rsi false", "theme light"]
    );
}

#[test]
fn hiding_an_indicator_keeps_its_z_and_legend_row_and_showing_remounts_in_place() {
    let rig = two_pane_rig();
    let handle = IndicatorRenderHandle { id: "rsi".into() };
    let z = rig.r.with(|st| st.scene.z_of("rsi"));
    rig.r.set_indicator_visible(&handle, false);
    assert!(rig.r.indicator_model("rsi").is_none());
    assert_eq!(rig.r.with(|st| st.scene.z_of("rsi")), z);
    assert_eq!(
        rig.host.legend_visibility().last(),
        Some(&("rsi".to_string(), false))
    );
    rig.r.remove_indicator(&handle);
    assert_eq!(rig.host.legend_removed(), ["rsi"]);
    assert_eq!(rig.r.with(|st| st.scene.z_of("rsi")), 0.0);
}

#[test]
fn restacking_a_legend_layer_untangles_shared_keys_and_reports_a_config_change() {
    let rig = data_rig(800.0, 400.0);
    let bars = rig.bars(50);
    let times: Vec<i64> = bars.iter().map(|b| b.time).collect();
    rig.mount_model(&line_model(
        "a",
        "price",
        true,
        &times,
        &vec![Some(100.0); 50],
    ));
    rig.mount_model(&line_model(
        "b",
        "price",
        true,
        &times,
        &vec![Some(101.0); 50],
    ));
    let changes = Rc::new(RefCell::new(0));
    let c = changes.clone();
    let _u = rig
        .r
        .on_config_changed(Box::new(move || *c.borrow_mut() += 1))
        .unwrap();
    let stack = rig.r.overlay_stack();
    assert_eq!(stack.len(), 3); // two indicators + the candles
    rig.r.lower_overlay("a");
    assert_eq!(*changes.borrow(), 1);
    assert_ne!(rig.r.overlay_stack(), stack);
    assert!(rig.r.layer_above("a").is_some() || rig.r.layer_below("a").is_some());
    // the legend order is front-most first
    assert_eq!(rig.host.legend_order().len(), 2);
}

#[test]
fn the_series_order_feature_reorders_and_reads_back_with_z_keys() {
    let rig = data_rig(800.0, 400.0);
    let bars = rig.bars(50);
    let times: Vec<i64> = bars.iter().map(|b| b.time).collect();
    rig.mount_model(&line_model(
        "a",
        "price",
        true,
        &times,
        &vec![Some(100.0); 50],
    ));
    rig.mount_model(&line_model(
        "b",
        "price",
        true,
        &times,
        &vec![Some(101.0); 50],
    ));
    rig.r
        .apply_feature("seriesOrder", &json!({ "id": "a", "to": "front" }));
    let order = rig.r.read_feature("seriesOrder").unwrap();
    let ids: Vec<&str> = order
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.last(), Some(&"a"));
    rig.r
        .apply_feature("seriesOrder", &json!({ "id": "b", "z": 99 }));
    let order = rig.r.read_feature("seriesOrder").unwrap();
    assert_eq!(
        order.as_array().unwrap().last().unwrap(),
        &json!({ "id": "b", "z": 99 })
    );
    rig.r.apply_feature("seriesOrder", &json!("junk"));
}
