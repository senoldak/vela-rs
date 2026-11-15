//! Ports of the original's painter tests: `drawings-painter`, `drawings-box-midline`,
//! `drawings-trend-angle-label` and `drawings-trendline-extend`. They record through
//! `RecordingCanvas`, whose text metric is 0.6 em per character (the originals used a fixed
//! 8 or 6 px per character), so the expected widths are derived from that rule.
//!
//! `sliceKeyFor` (in the same TS test file) belongs to the interaction controller, not the
//! painter, and is not ported here.

use super::*;
use crate::drawings::types::shapes::BoxDrawing;
use crate::drawings::{SerializedDrawing, TextPlace, TextSide};
use crate::model::LineStyle;
use crate::render::shared::drawing_geometry::dash_pattern;

/// What the originals' hand-rolled recording contexts captured: stroked paths with the
/// style at the time, stroked and filled rectangles, and drawn text.
#[derive(Clone, Debug, PartialEq)]
enum Rec {
    Stroke {
        color: String,
        width: f64,
        dash: Vec<f64>,
        segments: Vec<[f64; 4]>,
    },
    StrokeRect([f64; 4]),
    FillRect([f64; 4]),
    Text(String),
}

fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or(f64::NAN)
}

fn recorded(canvas: &RecordingCanvas) -> Vec<Rec> {
    let mut out = Vec::new();
    let mut color = String::new();
    let mut width = 1.0;
    let mut dash: Vec<f64> = Vec::new();
    let mut path: Vec<[f64; 4]> = Vec::new();
    let mut at = (0.0, 0.0);
    for op in canvas.ops() {
        let name = op[0].as_str().unwrap_or_default();
        match name {
            "strokeStyle" => color = op[1].as_str().unwrap_or_default().to_string(),
            "lineWidth" => width = num(&op[1]),
            "setLineDash" => {
                dash = op[1]
                    .as_array()
                    .map(|a| a.iter().map(num).collect())
                    .unwrap_or_default()
            }
            "beginPath" => path.clear(),
            "moveTo" => at = (num(&op[1]), num(&op[2])),
            "lineTo" => {
                let to = (num(&op[1]), num(&op[2]));
                path.push([at.0, at.1, to.0, to.1]);
                at = to;
            }
            "stroke" => out.push(Rec::Stroke {
                color: color.clone(),
                width,
                dash: dash.clone(),
                segments: path.clone(),
            }),
            "strokeRect" => out.push(Rec::StrokeRect([
                num(&op[1]),
                num(&op[2]),
                num(&op[3]),
                num(&op[4]),
            ])),
            "fillRect" => out.push(Rec::FillRect([
                num(&op[1]),
                num(&op[2]),
                num(&op[3]),
                num(&op[4]),
            ])),
            "fillText" => out.push(Rec::Text(op[1].as_str().unwrap_or_default().to_string())),
            _ => {}
        }
    }
    out
}

/// The strokes painted for a drawing.
fn strokes(d: &dyn Drawing, proj: &dyn Projector) -> Vec<Rec> {
    recorded(&paint_one_drawing(d, proj))
        .into_iter()
        .filter(|r| matches!(r, Rec::Stroke { .. }))
        .collect()
}

fn segments_of(rec: &Rec) -> Vec<[f64; 4]> {
    match rec {
        Rec::Stroke { segments, .. } => segments.clone(),
        _ => Vec::new(),
    }
}

/// Every `stroke()` call's strokeStyle in order (the original's `strokeRecordingCtx`).
fn stroke_colors(canvas: &RecordingCanvas) -> Vec<String> {
    let mut color = String::new();
    let mut out = Vec::new();
    for op in canvas.ops() {
        match op[0].as_str().unwrap_or_default() {
            "strokeStyle" => color = op[1].as_str().unwrap_or_default().to_string(),
            "stroke" => out.push(color.clone()),
            _ => {}
        }
    }
    out
}

fn hline(id: &str, price: f64) -> Box<dyn Drawing> {
    make("hline", id, vec![point(10.0, price)], None, None)
}

