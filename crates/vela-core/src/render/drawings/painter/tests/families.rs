//! Full call streams, one tool per family, expected values derived by reading the
//! original's `DrawingPainter` line by line. Coordinates and labels that the MODEL computes
//! (entry lines, zones, layouts, pattern ratios) are read from the model's public API, so
//! these tests pin the painter's own part: order, constants and arguments.

use std::f64::consts::PI;

use super::*;
use crate::drawings::types::channels::ParallelChannel;
use crate::drawings::types::fib::FibRatios;
use crate::drawings::types::measure::{DatePriceRange, PositionTool};
use crate::drawings::types::patterns::PatternDrawing;
use crate::drawings::types::price_action::FairValueGap;
use crate::render::shared::drawing_geometry::dash_pattern;

type Ops = Vec<Value>;

/// `save`, a pane-rect clip over the full plot, as `paint_all` opens every drawing.
fn clip_open(w: f64, h: f64) -> Ops {
    vec![
        json!(["save"]),
        json!(["beginPath"]),
        json!(["rect", 0.0, 0.0, w, h]),
        json!(["clip"]),
    ]
}

/// The strokes of `stroke_path`: style, begin, the path ops, stroke, dash reset.
fn stroked(color: &str, width: f64, dash: Vec<f64>, path: Ops) -> Ops {
    let mut ops = vec![
        json!(["strokeStyle", color]),
        json!(["lineWidth", width]),
        json!(["setLineDash", dash]),
        json!(["beginPath"]),
    ];
    ops.extend(path);
    ops.push(json!(["stroke"]));
    ops.push(json!(["setLineDash", []]));
    ops
}

fn seg(a: (f64, f64), b: (f64, f64)) -> Ops {
    vec![json!(["moveTo", a.0, a.1]), json!(["lineTo", b.0, b.1])]
}

fn assert_stream(canvas: &RecordingCanvas, expected: Ops) {
    let actual = canvas.ops();
    for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        assert_eq!(a, e, "call #{i} differs");
    }
    assert_eq!(actual.len(), expected.len(), "call count differs");
}

/// The label ops of `paint_label` for a flat (unrotated) text, no halo.
fn plain_label(font: &str, align: &str, fill: &str, lines: &[(&str, f64, f64)]) -> Ops {
    let mut ops = vec![
        json!(["save"]),
        json!(["font", font]),
        json!(["textBaseline", "top"]),
        json!(["textAlign", align]),
        json!(["fillStyle", fill]),
    ];
    for (t, x, y) in lines {
        ops.push(json!(["fillText", t, x, y]));
    }
    ops.push(json!(["restore"]));
    ops
}

fn sample_px(x: f64, y: f64) -> DrawingPoint {
    // The sample projector: x = time / 60000, y = (200 - price) / 0.25.
    point(x * 60_000.0, 200.0 - 0.25 * y)
}

