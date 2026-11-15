//! Ports of the sync-model sections of `workspace-core.test.ts`.

use super::*;
use serde_json::json;

fn ids() -> Vec<String> {
    ["c1", "c2", "c3", "c4"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn groups(pairs: &[(&str, &str)]) -> SyncSetting {
    SyncSetting::Groups(
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn off_and_absent_settings_follow_nothing() {
    assert!(sync_targets("c1", None, &ids()).is_empty());
    assert!(sync_targets("c1", Some(&SyncSetting::Flag(false)), &ids()).is_empty());
}

#[test]
fn true_links_every_cell_into_one_implicit_group() {
    let on = SyncSetting::Flag(true);
    assert_eq!(sync_targets("c1", Some(&on), &ids()), ["c2", "c3", "c4"]);
    assert_eq!(sync_targets("c3", Some(&on), &ids()), ["c1", "c2", "c4"]);
}

#[test]
fn a_record_links_same_group_cells_only() {
    let g = groups(&[("c1", "a"), ("c2", "a"), ("c3", "b")]);
    assert_eq!(sync_targets("c1", Some(&g), &ids()), ["c2"]); // c3 other group, c4 unlisted
    assert!(sync_targets("c3", Some(&g), &ids()).is_empty()); // alone in its group
    assert!(sync_targets("c4", Some(&g), &ids()).is_empty()); // an unlisted origin follows nothing
}

#[test]
fn link_groups_list_every_linked_cell_once() {
    assert_eq!(sync_groups(Some(&SyncSetting::Flag(true)), &ids()), [ids()]);
    assert_eq!(
        sync_groups(
            Some(&groups(&[
                ("c1", "a"),
                ("c2", "b"),
                ("c3", "a"),
                ("c4", "b")
            ])),
            &ids()
        ),
        [strs(&["c1", "c3"]), strs(&["c2", "c4"])]
    );
    assert_eq!(
        sync_groups(
            Some(&groups(&[("c1", "a"), ("c2", "a"), ("c3", "b")])),
            &ids()
        ),
        [strs(&["c1", "c2"])]
    );
    assert!(sync_groups(Some(&SyncSetting::Flag(false)), &ids()).is_empty());
    assert!(sync_groups(None, &ids()).is_empty());
}

fn drawing(extra: serde_json::Value) -> SerializedDrawing {
    let mut base = json!({
        "id": "d1",
        "type": "trendline",
        "paneId": "price",
        "anchors": [{ "time": 1, "price": 2 }],
        "style": { "lineColor": "#fff", "lineWidth": 2 },
    });
    for (k, v) in extra.as_object().unwrap() {
        base[k] = v.clone();
    }
    serde_json::from_value(base).expect("a drawing document")
}

#[test]
fn identical_drawings_are_the_same_linked_content_whatever_the_key_order_or_per_chart_state() {
    let base = drawing(json!({}));
    let reordered: SerializedDrawing = serde_json::from_str(
        r##"{"style":{"lineWidth":2,"lineColor":"#fff"},"anchors":[{"price":2,"time":1}],"paneId":"price","type":"trendline","id":"other"}"##,
    )
    .unwrap();
    assert_eq!(
        drawing_link_content(&reordered),
        drawing_link_content(&base)
    );
    // Lock, visibility, draw order and creation time stay per chart.
    let per_chart = drawing(
        json!({ "id": "dw-9", "locked": true, "visible": false, "zIndex": 40, "createdAt": 99 }),
    );
    assert_eq!(
        drawing_link_content(&per_chart),
        drawing_link_content(&base)
    );
    // Geometry, style, text and per-type settings are the drawing itself.
    let differs = |d: SerializedDrawing| {
        assert_ne!(drawing_link_content(&d), drawing_link_content(&base));
    };
    differs(drawing(json!({ "anchors": [{ "time": 1, "price": 3 }] })));
    differs(drawing(
        json!({ "style": { "lineColor": "#000", "lineWidth": 2 } }),
    ));
    differs(drawing(json!({ "text": { "value": "a" } })));
    differs(drawing(json!({ "props": { "extend": "right" } })));
    differs(drawing(json!({ "paneId": "p2" })));
}

#[test]
fn identical_drawings_pair_one_to_one_across_charts_in_each_charts_order() {
    let line = |id: &str, price: f64| {
        drawing(json!({ "id": id, "anchors": [{ "time": 1, "price": price }] }))
    };
    let c1 = [
        line("a1", 1.0),
        line("a2", 1.0),
        line("a3", 1.0),
        line("a4", 5.0),
    ];
    let c2 = [line("b1", 1.0), line("b2", 1.0), line("b3", 7.0)];
    let c3 = [line("x1", 1.0)];
    let links = pair_identical_drawings(&[
        CellDrawings {
            cell_id: "c1".into(),
            drawings: &c1,
        },
        CellDrawings {
            cell_id: "c2".into(),
            drawings: &c2,
        },
        CellDrawings {
            cell_id: "c3".into(),
            drawings: &c3,
        },
    ]);
    let as_pairs = |l: &Vec<(String, String)>| -> Vec<(String, String)> { l.clone() };
    assert_eq!(links.len(), 2);
    assert_eq!(
        as_pairs(&links[0]),
        [
            ("c1".to_string(), "a1".to_string()),
            ("c2".to_string(), "b1".to_string()),
            ("c3".to_string(), "x1".to_string())
        ]
    );
    // a3 has no partner left; a4 and b3 match nothing.
    assert_eq!(
        as_pairs(&links[1]),
        [
            ("c1".to_string(), "a2".to_string()),
            ("c2".to_string(), "b2".to_string())
        ]
    );
}

#[test]
fn pairing_nothing_yields_no_links() {
    assert!(pair_identical_drawings(&[]).is_empty());
    let one = [drawing(json!({}))];
    assert!(
        pair_identical_drawings(&[CellDrawings {
            cell_id: "c1".into(),
            drawings: &one
        }])
        .is_empty(),
        "a drawing alone on its chart pairs with nothing"
    );
}

#[test]
fn ranges_within_is_the_epsilon_short_circuit_on_both_edges() {
    let a = (1000.0, 2000.0);
    assert!(ranges_within(a, (1400.0, 1600.0), 500.0));
    assert!(!ranges_within(a, (1600.0, 2000.0), 500.0)); // from drifted past eps
    assert!(!ranges_within(a, (1000.0, 2601.0), 500.0)); // to drifted past eps
    assert!(ranges_within(a, a, 0.0));
}

#[test]
fn style_config_patch_carries_every_differing_look_and_only_the_looks() {
    let looks = json!({
        "layout": { "background": "#000" },
        "panes": { "separatorColor": "#111" },
        "grid": { "vertLines": { "visible": false } },
        "margins": { "top": 20 },
        "priceScale": { "invert": true },
        "crosshair": { "style": "dotted" },
        "animations": { "zoom": false },
        "candles": { "upColor": "#0f0" },
        "bars": { "upColor": "#0a0" },
        "line": { "width": 3 },
        "area": { "lineColor": "#00f" },
        "baseline": { "baselineLevel": 30 },
        "sessions": { "premarketColor": "#f001" },
    });
    let mut origin = looks.clone();
    // The chart type, the market's baseline price, draw order, marks, trade markers and the
    // (workspace-global) timezone stay per cell.
    for (k, v) in json!({
        "version": 1,
        "series": { "style": "line", "baseline": 101, "spacing": 2 },
        "stacking": { "candles": 5, "series": {} },
        "marks": { "visible": false, "groups": {} },
        "trades": { "visible": false },
        "timeScale": { "timezone": "Europe/Paris" },
    })
    .as_object()
    .unwrap()
    {
        origin[k] = v.clone();
    }
    let follower = json!({ "series": { "style": "candles", "baseline": null, "spacing": 1 } });
    let patch = style_config_patch(&origin, &follower).expect("a patch");
    let mut expected = looks;
    expected["series"] = json!({ "spacing": 2 });
    assert_eq!(Value::Object(patch), expected);
}

#[test]
fn style_config_patch_leaves_converged_blocks_out_and_is_none_once_the_looks_agree() {
    let doc = json!({ "layout": { "background": "#000" }, "candles": { "upColor": "#0f0" }, "series": { "style": "line", "spacing": 1 } });
    let mut other = doc.clone();
    other["series"] = json!({ "style": "candles", "spacing": 1 });
    assert!(style_config_patch(&doc, &other).is_none());
    let mut red = doc.clone();
    red["candles"] = json!({ "upColor": "#f00" });
    assert_eq!(
        Value::Object(style_config_patch(&doc, &red).unwrap()),
        json!({ "candles": { "upColor": "#0f0" } })
    );
}

#[test]
fn style_config_patch_mirrors_a_plugin_styles_candle_keys_but_none_of_its_own_settings() {
    let patch = style_config_patch(
        &json!({ "chartTypes": { "flow": { "candleUpColor": "#0f0", "rowTicks": 2 } } }),
        &json!({ "chartTypes": { "flow": { "candleWickUpColor": "#f00", "rowTicks": 8 }, "other": { "candleDownColor": "#00f" } } }),
    )
    .unwrap();
    assert_eq!(
        Value::Object(patch),
        json!({
            "chartTypes": {
                "flow": { "candleUpColor": "#0f0", "candleWickUpColor": null },
                "other": { "candleDownColor": null },
            }
        })
    );
}

#[test]
fn style_config_patch_is_none_on_shapeless_documents() {
    assert!(style_config_patch(&Value::Null, &json!({})).is_none());
    assert!(
        style_config_patch(&json!({ "candles": { "upColor": "#0f0" } }), &json!("nope")).is_none()
    );
    assert!(style_config_patch(&json!({ "layout": "not-an-object" }), &json!({})).is_none());
    assert!(style_config_patch(&json!([1, 2]), &json!({})).is_none());
    // A series block with a non-numeric spacing contributes nothing.
    assert!(style_config_patch(&json!({ "series": { "spacing": "wide" } }), &json!({})).is_none());
}
