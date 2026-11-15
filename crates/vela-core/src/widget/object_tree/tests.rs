use serde_json::json;

use super::*;
use crate::widget::object_tree_model::{IndicatorRow, PriceRow};

fn strs(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn draw(id: &str, pane: &str, z: f64) -> SerializedDrawing {
    serde_json::from_value(json!({
        "id": id, "type": "trendline", "paneId": pane, "anchors": [],
        "style": { "lineColor": "#fff", "lineWidth": 1, "lineStyle": "solid" },
        "locked": false, "visible": true, "zIndex": z, "createdAt": 0,
    }))
    .expect("a valid drawing")
}

fn price_row() -> TreeItem {
    TreeItem::Row(TreeRow::Price(PriceRow {
        label: "BTC".into(),
        visible: true,
    }))
}

fn ind_row(id: &str) -> TreeItem {
    TreeItem::Row(TreeRow::Indicator(IndicatorRow {
        id: id.into(),
        label: id.into(),
        visible: true,
        own_scale: false,
    }))
}

fn unit(id: &str, pane: &str) -> TreeItem {
    TreeItem::Unit(DrawUnit::Draw(Box::new(draw(id, pane, 0.0))))
}

fn tree_pane(id: &str, kind: PaneKind, items: Vec<TreeItem>) -> TreePane {
    TreePane {
        id: id.into(),
        kind,
        label: id.into(),
        order: 0,
        collapsed: false,
        maximized: false,
        items,
    }
}

/// Front-first: d1, a, price, d2 on the main pane; rsi alone in p1.
fn panes() -> Vec<TreePane> {
    vec![
        tree_pane(
            "price",
            PaneKind::Price,
            vec![
                unit("d1", "price"),
                ind_row("a"),
                price_row(),
                unit("d2", "price"),
            ],
        ),
        tree_pane("p1", PaneKind::Study, vec![ind_row("rsi")]),
    ]
}

fn live() -> Vec<LiveDrawing> {
    // Paint order: back to front.
    vec![
        LiveDrawing {
            id: "d2".into(),
            pane_id: "price".into(),
        },
        LiveDrawing {
            id: "d1".into(),
            pane_id: "price".into(),
        },
    ]
}

// ── the panel's state ──

#[test]
fn clicking_a_drawing_picks_toggles_and_clears() {
    let mut m = ObjectTreeModel::new();
    assert_eq!(m.on_draw_click("d1", false), ["d1"]);
    assert_eq!(
        m.on_draw_click("d1", false),
        Vec::<String>::new(),
        "the only pick, clicked again, clears"
    );
    m.on_draw_click("d1", false);
    assert_eq!(m.on_draw_click("d2", true), ["d1", "d2"]);
    assert_eq!(m.on_draw_click("d1", true), ["d2"]);
    assert_eq!(
        m.on_draw_click("d3", false),
        ["d3"],
        "a plain click replaces a multi-pick"
    );
    m.on_draw_click("d3", true);
    m.on_draw_click("d4", true);
    assert_eq!(m.on_draw_click("d3", false), ["d3"]);
    m.set_picked(&strs(&["x", "y"]));
    assert!(m.is_picked("y"));
}

#[test]
fn making_a_group_needs_an_ungrouped_selection_and_takes_over_the_pick() {
    let mut m = ObjectTreeModel::new();
    m.on_draw_click("d1", false);
    let id = m.make_group(&strs(&["d1", "d2"]), None).expect("a group");
    assert_eq!(id, "grp-1");
    assert_eq!(m.groups()[0].name, "Group 1");
    assert!(m.picked().is_empty());
    assert!(
        m.make_group(&strs(&["d2", "d3"]), None).is_none(),
        "d2 is grouped already"
    );
    assert!(m.make_group(&[], None).is_none());
    let second = m.make_group(&strs(&["d3"]), None).expect("another");
    assert_eq!(
        (second.as_str(), m.groups()[1].name.as_str()),
        ("grp-2", "Group 2")
    );
}

#[test]
fn an_empty_group_starts_in_the_main_chart_ready_to_rename() {
    let mut m = ObjectTreeModel::new();
    let id = m.make_empty_group(None);
    assert_eq!(m.groups()[0].pane.as_deref(), Some("price"));
    assert_eq!(m.renaming(), Some(id.as_str()));
    assert!(m.commit_rename(&id, "  Fibs  "));
    assert_eq!(m.groups()[0].name, "Fibs");
    assert_eq!(m.renaming(), None);
    // Enter and blur both land: the second is a no-op.
    assert!(!m.commit_rename(&id, "Other"));
    assert_eq!(m.groups()[0].name, "Fibs");
}

#[test]
fn an_empty_name_keeps_the_old_one_but_ends_the_rename() {
    let mut m = ObjectTreeModel::new();
    let id = m.make_group(&strs(&["d1"]), None).expect("a group");
    m.begin_rename(&id);
    assert!(m.commit_rename(&id, "   "));
    assert_eq!(m.groups()[0].name, "Group 1");
    assert_eq!(m.renaming(), None);
    assert!(!m.commit_rename("ghost", "x"));
}

#[test]
fn removing_a_group_returns_its_drawings_only_when_they_go_with_it() {
    let mut m = ObjectTreeModel::new();
    let a = m.make_group(&strs(&["d1", "d2"]), None).expect("a group");
    m.set_picked(&strs(&["d1", "d9"]));
    m.toggle_collapsed(&a);
    assert!(m.is_collapsed(&a));
    assert!(m.remove_group(&a, false).is_empty());
    assert!(m.groups().is_empty() && !m.is_collapsed(&a));
    let b = m.make_group(&strs(&["d1", "d2"]), None).expect("a group");
    m.set_picked(&strs(&["d1", "d9"]));
    assert_eq!(m.remove_group(&b, true), ["d1", "d2"]);
    assert_eq!(m.picked(), ["d9"], "the deleted members leave the pick");
    assert!(m.remove_group("ghost", true).is_empty());
    assert!(m.toggle_collapsed("x"));
}

#[test]
fn a_clone_joins_its_originals_group_and_membership_is_exclusive() {
    let mut m = ObjectTreeModel::new();
    let a = m.make_group(&strs(&["d1"]), None).expect("a group");
    m.file_clone("d1", "d1-copy");
    m.file_clone("loose", "loose-copy");
    assert_eq!(m.groups()[0].ids, ["d1", "d1-copy"]);
    let b = m.make_group(&strs(&["d5"]), None).expect("a group");
    m.assign(&b, &strs(&["d1"]));
    assert_eq!(
        m.groups().iter().find(|g| g.id == a).map(|g| g.ids.clone()),
        Some(strs(&["d1-copy"]))
    );
    m.unassign(&strs(&["d1-copy"]));
    assert!(
        !m.groups().iter().any(|g| g.id == a),
        "emptied by the move, it goes"
    );
}

#[test]
fn pruning_forgets_what_the_chart_no_longer_has() {
    let mut m = ObjectTreeModel::new();
    let g = m.make_group(&strs(&["d1", "gone"]), None).expect("a group");
    m.set_picked(&strs(&["d1", "gone"]));
    m.begin_rename(&g);
    let live: HashSet<String> = strs(&["d1"]).into_iter().collect();
    m.prune(&live);
    assert_eq!(m.picked(), ["d1"]);
    assert_eq!(m.groups()[0].ids, ["d1"]);
    assert_eq!(m.renaming(), Some(g.as_str()));
    m.prune(&HashSet::new());
    assert!(m.groups().is_empty());
    assert_eq!(m.renaming(), None, "the renamed group is gone");
    // With no groups at all a stale rename is dropped too.
    let mut lone = ObjectTreeModel::new();
    lone.begin_rename("grp-9");
    lone.prune(&HashSet::new());
    assert_eq!(lone.renaming(), None);
}

// ── drag ──

#[test]
fn a_press_is_a_click_until_it_clears_the_slop() {
    let mut d = DragSession::new(
        DragKind::Drawing,
        Some("d1".into()),
        "price",
        "Trend Line",
        10.0,
        10.0,
    );
    assert_eq!(d.on_move(12.0, 12.0), None);
    assert!(!d.active());
    assert_eq!(
        d.on_move(10.0, 14.0),
        Some(true),
        "exactly the slop begins the drag"
    );
    assert!(d.active());
    assert_eq!(
        d.on_move(10.0, 11.0),
        Some(false),
        "once begun it stays a drag"
    );
}

#[test]
fn the_insertion_slot_is_the_first_midpoint_below_the_pointer() {
    let rects = [
        Rect::new(0.0, 0.0, 100.0, 20.0),
        Rect::new(0.0, 20.0, 100.0, 40.0),
        Rect::new(0.0, 40.0, 100.0, 60.0),
    ];
    assert_eq!(insertion_slot(&rects, 5.0), 0);
    assert_eq!(insertion_slot(&rects, 15.0), 1);
    assert_eq!(insertion_slot(&rects, 30.0), 2);
    assert_eq!(insertion_slot(&rects, 59.0), 3);
    assert_eq!(insertion_slot(&[], 10.0), 0);
}

fn geometry() -> (Vec<GapGeometry>, Vec<PaneGeometry>) {
    let unit_rect = |i: f64| Rect::new(0.0, i * 20.0, 100.0, i * 20.0 + 20.0);
    let main = PaneGeometry {
        pane_id: "price".into(),
        kind: PaneKind::Price,
        rect: Rect::new(0.0, 0.0, 100.0, 80.0),
        units: (0..4)
            .map(|i| UnitGeometry {
                rect: unit_rect(f64::from(i)),
                group_id: None,
                subrows: vec![],
            })
            .collect(),
    };
    let study = PaneGeometry {
        pane_id: "p1".into(),
        kind: PaneKind::Study,
        rect: Rect::new(0.0, 90.0, 100.0, 110.0),
        units: vec![UnitGeometry {
            rect: Rect::new(0.0, 90.0, 100.0, 110.0),
            group_id: None,
            subrows: vec![],
        }],
    };
    let gap = GapGeometry {
        before: Some("p1".into()),
        after: Some("price".into()),
        rect: Rect::new(0.0, 80.0, 100.0, 90.0),
    };
    (vec![gap], vec![main, study])
}

const STACKABLE: PassFlags = PassFlags {
    stackable: true,
    repanable: true,
    interleave: true,
};

fn drag(kind: DragKind, id: Option<&str>, from: &str) -> DragSession {
    DragSession::new(kind, id.map(str::to_string), from, "x", 0.0, 0.0)
}

#[test]
fn an_indicator_over_a_thin_band_between_blocks_opens_a_new_pane() {
    let (gaps, geo) = geometry();
    let ps = panes();
    let d = drag(DragKind::Indicator, Some("rsi"), "p1");
    // The bands win and reach 4px past their edges.
    assert_eq!(
        resolve_drop(&d, 50.0, 85.0, &gaps, &geo, &ps, STACKABLE),
        Some(Drop::NewPane {
            before: Some("p1".into()),
            after: Some("price".into())
        })
    );
    assert!(matches!(
        resolve_drop(&d, 50.0, 78.0, &gaps, &geo, &ps, STACKABLE),
        Some(Drop::NewPane { .. })
    ));
    // A drawing never opens a pane: over the band it finds no block either.
    assert_eq!(
        resolve_drop(
            &drag(DragKind::Drawing, Some("d1"), "price"),
            50.0,
            85.0,
            &gaps,
            &geo,
            &ps,
            STACKABLE
        ),
        None
    );
}

#[test]
fn a_drop_over_a_block_takes_the_slot_under_the_pointer() {
    let (gaps, geo) = geometry();
    let ps = panes();
    let d = drag(DragKind::Drawing, Some("d1"), "price");
    assert_eq!(
        resolve_drop(&d, 50.0, 35.0, &gaps, &geo, &ps, STACKABLE),
        Some(Drop::Slot {
            pane_id: "price".into(),
            slot: 2
        })
    );
    assert_eq!(
        resolve_drop(&d, 50.0, 100.0, &gaps, &geo, &ps, STACKABLE),
        Some(Drop::Slot {
            pane_id: "p1".into(),
            slot: 1
        })
    );
    assert_eq!(
        resolve_drop(&d, 50.0, 500.0, &gaps, &geo, &ps, STACKABLE),
        None
    );
}

#[test]
fn the_candles_never_leave_the_main_pane() {
    let (gaps, geo) = geometry();
    let ps = panes();
    let d = drag(DragKind::Price, None, "price");
    assert_eq!(
        resolve_drop(&d, 50.0, 100.0, &gaps, &geo, &ps, STACKABLE),
        None
    );
    assert!(matches!(
        resolve_drop(&d, 50.0, 30.0, &gaps, &geo, &ps, STACKABLE),
        Some(Drop::Slot { .. })
    ));
}

#[test]
fn without_editable_stacking_an_indicator_can_only_join_a_pane_whole() {
    let (gaps, geo) = geometry();
    let ps = panes();
    let flags = PassFlags {
        stackable: false,
        ..STACKABLE
    };
    let d = drag(DragKind::Indicator, Some("rsi"), "p1");
    assert_eq!(
        resolve_drop(&d, 50.0, 30.0, &gaps, &geo, &ps, flags),
        Some(Drop::Merge {
            pane_id: "price".into()
        })
    );
    assert_eq!(
        resolve_drop(&d, 50.0, 100.0, &gaps, &geo, &ps, flags),
        None,
        "its own pane: nothing to do"
    );
}

#[test]
fn a_lone_drawing_over_a_group_block_joins_it_at_a_member_slot() {
    let (gaps, mut geo) = geometry();
    geo[0].units[1] = UnitGeometry {
        rect: Rect::new(0.0, 20.0, 100.0, 60.0),
        group_id: Some("g1".into()),
        subrows: vec![
            Rect::new(0.0, 30.0, 100.0, 40.0),
            Rect::new(0.0, 40.0, 100.0, 50.0),
        ],
    };
    let ps = panes();
    let d = drag(DragKind::Drawing, Some("d1"), "price");
    assert_eq!(
        resolve_drop(&d, 50.0, 42.0, &gaps, &geo, &ps, STACKABLE),
        Some(Drop::IntoGroup {
            pane_id: "price".into(),
            group_id: "g1".into(),
            member_slot: 1
        })
    );
    // A whole group only ever drops at the top level.
    let g = drag(DragKind::Group, Some("g2"), "price");
    assert!(matches!(
        resolve_drop(&g, 50.0, 42.0, &gaps, &geo, &ps, STACKABLE),
        Some(Drop::Slot { .. })
    ));
}

#[test]
fn without_a_shared_draw_order_space_drawing_slots_stop_at_the_leading_drawings() {
    let (gaps, geo) = geometry();
    let ps = panes(); // items: d1, a, price, d2 -> the first series row is at index 1
    let flags = PassFlags {
        interleave: false,
        ..STACKABLE
    };
    let d = drag(DragKind::Drawing, Some("d2"), "price");
    assert_eq!(
        resolve_drop(&d, 50.0, 75.0, &gaps, &geo, &ps, flags),
        Some(Drop::Slot {
            pane_id: "price".into(),
            slot: 1
        })
    );
}

// ── plans ──

fn plan(model: &mut ObjectTreeModel, d: &DragSession, drop: &Drop) -> DropPlan {
    plan_drop(model, d, drop, &panes(), true, &live(), true)
}

#[test]
fn moving_a_token_up_renormalizes_the_whole_column() {
    let mut m = ObjectTreeModel::new();
    let d = drag(DragKind::Drawing, Some("d2"), "price");
    let DropPlan::Stack(p) = plan(
        &mut m,
        &d,
        &Drop::Slot {
            pane_id: "price".into(),
            slot: 0,
        },
    ) else {
        panic!("a stack plan");
    };
    // New front-first order: d2, d1, a, price.
    assert_eq!(p.writes.candle_z, Some(1.0));
    assert_eq!(
        p.writes.series,
        [ZEntry {
            id: "a".into(),
            z: 2.0
        }]
    );
    assert_eq!(
        p.drawing_patches,
        [
            DrawingPatch {
                id: "d2".into(),
                z_index: 4.0,
                pane_id: None
            },
            DrawingPatch {
                id: "d1".into(),
                z_index: 3.0,
                pane_id: None
            },
        ]
    );
    assert!(p.move_indicator.is_none());
}

#[test]
fn a_drop_that_changes_nothing_costs_no_undo_step() {
    let mut m = ObjectTreeModel::new();
    // d1 onto its own slot, at the top.
    let d = drag(DragKind::Drawing, Some("d1"), "price");
    assert_eq!(
        plan(
            &mut m,
            &d,
            &Drop::Slot {
                pane_id: "price".into(),
                slot: 0
            }
        ),
        DropPlan::None
    );
    assert_eq!(
        plan(
            &mut m,
            &d,
            &Drop::Slot {
                pane_id: "price".into(),
                slot: 1
            }
        ),
        DropPlan::None
    );
}

#[test]
fn a_drawing_dropped_in_another_pane_is_repaned() {
    let mut m = ObjectTreeModel::new();
    let d = drag(DragKind::Drawing, Some("d1"), "price");
    let DropPlan::Stack(p) = plan(
        &mut m,
        &d,
        &Drop::Slot {
            pane_id: "p1".into(),
            slot: 0,
        },
    ) else {
        panic!("a stack plan");
    };
    assert_eq!(p.pane_id, "p1");
    assert_eq!(
        p.drawing_patches[0],
        DrawingPatch {
            id: "d1".into(),
            z_index: 2.0,
            pane_id: Some("p1".into())
        }
    );
    assert_eq!(
        p.writes.series,
        [ZEntry {
            id: "rsi".into(),
            z: 1.0
        }]
    );
    assert_eq!(p.writes.candle_z, None, "a study pane has no candles");
}

#[test]
fn an_indicator_from_another_pane_moves_there_first() {
    let mut m = ObjectTreeModel::new();
    let d = drag(DragKind::Indicator, Some("rsi"), "p1");
    let DropPlan::Stack(p) = plan(
        &mut m,
        &d,
        &Drop::Slot {
            pane_id: "price".into(),
            slot: 0,
        },
    ) else {
        panic!("a stack plan");
    };
    assert_eq!(
        p.move_indicator,
        Some(("rsi".to_string(), IndicatorTarget::Price))
    );
    assert!(p.writes.series.iter().any(|s| s.id == "rsi"));
}

#[test]
fn the_candles_restack_and_a_renderer_without_editable_stacking_writes_no_series_order() {
    let mut m = ObjectTreeModel::new();
    let d = drag(DragKind::Price, None, "price");
    let DropPlan::Stack(p) = plan(
        &mut m,
        &d,
        &Drop::Slot {
            pane_id: "price".into(),
            slot: 0,
        },
    ) else {
        panic!("a stack plan");
    };
    assert_eq!(p.writes.candle_z, Some(4.0));
    let mut m2 = ObjectTreeModel::new();
    let d2 = drag(DragKind::Drawing, Some("d2"), "price");
    let DropPlan::Stack(plain) = plan_drop(
        &mut m2,
        &d2,
        &Drop::Slot {
            pane_id: "price".into(),
            slot: 0,
        },
        &panes(),
        true,
        &live(),
        false,
    ) else {
        panic!("a stack plan");
    };
    assert_eq!(plain.writes.candle_z, None);
    assert!(plain.writes.series.is_empty());
    assert_eq!(plain.drawing_patches.len(), 2);
}

#[test]
fn a_drawing_dropped_on_a_group_joins_it_and_at_the_top_level_leaves_it() {
    let mut m = ObjectTreeModel::new();
    let g = m.make_group(&strs(&["d2"]), None).expect("a group");
    let d = drag(DragKind::Drawing, Some("d1"), "price");
    plan(
        &mut m,
        &d,
        &Drop::IntoGroup {
            pane_id: "price".into(),
            group_id: g.clone(),
            member_slot: 0,
        },
    );
    assert_eq!(m.groups()[0].ids, ["d2", "d1"]);
    plan(
        &mut m,
        &d,
        &Drop::Slot {
            pane_id: "price".into(),
            slot: 0,
        },
    );
    assert_eq!(
        m.groups()[0].ids,
        ["d2"],
        "back at the top level it leaves the group"
    );
}

#[test]
fn drawings_need_a_renderer_that_supports_them() {
    let mut m = ObjectTreeModel::new();
    let d = drag(DragKind::Drawing, Some("d2"), "price");
    let none = plan_drop(
        &mut m,
        &d,
        &Drop::Slot {
            pane_id: "price".into(),
            slot: 0,
        },
        &panes(),
        false,
        &live(),
        true,
    );
    assert_eq!(none, DropPlan::None);
    // An unknown target pane or an id-less indicator drag plans nothing.
    assert_eq!(
        plan(
            &mut m,
            &d,
            &Drop::Slot {
                pane_id: "ghost".into(),
                slot: 0
            }
        ),
        DropPlan::None
    );
    let idless = drag(DragKind::Indicator, None, "p1");
    assert_eq!(
        plan(
            &mut m,
            &idless,
            &Drop::Slot {
                pane_id: "price".into(),
                slot: 0
            }
        ),
        DropPlan::None
    );
}

#[test]
fn pane_drops_move_indicators_unless_they_would_rebuild_their_own_pane() {
    let mut m = ObjectTreeModel::new();
    let d = drag(DragKind::Indicator, Some("rsi"), "p1");
    assert_eq!(
        plan(
            &mut m,
            &d,
            &Drop::Merge {
                pane_id: "price".into()
            }
        ),
        DropPlan::MoveIndicator {
            id: "rsi".into(),
            to: IndicatorTarget::Price
        }
    );
    assert_eq!(
        plan(
            &mut m,
            &d,
            &Drop::Merge {
                pane_id: "p1".into()
            }
        ),
        DropPlan::None
    );
    assert_eq!(
        plan(
            &mut m,
            &d,
            &Drop::NewPane {
                before: Some("price".into()),
                after: Some("zz".into())
            }
        ),
        DropPlan::MoveIndicator {
            id: "rsi".into(),
            to: IndicatorTarget::NewPane {
                before: Some("price".into()),
                after: Some("zz".into())
            }
        }
    );
    // Alone in p1, dropped on a band touching p1: nowhere new to go.
    assert_eq!(
        plan(
            &mut m,
            &d,
            &Drop::NewPane {
                before: Some("p1".into()),
                after: None
            }
        ),
        DropPlan::None
    );
    assert_eq!(
        plan(
            &mut m,
            &d,
            &Drop::NewPane {
                before: None,
                after: Some("p1".into())
            }
        ),
        DropPlan::None
    );
}

#[test]
fn a_group_drags_as_one_run_in_its_own_stacking() {
    let mut m = ObjectTreeModel::new();
    let g = m.make_group(&strs(&["d1", "d2"]), None).expect("a group");
    let d = drag(DragKind::Group, Some(&g), "price");
    assert_eq!(
        dragged_drawings(&d, m.groups(), &live()),
        ["d1", "d2"],
        "front-most first"
    );
    assert!(
        dragged_drawings(
            &drag(DragKind::Group, Some("ghost"), "price"),
            m.groups(),
            &live()
        )
        .is_empty()
    );
    assert_eq!(
        dragged_drawings(
            &drag(DragKind::Drawing, Some("d9"), "price"),
            m.groups(),
            &live()
        ),
        ["d9"]
    );
    assert!(
        dragged_drawings(&drag(DragKind::Price, None, "price"), m.groups(), &live()).is_empty()
    );
}

#[test]
fn restacking_a_group_moves_its_run_to_an_end_of_the_column() {
    let g = DrawGroup::new("g1", "G", &["d1", "d2"]);
    // Front-first d1, a, price, d2: the run to the back keeps its order.
    let back = plan_restack_group(&g, &panes(), &live(), false, true).expect("a plan");
    assert_eq!(
        back.writes.drawings,
        [
            ZEntry {
                id: "d1".into(),
                z: 2.0
            },
            ZEntry {
                id: "d2".into(),
                z: 1.0
            }
        ]
    );
    assert_eq!(back.writes.candle_z, Some(3.0));
    // To the front d2 joins d1 at the top.
    let front = plan_restack_group(&g, &panes(), &live(), true, true).expect("a plan");
    assert_eq!(
        front.writes.drawings,
        [
            ZEntry {
                id: "d1".into(),
                z: 4.0
            },
            ZEntry {
                id: "d2".into(),
                z: 3.0
            }
        ]
    );
    // A run that already leads the column needs no plan.
    let lead = DrawGroup::new("g3", "L", &["d1"]);
    assert!(plan_restack_group(&lead, &panes(), &live(), true, true).is_none());
    assert!(
        plan_restack_group(
            &DrawGroup::new("g2", "E", &[]),
            &panes(),
            &live(),
            true,
            true
        )
        .is_none()
    );
}

// ── menus ──

fn inputs<'a>(
    panes: &'a [TreePane],
    groups: &'a [DrawGroup],
    picked: &'a [String],
    z: &'a HashMap<String, Vec<f64>>,
) -> MenuInputs<'a> {
    MenuInputs {
        panes,
        groups,
        picked,
        stackable: true,
        panes_supported: true,
        supports_indicator_settings: true,
        series_order: &[],
        candle_z: 0.0,
        drawing_z_by_pane: z,
        i18n: None,
    }
}

