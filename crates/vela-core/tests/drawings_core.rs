//! The drawing foundation: registry, store, settings patches, undo snapshots.

use serde_json::json;
use vela_core::drawings::types::lines::{HorizontalLine, TrendLine};
use vela_core::drawings::{
    Drawing, DrawingHistory, DrawingInit, DrawingPatch, DrawingPoint, DrawingRegistry,
    DrawingStore, DrawingStyle, LinearProjector, PartialDrawingStyle, Props, apply_tool_defaults,
    capture_tool_defaults,
};
use vela_core::model::LineStyle;

fn pt(time: f64, price: f64) -> DrawingPoint {
    DrawingPoint { time, price }
}

fn patch(value: serde_json::Value) -> Props {
    value.as_object().cloned().expect("an object")
}

#[test]
fn create_seeds_the_type_default_under_a_partial_override() {
    let registry = DrawingRegistry::builtin();
    let init = DrawingInit {
        style: Some(PartialDrawingStyle {
            line_color: Some("#ff0000".into()),
            ..Default::default()
        }),
        ..DrawingInit::on_pane("price")
    };
    let hline = registry.create("hline", init).expect("hline is registered");
    assert_eq!(hline.type_key(), "hline");
    assert_eq!(hline.base().style.line_color, "#ff0000");
    assert_eq!(hline.base().style.line_style, LineStyle::Dashed); // kept from the type default
    assert!(
        registry
            .create("nope", DrawingInit::on_pane("price"))
            .is_none()
    );
}

#[test]
fn trend_line_geometry_follows_its_anchors() {
    let proj = LinearProjector::sample();
    // (100, 400) → (300, 200) in pixels.
    let mut line = TrendLine::new(&DrawingInit::on_pane("price").anchors(vec![
        pt(100.0 * 60_000.0, 100.0),
        pt(300.0 * 60_000.0, 150.0),
    ]));
    assert!(line.is_complete());
    assert_eq!(
        line.handle_points(&proj),
        vec![(100.0, 400.0), (300.0, 200.0)]
    );
    assert!(line.hit_test(200.0, 300.0, &proj, 4.0));
    assert!(!line.hit_test(200.0, 320.0, &proj, 4.0));
    assert!(!line.hit_test(400.0, 100.0, &proj, 4.0)); // past the second anchor
    assert_eq!(line.hit_handle(301.0, 201.0, &proj, 4.0), Some(1));
    assert_eq!(line.hit_handle(200.0, 300.0, &proj, 4.0), None);
    let b = line.bounds(&proj).unwrap();
    assert_eq!((b.x, b.y, b.w, b.h), (100.0, 200.0, 200.0, 200.0));
    let range = line.price_range().unwrap();
    assert_eq!((range.min, range.max), (100.0, 150.0));

    line.apply_settings(&patch(json!({ "style.extendRight": true })));
    assert!(line.hit_test(400.0, 100.0, &proj, 4.0)); // the extension now reaches it

    line.drag_handle(0, pt(0.0, 120.0));
    assert_eq!(line.base().anchors[0], pt(0.0, 120.0));
    let moved = line.translate_body(60_000.0, -1.0, &line.base().anchors.clone());
    assert_eq!(moved[0], pt(60_000.0, 119.0));
}

#[test]
fn a_horizontal_line_moves_only_in_price_and_never_culls_by_time() {
    let proj = LinearProjector::sample();
    let mut line =
        HorizontalLine::new(&DrawingInit::on_pane("price").anchors(vec![pt(60_000.0, 150.0)]));
    assert!(line.hit_test(900.0, 201.0, &proj, 3.0));
    assert_eq!(line.time_extent(), None);
    line.drag_handle(0, pt(999.0, 160.0));
    assert_eq!(line.base().anchors[0], pt(60_000.0, 160.0));
    // A pane the projector does not know resolves nothing.
    let other = HorizontalLine::new(&DrawingInit::on_pane("study").anchors(vec![pt(0.0, 1.0)]));
    assert!(other.handle_points(&proj).is_empty());
    assert!(other.bounds(&proj).is_none());
}

#[test]
fn settings_patches_reach_style_text_and_base_fields() {
    let mut line = TrendLine::new(&DrawingInit::on_pane("price"));
    line.apply_settings(&patch(json!({
        "style.lineColor": "#123456",
        "style.lineWidth": 4,
        "style.lineStyle": "dotted",
        "text.value": "Breakout",
        "text.bold": true,
        "locked": true,
        "showOn": ["D", "W", "bogus"]
    })));
    let base = line.base();
    assert_eq!(base.style.line_color, "#123456");
    assert_eq!(base.style.line_width, 4.0);
    assert_eq!(base.style.line_style, LineStyle::Dotted);
    let text = base.text.as_ref().unwrap();
    assert_eq!(text.value, "Breakout");
    assert_eq!(text.bold, Some(true));
    assert!(base.locked);
    assert_eq!(base.show_on, Some(vec!["D".to_string(), "W".to_string()]));

    // A value of the wrong type leaves the block as it was; null clears an optional field.
    line.apply_settings(&patch(
        json!({ "style.lineWidth": "wide", "style.fillColor": "#abc" }),
    ));
    assert_eq!(line.base().style.line_width, 4.0);
    assert_eq!(line.base().style.fill_color.as_deref(), Some("#abc"));
    line.apply_settings(&patch(json!({ "style.fillColor": null })));
    assert_eq!(line.base().style.fill_color, None);
}