#[test]
fn a_trend_line_with_extension_arrow_and_text() {
    let mut d = make(
        "trendline",
        "t",
        vec![sample_px(100.0, 300.0), sample_px(300.0, 200.0)],
        None,
        Some(text_block("AB")),
    );
    d.apply_setting(
        "style",
        &json!({ "lineColor": "#38c0fd", "lineWidth": 2, "lineStyle": "solid",
                 "extendRight": true, "arrowRight": true }),
    );
    let canvas = paint_one_drawing(d.as_ref(), &LinearProjector::sample());

    // extendEndpoints(100, 300, 300, 200, 'right', 1000, 600): the line runs on to
    // x = width + 2 along slope -0.5.
    let ang = (200.0f64 - 300.0).atan2(300.0 - 100.0);
    let spread = PI / 7.0;
    let len = 9.0f64; // max(9, 2 * 3.5)
    let mut e = clip_open(1000.0, 600.0);
    e.extend(stroked(
        "#38c0fd",
        2.0,
        vec![],
        seg((100.0, 300.0), (1002.0, 300.0 + -0.5 * (1002.0 - 100.0))),
    ));
    e.extend([
        json!(["save"]),
        json!(["setLineDash", []]),
        json!(["fillStyle", "#38c0fd"]),
        json!(["beginPath"]),
        json!(["moveTo", 300.0, 200.0]),
        json!([
            "lineTo",
            300.0 - len * (ang - spread).cos(),
            200.0 - len * (ang - spread).sin()
        ]),
        json!([
            "lineTo",
            300.0 - len * (ang + spread).cos(),
            200.0 - len * (ang + spread).sin()
        ]),
        json!(["closePath"]),
        json!(["fill"]),
        json!(["restore"]),
        // The label rides the segment's middle, rotated, 6 px + one line above it, with a
        // halo in the chart background.
        json!(["save"]),
        json!(["translate", 200.0, 250.0]),
        json!(["rotate", ang]),
        json!(["font", "14px sans-serif"]),
        json!(["textBaseline", "top"]),
        json!(["textAlign", "center"]),
        json!(["lineJoin", "round"]),
        json!(["lineWidth", 3.0]),
        json!(["strokeStyle", "#101010"]),
        json!(["strokeText", "AB", 0.0, -26.0]),
        json!(["fillStyle", "#ffffff"]),
        json!(["fillText", "AB", 0.0, -26.0]),
        json!(["restore"]),
        json!(["restore"]),
    ]);
    assert_stream(&canvas, e);
}

#[test]
fn a_box_with_midline_and_text() {
    let d = make(
        "box",
        "b",
        vec![point(10.0, 20.0), point(110.0, 60.0)],
        Some(json!({ "midline": true })),
        Some(text_block("Range")),
    );
    let canvas = paint_one_drawing(d.as_ref(), &FnProj::new(100.0, 300.0, 100.0));
    // 5 characters at 14 px: 0.6 em each. The midline stops 6 px short of the text.
    let half = 5.0 * 14.0 * 0.6 / 2.0 + 6.0;
    let mut e = vec![
        json!(["save"]),
        json!(["fillStyle", "#38c0fd26"]),
        json!(["fillRect", 10.0, 40.0, 100.0, 40.0]),
        json!(["restore"]),
        json!(["save"]),
        json!(["font", "14px sans-serif"]),
        json!(["restore"]),
    ];
    let mut path = seg((10.0, 60.0), (60.0 - half, 60.0));
    path.extend(seg((60.0 + half, 60.0), (110.0, 60.0)));
    e.extend(stroked("#38c0fd", 1.0, vec![4.0, 3.0], path));
    e.extend([
        json!(["strokeStyle", "#38c0fd"]),
        json!(["lineWidth", 1.0]),
        json!(["setLineDash", []]),
        json!(["strokeRect", 10.0, 40.0, 100.0, 40.0]),
        json!(["setLineDash", []]),
    ]);
    e.extend(plain_label(
        "14px sans-serif",
        "center",
        "#ffffff",
        &[("Range", 60.0, 50.0)],
    ));
    assert_stream(&canvas, e);
}