fn labels(items: &[MenuItemDescriptor]) -> Vec<&str> {
    items.iter().map(|i| i.label.as_str()).collect()
}

#[test]
fn the_candles_menu_hides_and_restacks_beyond_the_extremes() {
    let ps = panes();
    let z = HashMap::from([("price".to_string(), vec![5.0, -3.0])]);
    let mut inp = inputs(&ps, &[], &[], &z);
    inp.series_order = &[];
    let mut b = MenuBuilder::new();
    let items = price_menu(&mut b, &inp, true);
    assert_eq!(labels(&items), ["Hide", "Bring to front", "Send to back"]);
    assert_eq!(
        b.commands[&items[0].id],
        TreeCommand::SetCandlesVisible(false)
    );
    assert_eq!(b.commands[&items[1].id], TreeCommand::SetCandleZ(6.0));
    assert_eq!(b.commands[&items[2].id], TreeCommand::SetCandleZ(-4.0));
    assert!(items[1].separator_before);
    let mut b2 = MenuBuilder::new();
    inp.stackable = false;
    assert_eq!(labels(&price_menu(&mut b2, &inp, false)), ["Show"]);
}

#[test]
fn an_indicator_menu_offers_its_surfaces_and_the_moves_that_change_something() {
    let ps = panes();
    let z = HashMap::new();
    let inp = inputs(&ps, &[], &[], &z);
    let mut b = MenuBuilder::new();
    let items = indicator_menu(&mut b, &inp, "a", true, "price");
    assert_eq!(
        labels(&items),
        [
            "Hide",
            "Indicator settings",
            "Move to",
            "Bring to front",
            "Send to back",
            "Remove"
        ]
    );
    let moves = items[2].submenu.as_ref().expect("a submenu");
    // From the main pane: the study pane, then a new pane below (none above the main pane).
    assert_eq!(labels(moves), ["p1", "New panel below"]);
    assert_eq!(
        b.commands[&moves[1].id],
        TreeCommand::MoveIndicator {
            id: "a".into(),
            to: IndicatorTarget::NewPane {
                before: None,
                after: Some("price".into())
            }
        }
    );
    // An indicator alone in its own study pane has nowhere new to go but the other panes.
    let mut b2 = MenuBuilder::new();
    let alone = move_items(&mut b2, &inp, "rsi", "p1");
    assert_eq!(labels(&alone), ["price"]);
    let mut b3 = MenuBuilder::new();
    let from_study = indicator_menu(
        &mut b3,
        &MenuInputs {
            panes_supported: false,
            supports_indicator_settings: false,
            ..inp
        },
        "a",
        false,
        "price",
    );
    assert_eq!(
        labels(&from_study),
        ["Show", "Bring to front", "Send to back", "Remove"]
    );
}