#[test]
fn the_store_orders_by_depth_and_round_trips_a_document() {
    let registry = DrawingRegistry::builtin();
    let mut store = DrawingStore::new();
    for (type_key, price) in [("trendline", 1.0), ("hline", 2.0), ("trendline", 3.0)] {
        let id = store.next_id();
        let init = DrawingInit {
            id: Some(id),
            ..DrawingInit::on_pane("price").anchors(vec![pt(0.0, price), pt(60_000.0, price)])
        };
        store.add(registry.create(type_key, init).unwrap());
    }
    let ids = |store: &DrawingStore| {
        store
            .all()
            .iter()
            .map(|d| d.base().id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&store), vec!["dw-1", "dw-2", "dw-3"]);

    store.send_to_back("dw-3", 1.0);
    assert_eq!(ids(&store), vec!["dw-3", "dw-1", "dw-2"]);
    store.bring_to_front("dw-3", 0.0);
    assert_eq!(ids(&store), vec!["dw-1", "dw-2", "dw-3"]);
    store.bring_to_front("dw-1", 50.0); // over the series stack too
    assert_eq!(store.get("dw-1").unwrap().base().z_index, 51.0);

    let before = store.revision();
    store.update(
        "dw-2",
        DrawingPatch {
            style: Some(DrawingStyle::line("#fff", 1.0, LineStyle::Solid)),
            text: Some(None),
            locked: Some(true),
            ..Default::default()
        },
    );
    assert!(store.revision() > before);
    assert!(store.get("dw-2").unwrap().base().locked);
    assert!(store.update("missing", DrawingPatch::default()).is_none());

    let doc = store.serialize();
    let json = serde_json::to_value(&doc).unwrap();
    let mut restored = DrawingStore::new();
    restored.load(&registry, &json);
    assert_eq!(restored.serialize(), doc);
    assert_eq!(restored.next_id(), "dw-4"); // past the loaded ids
    assert_eq!(restored.by_pane("price").len(), 3);
    assert!(restored.by_pane("other").is_empty());

    // Unknown types and malformed entries are skipped, not fatal.
    let mut messy = json.clone();
    messy["drawings"].as_array_mut().unwrap().push(json!({ "id": "x", "type": "from-the-future", "paneId": "price", "anchors": [], "style": {} }));
    messy["drawings"]
        .as_array_mut()
        .unwrap()
        .push(json!("garbage"));
    restored.load(&registry, &messy);
    assert_eq!(restored.len(), 3);

    assert!(restored.remove("dw-1"));
    assert!(!restored.remove("dw-1"));
    restored.clear();
    assert!(restored.is_empty());
}

#[test]
fn history_restores_store_snapshots() {
    let registry = DrawingRegistry::builtin();
    let mut store = DrawingStore::new();
    let mut history = DrawingHistory::default();
    let add = |store: &mut DrawingStore| {
        let id = store.next_id();
        store.add(
            registry
                .create(
                    "hline",
                    DrawingInit {
                        id: Some(id),
                        ..DrawingInit::on_pane("price").anchors(vec![pt(0.0, 1.0)])
                    },
                )
                .unwrap(),
        );
    };
    history.record(&store.serialize());
    add(&mut store);
    history.record(&store.serialize());
    add(&mut store);
    assert_eq!(store.len(), 2);

    let snapshot = history.undo(&store.serialize()).unwrap();
    store.load_document(&registry, &snapshot);
    assert_eq!(store.len(), 1);
    let snapshot = history.redo(&store.serialize()).unwrap();
    store.load_document(&registry, &snapshot);
    assert_eq!(store.len(), 2);
}

#[test]
fn reset_and_tool_defaults_carry_looks_but_not_words() {
    let registry = DrawingRegistry::builtin();
    let mut styled = registry
        .create("trendline", DrawingInit::on_pane("price"))
        .unwrap();
    styled.apply_settings(&patch(
        json!({ "style.lineColor": "#ff00ff", "text.value": "mine", "text.italic": true }),
    ));
    let defaults = capture_tool_defaults(styled.as_ref());

    let mut next = registry
        .create("trendline", DrawingInit::on_pane("price"))
        .unwrap();
    apply_tool_defaults(next.as_mut(), Some(&defaults));
    assert_eq!(next.base().style.line_color, "#ff00ff");
    let text = next.base().text.as_ref().unwrap();
    assert_eq!(text.value, ""); // the styling came over, the words did not
    assert_eq!(text.italic, Some(true));

    registry.reset_settings(styled.as_mut());
    assert_eq!(
        styled.base().style,
        registry.get("trendline").unwrap().default_style
    );
    assert_eq!(styled.base().text, None); // a trend line seeds no text block
}