fn ids(list: &[&str]) -> HashSet<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn proj() -> FnProj {
    FnProj::new(100.0, 200.0, 100.0)
}

// ── drawings-painter.test.ts ──

mod handles {
    use super::*;

    #[test]
    fn paints_handles_for_every_selected_id() {
        let (a, b) = (hline("a", 30.0), hline("b", 50.0));
        let targets = PaintTargets {
            selected: ids(&["a", "b"]),
            ..Default::default()
        };
        let canvas = paint(&[a.as_ref(), b.as_ref()], &proj(), &targets);
        assert_eq!(canvas.calls("arc").len(), 2); // one handle per selected hline
    }

    #[test]
    fn paints_handles_for_the_hovered_and_dragged_drawings_too() {
        let (a, b) = (hline("a", 30.0), hline("b", 50.0));
        let targets = PaintTargets {
            hovered: Some("a".into()),
            dragged: Some("b".into()),
            ..Default::default()
        };
        let canvas = paint(&[a.as_ref(), b.as_ref()], &proj(), &targets);
        assert_eq!(canvas.calls("arc").len(), 2);
    }

    #[test]
    fn paints_no_handles_when_nothing_is_targeted() {
        let (a, b) = (hline("a", 30.0), hline("b", 50.0));
        let canvas = paint(&[a.as_ref(), b.as_ref()], &proj(), &PaintTargets::default());
        assert_eq!(canvas.calls("arc").len(), 0);
    }

    #[test]
    fn paint_highlights_draws_handles_alone_for_the_highlighted_ones_only() {
        let (a, b) = (hline("a", 30.0), hline("b", 50.0));
        let canvas = RecordingCanvas::new();
        DrawingPainter::new().paint_highlights(
            &canvas,
            &[a.as_ref(), b.as_ref()],
            &proj(),
            &ids(&["b"]),
        );
        assert_eq!(canvas.calls("arc").len(), 1); // b's handle ...
        assert_eq!(canvas.calls("lineTo").len(), 0); // ... and neither body
    }

    #[test]
    fn handle_ids_are_selected_dragged_hovered_minus_the_muted_label() {
        let targets = PaintTargets {
            selected: ids(&["a", "m"]),
            hovered: Some("h".into()),
            dragged: Some("d".into()),
            muted_label: Some("m".into()),
        };
        assert_eq!(handle_ids_for(&targets), ids(&["a", "h", "d"]));
        assert!(handle_ids_for(&PaintTargets::default()).is_empty());
    }

    #[test]
    fn a_handle_is_a_gray_filled_blue_ringed_circle() {
        let canvas = RecordingCanvas::new();
        DrawingPainter::new().paint_handles(&canvas, &[(10.0, 20.0)]);
        assert_eq!(
            canvas.to_json(),
            json!([
                ["setLineDash", []],
                ["lineWidth", 1.5],
                ["beginPath"],
                [
                    "arc",
                    10.0,
                    20.0,
                    4.5,
                    0.0,
                    std::f64::consts::PI * 2.0,
                    false
                ],
                ["fillStyle", "rgba(120,123,134,0.55)"],
                ["fill"],
                ["strokeStyle", "#38c0fd"],
                ["stroke"],
            ])
        );
    }
}

mod muted_label {
    use super::*;

    fn label(id: &str) -> Box<dyn Drawing> {
        make(
            "text",
            id,
            vec![point(10.0, 50.0)],
            None,
            Some(text_block("Buy zone")),
        )
    }

    #[test]
    fn paints_a_text_label_normally() {
        let a = label("a");
        let canvas = paint_one_drawing(a.as_ref(), &proj());
        assert_eq!(canvas.texts(), ["Buy zone"]);
    }

    #[test]
    fn leaves_the_muted_label_to_its_inline_editor_and_only_that_one() {
        let (a, b) = (label("a"), label("b"));
        let targets = PaintTargets {
            muted_label: Some("a".into()),
            ..Default::default()
        };
        let canvas = paint(&[a.as_ref(), b.as_ref()], &proj(), &targets);
        assert_eq!(canvas.texts(), ["Buy zone"]); // 'b' still painted, 'a' muted
    }