#[test]
fn a_loose_drawing_offers_new_group_and_adding_to_an_existing_one() {
    let ps = panes();
    let z = HashMap::new();
    let groups = vec![DrawGroup::new("g1", "Fibs", &["d9"])];
    let inp = inputs(&ps, &groups, &[], &z);
    let mut b = MenuBuilder::new();
    let items = drawing_menu(&mut b, &inp, &draw("d1", "price", 1.0));
    assert_eq!(
        labels(&items),
        [
            "Hide",
            "Lock",
            "Duplicate",
            "Bring to front",
            "Send to back",
            "New group",
            "Add to group",
            "Remove"
        ]
    );
    let add = items
        .iter()
        .find(|i| i.label == "Add to group")
        .expect("the submenu");
    assert!(
        !add.separator_before,
        "the group entry above already separates"
    );
    assert_eq!(labels(add.submenu.as_ref().expect("members")), ["Fibs"]);
}

#[test]
fn a_grouped_drawing_offers_moving_between_groups_and_leaving_its_own() {
    let ps = panes();
    let z = HashMap::new();
    let groups = vec![
        DrawGroup::new("g1", "Fibs", &["d1"]),
        DrawGroup::new("g2", "Lines", &["d9"]),
    ];
    let inp = inputs(&ps, &groups, &[], &z);
    let mut b = MenuBuilder::new();
    let items = grouping_items(&mut b, &inp, "d1");
    assert_eq!(labels(&items), ["Move to group", "Remove from Fibs"]);
    assert!(
        items[0].separator_before,
        "nothing above it: it opens the section"
    );
    assert!(!items[1].separator_before);
}

