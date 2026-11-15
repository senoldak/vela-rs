//! Every registered tool paints a complete drawing without panicking, with a balanced
//! stream and finite arguments; with no anchors or too few it paints what the original does
//! (nothing, bar the few documented exceptions).

use super::*;

/// `n` anchors inside the sample projector (1000 x 600): a staircase near the bars.
fn anchors(n: usize) -> Vec<DrawingPoint> {
    (0..n)
        .map(|i| {
            let x = 100.0 + 70.0 * i as f64;
            let y = 300.0 + ((i * 53) % 7) as f64 * 14.0 - (i / 3) as f64 * 20.0;
            point(x * 60_000.0, 200.0 - 0.25 * y)
        })
        .collect()
}

fn registry_keys() -> Vec<&'static str> {
    DrawingRegistry::builtin()
        .entries()
        .iter()
        .map(|m| m.type_key)
        .collect()
}

fn drawing_with(key: &str, anchors: Vec<DrawingPoint>) -> Box<dyn Drawing> {
    let text = (key == "text").then(|| text_block("Hello"));
    make(key, "d", anchors, None, text)
}

#[test]
fn every_registered_type_is_covered() {
    assert_eq!(registry_keys().len(), 79);
}

#[test]
fn every_type_paints_a_sane_stream_when_complete() {
    let proj = bars_projector();
    for key in registry_keys() {
        let n = drawing_with(key, vec![]).anchor_schema().min.max(1);
        let a = match key {
            // A candle in the bullish gap / on the swing high.
            "fairvaluegap" => vec![point(100.0 * 60_000.0, 150.0)],
            "orderblock" => vec![point(50.0 * 60_000.0, 99.0)],
            "liquidity" => vec![point(35.0 * 5.0 * 60_000.0, 120.0)],
            _ => anchors(n),
        };
        let d = drawing_with(key, a);
        let canvas = paint_one_drawing(d.as_ref(), &proj);
        assert_sane(&canvas, key);
    }
}

#[test]
fn every_type_paints_a_sane_stream_when_selected_and_hovered() {
    let proj = bars_projector();
    for key in registry_keys() {
        let n = drawing_with(key, vec![]).anchor_schema().min.max(1);
        let d = drawing_with(key, anchors(n));
        let mut targets = PaintTargets::default();
        targets.selected.insert("d".to_string());
        targets.hovered = Some("d".to_string());
        let canvas = paint(&[d.as_ref()], &proj, &targets);
        assert_balanced(&canvas, key);
        for op in canvas.ops() {
            assert!(!has_non_finite(&op), "{key}: non-finite argument in {op}");
        }
    }
}

#[test]
fn no_anchors_paint_nothing() {
    let proj = bars_projector();
    for key in registry_keys() {
        let d = drawing_with(key, vec![]);
        let canvas = paint_one_drawing(d.as_ref(), &proj);
        assert_balanced(&canvas, key);
        // Only the pane clip wrapper may appear: never a drawn shape.
        let drawn: Vec<String> = canvas
            .names()
            .into_iter()
            .filter(|n| {
                !matches!(
                    n.as_str(),
                    "save" | "restore" | "beginPath" | "rect" | "clip"
                )
            })
            .collect();
        assert!(drawn.is_empty(), "{key}: painted {drawn:?} without anchors");
    }
}

#[test]
fn too_few_anchors_paint_what_the_original_does() {
    let proj = bars_projector();
    for key in registry_keys() {
        let min = drawing_with(key, vec![]).anchor_schema().min;
        if min < 2 {
            continue;
        }
        let d = drawing_with(key, anchors(min - 1));
        let canvas = paint_one_drawing(d.as_ref(), &proj);
        assert_balanced(&canvas, key);
        for op in canvas.ops() {
            assert!(!has_non_finite(&op), "{key}: non-finite argument in {op}");
        }
    }
}
