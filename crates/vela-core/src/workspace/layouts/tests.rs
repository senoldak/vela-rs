//! Ports of `workspace-core.test.ts` (layout registry, dynamic grids, grid styles, reducers)
//! and `layout-glyph.test.ts`.

use super::*;

fn registry() -> LayoutRegistry {
    LayoutRegistry::with_builtins()
}

fn ids(cells: &[LayoutCell]) -> Vec<String> {
    cells.iter().map(|c| c.id.clone()).collect()
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn grid16() -> LayoutDefinition {
    LayoutDefinition {
        id: "custom-16".into(),
        label: "16 grid".into(),
        cols: vec![1.0; 4],
        rows: vec![1.0; 4],
        areas: None,
        cells: (1..=16).map(|i| LayoutCell::new(format!("c{i}"))).collect(),
    }
}

fn big_left() -> LayoutDefinition {
    LayoutDefinition {
        id: "big-left".into(),
        label: "Big left".into(),
        cols: vec![2.0, 1.0],
        rows: vec![1.0, 1.0],
        areas: Some(strings(&["main a", "main b"])),
        cells: vec![
            LayoutCell::in_area("c1", "main"),
            LayoutCell::in_area("c2", "a"),
            LayoutCell::in_area("c3", "b"),
        ],
    }
}

#[test]
fn ships_the_v1_presets_with_canonical_slot_ids() {
    let r = registry();
    let listed: Vec<String> = r.layouts(None).into_iter().map(|l| l.id).collect();
    assert_eq!(listed, ["1", "2h", "2v", "4", "8"]);
    assert_eq!(
        ids(&r.definition("4", None).unwrap().cells),
        ["c1", "c2", "c3", "c4"]
    );
    assert_eq!(r.definition("8", None).unwrap().cells.len(), 8);
    // Slot ids are SHARED across layouts: the pool restores by id on 4 to 2 to 4.
    assert_eq!(ids(&r.definition("2h", None).unwrap().cells), ["c1", "c2"]);
}

#[test]
fn register_layout_is_the_sdk_seam() {
    let r = registry();
    r.register(grid16());
    assert_eq!(r.definition("custom-16", None).unwrap().cells.len(), 16);
    assert!(r.layouts(None).iter().any(|l| l.id == "custom-16"));
    let mut replaced = grid16();
    replaced.label = "Sixteen".into();
    r.register(replaced);
    assert_eq!(r.definition("custom-16", None).unwrap().label, "Sixteen"); // last registration wins
    assert_eq!(
        r.layouts(None)
            .iter()
            .filter(|l| l.id == "custom-16")
            .count(),
        1,
        "re-registering keeps one entry"
    );
    r.unregister("custom-16");
    assert!(r.definition("custom-16", None).is_none());
}

#[test]
fn clones_share_one_registry() {
    let a = registry();
    let b = a.clone();
    b.register(grid16());
    assert!(a.definition("custom-16", None).is_some());
}

#[test]
fn builtin_labels_are_messages_read_in_english_by_default() {
    let r = registry();
    assert_eq!(r.definition("2h", None).unwrap().label, "2 side by side");
    assert_eq!(r.definition("1", None).unwrap().label, "Single");
}

#[test]
fn layout_for_grid_lands_on_the_classic_presets() {
    let r = registry();
    for (rows, cols, id) in [
        (1., 1., "1"),
        (1., 2., "2h"),
        (2., 1., "2v"),
        (2., 2., "4"),
        (2., 4., "8"),
    ] {
        assert_eq!(r.layout_for_grid(rows, cols, None).id, id);
    }
}

#[test]
fn layout_for_grid_synthesizes_other_geometries_without_registering_them() {
    let r = registry();
    let def = r.layout_for_grid(3., 2., None);
    assert_eq!(def.id, "g3x2");
    assert_eq!(def.rows, [1.0, 1.0, 1.0]);
    assert_eq!(def.cols, [1.0, 1.0]);
    assert!(def.areas.is_none());
    assert_eq!(ids(&def.cells), ["c1", "c2", "c3", "c4", "c5", "c6"]);
    assert_eq!(def.label, "2 \u{d7} 3 grid");
    assert!(
        r.definition("g3x2", None).is_none(),
        "the registry stays presets-only"
    );
}

#[test]
fn layout_for_grid_clamps_to_the_picker_canvas() {
    let r = registry();
    assert_eq!(r.layout_for_grid(9., 0., None).id, "g4x1");
    assert_eq!(r.layout_for_grid(4., 4., None).cells.len(), 16);
    assert_eq!(r.layout_for_grid(f64::NAN, 2.4, None).id, "2h");
    assert_eq!(r.layout_for_grid(-3., 100., None).id, "g1x4");
}

#[test]
fn ensure_resolves_registered_dynamic_and_junk_ids() {
    let r = registry();
    assert_eq!(r.ensure("4"), r.definition("4", None));
    assert_eq!(r.ensure("g3x3").unwrap().cells.len(), 9);
    assert!(
        r.ensure("g5x5").is_none(),
        "beyond the canvas: not a picker id"
    );
    assert!(r.ensure("nope").is_none());
    assert!(r.ensure("g3x").is_none());
    assert!(r.ensure("g33x3").is_none());
    assert!(r.ensure("").is_none());
}

#[test]
fn layout_shape_reads_grids_back_and_bespoke_areas_stay_none() {
    let r = registry();
    assert_eq!(
        layout_shape(&r.definition("1", None).unwrap()),
        Some(LayoutShape { rows: 1, cols: 1 })
    );
    assert_eq!(
        layout_shape(&r.definition("8", None).unwrap()),
        Some(LayoutShape { rows: 2, cols: 4 })
    );
    assert_eq!(
        layout_shape(&r.layout_for_grid(3., 2., None)),
        Some(LayoutShape { rows: 3, cols: 2 })
    );
    assert_eq!(layout_shape(&big_left()), None);
    let mut five_wide = grid16();
    five_wide.cols = vec![1.0; 5];
    assert_eq!(layout_shape(&five_wide), None, "beyond the canvas");
}

#[test]
fn grid_styles_map_weights_to_fr_templates() {
    let r = registry();
    let s = grid_styles(&r.definition("4", None).unwrap(), None);
    assert_eq!(s.template_columns(), "1fr 1fr");
    assert_eq!(s.template_rows(), "1fr 1fr");
    assert_eq!(s.template_areas(), None);
    let two = grid_styles(&r.definition("2h", None).unwrap(), None);
    assert_eq!(two.template_columns(), "1fr 1fr");
    assert_eq!(two.template_rows(), "1fr");
}

#[test]
fn grid_styles_honor_track_sizes_only_when_the_length_matches() {
    let def = registry().definition("4", None).unwrap();
    let sizes = |cols: Vec<f64>| TrackSizes {
        cols: Some(cols),
        rows: None,
    };
    assert_eq!(
        grid_styles(&def, Some(&sizes(vec![2.0, 1.0]))).template_columns(),
        "2fr 1fr"
    );
    assert_eq!(
        grid_styles(&def, Some(&sizes(vec![2.0, 1.0, 1.0]))).template_columns(),
        "1fr 1fr",
        "stale sizes are ignored"
    );
    assert_eq!(
        grid_styles(&def, Some(&sizes(vec![1.5, 0.5]))).template_columns(),
        "1.5fr 0.5fr"
    );
}

#[test]
fn asymmetric_layouts_get_an_areas_template_and_per_cell_grid_areas() {
    let r = registry();
    r.register(big_left());
    let s = grid_styles(&r.definition("big-left", None).unwrap(), None);
    assert_eq!(s.template_areas().as_deref(), Some("\"main a\" \"main b\""));
    assert_eq!(s.per_cell["c1"].as_deref(), Some("main"));
    assert_eq!(s.per_cell["c3"].as_deref(), Some("b"));
    let plain = grid_styles(&r.definition("4", None).unwrap(), None);
    assert_eq!(plain.per_cell["c1"], None);
}

#[test]
fn active_after_layout_keeps_a_surviving_slot() {
    let all = strings(&["c1", "c2", "c3", "c4"]);
    assert_eq!(active_after_layout(Some("c3"), &all).as_deref(), Some("c3"));
    assert_eq!(
        active_after_layout(Some("c3"), &strings(&["c1", "c2"])).as_deref(),
        Some("c1"),
        "its slot left with the layout"
    );
    assert_eq!(
        active_after_layout(None, &strings(&["c1", "c2"])).as_deref(),
        Some("c1")
    );
    assert_eq!(active_after_layout(Some("c1"), &[]), None);
}

#[test]
fn order_after_layout_moves_a_pooling_active_identity_into_the_last_slot() {
    let abcd = strings(&["a", "b", "c", "d"]);
    // 2x2 to single: the active chart becomes the one remaining chart.
    assert_eq!(
        order_after_layout(&abcd, 1, Some("c")),
        ["c", "a", "b", "d"]
    );
    // 2x2 to 2 side by side: the first chart keeps its slot, active takes the second.
    assert_eq!(
        order_after_layout(&abcd, 2, Some("d")),
        ["a", "d", "b", "c"]
    );
}

#[test]
fn order_after_layout_leaves_the_order_alone_when_the_active_chart_survives_or_is_unknown() {
    let abcd = strings(&["a", "b", "c", "d"]);
    assert_eq!(order_after_layout(&abcd, 2, Some("b")), abcd);
    assert_eq!(order_after_layout(&abcd, 8, Some("d")), abcd);
    let ab = strings(&["a", "b"]);
    assert_eq!(order_after_layout(&ab, 1, None), ab);
    assert_eq!(order_after_layout(&ab, 1, Some("nope")), ab);
    assert_eq!(order_after_layout(&ab, 0, Some("b")), ab);
    assert!(order_after_layout(&[], 3, Some("x")).is_empty());
}

#[test]
fn occupancy_grid_reads_areas_and_auto_flows_plain_grids() {
    let r = registry();
    assert_eq!(
        occupancy_grid(&r.definition("4", None).unwrap()),
        vec![strings(&["c1", "c2"]), strings(&["c3", "c4"])]
    );
    assert_eq!(
        occupancy_grid(&big_left()),
        vec![strings(&["main", "a"]), strings(&["main", "b"])]
    );
    // Fewer slots than tracks: a placeholder that never equals a real slot.
    let mut short = r.definition("4", None).unwrap();
    short.cells.truncate(3);
    assert_eq!(occupancy_grid(&short)[1][1], "\u{b7}1x1");
}

fn seams(svg: &str) -> String {
    let marker = "<path d=\"";
    match svg.find(marker) {
        Some(at) => {
            let rest = &svg[at + marker.len()..];
            rest[..rest.find('"').unwrap()].to_string()
        }
        None => String::new(),
    }
}

#[test]
fn glyph_speaks_the_topbar_icon_language() {
    let r = registry();
    let svg = layout_glyph(&r.definition("4", None).unwrap());
    assert!(svg.starts_with("<svg "));
    for needle in [
        "viewBox=\"0 0 16 16\"",
        "stroke=\"currentColor\"",
        "stroke-width=\"1.2\"",
        "stroke-linejoin=\"round\"",
        "fill=\"none\"",
        "<rect x=\"1.5\" y=\"1.5\" width=\"13\" height=\"13\" rx=\"1.5\"/>",
    ] {
        assert!(svg.contains(needle), "missing {needle}");
    }
}

#[test]
fn glyph_draws_each_builtin_layout() {
    let r = registry();
    let glyph = |id: &str| layout_glyph(&r.definition(id, None).unwrap());
    assert_eq!(seams(&glyph("1")), "");
    assert!(!glyph("1").contains("<path"));
    assert_eq!(seams(&glyph("2h")), "M8 1.5V14.5");
    assert_eq!(seams(&glyph("2v")), "M1.5 8H14.5");
    assert_eq!(seams(&glyph("4")), "M8 1.5V14.5M1.5 8H14.5");
    assert_eq!(
        seams(&glyph("8")),
        "M4.75 1.5V14.5M8 1.5V14.5M11.25 1.5V14.5M1.5 8H14.5"
    );
}

#[test]
fn glyph_draws_the_picker_composed_grids() {
    let r = registry();
    assert_eq!(
        seams(&layout_glyph(&r.layout_for_grid(3., 3., None))),
        "M5.83 1.5V14.5M10.17 1.5V14.5M1.5 5.83H14.5M1.5 10.17H14.5"
    );
    assert_eq!(
        seams(&layout_glyph(&r.layout_for_grid(1., 3., None))),
        "M5.83 1.5V14.5M10.17 1.5V14.5"
    );
}

#[test]
fn glyph_keeps_a_spanning_cell_whole() {
    // The vertical seam runs the full height at 2/3; the horizontal one only splits the right column.
    assert_eq!(
        seams(&layout_glyph(&big_left())),
        "M10.17 1.5V14.5M10.17 8H14.5"
    );
    let wide_top = LayoutDefinition {
        id: "wide-top".into(),
        label: "Wide top".into(),
        cols: vec![1.0, 1.0],
        rows: vec![1.0, 1.0],
        areas: Some(strings(&["top top", "l r"])),
        cells: vec![
            LayoutCell::in_area("c1", "top"),
            LayoutCell::in_area("c2", "l"),
            LayoutCell::in_area("c3", "r"),
        ],
    };
    assert_eq!(seams(&layout_glyph(&wide_top)), "M8 8V14.5M1.5 8H14.5");
}

#[test]
fn glyph_places_seams_at_the_declared_track_weights() {
    let weighted = LayoutDefinition {
        id: "wide-left".into(),
        label: "Wide left".into(),
        cols: vec![3.0, 1.0],
        rows: vec![1.0],
        areas: None,
        cells: vec![LayoutCell::new("c1"), LayoutCell::new("c2")],
    };
    assert_eq!(seams(&layout_glyph(&weighted)), "M11.25 1.5V14.5");
}

#[test]
fn glyph_survives_garbage_weights() {
    let mut def = big_left();
    def.cols = vec![f64::NAN, -4.0];
    def.rows = vec![];
    // Bad or missing weights read as 1.
    assert_eq!(seams(&layout_glyph(&def)), "M8 1.5V14.5M8 8H14.5");
    let empty = LayoutDefinition {
        id: "e".into(),
        label: "e".into(),
        cols: vec![],
        rows: vec![],
        areas: None,
        cells: vec![],
    };
    assert!(layout_glyph(&empty).contains("<rect"));
}