#[test]
fn a_multi_pick_groups_as_a_whole_and_offers_no_per_row_moves() {
    let ps = panes();
    let z = HashMap::new();
    let groups = vec![DrawGroup::new("g1", "Fibs", &["d9"])];
    let picked = strs(&["d1", "d2"]);
    let inp = inputs(&ps, &groups, &picked, &z);
    let mut b = MenuBuilder::new();
    let items = grouping_items(&mut b, &inp, "d1");
    assert_eq!(labels(&items), ["Group selection (2)"]);
    assert_eq!(
        b.commands[&items[0].id],
        TreeCommand::MakeGroup(picked.clone())
    );
    // A pick that includes a grouped drawing cannot form a group.
    let mixed = strs(&["d1", "d9"]);
    let inp2 = inputs(&ps, &groups, &mixed, &z);
    assert!(grouping_items(&mut MenuBuilder::new(), &inp2, "d1").is_empty());
    // A row outside the multi-pick acts on itself alone.
    assert_eq!(
        labels(&grouping_items(&mut MenuBuilder::new(), &inp, "d3")),
        ["New group", "Add to group"]
    );
}

#[test]
fn a_group_menu_reads_the_aggregate_state_and_an_empty_group_only_renames_or_removes() {
    let ps = panes();
    let z = HashMap::new();
    let inp = inputs(&ps, &[], &[], &z);
    let g = DrawGroup::new("g1", "Fibs", &["d1", "d2"]);
    let mut hidden = draw("d1", "price", 1.0);
    hidden.visible = false;
    let mut b = MenuBuilder::new();
    let items = group_menu(
        &mut b,
        &inp,
        &g,
        &[hidden.clone(), draw("d2", "price", 2.0)],
    );
    assert_eq!(
        labels(&items),
        [
            "Rename\u{2026}",
            "Hide all",
            "Lock all",
            "Select all",
            "Bring all to front",
            "Send all to back",
            "Ungroup",
            "Remove all"
        ]
    );
    let mut b2 = MenuBuilder::new();
    let mut other = draw("d2", "price", 2.0);
    other.visible = false;
    let all_hidden = group_menu(&mut b2, &inp, &g, &[hidden, other]);
    assert_eq!(all_hidden[1].label, "Show all");
    assert_eq!(
        b2.commands[&all_hidden[1].id],
        TreeCommand::SetMembersVisible {
            ids: strs(&["d1", "d2"]),
            visible: true
        }
    );
    let empty = DrawGroup::new("g2", "New", &[]);
    let only = group_menu(&mut MenuBuilder::new(), &inp, &empty, &[]);
    assert_eq!(labels(&only), ["Rename\u{2026}", "Remove"]);
    // No live members: no restack rows.
    let no_members = group_menu(&mut MenuBuilder::new(), &inp, &g, &[]);
    assert!(!labels(&no_members).contains(&"Bring all to front"));
}