#[test]
fn a_fibonacci_retracement_with_bands_and_labels() {
    let props = json!({
        "levels": [
            { "ratio": 0, "color": "#787b86", "enabled": true, "label": "Start" },
            { "ratio": 0.5, "color": "#089981", "enabled": true },
            { "ratio": 1, "color": "#f23645", "enabled": true },
        ]
    });
    let d = make(
        "fibretracement",
        "f",
        vec![sample_px(100.0, 100.0), sample_px(400.0, 400.0)],
        Some(props),
        None,
    );
    let proj = LinearProjector::sample();
    let fib = d
        .as_any()
        .downcast_ref::<crate::drawings::types::fib::FibRetracement>()
        .unwrap();
    let lines = fib.entry_lines(&proj).unwrap();
    let bands = fib.fill_bands(&proj);
    assert_eq!(lines.len(), 3);
    assert!(!bands.is_empty());
    let style = d.base().style.clone();

    let canvas = paint_one_drawing(d.as_ref(), &proj);
    let mut e = clip_open(1000.0, 600.0);
    for b in &bands {
        e.extend([
            json!(["save"]),
            json!(["globalAlpha", 0.06 * 1.0]),
            json!(["fillStyle", b.color]),
            json!(["fillRect", b.x, b.y, b.w, b.h]),
            json!(["restore"]),
        ]);
    }
    e.push(json!(["textBaseline", "middle"]));
    for l in &lines {
        e.extend(stroked(
            &l.color,
            style.line_width,
            dash_pattern(style.line_style, style.line_width),
            seg((l.x1, l.y1), (l.x2, l.y2)),
        ));
        let align = match l.number_align {
            crate::drawings::types::fib::FibAlign::Left => "left",
            crate::drawings::types::fib::FibAlign::Center => "center",
            crate::drawings::types::fib::FibAlign::Right => "right",
        };
        e.extend([
            json!(["fillStyle", l.color]),
            json!(["font", "12px sans-serif"]), // numbers default small
            json!(["textAlign", align]),
            json!(["fillText", l.number_text, l.number_x, l.number_y]),
        ]);
        if let Some(label) = &l.label {
            e.extend([
                json!(["font", "14px sans-serif"]), // labels default normal
                json!(["textAlign", "center"]),
                json!(["fillText", label, l.label_x, l.label_y]),
            ]);
        }
    }
    e.extend([
        json!(["textAlign", "left"]),
        json!(["textBaseline", "alphabetic"]),
        json!(["restore"]),
    ]);
    assert_stream(&canvas, e);
    assert!(canvas.texts().contains(&"Start".to_string()));
}

#[test]
fn a_parallel_channel_with_fill() {
    let d = make(
        "parallelchannel",
        "c",
        vec![
            sample_px(100.0, 400.0),
            sample_px(400.0, 300.0),
            sample_px(250.0, 250.0),
        ],
        None,
        None,
    );
    let proj = LinearProjector::sample();
    let ch = d.as_any().downcast_ref::<ParallelChannel>().unwrap();
    let g = ch.geometry(&proj).unwrap();
    let fill = g.fill.clone().unwrap();
    let style = d.base().style.clone();
    let fill_color = style
        .fill_color
        .clone()
        .expect("the channel's default fill");

    let canvas = paint_one_drawing(d.as_ref(), &proj);
    let mut e = clip_open(1000.0, 600.0);
    e.extend([
        json!(["save"]),
        json!(["fillStyle", fill_color]),
        json!(["beginPath"]),
        json!(["moveTo", fill[0].0, fill[0].1]),
    ]);
    for p in &fill[1..] {
        e.push(json!(["lineTo", p.0, p.1]));
    }
    e.extend([json!(["closePath"]), json!(["fill"]), json!(["restore"])]);
    for s in &g.segments {
        e.extend(stroked(
            &style.line_color,
            style.line_width,
            dash_pattern(style.line_style, style.line_width),
            seg((s[0], s[1]), (s[2], s[3])),
        ));
    }
    e.push(json!(["restore"]));
    assert_stream(&canvas, e);
}