    #[test]
    fn hides_the_muted_labels_drag_handle_too() {
        let a = label("a");
        let targets = PaintTargets {
            selected: ids(&["a"]),
            muted_label: Some("a".into()),
            ..Default::default()
        };
        let canvas = paint(&[a.as_ref()], &proj(), &targets);
        assert_eq!(canvas.calls("arc").len(), 0);
    }
}

mod text_frame {
    use super::*;

    fn frame(targets: PaintTargets) -> Vec<String> {
        let a = make(
            "text",
            "a",
            vec![point(10.0, 50.0)],
            None,
            Some(text_block("Buy zone")),
        );
        let canvas = paint(&[a.as_ref()], &proj(), &targets);
        // Targeting also paints the handle (a hex-colored border); the frame is the
        // themed rgba stroke.
        stroke_colors(&canvas)
            .into_iter()
            .filter(|s| s.starts_with("rgba("))
            .collect()
    }

    #[test]
    fn frames_a_selected_text_label_and_a_hovered_one_more_faintly() {
        let selected = frame(PaintTargets {
            selected: ids(&["a"]),
            ..Default::default()
        });
        let hovered = frame(PaintTargets {
            hovered: Some("a".into()),
            ..Default::default()
        });
        assert_eq!(selected.len(), 1);
        assert_eq!(hovered.len(), 1);
        assert!(selected[0].contains("0.3")); // firm once clicked
        assert!(hovered[0].contains("0.12")); // a hint under the cursor
        assert_eq!(selected[0], "rgba(255,255,255,0.3)");
    }

    #[test]
    fn leaves_an_untargeted_label_unframed() {
        assert!(frame(PaintTargets::default()).is_empty());
        assert!(
            frame(PaintTargets {
                selected: ids(&["other"]),
                hovered: Some("other".into()),
                ..Default::default()
            })
            .is_empty()
        );
    }

    #[test]
    fn the_frame_is_a_rounded_rect_around_the_text() {
        let a = make(
            "text",
            "a",
            vec![point(10.0, 50.0)],
            None,
            Some(text_block("Buy zone")),
        );
        let targets = PaintTargets {
            selected: ids(&["a"]),
            ..Default::default()
        };
        let canvas = paint(&[a.as_ref()], &proj(), &targets);
        // The label sits at (12, 52) top-left; 8 chars at 14 px are 8 * 14 * 0.6 wide.
        let w: f64 = 8.0 * 14.0 * 0.6;
        let (x, top, lh, fs): (f64, f64, f64, f64) = (12.0, 52.0, 20.0, 14.0);
        let rx = x - 5.0;
        let ry = top - (lh - fs) / 2.0 - 3.0;
        let rw = w + 10.0;
        let rh = lh + 6.0;
        let rr = 4.0f64.min(rw / 2.0).min(rh / 2.0);
        let ops = canvas.ops();
        let at = ops
            .iter()
            .position(|op| *op == json!(["moveTo", rx + rr, ry]))
            .expect("the frame path");
        assert_eq!(ops[at - 1], json!(["beginPath"]));
        assert_eq!(
            ops[at + 1],
            json!(["arcTo", rx + rw, ry, rx + rw, ry + rh, rr])
        );
        assert_eq!(ops[at + 5], json!(["closePath"]));
        assert_eq!(ops[at + 6], json!(["stroke"]));
    }
}

mod pane_separation {
    use super::*;

    #[test]
    fn clips_each_drawing_to_its_pane_rect() {
        let (a, b) = (hline("a", 30.0), hline("b", 50.0));
        let p = proj().paned();
        let canvas = paint(&[a.as_ref(), b.as_ref()], &p, &PaintTargets::default());
        assert_eq!(canvas.calls("clip").len(), 2); // one clip per painted drawing
        assert_eq!(
            canvas.calls("rect")[0],
            json!(["rect", 0.0, 0.0, 200.0, 100.0])
        );
    }

