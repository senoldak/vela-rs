//! The object tree's layout rules: pane grouping, the unified front-first stack (drawings +
//! indicators + candles in one z-ordered column), pane naming, per-pane drawing ownership
//! and group folding. Pure functions over plain values.

use std::collections::HashSet;

use serde_json::json;

use super::*;
use crate::drawings::registry::DrawingRegistry;

fn pane(
    id: &str,
    kind: PaneKind,
    order: usize,
    indicators: &[(&str, Option<&str>, Option<&str>, bool)],
) -> PaneInfo {
    PaneInfo {
        id: id.into(),
        kind,
        order,
        collapsed: false,
        maximized: false,
        height_weight: None,
        indicators: indicators
            .iter()
            .map(|(id, title, short, own)| PaneIndicatorInfo {
                id: (*id).into(),
                title: title.unwrap_or(id).to_string(),
                shorttitle: short.map(str::to_string),
                own_scale: *own,
            })
            .collect(),
    }
}

/// An indicator spec with just an id.
fn ids(
    list: &[&'static str],
) -> Vec<(
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    bool,
)> {
    list.iter().map(|id| (*id, None, None, false)).collect()
}

fn draw(id: &str, pane_id: &str, z: f64) -> SerializedDrawing {
    serde_json::from_value(json!({
        "id": id,
        "type": "trendline",
        "paneId": pane_id,
        "anchors": [],
        "style": { "lineColor": "#fff", "lineWidth": 1, "lineStyle": "solid" },
        "locked": false,
        "visible": true,
        "zIndex": z,
        "createdAt": 0,
    }))
    .expect("a valid drawing")
}

fn draw_with(id: &str, pane_id: &str, z: f64, patch: serde_json::Value) -> SerializedDrawing {
    let mut value = serde_json::to_value(draw(id, pane_id, z)).expect("serializes");
    for (k, v) in patch.as_object().expect("an object") {
        value[k] = v.clone();
    }
    serde_json::from_value(value).expect("a valid drawing")
}

struct Rig {
    panes: Vec<PaneInfo>,
    stackable: bool,
    interleave: bool,
    z_order: Vec<ZEntry>,
    candle_z: f64,
    drawings: Vec<SerializedDrawing>,
    groups: Vec<DrawGroup>,
    visible: fn(&str) -> bool,
    handle_title: fn(&str) -> Option<String>,
}

impl Rig {
    fn new() -> Self {
        Self {
            panes: vec![pane("price", PaneKind::Price, 0, &[])],
            stackable: true,
            interleave: true,
            z_order: Vec::new(),
            candle_z: 0.0,
            drawings: Vec::new(),
            groups: Vec::new(),
            visible: |_| true,
            handle_title: |_| None,
        }
    }

    fn z(mut self, entries: &[(&str, f64)]) -> Self {
        self.z_order = entries
            .iter()
            .map(|(id, z)| ZEntry {
                id: (*id).into(),
                z: *z,
            })
            .collect();
        self
    }

    fn tree(&self) -> Vec<TreePane> {
        let snap = TreeSnapshot {
            panes: &self.panes,
            i18n: None,
            registry: None,
            indicator_visible: &self.visible,
            handle_title: &self.handle_title,
            stackable: self.stackable,
            interleave: self.interleave,
            z_order: &self.z_order,
            candle_z: self.candle_z,
            price_label: "BTCUSDT".into(),
            price_visible: true,
            drawings: &self.drawings,
            groups: &self.groups,
        };
        build_tree(&snap)
    }
}

fn token_names(tokens: &[StackToken]) -> Vec<String> {
    tokens
        .iter()
        .map(|t| t.id().map_or_else(|| "price".to_string(), str::to_string))
        .collect()
}

/// A pane's stack flattened to readable names: drawings by id, indicators by id, the candles
/// as `price`: one name per z key, front-most first.
fn names(p: &TreePane) -> Vec<String> {
    token_names(&pane_tokens(p))
}

fn dw(id: &str) -> StackToken {
    StackToken::Drawing(id.into())
}
fn ind(id: &str) -> StackToken {
    StackToken::Indicator(id.into())
}
const PRICE: StackToken = StackToken::Price;

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

// ── buildTree: the unified stack ──

#[test]
fn orders_the_candles_among_the_overlays_by_z_front_most_first() {
    let mut rig = Rig::new().z(&[("a", -1.0), ("b", 5.0)]);
    rig.panes = vec![pane("price", PaneKind::Price, 0, &ids(&["a", "b"]))];
    // b (z 5) is in front of the candles (z 0), which are in front of a (z -1).
    assert_eq!(names(&rig.tree()[0]), ["b", "price", "a"]);
}

#[test]
fn on_a_shared_key_it_follows_the_paint_order_z_order_lists() {
    // z_order comes back to front as the indicators paint: b paints after a, so b is in
    // front, and both paint after the candles that share their key.
    let mut rig = Rig::new().z(&[("a", 0.0), ("b", 0.0)]);
    rig.panes = vec![pane("price", PaneKind::Price, 0, &ids(&["a", "b"]))];
    assert_eq!(names(&rig.tree()[0]), ["b", "a", "price"]);
}

#[test]
fn interleaves_drawings_into_the_column_by_their_z() {
    let mut rig = Rig::new().z(&[("a", -2.0), ("b", 3.0)]);
    rig.panes = vec![pane("price", PaneKind::Price, 0, &ids(&["a", "b"]))];
    rig.drawings = vec![
        draw("deep", "price", -9.0),
        draw("mid", "price", -1.0),
        draw("top", "price", 7.0),
    ];
    assert_eq!(
        names(&rig.tree()[0]),
        ["top", "b", "price", "mid", "a", "deep"]
    );
}

#[test]
fn a_drawing_tying_a_series_z_paints_under_it() {
    let mut rig = Rig::new().z(&[("a", 4.0)]);
    rig.panes = vec![pane("price", PaneKind::Price, 0, &ids(&["a"]))];
    rig.drawings = vec![draw("tied", "price", 0.0)];
    assert_eq!(names(&rig.tree()[0]), ["a", "price", "tied"]);
}

#[test]
fn without_a_shared_draw_order_space_the_drawings_lead() {
    let mut rig = Rig::new().z(&[("a", 1.0)]);
    rig.panes = vec![pane("price", PaneKind::Price, 0, &ids(&["a"]))];
    rig.drawings = vec![draw("low", "price", -9.0)]; // would sit at the back if interleaved
    rig.interleave = false;
    assert_eq!(names(&rig.tree()[0]), ["low", "a", "price"]);
}

#[test]
fn without_editable_stacking_the_candles_lead_and_overlays_follow_in_pane_order() {
    let mut rig = Rig::new();
    rig.panes = vec![pane("price", PaneKind::Price, 0, &ids(&["a", "b"]))];
    rig.stackable = false;
    rig.interleave = false;
    assert_eq!(names(&rig.tree()[0]), ["price", "a", "b"]);
}

#[test]
fn study_panes_stack_too() {
    let mut rig = Rig::new().z(&[("rsi", 0.0)]);
    rig.panes = vec![
        pane("price", PaneKind::Price, 0, &[]),
        pane(
            "p1",
            PaneKind::Study,
            1,
            &[("rsi", Some("RSI 14"), None, false)],
        ),
    ];
    rig.drawings = vec![draw("over", "p1", 5.0), draw("under", "p1", -5.0)];
    let tree = rig.tree();
    assert_eq!(tree[1].label, "RSI 14");
    assert_eq!(names(&tree[1]), ["over", "rsi", "under"]);
}

#[test]
fn indicator_rows_carry_visibility_and_the_own_scale_flag_titled_from_the_pane_model() {
    let mut rig = Rig::new();
    rig.panes = vec![pane(
        "price",
        PaneKind::Price,
        0,
        &[("a", Some("SMA 20"), None, true)],
    )];
    rig.visible = |id| id != "a";
    rig.handle_title = |_| Some("Indicator".into());
    let tree = rig.tree();
    let row = pane_rows(&tree[0])
        .into_iter()
        .find_map(|r| match r {
            TreeRow::Indicator(i) => Some(i.clone()),
            TreeRow::Price(_) => None,
        })
        .expect("an indicator row");
    assert_eq!(
        row,
        IndicatorRow {
            id: "a".into(),
            label: "SMA 20".into(),
            visible: false,
            own_scale: true
        }
    );
}

#[test]
fn an_indicator_row_wears_its_compact_name_while_the_pane_keeps_the_full_title() {
    let mut rig = Rig::new().z(&[("rsi", 0.0)]);
    rig.panes = vec![
        pane("price", PaneKind::Price, 0, &[]),
        pane(
            "p1",
            PaneKind::Study,
            1,
            &[("rsi", Some("Relative Strength Index"), Some("RSI"), false)],
        ),
    ];
    let tree = rig.tree();
    let label = pane_rows(&tree[1]).into_iter().find_map(|r| match r {
        TreeRow::Indicator(i) => Some(i.label.clone()),
        TreeRow::Price(_) => None,
    });
    assert_eq!(label.as_deref(), Some("RSI"));
    assert_eq!(tree[1].label, "Relative Strength Index");
}

#[test]
fn falls_back_to_the_handle_title_then_the_id_when_the_pane_model_has_no_title() {
    let mut rig = Rig::new();
    rig.panes = vec![pane(
        "price",
        PaneKind::Price,
        0,
        &[("a", Some(""), None, false), ("b", Some(""), None, false)],
    )];
    rig.handle_title = |id| (id == "a").then(|| "My RSI".to_string());
    let tree = rig.tree();
    let labels: Vec<String> = pane_rows(&tree[0])
        .into_iter()
        .filter_map(|r| match r {
            TreeRow::Indicator(i) => Some(i.label.clone()),
            TreeRow::Price(_) => None,
        })
        .collect();
    assert!(labels.contains(&"My RSI".to_string()) && labels.contains(&"b".to_string()));
}

#[test]
fn a_group_surfaces_at_its_front_most_members_slot_members_contiguous_under_it() {
    let mut rig = Rig::new().z(&[("a", 0.0)]);
    rig.panes = vec![pane("price", PaneKind::Price, 0, &ids(&["a"]))];
    rig.drawings = vec![
        draw("m1", "price", -5.0),
        draw("m2", "price", 5.0),
        draw("loose", "price", 9.0),
    ];
    rig.groups = vec![DrawGroup::new("g1", "G", &["m1", "m2"])];
    let tree = rig.tree();
    // The bundle sits where m2 (front-most member, z 5) sits, carrying m1 with it.
    assert_eq!(names(&tree[0]), ["loose", "m2", "m1", "a", "price"]);
    assert!(matches!(
        tree[0].items[1],
        TreeItem::Unit(DrawUnit::Group { .. })
    ));
}

#[test]
fn a_group_started_empty_leads_the_main_chart() {
    for interleave in [true, false] {
        let mut rig = Rig::new().z(&[("a", 20.0)]);
        rig.panes = vec![pane("price", PaneKind::Price, 0, &ids(&["a"]))];
        rig.drawings = vec![draw("d1", "price", 9.0)];
        rig.groups = vec![DrawGroup {
            pane: Some("price".into()),
            ..DrawGroup::new("g1", "Group 1", &[])
        }];
        rig.interleave = interleave;
        let tree = rig.tree();
        match &tree[0].items[0] {
            TreeItem::Unit(DrawUnit::Group { group, members }) => {
                assert_eq!(group.id, "g1");
                assert!(members.is_empty());
            }
            other => panic!("expected the empty group first, got {other:?}"),
        }
        // It holds no drawings, so the stack itself is unchanged.
        let expected = if interleave {
            ["a", "d1", "price"]
        } else {
            ["d1", "a", "price"]
        };
        assert_eq!(names(&tree[0]), expected);
    }
}

#[test]
fn a_chart_holding_only_an_empty_group_is_not_an_empty_tree() {
    let mut rig = Rig::new();
    rig.groups = vec![DrawGroup::new("g1", "Group 1", &[])];
    assert!(!tree_is_empty(&rig.tree()));
}

#[test]
fn the_tree_is_empty_only_when_nothing_but_the_candles_is_on_the_chart() {
    assert!(tree_is_empty(&Rig::new().tree()));
    let mut with_indicator = Rig::new();
    with_indicator.panes = vec![pane("price", PaneKind::Price, 0, &ids(&["sma"]))];
    assert!(!tree_is_empty(&with_indicator.tree()));
    let mut with_drawing = Rig::new();
    with_drawing.drawings = vec![draw("d1", "price", 1.0)];
    assert!(!tree_is_empty(&with_drawing.tree()));
}

// ── tokens ──

#[test]
fn flattens_a_pane_to_one_token_per_z_key_a_group_contributing_one_per_member() {
    let mut rig = Rig::new().z(&[("a", 1.0)]);
    rig.panes = vec![pane("price", PaneKind::Price, 0, &ids(&["a"]))];
    rig.drawings = vec![draw("m1", "price", 4.0), draw("m2", "price", 5.0)];
    rig.groups = vec![DrawGroup::new("g1", "G", &["m1", "m2"])];
    let tree = rig.tree();
    assert_eq!(names(&tree[0]), ["m2", "m1", "a", "price"]);
    // Rendered items: [group, a, price]: slot 1 (after the group) is token index 2.
    assert_eq!(token_index_of_slot(&tree[0], 0), 0);
    assert_eq!(token_index_of_slot(&tree[0], 1), 2);
    assert_eq!(token_index_of_slot(&tree[0], 3), 4);
    assert_eq!(token_index_of_slot(&tree[0], 99), 4);
}

#[test]
fn same_token_matches_by_kind_and_id() {
    assert!(same_token(&PRICE, &StackToken::Price));
    assert!(same_token(&ind("a"), &ind("a")));
    assert!(!same_token(&ind("a"), &dw("a")));
    assert!(!same_token(&ind("a"), &PRICE));
}

#[test]
fn a_drop_inside_a_group_lands_at_the_member_it_points_at_clamped_past_the_last() {
    let mut rig = Rig::new().z(&[("a", 1.0)]);
    rig.panes = vec![pane("price", PaneKind::Price, 0, &ids(&["a"]))];
    rig.drawings = vec![draw("m1", "price", 4.0), draw("m2", "price", 5.0)];
    rig.groups = vec![DrawGroup::new("g1", "G", &["m1", "m2"])];
    let tree = rig.tree();
    // Tokens: m2, m1, a, price: the group's run starts at 0.
    assert_eq!(group_token_index(&tree[0], "g1", 0), 0);
    assert_eq!(group_token_index(&tree[0], "g1", 1), 1);
    assert_eq!(group_token_index(&tree[0], "g1", 9), 2); // past the last member: after the run
    assert_eq!(group_token_index(&tree[0], "gone", 0), 4); // unknown group: end of the stack
}

// ── placeTokens ──

fn stack() -> Vec<StackToken> {
    // Front-first: d1, a, price, d2.
    vec![dw("d1"), ind("a"), PRICE, dw("d2")]
}

#[test]
fn moves_a_token_up_to_the_index_the_pointer_picked() {
    assert_eq!(
        token_names(&place_tokens(&stack(), &[dw("d2")], 0)),
        ["d2", "d1", "a", "price"]
    );
    assert_eq!(
        token_names(&place_tokens(&stack(), &[dw("d2")], 2)),
        ["d1", "a", "d2", "price"]
    );
}

#[test]
fn moves_a_token_down_accounting_for_the_gap_it_leaves_behind() {
    // Index 4 is "after d2" as rendered; with d1 lifted out, that is the end of the list.
    assert_eq!(
        token_names(&place_tokens(&stack(), &[dw("d1")], 4)),
        ["a", "price", "d2", "d1"]
    );
    assert_eq!(
        token_names(&place_tokens(&stack(), &[dw("d1")], 3)),
        ["a", "price", "d1", "d2"]
    );
}

#[test]
fn dropping_a_token_back_onto_its_own_edges_is_a_no_op() {
    assert_eq!(
        token_names(&place_tokens(&stack(), &[PRICE], 2)),
        ["d1", "a", "price", "d2"]
    );
    assert_eq!(
        token_names(&place_tokens(&stack(), &[PRICE], 3)),
        ["d1", "a", "price", "d2"]
    );
}

#[test]
fn a_groups_run_moves_as_one_contiguous_block() {
    let s = vec![dw("m1"), dw("m2"), ind("a"), PRICE];
    assert_eq!(
        token_names(&place_tokens(&s, &[dw("m1"), dw("m2")], 4)),
        ["a", "price", "m1", "m2"]
    );
    assert_eq!(
        token_names(&place_tokens(&s, &[dw("m1"), dw("m2")], 3)),
        ["a", "m1", "m2", "price"]
    );
}

#[test]
fn inserts_tokens_that_were_not_in_the_stack() {
    assert_eq!(
        token_names(&place_tokens(&stack(), &[dw("new")], 3)),
        ["d1", "a", "price", "new", "d2"]
    );
}

#[test]
fn clamps_an_index_beyond_either_end() {
    assert_eq!(
        token_names(&place_tokens(&stack(), &[dw("d2")], 99)),
        ["d1", "a", "price", "d2"]
    );
    assert_eq!(
        token_names(&place_tokens(&stack(), &[dw("d1")], -5)),
        ["d1", "a", "price", "d2"]
    );
}

#[test]
fn tokens_equal_spots_the_unchanged_stack_a_no_op_guard_needs() {
    assert!(tokens_equal(&stack(), &place_tokens(&stack(), &[PRICE], 3)));
    assert!(!tokens_equal(
        &stack(),
        &place_tokens(&stack(), &[PRICE], 0)
    ));
    assert!(!tokens_equal(&stack(), &stack()[..2]));
}

// ── stackWrites / zStackBounds ──

fn z(id: &str, z: f64) -> ZEntry {
    ZEntry { id: id.into(), z }
}

#[test]
fn turns_a_front_first_stack_into_descending_z_keys_routed_per_kind() {
    let placed = vec![dw("top"), ind("a"), PRICE, dw("deep")];
    assert_eq!(
        stack_writes(&placed),
        StackWrites {
            candle_z: Some(2.0),
            series: vec![z("a", 3.0)],
            drawings: vec![z("top", 4.0), z("deep", 1.0)],
        }
    );
}

#[test]
fn a_study_pane_has_no_candles_to_write() {
    assert_eq!(
        stack_writes(&[dw("d"), ind("rsi")]),
        StackWrites {
            candle_z: None,
            series: vec![z("rsi", 1.0)],
            drawings: vec![z("d", 2.0)]
        }
    );
    assert_eq!(
        stack_writes(&[]),
        StackWrites {
            candle_z: None,
            series: vec![],
            drawings: vec![]
        }
    );
}

#[test]
fn z_bounds_span_the_overlays_and_the_candles_always_including_zero() {
    assert_eq!(
        z_stack_bounds(&[z("a", 3.0), z("b", -2.0)], 1.0, &[]),
        (3.0, -2.0)
    );
    assert_eq!(z_stack_bounds(&[], 0.0, &[]), (0.0, 0.0));
    // The candles alone can define an extreme.
    assert_eq!(z_stack_bounds(&[z("a", 1.0)], 9.0, &[]), (9.0, 0.0));
    assert_eq!(z_stack_bounds(&[z("a", 1.0)], -9.0, &[]), (1.0, -9.0));
}

#[test]
fn z_bounds_fold_in_drawing_z_keys_when_they_share_the_space() {
    assert_eq!(
        z_stack_bounds(&[z("a", 3.0)], 0.0, &[7.0, -4.0]),
        (7.0, -4.0)
    );
}

// ── paneLabel ──

#[test]
fn names_the_price_pane_the_main_chart() {
    let p = pane("price", PaneKind::Price, 0, &[]);
    assert_eq!(pane_label(&p, &|_| None, None), price_pane_label());
    assert_eq!(price_pane_label(), "Main chart");
}

#[test]
fn a_study_pane_borrows_the_title_of_the_indicator_owning_its_scale() {
    let p = pane(
        "p1",
        PaneKind::Study,
        1,
        &[
            ("own", Some("Volume"), None, true),
            ("master", Some("RSI 14"), None, false),
        ],
    );
    assert_eq!(pane_label(&p, &|_| None, None), "RSI 14");
    // With only own-scale indicators the first one names the pane.
    let only_own = pane(
        "p1",
        PaneKind::Study,
        1,
        &[("own", Some("Volume"), None, true)],
    );
    assert_eq!(pane_label(&only_own, &|_| None, None), "Volume");
}

#[test]
fn falls_back_to_the_handle_title_then_to_the_slot_number_when_the_pane_is_empty() {
    let untitled = pane("p1", PaneKind::Study, 1, &[("x", Some(""), None, false)]);
    assert_eq!(
        pane_label(&untitled, &|_| Some("MACD".into()), None),
        "MACD"
    );
    assert_eq!(
        pane_label(&pane("p2", PaneKind::Study, 2, &[]), &|_| None, None),
        "Panel 3"
    );
}

// ── paneDrawings ──

fn pane_set(list: &[&str]) -> HashSet<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn draw_ids(list: &[SerializedDrawing]) -> Vec<&str> {
    list.iter().map(|d| d.id.as_str()).collect()
}

#[test]
fn lists_a_panes_own_drawings_front_most_first() {
    let all = vec![
        draw("d1", "p1", 1.0),
        draw("d2", "p1", 2.0),
        draw("d3", "price", 3.0),
    ];
    let ids = pane_set(&["price", "p1"]);
    assert_eq!(draw_ids(&pane_drawings(&all, "p1", &ids)), ["d2", "d1"]);
}

#[test]
fn folds_orphans_into_the_price_pane() {
    let all = vec![draw("d1", "price", 1.0), draw("orphan", "gone", 2.0)];
    let ids = pane_set(&["price", "p1"]);
    assert_eq!(
        draw_ids(&pane_drawings(&all, "price", &ids)),
        ["orphan", "d1"]
    );
    assert!(pane_drawings(&all, "p1", &ids).is_empty());
}

// ── drawingUnits ──

#[test]
fn a_lone_drawing_is_its_own_unit() {
    let units = drawing_units(&[draw("d1", "price", 1.0)], &[]);
    match units.as_slice() {
        [DrawUnit::Draw(d)] => assert_eq!(d.id, "d1"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn a_group_surfaces_once_at_its_front_most_members_slot_holding_every_member() {
    let front = vec![
        draw("d3", "price", 3.0),
        draw("d2", "price", 2.0),
        draw("d1", "price", 1.0),
    ];
    let groups = vec![DrawGroup::new("g1", "Group 1", &["d1", "d3"])];
    let units = drawing_units(&front, &groups);
    assert_eq!(units.len(), 2);
    match &units[0] {
        DrawUnit::Group { members, .. } => assert_eq!(draw_ids(members), ["d3", "d1"]),
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(&units[1], DrawUnit::Draw(d) if d.id == "d2"));
}

#[test]
fn an_empty_group_shows_in_its_own_panel_or_in_the_main_chart_once_that_panel_is_gone() {
    let mk = |id: &str, ids: &[&str], pane: Option<&str>| DrawGroup {
        pane: pane.map(str::to_string),
        ..DrawGroup::new(id, id, ids)
    };
    let groups = vec![
        mk("here", &[], Some("p2")),
        mk("gone", &[], Some("p9")),
        mk("main", &[], None),
        mk("full", &["d1"], Some("p2")),
    ];
    let pane_ids = pane_set(&["price", "p2"]);
    let in_p2: Vec<&str> = empty_groups_in(&groups, "p2", &pane_ids)
        .iter()
        .map(|g| g.id.as_str())
        .collect();
    assert_eq!(in_p2, ["here"]);
    let in_price: Vec<&str> = empty_groups_in(&groups, "price", &pane_ids)
        .iter()
        .map(|g| g.id.as_str())
        .collect();
    assert_eq!(in_price, ["gone", "main"]);
}

#[test]
fn group_of_finds_a_drawings_bundle_or_nothing() {
    let groups = vec![DrawGroup::new("g1", "G", &["d1"])];
    assert_eq!(group_of(&groups, "d1").map(|g| g.id.as_str()), Some("g1"));
    assert!(group_of(&groups, "d2").is_none());
}

// ── group state and membership ──

fn g(id: &str, name: &str, ids: &[&str]) -> DrawGroup {
    DrawGroup::new(id, name, ids)
}

#[test]
fn a_group_reads_as_hidden_or_locked_only_when_every_member_is() {
    let shown = draw("d1", "price", 1.0);
    let hidden = draw_with("d2", "price", 2.0, json!({ "visible": false }));
    let locked = draw_with("d3", "price", 3.0, json!({ "locked": true }));
    assert_eq!(
        group_state(&[hidden.clone(), hidden.clone()]),
        (true, false)
    );
    assert_eq!(
        group_state(&[hidden.clone(), shown.clone()]),
        (false, false)
    );
    assert_eq!(
        group_state(&[locked.clone(), locked.clone()]),
        (false, true)
    );
    assert_eq!(group_state(&[locked, shown]), (false, false));
    // An empty bundle is neither: there is nothing to report on.
    assert_eq!(group_state(&[]), (false, false));
}

#[test]
fn only_an_ungrouped_non_empty_selection_can_form_a_new_group() {
    let groups = vec![g("g1", "Group 1", &["d1"])];
    assert!(!can_group(&[], &groups));
    assert!(can_group(&strings(&["d2", "d3"]), &groups));
    assert!(!can_group(&strings(&["d1", "d2"]), &groups));
}

#[test]
fn names_a_new_group_after_the_lowest_number_still_free() {
    assert_eq!(next_group_name(&[], None), "Group 1");
    assert_eq!(
        next_group_name(&[g("a", "Group 1", &["d1"])], None),
        "Group 2"
    );
    // A hole left by a deleted group gets reused rather than skipped.
    assert_eq!(
        next_group_name(
            &[g("a", "Group 2", &["d1"]), g("b", "Group 3", &["d2"])],
            None
        ),
        "Group 1"
    );
    // Renamed groups don't block the numbering.
    assert_eq!(next_group_name(&[g("a", "Fibs", &["d1"])], None), "Group 1");
}

#[test]
fn pruning_drops_dead_members_and_the_groups_left_empty_by_them() {
    let groups = vec![
        g("g1", "Group 1", &["d1", "gone"]),
        g("g2", "Group 2", &["dead"]),
    ];
    assert_eq!(
        prune_groups(&groups, &pane_set(&["d1"])),
        [g("g1", "Group 1", &["d1"])]
    );
}

#[test]
fn removing_a_drawing_from_its_group_leaves_the_others_alone_and_empties_disappear() {
    let groups = vec![
        g("g1", "Group 1", &["d1", "d2"]),
        g("g2", "Group 2", &["d3"]),
    ];
    assert_eq!(
        remove_from_groups(&groups, &strings(&["d1"])),
        [g("g1", "Group 1", &["d2"]), g("g2", "Group 2", &["d3"])]
    );
    assert_eq!(
        remove_from_groups(&groups, &strings(&["d3"])),
        [g("g1", "Group 1", &["d1", "d2"])]
    );
}

#[test]
fn a_group_started_empty_waits_for_its_first_drawing_then_dissolves_like_any_other() {
    let fresh = g("g1", "Group 1", &[]);
    // Pruning and unrelated moves leave it alone while it waits.
    assert_eq!(
        prune_groups(std::slice::from_ref(&fresh), &pane_set(&["d1"])),
        std::slice::from_ref(&fresh)
    );
    assert_eq!(
        remove_from_groups(
            &[fresh.clone(), g("g2", "Group 2", &["d1", "d2"])],
            &strings(&["d1"])
        ),
        [fresh.clone(), g("g2", "Group 2", &["d2"])]
    );
    let joined = assign_to_group(&[fresh], "g1", &strings(&["d1"]));
    assert_eq!(joined, [g("g1", "Group 1", &["d1"])]);
    // Its last drawing leaving takes it away, as it would any group.
    assert!(remove_from_groups(&joined, &strings(&["d1"])).is_empty());
    assert!(prune_groups(&joined, &HashSet::new()).is_empty());
    let mut both = joined.clone();
    both.push(g("g2", "Group 2", &["d2"]));
    assert_eq!(
        assign_to_group(&both, "g2", &strings(&["d1"])),
        [g("g2", "Group 2", &["d2", "d1"])]
    );
}

#[test]
fn joining_a_group_is_exclusive_the_drawing_leaves_the_one_it_was_in() {
    let groups = vec![
        g("g1", "Group 1", &["d1", "d2"]),
        g("g2", "Group 2", &["d3"]),
    ];
    assert_eq!(
        assign_to_group(&groups, "g2", &strings(&["d1"])),
        [
            g("g1", "Group 1", &["d2"]),
            g("g2", "Group 2", &["d3", "d1"])
        ]
    );
    // Moving the last member out dissolves the group it left.
    assert_eq!(
        assign_to_group(
            &[g("g1", "Group 1", &["d1"]), g("g2", "Group 2", &["d2"])],
            "g2",
            &strings(&["d1"])
        ),
        [g("g2", "Group 2", &["d2", "d1"])]
    );
    // Re-adding a member it already has is not a duplicate.
    assert_eq!(
        assign_to_group(&groups, "g1", &strings(&["d1"])),
        [
            g("g1", "Group 1", &["d2", "d1"]),
            g("g2", "Group 2", &["d3"])
        ]
    );
}

// ── drawing names ──

#[test]
fn uses_the_type_registry_label_appending_the_drawings_own_text() {
    let registry = DrawingRegistry::builtin();
    assert_eq!(
        drawing_meta("trendline", None, Some(&registry)).0,
        "Trend Line"
    );
    assert_eq!(
        drawing_label(&draw("d1", "price", 1.0), None, Some(&registry)),
        "Trend Line"
    );
    let with_text = draw_with(
        "d2",
        "price",
        1.0,
        json!({ "text": { "value": "target", "size": "normal", "hAlign": "center", "vAlign": "center" } }),
    );
    assert_eq!(
        drawing_label(&with_text, None, Some(&registry)),
        "Trend Line \u{2014} target"
    );
    // A blank text adds nothing.
    let blank = draw_with(
        "d3",
        "price",
        1.0,
        json!({ "text": { "value": "   ", "size": "normal", "hAlign": "center", "vAlign": "center" } }),
    );
    assert_eq!(drawing_label(&blank, None, Some(&registry)), "Trend Line");
}

#[test]
fn an_unregistered_type_still_reads_as_a_name_title_cased() {
    let registry = DrawingRegistry::builtin();
    assert_eq!(
        drawing_meta("mystery", None, Some(&registry)),
        ("Mystery".to_string(), None)
    );
    assert_eq!(drawing_meta("two words", None, None).0, "Two Words");
    assert_eq!(drawing_meta("", None, None).0, "Drawing");
}