#[test]
fn a_callout_with_its_tail() {
    let d = make(
        "callout",
        "c",
        // target (150, 20), box center (60, 60) on x = time, y = 100 - price
        vec![point(150.0, 80.0), point(60.0, 40.0)],
        None,
        Some(text_block("Hi")),
    );
    let style = d.base().style.clone();
    let fill = style
        .fill_color
        .clone()
        .unwrap_or_else(|| "#101010".to_string());
    let canvas = paint_one_drawing(d.as_ref(), &FnProj::new(100.0, 300.0, 100.0));

    let (tx, ty, cx, cy) = (150.0f64, 20.0f64, 60.0f64, 60.0f64);
    let tw = (2.0f64 * 14.0 * 0.6).max(8.0);
    let (pad_x, pad_y, lh) = (9.0, 6.0, 20.0);
    let w = tw + pad_x * 2.0;
    let h = 1.0 * lh + pad_y * 2.0;
    let (x, y) = (cx - w / 2.0, cy - h / 2.0);
    let r = 5.0f64.min(w / 2.0).min(h / 2.0);
    let (dx, dy) = (tx - cx, ty - cy);
    let half = 6.0f64.max(w.min(h) * 0.18);
    // |dx| / hw >= |dy| / hh: the tail leaves the right edge.
    let ex = x + w;
    let at = (y + r + half).max((y + h - r - half).min(cy + (dy * (ex - cx)) / dx));
    let path = |e: &mut Ops| {
        e.extend([
            json!(["beginPath"]),
            json!(["moveTo", x + r, y]),
            json!(["lineTo", x + w - r, y]),
            json!(["arcTo", x + w, y, x + w, y + r, r]),
            json!(["lineTo", x + w, at - half]),
            json!(["lineTo", tx, ty]),
            json!(["lineTo", x + w, at + half]),
            json!(["lineTo", x + w, y + h - r]),
            json!(["arcTo", x + w, y + h, x + w - r, y + h, r]),
            json!(["lineTo", x + r, y + h]),
            json!(["arcTo", x, y + h, x, y + h - r, r]),
            json!(["lineTo", x, y + r]),
            json!(["arcTo", x, y, x + r, y, r]),
            json!(["closePath"]),
        ]);
    };
    let mut e = vec![
        json!(["font", "14px sans-serif"]),
        json!(["save"]),
        json!(["fillStyle", fill]),
    ];
    path(&mut e);
    e.extend([
        json!(["fill"]),
        json!(["restore"]),
        json!(["strokeStyle", style.line_color]),
        json!(["lineWidth", style.line_width]),
        json!([
            "setLineDash",
            dash_pattern(style.line_style, style.line_width)
        ]),
    ]);
    path(&mut e);
    e.extend([
        json!(["stroke"]),
        json!(["setLineDash", []]),
        // The text sits on the plate: contrast against the (dark) background fill.
        json!([
            "fillStyle",
            crate::render::shared::drawing_geometry::contrast_color(Some(&fill))
        ]),
        json!(["textBaseline", "top"]),
        json!(["textAlign", "left"]),
        json!(["fillText", "Hi", x + pad_x, y + pad_y]),
    ]);
    assert_stream(&canvas, e);
}