    #[test]
    fn skips_drawings_on_a_hidden_zero_height_pane_entirely() {
        let on_hidden = DrawingRegistry::builtin()
            .create("hline", {
                let mut init = DrawingInit::on_pane("hidden").anchors(vec![point(10.0, 30.0)]);
                init.id = Some("h".into());
                init
            })
            .unwrap();
        let targets = PaintTargets {
            selected: ids(&["h"]),
            ..Default::default()
        };
        let canvas = paint(&[on_hidden.as_ref()], &proj().paned(), &targets);
        assert_eq!(canvas.calls("stroke").len(), 0); // nothing painted
        assert_eq!(canvas.calls("arc").len(), 0); // no handles either
        assert!(canvas.is_empty());
    }

    #[test]
    fn skips_drawings_on_a_gone_pane() {
        let gone = DrawingRegistry::builtin()
            .create("hline", {
                DrawingInit::on_pane("nowhere").anchors(vec![point(10.0, 30.0)])
            })
            .unwrap();
        let canvas = paint(&[gone.as_ref()], &proj().paned(), &PaintTargets::default());
        assert!(canvas.is_empty());
    }

    #[test]
    fn paints_unclipped_when_the_projector_exposes_no_pane_geometry() {
        let a = hline("a", 30.0);
        let canvas = paint(&[a.as_ref()], &proj(), &PaintTargets::default());
        assert_eq!(canvas.calls("clip").len(), 0);
        assert!(!canvas.calls("stroke").is_empty());
    }

    #[test]
    fn an_invisible_drawing_paints_nothing_and_gets_no_handles() {
        let mut a = hline("a", 30.0);
        a.base_mut().visible = false;
        let targets = PaintTargets {
            selected: ids(&["a"]),
            ..Default::default()
        };
        assert!(paint(&[a.as_ref()], &proj(), &targets).is_empty());
    }
}

mod trendline_label {
    use super::*;

    fn labelled() -> Box<dyn Drawing> {
        make(
            "trendline",
            "t",
            vec![point(0.0, 0.0), point(50.0, 50.0)],
            None,
            Some(text_block("zone")),
        )
    }

    #[test]
    fn rotates_the_label_to_the_upright_segment_angle() {
        let d = labelled();
        let canvas = paint_one_drawing(d.as_ref(), &proj());
        let rotations = canvas.calls("rotate");
        assert_eq!(rotations.len(), 1);
        // y = 100 - price: (0,100) to (50,50) is up-right, so -pi/4.
        let angle = rotations[0][1].as_f64().unwrap();
        assert!((angle + std::f64::consts::FRAC_PI_4).abs() < 1e-9);
    }

    #[test]
    fn keeps_the_label_readable_with_a_halo_in_the_charts_background() {
        let d = labelled();
        let canvas = paint_one_drawing(d.as_ref(), &proj());
        let mut halos = Vec::new();
        let mut color = String::new();
        for op in canvas.ops() {
            match op[0].as_str().unwrap() {
                "strokeStyle" => color = op[1].as_str().unwrap().to_string(),
                "strokeText" => halos.push(color.clone()),
                _ => {}
            }
        }
        assert_eq!(halos, ["#101010"]);
    }

    #[test]
    fn does_not_rotate_a_flat_text_annotation() {
        let d = make(
            "text",
            "a",
            vec![point(10.0, 50.0)],
            None,
            Some(text_block("note")),
        );
        let canvas = paint_one_drawing(d.as_ref(), &proj());
        assert_eq!(canvas.calls("rotate").len(), 0);
    }
}

mod arrow_mark_label {
    use super::*;

    fn arrow(key: &str, value: &str) -> Box<dyn Drawing> {
        make(
            key,
            key,
            vec![point(10.0, 50.0)],
            None,
            Some(text_block(value)),
        )
    }