#[test]
fn menu_ids_are_unique_across_nesting_levels() {
    let ps = panes();
    let z = HashMap::new();
    let inp = inputs(&ps, &[], &[], &z);
    let mut b = MenuBuilder::new();
    let items = indicator_menu(&mut b, &inp, "a", true, "price");
    let mut ids: Vec<String> = Vec::new();
    for i in &items {
        ids.push(i.id.clone());
        for s in i.submenu.iter().flatten() {
            ids.push(s.id.clone());
        }
    }
    let unique: HashSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len());
}

#[test]
fn lookups_find_indicator_rows_and_group_units() {
    let ps = panes();
    let (pane, row) = find_indicator_row(&ps, "rsi").expect("the row");
    assert_eq!((pane.id.as_str(), row.id.as_str()), ("p1", "rsi"));
    assert!(find_indicator_row(&ps, "ghost").is_none());
    let grouped = tree_pane(
        "price",
        PaneKind::Price,
        vec![TreeItem::Unit(DrawUnit::Group {
            group: DrawGroup::new("g1", "G", &["d1"]),
            members: vec![draw("d1", "price", 1.0)],
        })],
    );
    assert!(group_unit(&grouped, "g1").is_some() && group_unit(&grouped, "g2").is_none());
}

#[test]
fn the_labels_read_in_english() {
    assert_eq!(TreeText::ObjectTree.text(None), "Object tree");
    assert_eq!(text_of(None, &object_tree_title(), &[]), "Object tree");
    assert_eq!(group_drawings_label(1, None), "Group this drawing");
    assert_eq!(group_drawings_label(3, None), "Group these drawings");
    assert_eq!(selected_label(4, None), "4 selected");
    assert_eq!(
        group_members_label(1, None),
        "1 drawing \u{2014} drag to move it"
    );
    assert_eq!(
        group_members_label(2, None),
        "2 drawings \u{2014} drag to move them together"
    );
    assert_eq!(REFRESH_EVENTS.len(), 8);
}