#[test]
fn a_harmonic_pattern_with_labels_ratios_and_its_name_badge() {
    let anchors: Vec<DrawingPoint> = [
        (100.0, 400.0),
        (200.0, 200.0),
        (300.0, 320.0),
        (400.0, 250.0),
        (500.0, 330.0),
    ]
    .iter()
    .map(|&(x, y)| sample_px(x, y))
    .collect();
    let d = make("gartley", "g", anchors, None, None);
    let proj = LinearProjector::sample();
    let pat = d.as_any().downcast_ref::<PatternDrawing>().unwrap();
    let pts = d.handle_points(&proj);
    assert_eq!(pts.len(), 5);
    let style = d.base().style.clone();
    let valid = pat.valid();
    let labels = pat.vertex_labels();
    let triangles = pat.fill_triangles();
    assert!(!triangles.is_empty());

    let canvas = paint_one_drawing(d.as_ref(), &proj);
    let mut e = clip_open(1000.0, 600.0);
    // The wash: the validity tint at 0.1 alpha (no user fill color).
    let tint = match valid {
        Some(true) => "#0ecb81",
        Some(false) => "#f6465d",
        None => style.line_color.as_str(),
    };
    for &[i, j, k] in triangles {
        e.extend([
            json!(["save"]),
            json!(["globalAlpha", 0.1 * 1.0]),
            json!(["fillStyle", tint]),
            json!(["beginPath"]),
            json!(["moveTo", pts[i].0, pts[i].1]),
            json!(["lineTo", pts[j].0, pts[j].1]),
            json!(["lineTo", pts[k].0, pts[k].1]),
            json!(["closePath"]),
            json!(["fill"]),
            json!(["restore"]),
        ]);
    }
    let mut path = vec![json!(["moveTo", pts[0].0, pts[0].1])];
    path.extend(pts[1..].iter().map(|p| json!(["lineTo", p.0, p.1])));
    e.extend(stroked(
        &style.line_color,
        style.line_width,
        dash_pattern(style.line_style, style.line_width),
        path,
    ));
    e.extend([
        json!(["fillStyle", "#ffffff"]),
        json!(["font", "bold 12px sans-serif"]),
        json!(["textAlign", "center"]),
    ]);
    for (i, p) in pts.iter().enumerate() {
        let prev = if i > 0 { pts[i - 1].1 } else { p.1 };
        let next = pts.get(i + 1).map_or(p.1, |n| n.1);
        let above = p.1 <= (prev + next) / 2.0;
        let label = crate::i18n::source_text(&labels[i]);
        e.extend([
            json!(["textBaseline", if above { "bottom" } else { "top" }]),
            json!([
                "fillText",
                label,
                p.0,
                if above { p.1 - 7.0 } else { p.1 + 7.0 }
            ]),
        ]);
    }
    assert!(pat.leg_ratios());
    e.extend([
        json!(["font", "11px sans-serif"]),
        json!(["textAlign", "left"]),
        json!(["textBaseline", "middle"]),
    ]);
    for i in 2..pts.len() {
        if let Some(r) = pat.ratio_at(i) {
            let color = match pat.ratio_ok(i) {
                Some(true) => "#0ecb81",
                Some(false) => "#f6465d",
                None => "#ffffff",
            };
            e.extend([
                json!(["fillStyle", color]),
                json!([
                    "fillText",
                    crate::util::to_fixed(r, 3),
                    (pts[i - 1].0 + pts[i].0) / 2.0 + 6.0,
                    (pts[i - 1].1 + pts[i].1) / 2.0
                ]),
            ]);
        }
    }
    let (dp, prev) = (pts[4], pts[3]);
    let above = dp.1 <= prev.1;
    e.extend([
        json!(["font", "bold 12px sans-serif"]),
        json!([
            "fillStyle",
            if valid == Some(false) {
                "#f6465d"
            } else {
                "#0ecb81"
            }
        ]),
        json!(["textAlign", "center"]),
        json!(["textBaseline", if above { "bottom" } else { "top" }]),
        json!([
            "fillText",
            format!(
                "Gartley {}",
                if valid == Some(true) {
                    '\u{2713}'
                } else {
                    '\u{2717}'
                }
            ),
            dp.0,
            if above { dp.1 - 22.0 } else { dp.1 + 22.0 }
        ]),
        json!(["textAlign", "left"]),
        json!(["textBaseline", "alphabetic"]),
        json!(["restore"]),
    ]);
    assert_stream(&canvas, e);
}