    /// `(text, x, y, align)` of every `fillText`, the align being the last one set.
    fn texts(canvas: &RecordingCanvas) -> Vec<(String, f64, f64, String)> {
        let mut align = String::new();
        let mut out = Vec::new();
        for op in canvas.ops() {
            match op[0].as_str().unwrap() {
                "textAlign" => align = op[1].as_str().unwrap().to_string(),
                "fillText" => out.push((
                    op[1].as_str().unwrap().to_string(),
                    num(&op[2]),
                    num(&op[3]),
                    align.clone(),
                )),
                _ => {}
            }
        }
        out
    }

    #[test]
    fn centers_an_up_arrow_label_below_the_glyph() {
        let d = arrow("arrowmarkup", "23");
        let canvas = paint_one_drawing(d.as_ref(), &proj());
        assert_eq!(
            texts(&canvas),
            [("23".to_string(), 10.0, 76.0, "center".to_string())]
        );
    }

    #[test]
    fn centers_a_down_arrow_label_above_the_glyph() {
        let d = arrow("arrowmarkdown", "23");
        let canvas = paint_one_drawing(d.as_ref(), &proj());
        let t = texts(&canvas);
        assert_eq!(t.len(), 1);
        assert_eq!(
            (t[0].0.as_str(), t[0].1, t[0].3.as_str()),
            ("23", 10.0, "center")
        );
        assert!(t[0].2 < 24.0);
    }

    #[test]
    fn keeps_multiline_down_arrow_labels_above_the_glyph() {
        let d = arrow("arrowmarkdown", "First line\nSecond line");
        let canvas = paint_one_drawing(d.as_ref(), &proj());
        let t = texts(&canvas);
        assert_eq!(
            t.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
            ["First line", "Second line"]
        );
        for label in &t {
            assert_eq!(label.1, 10.0);
            assert_eq!(label.3, "center");
            assert!(label.2 < 24.0);
        }
        assert!(t[1].2 > t[0].2);
    }
}

// ── drawings-box-midline.test.ts ──

mod box_midline {
    use super::*;

    const ANCHORS: [(f64, f64); 2] = [(10.0, 20.0), (110.0, 60.0)];

    fn linear() -> FnProj {
        FnProj::new(100.0, 300.0, 100.0)
    }

    fn rect_with(anchors: [(f64, f64); 2], text: Option<DrawingText>) -> Box<dyn Drawing> {
        make(
            "box",
            "r",
            anchors.iter().map(|&(t, p)| point(t, p)).collect(),
            None,
            text,
        )
    }

    fn rect() -> Box<dyn Drawing> {
        rect_with(ANCHORS, None)
    }

    fn settings(d: &dyn Drawing) -> crate::drawings::types::shapes::BoxSettings {
        d.as_any()
            .downcast_ref::<BoxDrawing>()
            .unwrap()
            .settings
            .clone()
    }

    fn midline_on(d: &mut dyn Drawing) {
        d.apply_setting("box.midline", &json!(true));
    }

    #[test]
    fn is_off_on_a_new_rectangle_and_an_old_one_paints_as_before() {
        let r = rect();
        assert!(!settings(r.as_ref()).midline);
        assert!(strokes(r.as_ref(), &linear()).is_empty());
        // Saved before rectangles had a midline: no per-type settings at all.
        let old: SerializedDrawing = serde_json::from_value(json!({
            "id": "old", "type": "box", "paneId": "price",
            "anchors": [{ "time": 10, "price": 20 }, { "time": 110, "price": 60 }],
            "style": { "lineColor": "#2962ff", "lineWidth": 1, "lineStyle": "solid", "fillColor": "#2962ff26" },
            "locked": false, "visible": true, "zIndex": 0, "createdAt": 1
        }))
        .unwrap();
        let old = DrawingRegistry::builtin().deserialize(&old).unwrap();
        assert!(!settings(old.as_ref()).midline);
        assert_eq!(
            recorded(&paint_one_drawing(old.as_ref(), &linear())),
            [
                Rec::FillRect([10.0, 40.0, 100.0, 40.0]),
                Rec::StrokeRect([10.0, 40.0, 100.0, 40.0]),
            ]
        );
    }

    #[test]
    fn switched_on_draws_one_line_halfway_across_the_full_width() {
        let mut r = rect();
        midline_on(r.as_mut());
        // Top edge y = 40, bottom edge y = 80.
        let lines = strokes(r.as_ref(), &linear());
        assert_eq!(lines.len(), 1);
        assert_eq!(segments_of(&lines[0]), [[10.0, 60.0, 110.0, 60.0]]);
    }

    #[test]
    fn on_a_log_scale_still_splits_the_box_into_two_equal_halves_on_screen() {
        let mut r = rect_with([(10.0, 10.0), (110.0, 1000.0)], None);
        midline_on(r.as_mut());
        let p = linear().log();
        let top = p.y_of(1000.0, "price").unwrap();
        let bottom = p.y_of(10.0, "price").unwrap();
        let lines = strokes(r.as_ref(), &p);
        assert!((segments_of(&lines[0])[0][1] - (top + bottom) / 2.0).abs() < 1e-9);
    }

    #[test]
    fn spans_the_box_whichever_way_it_was_drawn_and_follows_it_when_resized_or_moved() {
        let mut r = rect_with([(110.0, 60.0), (10.0, 20.0)], None);
        midline_on(r.as_mut());
        assert_eq!(
            segments_of(&strokes(r.as_ref(), &linear())[0]),
            [[10.0, 60.0, 110.0, 60.0]]
        );
        r.drag_handle(1, point(210.0, 0.0)); // the second corner: now 60 to 0
        assert_eq!(
            segments_of(&strokes(r.as_ref(), &linear())[0]),
            [[110.0, 70.0, 210.0, 70.0]]
        );
        let moved = r.translate_body(5.0, 10.0, &r.base().anchors.clone());
        r.base_mut().anchors = moved;
        assert_eq!(
            segments_of(&strokes(r.as_ref(), &linear())[0]),
            [[115.0, 60.0, 215.0, 60.0]]
        );
    }