#[test]
fn a_position_tool_with_zones_levels_and_labels() {
    let d = make(
        "position",
        "p",
        vec![sample_px(100.0, 300.0), sample_px(400.0, 200.0)],
        None,
        None,
    );
    let proj = LinearProjector::sample();
    let pos = d.as_any().downcast_ref::<PositionTool>().unwrap();
    let l = pos.layout(&proj).unwrap();
    let style = d.base().style.clone();
    let dash = dash_pattern(style.line_style, style.line_width);
    assert!(pos.show_text && pos.show_header);

    let canvas = paint_one_drawing(d.as_ref(), &proj);
    let w = l.x2 - l.x1;
    let zone = |ya: f64, yb: f64, color: &str| {
        vec![
            json!(["save"]),
            json!(["globalAlpha", 0.13 * 1.0]),
            json!(["fillStyle", color]),
            json!(["fillRect", l.x1, ya.min(yb), w, (yb - ya).abs()]),
            json!(["restore"]),
        ]
    };
    let mut e = clip_open(1000.0, 600.0);
    e.extend(zone(l.ey, l.ty, &pos.profit_color));
    e.extend(zone(l.ey, l.sy, &pos.loss_color));
    e.extend(stroked(
        &pos.profit_color,
        style.line_width,
        dash.clone(),
        seg((l.x1, l.ty), (l.x2, l.ty)),
    ));
    e.extend(stroked(
        &pos.loss_color,
        style.line_width,
        dash.clone(),
        seg((l.x1, l.sy), (l.x2, l.sy)),
    ));
    e.extend(stroked(
        &style.line_color,
        style.line_width,
        dash,
        seg((l.x1, l.ey), (l.x2, l.ey)),
    ));
    let cx = l.x1 + w / 2.0;
    let top_y = l.ey.min(l.sy).min(l.ty) - 9.0;
    e.extend([
        json!(["fillStyle", "#ffffff"]),
        json!(["textAlign", "center"]),
        json!(["textBaseline", "middle"]),
        json!(["font", "14px sans-serif"]),
    ]);
    let (mut header_y, loss_y) = (top_y, top_y);
    if pos.show_loss_size {
        header_y = top_y - (14.0f64 * 1.15 + 0.5).floor();
    }
    e.push(json!(["fillText", pos.header_label(), cx, header_y]));
    e.push(json!(["font", "13px sans-serif"])); // max(10, 14 - 1)
    if pos.show_loss_size {
        e.push(json!(["fillText", pos.loss_size_label(), cx, loss_y]));
    }
    if pos.show_target_label {
        e.push(json!([
            "fillText",
            pos.target_label(),
            cx,
            (l.ey + l.ty) / 2.0
        ]));
    }
    if pos.show_stop_label {
        e.push(json!([
            "fillText",
            pos.stop_label(),
            cx,
            (l.ey + l.sy) / 2.0
        ]));
    }
    e.extend([
        json!(["textAlign", "left"]),
        json!(["textBaseline", "alphabetic"]),
        json!(["restore"]),
    ]);
    assert_stream(&canvas, e);
}

#[test]
fn the_date_and_price_range_measure() {
    let d = make(
        "datepricerange",
        "m",
        vec![sample_px(100.0, 400.0), sample_px(300.0, 200.0)],
        None,
        None,
    );
    let proj = LinearProjector::sample();
    let m = d.as_any().downcast_ref::<DatePriceRange>().unwrap();
    assert!(m.is_up());
    let lines = m.measure_label(&proj);
    assert!(!lines.is_empty());
    let style = d.base().style.clone();

    let canvas = paint_one_drawing(d.as_ref(), &proj);
    // Corners (100, 400) and (300, 200): x 100, y 200, w 200, h 200; an up measure is green.
    let (x, y, w, h) = (100.0, 200.0, 200.0, 200.0);
    let lh = 14.0f64 * 1.35;
    let lh = (lh + 0.5).floor();
    let start_y = y + h / 2.0 - ((lines.len() as f64 - 1.0) * lh) / 2.0;
    let mut e = clip_open(1000.0, 600.0);
    e.extend([
        json!(["save"]),
        json!(["globalAlpha", 0.12 * 1.0]),
        json!(["fillStyle", "#089981"]),
        json!(["fillRect", x, y, w, h]),
        json!(["restore"]),
        json!([
            "setLineDash",
            dash_pattern(style.line_style, style.line_width)
        ]),
        json!(["strokeStyle", "#089981"]),
        json!(["lineWidth", style.line_width]),
        json!(["strokeRect", x, y, w, h]),
        json!(["setLineDash", []]),
        json!(["fillStyle", "#ffffff"]),
        json!(["font", "14px sans-serif"]),
        json!(["textAlign", "center"]),
        json!(["textBaseline", "middle"]),
    ]);
    for (i, line) in lines.iter().enumerate() {
        e.push(json!([
            "fillText",
            line,
            x + w / 2.0,
            start_y + i as f64 * lh
        ]));
    }
    e.extend([
        json!(["textAlign", "left"]),
        json!(["textBaseline", "alphabetic"]),
        json!(["restore"]),
    ]);
    assert_stream(&canvas, e);
}