    #[test]
    fn takes_the_border_color_until_one_is_picked_and_is_dashed_at_the_border_width() {
        let mut r = rect();
        r.apply_setting(
            "style",
            &json!({ "lineColor": "#ff8800", "lineWidth": 2, "lineStyle": "solid" }),
        );
        midline_on(r.as_mut());
        let first = strokes(r.as_ref(), &linear());
        assert_eq!(
            first[0],
            Rec::Stroke {
                color: "#ff8800".into(),
                width: 2.0,
                dash: dash_pattern(LineStyle::Dashed, 2.0),
                segments: vec![[10.0, 60.0, 110.0, 60.0]],
            }
        );
        // The border changes: the midline follows.
        r.apply_setting("style.lineColor", &json!("#00ff00"));
        match &strokes(r.as_ref(), &linear())[0] {
            Rec::Stroke { color, .. } => assert_eq!(color, "#00ff00"),
            other => panic!("{other:?}"),
        }
        r.apply_setting("box.midColor", &json!("#ffffff"));
        r.apply_setting("box.midStyle", &json!("dotted"));
        r.apply_setting("style.lineColor", &json!("#123456")); // a picked color stays put
        match &strokes(r.as_ref(), &linear())[0] {
            Rec::Stroke { color, dash, .. } => {
                assert_eq!(color, "#ffffff");
                assert_eq!(dash, &dash_pattern(LineStyle::Dotted, 2.0));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn survives_saving_and_reloading_and_an_unpicked_color_keeps_following_the_border() {
        let mut r = rect();
        midline_on(r.as_mut());
        r.apply_setting("box.midStyle", &json!("dotted"));
        let saved = serde_json::to_string(&r.serialize()).unwrap();
        let doc: SerializedDrawing = serde_json::from_str(&saved).unwrap();
        let mut back = DrawingRegistry::builtin().deserialize(&doc).unwrap();
        let s = settings(back.as_ref());
        assert!(s.midline && s.mid_color.is_none());
        assert_eq!(s.mid_style, LineStyle::Dotted);
        assert_eq!(
            strokes(back.as_ref(), &linear()),
            strokes(r.as_ref(), &linear())
        );
        back.apply_setting("style.lineColor", &json!("#abcdef"));
        match &strokes(back.as_ref(), &linear())[0] {
            Rec::Stroke { color, .. } => assert_eq!(color, "#abcdef"),
            other => panic!("{other:?}"),
        }
    }

    fn labelled(value: &str) -> Box<dyn Drawing> {
        let mut r = rect_with(ANCHORS, Some(text_block(value)));
        midline_on(r.as_mut());
        r
    }

    /// The width of `chars` characters at the box's 14 px label font.
    fn width_of(chars: f64) -> f64 {
        chars * 14.0 * 0.6
    }

    #[test]
    fn breaks_around_the_label_keeping_clear_of_the_text_on_both_sides() {
        // "Range" at the box's center (x = 60): the line stops 6 px short of each side.
        let half = width_of(5.0) / 2.0 + 6.0;
        assert_eq!(
            segments_of(&strokes(labelled("Range").as_ref(), &linear())[0]),
            [
                [10.0, 60.0, 60.0 - half, 60.0],
                [60.0 + half, 60.0, 110.0, 60.0]
            ]
        );
    }

    #[test]
    fn paints_over_the_fill_under_the_border_and_under_the_label() {
        let kinds: Vec<&str> = recorded(&paint_one_drawing(labelled("IB").as_ref(), &linear()))
            .iter()
            .map(|r| match r {
                Rec::FillRect(_) => "fillRect",
                Rec::Stroke { .. } => "stroke",
                Rec::StrokeRect(_) => "strokeRect",
                Rec::Text(_) => "text",
            })
            .collect();
        assert_eq!(kinds, ["fillRect", "stroke", "strokeRect", "text"]);
    }

    #[test]
    fn a_multi_line_label_clears_the_line_by_its_widest_line() {
        let half = width_of(7.0) / 2.0 + 6.0; // "Session"
        assert_eq!(
            segments_of(&strokes(labelled("Asia\nSession").as_ref(), &linear())[0]),
            [
                [10.0, 60.0, 60.0 - half, 60.0],
                [60.0 + half, 60.0, 110.0, 60.0]
            ]
        );
    }

    #[test]
    fn a_label_wider_than_the_box_leaves_no_line_and_an_empty_label_leaves_it_whole() {
        assert!(
            segments_of(
                &strokes(
                    labelled("Asian Session Range wide label").as_ref(),
                    &linear()
                )[0]
            )
            .is_empty()
        );
        assert_eq!(
            segments_of(&strokes(labelled("").as_ref(), &linear())[0]),
            [[10.0, 60.0, 110.0, 60.0]]
        );
    }
}

// ── drawings-trend-angle-label.test.ts ──

mod trend_angle_label {
    use super::*;

    /// Does the segment cross the rectangle? (Sampled: the rects are small.)
    fn crosses(seg: [f64; 4], r: (f64, f64, f64, f64)) -> bool {
        (0..=400).any(|i| {
            let t = i as f64 / 400.0;
            let x = seg[0] + (seg[2] - seg[0]) * t;
            let y = seg[1] + (seg[3] - seg[1]) * t;
            x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3
        })
    }

    #[test]
    fn the_readout_stays_clear_of_its_line() {
        let cases = [
            ("rising to the right", point(300.0, 160.0)),
            ("falling to the right", point(300.0, 40.0)),
            ("rising to the left", point(20.0, 160.0)),
            ("falling to the left", point(20.0, 40.0)),
            ("nearly flat", point(300.0, 108.0)),
        ];
        for (name, end) in cases {
            let start = point(120.0, 100.0);
            let d = make("trendangle", "t", vec![start, end], None, None);
            let canvas = paint_one_drawing(d.as_ref(), &FnProj::new(200.0, 400.0, 200.0));
            let label = canvas
                .calls("fillText")
                .into_iter()
                .find(|op| op[1].as_str().unwrap().ends_with('\u{b0}'))
                .unwrap_or_else(|| panic!("{name}: no readout"));
            let (s, x, y) = (label[1].as_str().unwrap(), num(&label[2]), num(&label[3]));
            // Alphabetic baseline at an 11 px font: the glyphs span ~9 px above it, ~3 below.
            let glyphs = (x, y - 9.0, s.chars().count() as f64 * 6.0, 12.0);
            let seg = [start.time, 200.0 - start.price, end.time, 200.0 - end.price];
            assert!(!crosses(seg, glyphs), "{name}");
        }
    }

    #[test]
    fn the_readout_is_the_screen_angle_in_degrees() {
        let d = make(
            "trendangle",
            "t",
            vec![point(120.0, 100.0), point(220.0, 200.0)],
            None,
            None,
        );
        let canvas = paint_one_drawing(d.as_ref(), &FnProj::new(300.0, 400.0, 300.0));
        // Up-right at 45 degrees on screen (y = 300 - price).
        assert!(canvas.texts().contains(&"45.0\u{b0}".to_string()));
    }
}

// ── drawings-trendline-extend.test.ts ──

mod trendline_extend {
    use super::*;

    fn line(extend_left: bool, extend_right: bool) -> Box<dyn Drawing> {
        let mut d = make(
            "trendline",
            "t",
            vec![point(50.0, 50.0), point(100.0, 50.0)],
            None,
            None,
        );
        d.apply_setting(
            "style",
            &json!({ "lineColor": "#fff", "lineWidth": 1, "lineStyle": "solid",
                     "extendLeft": extend_left, "extendRight": extend_right }),
        );
        d
    }

    /// The x-span the painter strokes for the line.
    fn painted_span(d: &dyn Drawing) -> (f64, f64) {
        let canvas = paint_one_drawing(d, &FnProj::new(100.0, 200.0, 100.0));
        let xs: Vec<f64> = canvas
            .ops()
            .iter()
            .filter(|op| matches!(op[0].as_str(), Some("moveTo" | "lineTo")))
            .map(|op| num(&op[1]))
            .collect();
        (
            xs.iter().copied().fold(f64::INFINITY, f64::min),
            xs.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        )
    }

    #[test]
    fn a_plain_trend_line_stops_at_its_points() {
        let d = line(false, false);
        let p = FnProj::new(100.0, 200.0, 100.0);
        assert!(!d.hit_test(150.0, 50.0, &p, 3.0));
        assert!(!d.hit_test(20.0, 50.0, &p, 3.0));
        assert_eq!(painted_span(d.as_ref()), (50.0, 100.0));
    }

    #[test]
    fn extended_right_it_runs_on_to_the_right_edge_and_can_be_clicked_there() {
        let d = line(false, true);
        let p = FnProj::new(100.0, 200.0, 100.0);
        assert!(d.hit_test(180.0, 50.0, &p, 3.0));
        assert!(!d.hit_test(20.0, 50.0, &p, 3.0));
        let (from, to) = painted_span(d.as_ref());
        assert_eq!(from, 50.0);
        assert!(to >= 200.0);
    }

    #[test]
    fn extended_both_ways_it_crosses_the_whole_chart() {
        let d = line(true, true);
        let p = FnProj::new(100.0, 200.0, 100.0);
        assert!(d.hit_test(5.0, 50.0, &p, 3.0));
        assert!(d.hit_test(195.0, 50.0, &p, 3.0));
        let (from, to) = painted_span(d.as_ref());
        assert!(from <= 0.0);
        assert!(to >= 200.0);
    }
}

#[test]
fn text_place_and_side_move_the_rotated_label() {
    let mut text = text_block("AB");
    text.place = Some(TextPlace::Start);
    text.side = Some(TextSide::Below);
    let d = make(
        "trendline",
        "t",
        vec![point(0.0, 100.0), point(100.0, 100.0)],
        None,
        Some(text),
    );
    let canvas = paint_one_drawing(d.as_ref(), &FnProj::new(100.0, 200.0, 100.0));
    // A flat line: no rotation (angle 0), pivot 12% along it, text 6 px under it.
    assert!(canvas.ops().contains(&json!(["translate", 12.0, 0.0])));
    assert!(canvas.ops().contains(&json!(["fillText", "AB", 0.0, 6.0])));
}