#[test]
fn a_fair_value_gap_with_bars() {
    // The bullish gap at bar 20 (x = 100 px): bar 19's high stays below bar 21's low.
    let d = make(
        "fairvaluegap",
        "g",
        vec![sample_px(100.0, 150.0)],
        None,
        None,
    );
    let proj = bars_projector();
    let gap = d.as_any().downcast_ref::<FairValueGap>().unwrap();
    let z = gap.zone(&proj).expect("a zone under the anchor candle");
    let canvas = paint_one_drawing(d.as_ref(), &proj);

    let stroke_hex = super::super::helpers::to_hex6(&z.fill, &z.fill);
    let h = z.y_bottom - z.y_top;
    let mut e = clip_open(1000.0, 600.0);
    e.extend([
        json!(["save"]),
        json!(["globalAlpha", 1.0 * if z.faded { 0.6 } else { 1.0 }]),
        json!(["fillStyle", z.fill]),
        json!(["fillRect", z.x1, z.y_top, z.x2 - z.x1, h]),
    ]);
    let rnd = |v: f64| (v + 0.5).floor();
    if z.border {
        e.extend([
            json!(["lineWidth", 1.0]),
            json!(["setLineDash", []]),
            json!(["strokeStyle", stroke_hex]),
            json!([
                "strokeRect",
                rnd(z.x1) + 0.5,
                rnd(z.y_top) + 0.5,
                rnd(z.x2 - z.x1),
                rnd(h)
            ]),
        ]);
    }
    if let Some(mid) = z.mid_y {
        let y = rnd(mid) + 0.5;
        e.extend(stroked(
            &stroke_hex,
            1.0,
            vec![4.0, 3.0],
            seg((z.x1, y), (z.x2, y)),
        ));
    }
    e.push(json!(["restore"]));
    if let Some(f) = &z.flip {
        e.extend([
            json!(["save"]),
            json!(["fillStyle", f.fill]),
            json!(["fillRect", f.x1, z.y_top, f.x2 - f.x1, h]),
            json!(["lineWidth", 1.0]),
            json!([
                "strokeStyle",
                super::super::helpers::to_hex6(&f.fill, &f.fill)
            ]),
            json!(["setLineDash", [5.0, 4.0]]),
            json!([
                "strokeRect",
                rnd(f.x1) + 0.5,
                rnd(z.y_top) + 0.5,
                rnd(f.x2 - f.x1),
                rnd(h)
            ]),
            json!(["restore"]),
        ]);
    }
    if let Some(m) = &z.marker {
        e.extend([
            json!(["beginPath"]),
            json!(["arc", m.x, m.y, 3.5, 0.0, PI * 2.0, false]),
            json!(["fillStyle", stroke_hex]),
            json!(["fill"]),
        ]);
    }
    e.push(json!(["restore"]));
    assert_stream(&canvas, e);
    // The bullish zone is the default up color's hue.
    assert!(z.fill.starts_with('#') || z.fill.starts_with("rgba"));
}

#[test]
fn a_fair_value_gap_without_a_zone_leaves_a_faint_tick() {
    // Bar 5 is inside no gap.
    let d = make(
        "fairvaluegap",
        "g",
        vec![sample_px(25.0, 150.0)],
        None,
        None,
    );
    let canvas = paint_one_drawing(d.as_ref(), &bars_projector());
    let mut e = clip_open(1000.0, 600.0);
    e.extend(stroked(
        "rgba(255,255,255,0.35)",
        1.0,
        vec![4.0, 3.0],
        seg((25.5, 0.0), (25.5, 600.0)),
    ));
    e.push(json!(["restore"]));
    assert_stream(&canvas, e);
}
